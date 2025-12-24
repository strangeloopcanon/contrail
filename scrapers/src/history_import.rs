use crate::claude::{parse_claude_line, parse_claude_session_line};
use crate::codex::parse_codex_line;
use crate::config::ContrailConfig;
use crate::cursor::{read_cursor_messages, timestamp_from_metadata};
use crate::parse::parse_timestamp_value;
use crate::sentry::Sentry;
use crate::types::{Interaction, MasterLog};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;
use walkdir::WalkDir;

const MAX_ANTIGRAVITY_CHARS: usize = 20_000;

#[derive(Debug, Default, Clone)]
pub struct ImportStats {
    pub imported: usize,
    pub skipped: usize,
    pub errors: usize,
}

pub fn import_history(config: &ContrailConfig) -> Result<ImportStats> {
    let mut stats = ImportStats::default();

    if let Some(dir) = config.log_path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("create log dir {dir:?}"))?;
    }

    let mut existing = load_existing_keys(&config.log_path)?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&config.log_path)
        .with_context(|| format!("open master log at {:?}", config.log_path))?;
    let mut writer = std::io::BufWriter::new(&mut file);
    let sentry = Sentry::new();

    if config.enable_codex {
        import_codex_root(
            &config.codex_root,
            &mut writer,
            &sentry,
            &mut existing,
            &mut stats,
        )?;
    }
    if config.enable_claude {
        import_claude_file(
            &config.claude_history,
            &mut writer,
            &sentry,
            &mut existing,
            &mut stats,
        )?;
        // Also import detailed session files from claude projects directory
        import_claude_projects_root(
            &config.claude_projects,
            &mut writer,
            &sentry,
            &mut existing,
            &mut stats,
        )?;
    }
    if config.enable_cursor {
        import_cursor_root(
            &config.cursor_storage,
            &mut writer,
            &sentry,
            &mut existing,
            &mut stats,
        )?;
    }
    if config.enable_antigravity {
        import_antigravity_root(
            &config.antigravity_brain,
            &mut writer,
            &sentry,
            &mut existing,
            &mut stats,
        )?;
    }

    writer.flush().context("flush master log writer")?;
    Ok(stats)
}

fn import_codex_root(
    root: &Path,
    writer: &mut dyn Write,
    sentry: &Sentry,
    existing: &mut HashSet<u64>,
    stats: &mut ImportStats,
) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }

    let mut files: Vec<PathBuf> = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("jsonl"))
        .map(|e| e.path().to_path_buf())
        .collect();

    files.sort();

    for path in files {
        if let Err(e) = import_codex_file(&path, writer, sentry, existing, stats) {
            eprintln!("import codex file failed: {:?}: {e}", path);
            stats.errors += 1;
        }
    }
    Ok(())
}

