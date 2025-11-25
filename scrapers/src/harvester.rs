use crate::cursor::{fingerprint, read_cursor_messages};
use crate::log_writer::LogWriter;
use crate::notifier::Notifier;
use crate::sentry::Sentry;
use crate::types::{Interaction, MasterLog};
use anyhow::{Context, Result};
use chrono::{Datelike, Local, Utc};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::time::Duration;
use tokio::time::{sleep, Instant};
use uuid::Uuid;

pub struct Harvester {
    sentry: Sentry,
    notifier: Notifier,
    log_writer: LogWriter,
}

impl Harvester {
    pub fn new(log_writer: LogWriter) -> Self {
        Self {
            sentry: Sentry::new(),
            notifier: Notifier::new(),
            log_writer,
        }
    }

    pub async fn run_cursor_watcher(&self) -> Result<()> {
        println!("Starting Universal Cursor Watcher...");
        let home = dirs::home_dir().context("Could not find home directory")?;
        let cursor_base = home.join("Library/Application Support/Cursor/User/workspaceStorage");

        if !cursor_base.exists() {
            println!("Cursor workspaceStorage not found.");
            return Ok(());
        }

        let (tx, rx) = channel();
        // Recursive watch on the root storage folder
        let mut watcher = RecommendedWatcher::new(tx, Config::default())?;
        if let Err(e) = watcher.watch(&cursor_base, RecursiveMode::Recursive) {
            println!("Failed to watch Cursor DB: {:?}", e);
            return Ok(());
        }

        println!("Watching all Cursor workspaces at {:?}", cursor_base);

        let mut last_activity = Instant::now();
        let mut generating = false;
        let mut active_project = "Unknown".to_string();
        let mut workspace_hash = "unknown_hash".to_string();
        let mut last_state_path: Option<PathBuf> = None;
        let mut last_cursor_snapshot: Option<u64> = None;

        loop {
            if let Ok(res) = rx.try_recv() {
                match res {
                    Ok(event) => {
                        // Check if the changed file is state.vscdb
                        let mut is_db_change = false;

                        for path in event.paths {
                            if path.file_name().and_then(|s| s.to_str()) == Some("state.vscdb") {
                                is_db_change = true;
                                last_state_path = Some(path.clone());
                                // Get Workspace Hash (Parent Dir Name)
                                if let Some(parent) = path.parent() {
                                    if let Some(hash) = parent.file_name().and_then(|s| s.to_str())
                                    {
                                        workspace_hash = hash.to_string();
                                    }

                                    // Try to resolve project name from workspace.json in parent dir
                                    let workspace_json = parent.join("workspace.json");
                                    if let Ok(content) = fs::read_to_string(&workspace_json) {
                                        // Parse as JSON to get folder path
                                        if let Ok(json) =
                                            serde_json::from_str::<serde_json::Value>(&content)
                                        {
                                            if let Some(folder) =
                                                json.get("folder").and_then(|v| v.as_str())
                                            {
                                                // Remove file:// prefix if present
                                                active_project =
                                                    folder.replace("file://", "").to_string();
                                                // Decode URL encoding if needed (simple version)
                                                active_project = active_project.replace("%20", " ");
                                            } else if let Some(name) =
                                                json.get("name").and_then(|v| v.as_str())
                                            {
                                                active_project = name.to_string();
                                            }
                                        }
                                    }
                                }
                                break;
                            }
                        }

                        if is_db_change {
                            last_activity = Instant::now();
                            if !generating {
                                generating = true;
                                println!(
                                    "Cursor active in project: {} ({})",
                                    active_project, workspace_hash
                                );
                            }
                        }
                    }
                    Err(e) => println!("Watch error: {:?}", e),
                }
            }

            // Check silence
            if generating && last_activity.elapsed() > Duration::from_secs(5) {
                generating = false;
                println!("Cursor finished generating in {}.", active_project);
                self.notifier.send_notification(
                    "AI Task Complete",
                    &format!("Cursor finished in {}", active_project),
                );

                let mut extra_metadata = serde_json::Map::new();

                if let Some(db_path) = last_state_path.as_ref() {
                    match read_cursor_messages(db_path) {
                        Ok(messages) if !messages.is_empty() => {
                            let message_count = messages.len();
                            let snapshot = fingerprint(&messages);
                            if Some(snapshot) != last_cursor_snapshot {
                                for message in messages {
                                    self.log_interaction_with_metadata(
                                        "cursor",
                                        &workspace_hash,
                                        &active_project,
                                        &message.content,
                                        &message.role,
                                        message.metadata,
                                    )
                                    .await?;
                                }
                                extra_metadata.insert(
                                    "cursor_message_count".to_string(),
                                    serde_json::json!(message_count),
                                );
                                last_cursor_snapshot = Some(snapshot);
                            } else {
                                println!("Cursor snapshot unchanged; skipping duplicate log write");
                            }
                        }
                        Ok(_) => {
                            println!("Cursor state snapshot contained no chat messages.");
                        }
                        Err(e) => {
                            eprintln!("Failed to read Cursor state: {:?}", e);
                        }
                    }
                }

                // Capture Git Context & Effects
                if let Ok(repo) = std::process::Command::new("git")
                    .arg("-C")
                    .arg(&active_project)
                    .arg("rev-parse")
                    .arg("--abbrev-ref")
                    .arg("HEAD")
                    .output()
                {
                    if let Ok(branch) = String::from_utf8(repo.stdout) {
                        extra_metadata.insert(
                            "git_branch".to_string(),
                            serde_json::Value::String(branch.trim().to_string()),
                        );
                    }
                }

                // Capture File Effects (What changed?)
                if let Ok(status) = std::process::Command::new("git")
                    .arg("-C")
                    .arg(&active_project)
                    .arg("status")
                    .arg("--short")
                    .output()
                {
                    if let Ok(changes) = String::from_utf8(status.stdout) {
                        let effects: Vec<String> = changes.lines().map(|s| s.to_string()).collect();
                        if !effects.is_empty() {
                            extra_metadata
                                .insert("file_effects".to_string(), serde_json::json!(effects));
                        }
                    }
                }

                self.log_interaction_with_metadata(
                    "cursor",
                    &workspace_hash, // Use Hash as Session ID
                    &active_project,
                    "Cursor generation completed (Text capture pending DB schema analysis)",
                    "assistant",
                    extra_metadata,
                )
                .await?;
            }

            sleep(Duration::from_millis(100)).await;
        }
    }

