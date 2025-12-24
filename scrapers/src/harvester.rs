use crate::claude::{parse_claude_line, parse_claude_session_line};
use crate::codex::parse_codex_line;
use crate::config::ContrailConfig;
use crate::cursor::{fingerprint, read_cursor_messages, timestamp_from_metadata};
use crate::log_writer::LogWriter;
use crate::notifier::Notifier;
use crate::sentry::Sentry;
use crate::types::{Interaction, MasterLog};
use anyhow::Result;
use chrono::DateTime;
use chrono::{Datelike, Local, Utc};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::time::Duration;
use tokio::time::{sleep, Instant};
use uuid::Uuid;

pub struct Harvester {
    sentry: Sentry,
    notifier: Notifier,
    log_writer: LogWriter,
    config: ContrailConfig,
}

impl Harvester {
    pub fn new(log_writer: LogWriter, config: ContrailConfig) -> Self {
        Self {
            sentry: Sentry::new(),
            notifier: Notifier::new(),
            log_writer,
            config,
        }
    }

    pub async fn run_cursor_watcher(&self) -> Result<()> {
        println!("Starting Universal Cursor Watcher...");
        let cursor_base = self.config.cursor_storage.clone();

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
            if generating
                && last_activity.elapsed() > Duration::from_secs(self.config.cursor_silence_secs)
            {
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
                                    let ts = timestamp_from_metadata(&message.metadata);
                                    self.log_interaction_with_metadata(
                                        "cursor",
                                        &workspace_hash,
                                        &active_project,
                                        &message.content,
                                        &message.role,
                                        message.metadata,
                                        ts,
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
                    "Session Ended",
                    "system",
                    extra_metadata,
                    Some(Utc::now()),
                )
                .await?;
            }

            sleep(Duration::from_millis(100)).await;
        }
    }

    pub async fn run_codex_watcher(&self) -> Result<()> {
        println!("Starting Codex Watcher...");
        let codex_root = self.config.codex_root.clone();
        let mut file_positions: HashMap<PathBuf, u64> = HashMap::new();
        let mut file_activity: HashMap<PathBuf, Instant> = HashMap::new();
        let mut file_generating: HashMap<PathBuf, bool> = HashMap::new();
        let mut file_saw_token_count: HashMap<PathBuf, bool> = HashMap::new();

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
                        let current_len = reader.get_ref().metadata().map(|m| m.len()).unwrap_or(0);
                        if current_len < *pos {
                            // file truncated/rotated
                            *pos = 0;
                        }
                        reader.seek(SeekFrom::Start(*pos))?;
                        let mut line = String::new();
                        let mut saw_token_count = *file_saw_token_count.get(path).unwrap_or(&false);

                        while reader.read_line(&mut line)? > 0 {
                            let len = line.len() as u64;
                            let mut project_context = "Codex Session".to_string();
                            let mut extra_metadata = Map::new();
                            let mut role = "assistant".to_string();
                            let mut content = line.clone();
                            let mut timestamp: Option<DateTime<Utc>> = None;

                            if let Some(parsed) = parse_codex_line(&line) {
                                if let Some(cwd) = parsed.project_context {
                                    project_context = cwd.clone();
                                }
                                role = parsed.role;
                                content = parsed.content;
                                timestamp = parsed.timestamp;
                                for (k, v) in parsed.metadata {
                                    if k.starts_with("usage_") {
                                        saw_token_count = true;
                                    }
                                    extra_metadata.insert(k, v);
                                }
                            }

                            self.log_interaction_with_metadata(
                                "codex-cli",
                                path.file_name().unwrap().to_str().unwrap(),
                                &project_context,
                                &content,
                                &role,
                                extra_metadata,
                                timestamp,
                            )
                            .await?;

                            *pos += len;
                            line.clear();
                            file_generating.insert(path.clone(), true);
                            file_activity.insert(path.clone(), Instant::now());
                        }

                        file_saw_token_count.insert(path.clone(), saw_token_count);
                    } else {
                        to_remove.push(path.clone());
                    }
                }

                // Session end detection across iterations
                for (path, last) in file_activity.clone() {
                    if !file_generating.get(&path).copied().unwrap_or(false) {
                        continue;
                    }
                    if last.elapsed() > Duration::from_secs(self.config.codex_silence_secs) {
                        let saw_tokens = file_saw_token_count.get(&path).copied().unwrap_or(false);
                        let mut completion_metadata = Map::new();
                        completion_metadata
                            .insert("interrupted".to_string(), Value::Bool(!saw_tokens));
                        self.notifier
                            .send_notification("AI Task Complete", "Codex CLI finished.");
                        self.log_interaction_with_metadata(
                            "codex-cli",
                            path.file_name().unwrap().to_str().unwrap(),
                            "Codex Session",
                            "Session Ended",
                            "system",
                            completion_metadata,
                            Some(Utc::now()),
                        )
                        .await?;
                        file_generating.insert(path.clone(), false);
                        file_saw_token_count.insert(path.clone(), false);
                    }
                }

                for path in to_remove {
                    file_positions.remove(&path);
                    file_activity.remove(&path);
                    file_generating.remove(&path);
                    file_saw_token_count.remove(&path);
                }
            }

            sleep(Duration::from_secs(2)).await;
        }
    }
    pub async fn run_antigravity_watcher(&self) -> Result<()> {
        println!("Starting Antigravity Watcher...");
        let brain_dir = self.config.antigravity_brain.clone();

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
                    let mut last_task_pos = fs::metadata(&task_md).map(|m| m.len()).unwrap_or(0);
                    let mut last_plan_pos = fs::metadata(&plan_md).map(|m| m.len()).unwrap_or(0);

                    loop {
                        if let Ok(Ok(_event)) = rx.try_recv() {
                            // Check task.md
                            if let Ok(metadata) = fs::metadata(&task_md) {
                                let current_size = metadata.len();
                                if current_size < last_task_pos {
                                    last_task_pos = 0;
                                }
                                if current_size > last_task_pos {
                                    if let Ok(mut file) = fs::File::open(&task_md) {
                                        let mut reader = BufReader::new(&mut file);
                                        let _ = reader.seek(SeekFrom::Start(last_task_pos));
                                        let mut buf = String::new();
                                        let _ = reader.read_to_string(&mut buf);
                                        if !buf.trim().is_empty() {
                                            self.log_interaction_with_metadata(
                                                "antigravity",
                                                session_path.file_name().unwrap().to_str().unwrap(),
                                                "Antigravity Brain",
                                                &buf,
                                                "assistant",
                                                Map::new(),
                                                Some(Utc::now()),
                                            )
                                            .await?;
                                        }
                                    }
                                    last_task_pos = current_size;
                                }
                            }
                            // Check implementation_plan.md
                            if let Ok(metadata) = fs::metadata(&plan_md) {
                                let current_size = metadata.len();
                                if current_size < last_plan_pos {
                                    last_plan_pos = 0;
                                }
                                if current_size > last_plan_pos {
                                    if let Ok(mut file) = fs::File::open(&plan_md) {
                                        let mut reader = BufReader::new(&mut file);
                                        let _ = reader.seek(SeekFrom::Start(last_plan_pos));
                                        let mut buf = String::new();
                                        let _ = reader.read_to_string(&mut buf);
                                        if !buf.trim().is_empty() {
                                            self.log_interaction_with_metadata(
                                                "antigravity",
                                                session_path.file_name().unwrap().to_str().unwrap(),
                                                "Antigravity Brain",
                                                &buf,
                                                "assistant",
                                                Map::new(),
                                                Some(Utc::now()),
                                            )
                                            .await?;
                                        }
                                    }
                                    last_plan_pos = current_size;
                                }
                            }
                        }
                        sleep(Duration::from_millis(500)).await;

                        // Check for newer sessions occasionally
                        if Utc::now().timestamp() % 10 == 0 {
                            if let Ok(entries) = fs::read_dir(&brain_dir) {
                                let mut found_newer = false;
                                for entry in entries.flatten() {
                                    if let Ok(meta) = entry.metadata() {
                                        if let Ok(mod_time) = meta.modified() {
                                            if mod_time > latest_time {
                                                println!(
                                                    "Found newer Antigravity session, switching..."
                                                );
                                                found_newer = true;
                                                break;
                                            }
                                        }
                                    }
                                }
                                if found_newer {
                                    break;
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
        let claude_history = self.config.claude_history.clone();

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
                if current_len < pos {
                    pos = 0;
                }
                if current_len > pos {
                    reader.seek(SeekFrom::Start(pos))?;
                    let mut line = String::new();
                    while reader.read_line(&mut line)? > 0 {
                        println!("New Claude line");
                        let mut metadata = Map::new();
                        let mut project_context = "Claude Global".to_string();
                        let mut role = "user_or_assistant".to_string();
                        let mut session_id = "history".to_string();
                        let mut content = line.clone();
                        let mut timestamp: Option<DateTime<Utc>> = None;

                        if let Some(parsed) = parse_claude_line(&line) {
                            role = parsed.role;
                            content = parsed.content;
                            timestamp = parsed.timestamp;
                            if let Some(id) = parsed.session_id {
                                session_id = id.clone();
                            }
                            if let Some(cwd) = parsed.project_context {
                                project_context = cwd.clone();
                                cwd_cache.insert(session_id.clone(), cwd);
                            } else if let Some(cached) = cwd_cache.get(&session_id) {
                                project_context = cached.clone();
                            }
                            for (k, v) in parsed.metadata {
                                metadata.insert(k, v);
                            }
                        }

                        self.log_interaction_with_metadata(
                            "claude-code",
                            &session_id,
                            &project_context,
                            &content,
                            &role,
                            metadata,
                            timestamp,
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

                if generating
                    && last_activity.elapsed()
                        > Duration::from_secs(self.config.claude_silence_secs)
                {
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

    /// Watch Claude Code's project session files for detailed token usage data.
    /// These files are located in ~/.claude/projects/*/*.jsonl
    pub async fn run_claude_projects_watcher(&self) -> Result<()> {
        println!("Starting Claude Projects Watcher...");
        let claude_projects = self.config.claude_projects.clone();

        if !claude_projects.exists() {
            println!("Claude projects directory not found at {:?}", claude_projects);
            return Ok(());
        }

        println!("Watching Claude projects at {:?}", claude_projects);

        // Track file positions for incremental reading
        let mut file_positions: HashMap<PathBuf, u64> = HashMap::new();
        let mut last_activity = Instant::now();
        let mut generating = false;

        loop {
            // Scan all project directories
            if let Ok(project_dirs) = fs::read_dir(&claude_projects) {
                for project_entry in project_dirs.flatten() {
                    let project_path = project_entry.path();
                    if !project_path.is_dir() {
                        continue;
                    }

                    // Scan for .jsonl files in this project
                    if let Ok(session_files) = fs::read_dir(&project_path) {
                        for session_entry in session_files.flatten() {
                            let session_path = session_entry.path();
                            if session_path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                                continue;
                            }

                            // Initialize position if new file
                            let pos = file_positions.entry(session_path.clone()).or_insert(0);

                            // Read new content
                            if let Ok(file) = fs::File::open(&session_path) {
                                let mut reader = BufReader::new(file);
                                let current_len = reader.get_ref().metadata().map(|m| m.len()).unwrap_or(0);

                                if current_len < *pos {
                                    // File truncated/rotated
                                    *pos = 0;
                                }

                                if current_len > *pos {
                                    if reader.seek(SeekFrom::Start(*pos)).is_err() {
                                        continue;
                                    }

                                    let mut line = String::new();
                                    while reader.read_line(&mut line).unwrap_or(0) > 0 {
                                        let len = line.len() as u64;

                                        if let Some(parsed) = parse_claude_session_line(&line) {
                                            let project_context = parsed
                                                .project_context
                                                .clone()
                                                .unwrap_or_else(|| "Claude Session".to_string());

                                            let session_id = parsed
                                                .session_id
                                                .clone()
                                                .unwrap_or_else(|| {
                                                    session_path
                                                        .file_stem()
                                                        .and_then(|s| s.to_str())
                                                        .unwrap_or("unknown")
                                                        .to_string()
                                                });

                                            self.log_interaction_with_metadata(
                                                "claude-code",
                                                &session_id,
                                                &project_context,
                                                &parsed.content,
                                                &parsed.role,
                                                parsed.metadata,
                                                parsed.timestamp,
                                            )
                                            .await?;

                                            last_activity = Instant::now();
                                            if !generating {
                                                generating = true;
                                                println!(
                                                    "Claude Code active in project: {}",
                                                    project_context
                                                );
                                            }
                                        }

                                        *pos += len;
                                        line.clear();
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Session end detection
            if generating
                && last_activity.elapsed() > Duration::from_secs(self.config.claude_silence_secs)
            {
                generating = false;
                self.notifier
                    .send_notification("AI Task Complete", "Claude Code finished.");
            }

            sleep(Duration::from_secs(2)).await;
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn log_interaction_with_metadata(
        &self,
        source: &str,
        session: &str,
        project: &str,
        content: &str,
        role: &str,
        extra_metadata: serde_json::Map<String, serde_json::Value>,
        timestamp: Option<DateTime<Utc>>,
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
                    let copied = (clean_content.len() > threshold
                        && clip_text.contains(&clean_content[..threshold]))
                        || clean_content == clip_text;
                    if copied {
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
            timestamp: timestamp.unwrap_or_else(Utc::now),
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