fn import_codex_file(
    path: &Path,
    writer: &mut dyn Write,
    sentry: &Sentry,
    existing: &mut HashSet<u64>,
    stats: &mut ImportStats,
) -> Result<()> {
    let file = fs::File::open(path).with_context(|| format!("open codex file {path:?}"))?;
    let reader = BufReader::new(file);

    let session_id = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    let mut session_start_ts: Option<DateTime<Utc>> = None;
    let mut last_ts: Option<DateTime<Utc>> = None;
    let mut wrote_session_start = false;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                stats.errors += 1;
                eprintln!("read line failed: {e}");
                continue;
            }
        };

        let parsed_json = serde_json::from_str::<Value>(&line).ok();
        if let Some(value) = parsed_json.as_ref() {
            if is_codex_session_header(value) {
                if let Some(ts) = extract_timestamp(value) {
                    session_start_ts = Some(ts);
                    last_ts = Some(ts);
                }
                continue;
            }
        }

        let mut metadata = Map::new();
        metadata.insert("imported".to_string(), Value::Bool(true));

        let mut role = "assistant".to_string();
        let mut content = line.clone();
        let mut project_context = "Imported History".to_string();
        let mut timestamp = None;

        if let Some(parsed) = parse_codex_line(&line) {
            role = parsed.role;
            content = parsed.content;
            timestamp = parsed.timestamp;
            if let Some(ctx) = parsed.project_context {
                project_context = ctx;
            }
            for (k, v) in parsed.metadata {
                metadata.insert(k, v);
            }
        } else if let Some(value) = parsed_json.as_ref() {
            timestamp = extract_timestamp(value);
        }

        if !wrote_session_start {
            if let Some(session_start) = session_start_ts.as_ref() {
                metadata.insert(
                    "session_started_at".to_string(),
                    Value::String(session_start.to_rfc3339()),
                );
                wrote_session_start = true;
            }
        }

        let ts = match timestamp {
            Some(ts) => ts,
            None => {
                metadata.insert("timestamp_inferred".to_string(), Value::Bool(true));
                last_ts
                    .map(|t| t + chrono::Duration::milliseconds(1))
                    .unwrap_or_else(Utc::now)
            }
        };
        last_ts = Some(ts);

        let (content, flags) = sentry.scan_and_redact(&content);

        let key = dedupe_key("codex-cli", &session_id, &content);
        if existing.contains(&key) {
            stats.skipped += 1;
            continue;
        }
        existing.insert(key);

        let log = MasterLog {
            event_id: Uuid::new_v4(),
            timestamp: ts,
            source_tool: "codex-cli".to_string(),
            project_context,
            session_id: session_id.clone(),
            interaction: Interaction {
                role,
                content,
                artifacts: None,
            },
            security_flags: flags,
            metadata: Value::Object(metadata),
        };

        if log.validate_schema().is_ok() {
            writeln!(writer, "{}", serde_json::to_string(&log)?)?;
            stats.imported += 1;
        } else {
            stats.errors += 1;
        }
    }

    Ok(())
}

fn is_codex_session_header(value: &Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    if !obj.contains_key("id") || !obj.contains_key("timestamp") {
        return false;
    }
    if obj.contains_key("type") || obj.contains_key("role") || obj.contains_key("content") {
        return false;
    }
    true
}

fn import_claude_file(
    path: &Path,
    writer: &mut dyn Write,
    sentry: &Sentry,
    existing: &mut HashSet<u64>,
    stats: &mut ImportStats,
) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let file = fs::File::open(path).with_context(|| format!("open claude file {path:?}"))?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                stats.errors += 1;
                eprintln!("read line failed: {e}");
                continue;
            }
        };

        let mut metadata = Map::new();
        metadata.insert("imported".to_string(), Value::Bool(true));

        let mut role = "user_or_assistant".to_string();
        let mut content = line.clone();
        let mut project_context = "Claude Global".to_string();
        let mut session_id = "history".to_string();
        let mut timestamp = None;

        if let Some(parsed) = parse_claude_line(&line) {
            role = parsed.role;
            content = parsed.content;
            timestamp = parsed.timestamp;
            let parsed_session_id = parsed.session_id.clone();
            if let Some(id) = parsed_session_id.as_ref() {
                session_id = id.clone();
            }
            if let Some(ctx) = parsed.project_context {
                project_context = ctx;
            } else if let Some(id) = parsed_session_id {
                project_context = id;
            }
            for (k, v) in parsed.metadata {
                metadata.insert(k, v);
            }
        } else if let Ok(parsed) = serde_json::from_str::<Value>(&line) {
            timestamp = extract_timestamp(&parsed);
        }

        let (content, flags) = sentry.scan_and_redact(&content);

        let key = dedupe_key("claude-code", &session_id, &content);
        if existing.contains(&key) {
            stats.skipped += 1;
            continue;
        }
        existing.insert(key);

        let log = MasterLog {
            event_id: Uuid::new_v4(),
            timestamp: timestamp.unwrap_or_else(Utc::now),
            source_tool: "claude-code".to_string(),
            project_context,
            session_id,
            interaction: Interaction {
                role,
                content,
                artifacts: None,
            },
            security_flags: flags,
            metadata: Value::Object(metadata),
        };

        if log.validate_schema().is_ok() {
            writeln!(writer, "{}", serde_json::to_string(&log)?)?;
            stats.imported += 1;
        } else {
            stats.errors += 1;
        }
    }

    Ok(())
}