    pub async fn run_codex_watcher(&self) -> Result<()> {
        println!("Starting Codex Watcher...");
        let home = dirs::home_dir().context("Could not find home directory")?;
        let codex_root = home.join(".codex/sessions");
        let mut file_positions: HashMap<PathBuf, u64> = HashMap::new();
        let mut file_activity: HashMap<PathBuf, Instant> = HashMap::new();

        loop {
            let now = Local::now();
            let date_path = codex_root.join(format!(
                "{}/{:02}/{:02}",
                now.year(),
                now.month(),
                now.day()
            ));

            if date_path.exists() {
                for entry in fs::read_dir(&date_path)? {
                    let entry = entry?;
                    let path = entry.path();
                    if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                        file_positions.entry(path.clone()).or_insert_with(|| {
                            fs::File::open(&path)
                                .ok()
                                .and_then(|f| {
                                    let mut r = BufReader::new(f);
                                    r.seek(SeekFrom::End(0)).ok()
                                })
                                .unwrap_or(0)
                        });
                    }
                }

                let mut to_remove = Vec::new();
                for (path, pos) in file_positions.iter_mut() {
                    if let Ok(file) = fs::File::open(path) {
                        let mut reader = BufReader::new(file);
                        reader.seek(SeekFrom::Start(*pos))?;
                        let mut line = String::new();
                        let mut saw_token_count = false;
                        let mut generating = false;

                        while reader.read_line(&mut line)? > 0 {
                            let len = line.len() as u64;
                            let mut project_context = "Codex Session".to_string();
                            let mut extra_metadata = Map::new();

                            if let Ok(json) = serde_json::from_str::<Value>(&line) {
                                if let Some(payload) = json.get("payload") {
                                    if let Some(cwd) = payload.get("cwd").and_then(|s| s.as_str()) {
                                        project_context = cwd.to_string();
                                        extra_metadata.insert(
                                            "cwd".to_string(),
                                            Value::String(project_context.clone()),
                                        );
                                    }
                                    if let Some(model) =
                                        payload.get("model").and_then(|s| s.as_str())
                                    {
                                        extra_metadata.insert(
                                            "model".to_string(),
                                            Value::String(model.to_string()),
                                        );
                                    }
                                    if let Some(info) = payload.get("info") {
                                        append_usage(&mut extra_metadata, info);
                                        saw_token_count = true;
                                    }
                                    if let Some(metrics) = payload.get("metrics") {
                                        append_metrics(&mut extra_metadata, metrics);
                                    }
                                }
                                if let Some(turn_context) = json.get("turn_context") {
                                    if let Some(cwd) =
                                        turn_context.get("cwd").and_then(|s| s.as_str())
                                    {
                                        project_context = cwd.to_string();
                                        extra_metadata.insert(
                                            "cwd".to_string(),
                                            Value::String(project_context.clone()),
                                        );
                                    }
                                }
                                if let Some(ts) = extract_timestamp_value(&json) {
                                    extra_metadata
                                        .insert("timestamp".to_string(), Value::String(ts));
                                }
                            }

                            self.log_interaction_with_metadata(
                                "codex-cli",
                                path.file_name().unwrap().to_str().unwrap(),
                                &project_context,
                                &line,
                                "assistant",
                                extra_metadata,
                            )
                            .await?;

                            *pos += len;
                            line.clear();
                            generating = true;
                            file_activity.insert(path.clone(), Instant::now());
                        }

                        if generating {
                            if let Some(last) = file_activity.get(path) {
                                if last.elapsed() > Duration::from_secs(3) {
                                    let mut completion_metadata = Map::new();
                                    completion_metadata.insert(
                                        "interrupted".to_string(),
                                        Value::Bool(!saw_token_count),
                                    );
                                    self.notifier.send_notification(
                                        "AI Task Complete",
                                        "Codex CLI finished.",
                                    );
                                    self.log_interaction_with_metadata(
                                        "codex-cli",
                                        path.file_name().unwrap().to_str().unwrap(),
                                        "Codex Session",
                                        "Session Ended",
                                        "system",
                                        completion_metadata,
                                    )
                                    .await?;
                                }
                            }
                        }
                    } else {
                        to_remove.push(path.clone());
                    }
                }

                for path in to_remove {
                    file_positions.remove(&path);
                    file_activity.remove(&path);
                }
            }

            sleep(Duration::from_secs(2)).await;
        }
    }
    pub async fn run_antigravity_watcher(&self) -> Result<()> {
        println!("Starting Antigravity Watcher...");
        let home = dirs::home_dir().context("Could not find home directory")?;
        let brain_dir = home.join(".gemini/antigravity/brain");

        // Watch the brain directory for the latest session
        loop {
            let mut latest_session = None;
            let mut latest_time = std::time::SystemTime::UNIX_EPOCH;

            if brain_dir.exists() {
                for entry in fs::read_dir(&brain_dir)? {
                    let entry = entry?;
                    if entry.file_type()?.is_dir() {
                        if let Ok(metadata) = entry.metadata() {
                            if let Ok(modified) = metadata.modified() {
                                if modified > latest_time {
                                    latest_time = modified;
                                    latest_session = Some(entry.path());
                                }
                            }
                        }
                    }
                }
            }

            if let Some(session_path) = latest_session {
                let task_md = session_path.join("task.md");
                let plan_md = session_path.join("implementation_plan.md");

                // Watch both files
                let (tx, rx) = channel();
                let mut watcher = RecommendedWatcher::new(tx, Config::default())?;

                let mut watching = false;
                if task_md.exists() {
                    let _ = watcher.watch(&task_md, RecursiveMode::NonRecursive);
                    watching = true;
                }
                if plan_md.exists() {
                    let _ = watcher.watch(&plan_md, RecursiveMode::NonRecursive);
                    watching = true;
                }

                if watching {
                    println!("Watching Antigravity Session: {:?}", session_path);
                    let mut last_task_size = 0;
                    let mut last_plan_size = 0;

                    // Init sizes
                    if let Ok(m) = fs::metadata(&task_md) {
                        last_task_size = m.len();
                    }
                    if let Ok(m) = fs::metadata(&plan_md) {
                        last_plan_size = m.len();
                    }

                    loop {
                        if let Ok(Ok(_event)) = rx.try_recv() {
                            // Check task.md
                            if let Ok(metadata) = fs::metadata(&task_md) {
                                let current_size = metadata.len();
                                if current_size > last_task_size {
                                    self.log_interaction(
                                        "antigravity",
                                        session_path.file_name().unwrap().to_str().unwrap(),
                                        "Antigravity Brain",
                                        "Task updated",
                                        "assistant",
                                    )
                                    .await?;
                                    last_task_size = current_size;
                                }
                            }
                            // Check implementation_plan.md
                            if let Ok(metadata) = fs::metadata(&plan_md) {
                                let current_size = metadata.len();
                                if current_size > last_plan_size {
                                    self.log_interaction(
                                        "antigravity",
                                        session_path.file_name().unwrap().to_str().unwrap(),
                                        "Antigravity Brain",
                                        "Implementation Plan updated",
                                        "assistant",
                                    )
                                    .await?;
                                    last_plan_size = current_size;
                                }
                            }
                        }
                        sleep(Duration::from_millis(500)).await;

                        // Check for newer sessions occasionally
                        if Utc::now().timestamp() % 10 == 0 {
                            if let Ok(entries) = fs::read_dir(&brain_dir) {
                                for entry in entries.flatten() {
                                    if let Ok(meta) = entry.metadata() {
                                        if let Ok(mod_time) = meta.modified() {
                                            if mod_time > latest_time {
                                                println!(
                                                    "Found newer Antigravity session, switching..."
                                                );
                                                break; // Break inner loop
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            sleep(Duration::from_secs(5)).await;
        }
    }

    pub async fn run_claude_watcher(&self) -> Result<()> {
        println!("Starting Claude Watcher...");
        let home = dirs::home_dir().context("Could not find home directory")?;
        let claude_history = home.join(".claude/history.jsonl");

        if claude_history.exists() {
            println!("Watching Claude History: {:?}", claude_history);
            let file = fs::File::open(&claude_history)?;
            let mut reader = BufReader::new(file);
            let mut pos = reader.seek(SeekFrom::End(0))?;

            let mut last_activity = Instant::now();
            let mut generating = false;
            let mut cwd_cache: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();

            loop {
                let current_len = fs::metadata(&claude_history)?.len();
                if current_len > pos {
                    reader.seek(SeekFrom::Start(pos))?;
                    let mut line = String::new();
                    while reader.read_line(&mut line)? > 0 {
                        println!("New Claude line: {}", line.trim());
                        let mut metadata = Map::new();
                        let mut project_context = "Claude Global".to_string();
                        let mut role = "user_or_assistant".to_string();
                        let mut session_id = "history".to_string();
                        if let Ok(json) = serde_json::from_str::<Value>(&line) {
                            if let Some(conv) = json.get("conversation_id").and_then(|c| c.as_str())
                            {
                                session_id = conv.to_string();
                                metadata.insert(
                                    "conversation_id".to_string(),
                                    Value::String(conv.to_string()),
                                );
                            }
                            if let Some(model) = json.get("model").and_then(|m| m.as_str()) {
                                metadata
                                    .insert("model".to_string(), Value::String(model.to_string()));
                            }
                            if let Some(usage) = json.get("usage") {
                                append_usage(&mut metadata, usage);
                            }
                            if let Some(metrics) = json.get("metrics") {
                                append_metrics(&mut metadata, metrics);
                            }
                            if let Some(r) = json.get("role").and_then(|r| r.as_str()) {
                                role = r.to_string();
                            }
                            if let Some(cwd) = extract_claude_cwd(&json) {
                                project_context = cwd.clone();
                                metadata.insert("cwd".to_string(), Value::String(cwd.clone()));
                                cwd_cache.insert(session_id.clone(), cwd);
                            } else if let Some(cached) = cwd_cache.get(&session_id) {
                                project_context = cached.clone();
                                metadata.insert("cwd".to_string(), Value::String(cached.clone()));
                            } else if let Some(conv) =
                                json.get("conversation_id").and_then(|c| c.as_str())
                            {
                                project_context = conv.to_string();
                            }
                            if let Some(ts) = extract_timestamp_value(&json) {
                                metadata.insert("timestamp".to_string(), Value::String(ts));
                            }
                        }
                        self.log_interaction_with_metadata(
                            "claude-code",
                            &session_id,
                            &project_context,
                            &line,
                            &role, // Claude history might mix roles
                            metadata,
                        )
                        .await?;

                        pos += line.len() as u64;
                        line.clear();
                        last_activity = Instant::now();
                        if !generating {
                            generating = true;
                        }
                    }
                }

                if generating && last_activity.elapsed() > Duration::from_secs(5) {
                    generating = false;
                    self.notifier
                        .send_notification("AI Task Complete", "Claude Code finished.");
                }

                sleep(Duration::from_millis(500)).await;
            }
        } else {
            println!("Claude history not found at {:?}", claude_history);
        }
        Ok(())
    }

    async fn log_interaction(
        &self,
        source: &str,
        session: &str,
        project: &str,
        content: &str,
        role: &str,
    ) -> Result<()> {
        self.log_interaction_with_metadata(
            source,
            session,
            project,
            content,
            role,
            serde_json::Map::new(),
        )
        .await
    }

    async fn log_interaction_with_metadata(
        &self,
        source: &str,
        session: &str,
        project: &str,
        content: &str,
        role: &str,
        extra_metadata: serde_json::Map<String, serde_json::Value>,
    ) -> Result<()> {
        let (clean_content, flags) = self.sentry.scan_and_redact(content);

        let mut metadata = serde_json::Map::new();
        metadata.insert(
            "user".to_string(),
            serde_json::Value::String(whoami::username()),
        );
        metadata.insert(
            "hostname".to_string(),
            serde_json::Value::String(whoami::devicename()),
        );

        // Check Clipboard for leaks (did user copy this?)
        if role == "assistant" {
            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                if let Ok(clip_text) = clipboard.get_text() {
                    // Simple heuristic: if clipboard contains a significant chunk of the content
                    // or if content is short and matches exactly.
                    let threshold = 20; // min chars to check
                    if clean_content.len() > threshold
                        && clip_text.contains(&clean_content[..threshold])
                    {
                        metadata.insert(
                            "copied_to_clipboard".to_string(),
                            serde_json::Value::Bool(true),
                        );
                    } else if clean_content == clip_text {
                        metadata.insert(
                            "copied_to_clipboard".to_string(),
                            serde_json::Value::Bool(true),
                        );
                    }
                }
            }
        }

        // Merge extra metadata
        for (k, v) in extra_metadata {
            metadata.insert(k, v);
        }

        let log = MasterLog {
            event_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            source_tool: source.to_string(),
            project_context: project.to_string(),
            session_id: session.to_string(),
            interaction: Interaction {
                role: role.to_string(),
                content: clean_content,
                artifacts: None,
            },
            security_flags: flags,
            metadata: serde_json::Value::Object(metadata),
        };

        log.validate_schema()?;
        self.log_writer.write(log)?;
        Ok(())
    }
}

fn append_usage(meta: &mut Map<String, Value>, value: &Value) {
    if let Some(obj) = value.as_object() {
        for (k, v) in obj {
            match k.as_str() {
                "total" | "total_tokens" | "totalTokens" => {
                    insert_scalar(meta, "usage_total_tokens", v)
                }
                "prompt" | "prompt_tokens" | "promptTokens" | "input" => {
                    insert_scalar(meta, "usage_prompt_tokens", v)
                }
                "completion" | "completion_tokens" | "completionTokens" | "output" => {
                    insert_scalar(meta, "usage_completion_tokens", v)
                }
                _ => {}
            }
        }
    }
}

fn append_metrics(meta: &mut Map<String, Value>, value: &Value) {
    if let Some(obj) = value.as_object() {
        for (k, v) in obj {
            match k.as_str() {
                "latency" | "latencyMs" | "latency_ms" => insert_scalar(meta, "latency_ms", v),
                "duration" | "durationMs" | "duration_ms" => insert_scalar(meta, "duration_ms", v),
                "wallTime" | "wall_time_ms" => insert_scalar(meta, "wall_time_ms", v),
                _ => {}
            }
        }
    }
}

fn insert_scalar(meta: &mut Map<String, Value>, key: &str, value: &Value) {
    match value {
        Value::String(s) => {
            meta.insert(key.to_string(), Value::String(s.clone()));
        }
        Value::Number(n) => {
            meta.insert(key.to_string(), Value::Number(n.clone()));
        }
        Value::Bool(b) => {
            meta.insert(key.to_string(), Value::Bool(*b));
        }
        _ => {}
    }
}

fn extract_timestamp_value(json: &Value) -> Option<String> {
    json.get("timestamp")
        .or_else(|| json.get("created_at"))
        .or_else(|| json.get("createdAt"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn extract_claude_cwd(json: &Value) -> Option<String> {
    let candidate_keys = [
        "cwd",
        "working_dir",
        "workdir",
        "project_root",
        "path",
        "root",
    ];

    if let Some(obj) = json.as_object() {
        for key in candidate_keys {
            if let Some(val) = obj.get(key).and_then(|v| v.as_str()) {
                if looks_like_path(val) {
                    return Some(val.to_string());
                }
            }
        }
        if let Some(tool_use) = obj.get("tool_use").and_then(|v| v.as_object()) {
            if let Some(args) = tool_use.get("arguments").and_then(|v| v.as_str()) {
                // naive scan for a path-like token
                if let Some(pos) = args.find("/Users/") {
                    let snippet = &args[pos..];
                    if let Some(end) = snippet.find('"') {
                        let path = &snippet[..end];
                        if looks_like_path(path) {
                            return Some(path.to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

fn looks_like_path(val: &str) -> bool {
    val.starts_with('/') && val.len() > 4
}