/// Import Claude Code project session files from ~/.claude/projects/*/*.jsonl
/// These contain detailed token usage information.
fn import_claude_projects_root(
    projects_dir: &Path,
    writer: &mut dyn Write,
    sentry: &Sentry,
    existing: &mut HashSet<u64>,
    stats: &mut ImportStats,
) -> Result<()> {
    if !projects_dir.exists() {
        return Ok(());
    }

    // Iterate through project directories
    for project_entry in fs::read_dir(projects_dir)? {
        let project_entry = project_entry?;
        let project_path = project_entry.path();
        if !project_path.is_dir() {
            continue;
        }

        // Find all .jsonl session files in this project
        for session_entry in fs::read_dir(&project_path)? {
            let session_entry = session_entry?;
            let session_path = session_entry.path();
            if session_path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }

            if let Err(e) = import_claude_session_file(&session_path, writer, sentry, existing, stats) {
                eprintln!("import claude session file failed: {:?}: {e}", session_path);
                stats.errors += 1;
            }
        }
    }

    Ok(())
}

fn import_claude_session_file(
    path: &Path,
    writer: &mut dyn Write,
    sentry: &Sentry,
    existing: &mut HashSet<u64>,
    stats: &mut ImportStats,
) -> Result<()> {
    let file = fs::File::open(path).with_context(|| format!("open claude session file {path:?}"))?;
    let reader = BufReader::new(file);

    let default_session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                stats.errors += 1;
                eprintln!("read line failed: {e}");
                continue;
            }
        };

        let Some(parsed) = parse_claude_session_line(&line) else {
            continue;
        };

        let mut metadata = parsed.metadata;
        metadata.insert("imported".to_string(), Value::Bool(true));

        let session_id = parsed.session_id.unwrap_or_else(|| default_session_id.clone());
        let project_context = parsed.project_context.unwrap_or_else(|| "Claude Session".to_string());

        let (content, flags) = sentry.scan_and_redact(&parsed.content);

        let key = dedupe_key("claude-code", &session_id, &content);
        if existing.contains(&key) {
            stats.skipped += 1;
            continue;
        }
        existing.insert(key);

        let log = MasterLog {
            event_id: Uuid::new_v4(),
            timestamp: parsed.timestamp.unwrap_or_else(Utc::now),
            source_tool: "claude-code".to_string(),
            project_context,
            session_id,
            interaction: Interaction {
                role: parsed.role,
                content,
                artifacts: None,
            },
            security_flags: flags,
            metadata: Value::Object(metadata),
        };

        if log.validate_schema().is_ok() {
            writeln!(writer, "{}", serde_json::to_string(&log)?)?;
            stats.imported += 1;
        } else {
            stats.errors += 1;
        }
    }

    Ok(())
}

fn import_cursor_root(
    root: &Path,
    writer: &mut dyn Write,
    sentry: &Sentry,
    existing: &mut HashSet<u64>,
    stats: &mut ImportStats,
) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }

    let mut dbs: Vec<PathBuf> = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.file_name() == "state.vscdb")
        .map(|e| e.path().to_path_buf())
        .collect();

    dbs.sort();

    for db_path in dbs {
        if let Err(e) = import_cursor_db(&db_path, writer, sentry, existing, stats) {
            eprintln!("import cursor db failed: {:?}: {e}", db_path);
            stats.errors += 1;
        }
    }

    Ok(())
}

fn import_cursor_db(
    db_path: &Path,
    writer: &mut dyn Write,
    sentry: &Sentry,
    existing: &mut HashSet<u64>,
    stats: &mut ImportStats,
) -> Result<()> {
    let workspace_dir = db_path.parent().context("cursor db path missing parent")?;
    let session_id = workspace_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    let project_context = resolve_cursor_project_context(workspace_dir)
        .unwrap_or_else(|| workspace_dir.to_string_lossy().to_string());

    let messages = read_cursor_messages(db_path)?;
    if messages.is_empty() {
        return Ok(());
    }

    let base_ts = db_path
        .metadata()
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(system_time_to_utc)
        .unwrap_or_else(Utc::now);
    let mut last_ts: Option<DateTime<Utc>> = None;

    for message in messages {
        let mut metadata = Map::new();
        metadata.insert("imported".to_string(), Value::Bool(true));
        metadata.insert(
            "cursor_workspace_hash".to_string(),
            Value::String(session_id.clone()),
        );

        for (k, v) in message.metadata {
            metadata.insert(k, v);
        }

        let ts = match timestamp_from_metadata(&metadata) {
            Some(ts) => {
                last_ts = Some(ts);
                ts
            }
            None => {
                metadata.insert("timestamp_inferred".to_string(), Value::Bool(true));
                let inferred = last_ts
                    .map(|t| t + chrono::Duration::milliseconds(1))
                    .unwrap_or(base_ts);
                last_ts = Some(inferred);
                inferred
            }
        };

        let (content, flags) = sentry.scan_and_redact(&message.content);
        let key = dedupe_key("cursor", &session_id, &content);
        if existing.contains(&key) {
            stats.skipped += 1;
            continue;
        }
        existing.insert(key);

        let log = MasterLog {
            event_id: Uuid::new_v4(),
            timestamp: ts,
            source_tool: "cursor".to_string(),
            project_context: project_context.clone(),
            session_id: session_id.clone(),
            interaction: Interaction {
                role: message.role,
                content,
                artifacts: None,
            },
            security_flags: flags,
            metadata: Value::Object(metadata),
        };

        if log.validate_schema().is_ok() {
            writeln!(writer, "{}", serde_json::to_string(&log)?)?;
            stats.imported += 1;
        } else {
            stats.errors += 1;
        }
    }

    Ok(())
}

fn resolve_cursor_project_context(workspace_dir: &Path) -> Option<String> {
    let workspace_json_path = workspace_dir.join("workspace.json");
    let content = fs::read_to_string(&workspace_json_path).ok()?;
    let value = serde_json::from_str::<Value>(&content).ok()?;

    if let Some(folder) = value.get("folder").and_then(Value::as_str) {
        return Some(folder.replace("file://", "").replace("%20", " "));
    }
    value
        .get("name")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
}

fn import_antigravity_root(
    brain_dir: &Path,
    writer: &mut dyn Write,
    sentry: &Sentry,
    existing: &mut HashSet<u64>,
    stats: &mut ImportStats,
) -> Result<()> {
    if !brain_dir.exists() {
        return Ok(());
    }

    let mut sessions: Vec<PathBuf> = fs::read_dir(brain_dir)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().ok().is_some_and(|t| t.is_dir()))
        .map(|entry| entry.path())
        .collect();
    sessions.sort();

    for session_path in sessions {
        if let Err(e) = import_antigravity_session(&session_path, writer, sentry, existing, stats) {
            eprintln!("import antigravity session failed: {:?}: {e}", session_path);
            stats.errors += 1;
        }
    }

    Ok(())
}

fn import_antigravity_session(
    session_dir: &Path,
    writer: &mut dyn Write,
    sentry: &Sentry,
    existing: &mut HashSet<u64>,
    stats: &mut ImportStats,
) -> Result<()> {
    let session_id = session_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    let mut image_ext_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    let mut image_count = 0usize;
    let mut total_files = 0usize;
    let mut total_bytes = 0u64;

    for entry in fs::read_dir(session_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        total_files += 1;
        if let Ok(meta) = entry.metadata() {
            total_bytes = total_bytes.saturating_add(meta.len());
        }
        if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
            let ext = ext.to_ascii_lowercase();
            if matches!(
                ext.as_str(),
                "png" | "jpg" | "jpeg" | "webp" | "gif" | "svg"
            ) {
                image_count += 1;
                *image_ext_counts.entry(ext).or_insert(0) += 1;
            }
        }
    }

    // Write a per-session summary event for artifacts.
    let mut summary_meta = Map::new();
    summary_meta.insert("imported".to_string(), Value::Bool(true));
    summary_meta.insert(
        "antigravity_total_files".to_string(),
        Value::Number((total_files as u64).into()),
    );
    summary_meta.insert(
        "antigravity_total_bytes".to_string(),
        Value::Number(total_bytes.into()),
    );
    summary_meta.insert(
        "antigravity_image_count".to_string(),
        Value::Number((image_count as u64).into()),
    );
    summary_meta.insert(
        "antigravity_image_exts".to_string(),
        serde_json::to_value(&image_ext_counts).unwrap_or(Value::Object(Map::new())),
    );

    let summary_content = format!(
        "Antigravity session summary: images={image_count}, files={total_files}, bytes={total_bytes}"
    );
    let (summary_content, summary_flags) = sentry.scan_and_redact(&summary_content);
    let summary_key = dedupe_key("antigravity", &session_id, &summary_content);
    if !existing.contains(&summary_key) {
        existing.insert(summary_key);

        let summary_log = MasterLog {
            event_id: Uuid::new_v4(),
            timestamp: session_dir
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(system_time_to_utc)
                .unwrap_or_else(Utc::now),
            source_tool: "antigravity".to_string(),
            project_context: "Antigravity Brain".to_string(),
            session_id: session_id.clone(),
            interaction: Interaction {
                role: "system".to_string(),
                content: summary_content,
                artifacts: None,
            },
            security_flags: summary_flags,
            metadata: Value::Object(summary_meta),
        };

        if summary_log.validate_schema().is_ok() {
            writeln!(writer, "{}", serde_json::to_string(&summary_log)?)?;
            stats.imported += 1;
        } else {
            stats.errors += 1;
        }
    } else {
        stats.skipped += 1;
    }

    // Import text artifacts (prefer *.md.resolved when present).
    let mut md_files: Vec<PathBuf> = fs::read_dir(session_dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| path.extension().and_then(|s| s.to_str()) == Some("md"))
        .collect();
    md_files.sort();

    for md_path in md_files {
        let import_path = pick_antigravity_text_variant(&md_path);
        if let Err(e) = import_antigravity_file(
            &session_id,
            &md_path,
            &import_path,
            writer,
            sentry,
            existing,
            stats,
        ) {
            eprintln!("import antigravity artifact failed: {:?}: {e}", md_path);
            stats.errors += 1;
        }
    }

    Ok(())
}

fn pick_antigravity_text_variant(base: &Path) -> PathBuf {
    let resolved = PathBuf::from(format!("{}.resolved", base.display()));
    if resolved.exists() {
        return resolved;
    }
    base.to_path_buf()
}

fn import_antigravity_file(
    session_id: &str,
    base_path: &Path,
    import_path: &Path,
    writer: &mut dyn Write,
    sentry: &Sentry,
    existing: &mut HashSet<u64>,
    stats: &mut ImportStats,
) -> Result<()> {
    let file_name = base_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("artifact.md")
        .to_string();

    let mut metadata = Map::new();
    metadata.insert("imported".to_string(), Value::Bool(true));
    metadata.insert(
        "antigravity_file".to_string(),
        Value::String(file_name.clone()),
    );
    metadata.insert(
        "antigravity_variant".to_string(),
        Value::String(
            import_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&file_name)
                .to_string(),
        ),
    );

    let meta_json = read_antigravity_metadata(base_path);
    if let Some(meta_json) = meta_json.as_ref() {
        if let Some(obj) = meta_json.as_object() {
            if let Some(artifact_type) = obj.get("artifactType").and_then(Value::as_str) {
                metadata.insert(
                    "antigravity_artifact_type".to_string(),
                    Value::String(artifact_type.to_string()),
                );
            }
            if let Some(summary) = obj.get("summary").and_then(Value::as_str) {
                metadata.insert(
                    "antigravity_artifact_summary".to_string(),
                    Value::String(summary.to_string()),
                );
            }
        }
        metadata.insert("antigravity_metadata".to_string(), meta_json.clone());
    }

    let mut timestamp = meta_json
        .as_ref()
        .and_then(|v| v.get("updatedAt"))
        .and_then(parse_timestamp_value);

    if timestamp.is_none() {
        timestamp = base_path
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(system_time_to_utc);
    }
    let timestamp = timestamp.unwrap_or_else(Utc::now);

    let raw = fs::read_to_string(import_path)?;
    let body = trim_chars(&raw, MAX_ANTIGRAVITY_CHARS);
    if body.trim().is_empty() {
        return Ok(());
    }

    let content = format!("Antigravity artifact: {file_name}\n\n{body}");
    let (content, flags) = sentry.scan_and_redact(&content);

    let key = dedupe_key("antigravity", session_id, &content);
    if existing.contains(&key) {
        stats.skipped += 1;
        return Ok(());
    }
    existing.insert(key);

    let log = MasterLog {
        event_id: Uuid::new_v4(),
        timestamp,
        source_tool: "antigravity".to_string(),
        project_context: "Antigravity Brain".to_string(),
        session_id: session_id.to_string(),
        interaction: Interaction {
            role: "assistant".to_string(),
            content,
            artifacts: None,
        },
        security_flags: flags,
        metadata: Value::Object(metadata),
    };

    if log.validate_schema().is_ok() {
        writeln!(writer, "{}", serde_json::to_string(&log)?)?;
        stats.imported += 1;
    } else {
        stats.errors += 1;
    }

    Ok(())
}

fn read_antigravity_metadata(base_path: &Path) -> Option<Value> {
    let meta_path = PathBuf::from(format!("{}.metadata.json", base_path.display()));
    let raw = fs::read_to_string(meta_path).ok()?;
    serde_json::from_str::<Value>(&raw).ok()
}

fn system_time_to_utc(t: std::time::SystemTime) -> Option<DateTime<Utc>> {
    let duration = t.duration_since(std::time::UNIX_EPOCH).ok()?;
    DateTime::<Utc>::from_timestamp(duration.as_secs() as i64, duration.subsec_nanos())
}

fn trim_chars(input: &str, max_chars: usize) -> String {
    input.chars().take(max_chars).collect()
}

fn extract_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    let as_str = value
        .get("timestamp")
        .or_else(|| value.get("created_at"))
        .or_else(|| value.get("createdAt"))
        .and_then(|v| v.as_str());

    if let Some(ts) = as_str {
        if let Ok(dt) = DateTime::parse_from_rfc3339(ts) {
            return Some(dt.with_timezone(&Utc));
        }
    }

    let as_i64 = value
        .get("created_at")
        .or_else(|| value.get("createdAt"))
        .or_else(|| value.get("timestamp"))
        .and_then(|v| v.as_i64());
    as_i64.and_then(|n| DateTime::<Utc>::from_timestamp(n, 0))
}

fn load_existing_keys(path: &Path) -> Result<HashSet<u64>> {
    let mut keys = HashSet::new();
    if !path.exists() {
        return Ok(keys);
    }
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line?;
        let Ok(json) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let source = json
            .get("source_tool")
            .and_then(Value::as_str)
            .unwrap_or("");
        let session = json.get("session_id").and_then(Value::as_str).unwrap_or("");
        let content = json
            .pointer("/interaction/content")
            .and_then(Value::as_str)
            .unwrap_or("");
        if source.is_empty() || session.is_empty() {
            continue;
        }
        keys.insert(dedupe_key(source, session, content));
    }
    Ok(keys)
}

fn dedupe_key(source: &str, session: &str, content: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut h);
    session.hash(&mut h);
    content.hash(&mut h);
    h.finish()
}
