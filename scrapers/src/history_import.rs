use crate::claude::parse_claude_line;
use crate::codex::parse_codex_line;
use crate::config::ContrailConfig;
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
        } else if let Ok(parsed) = serde_json::from_str::<Value>(&line) {
            timestamp = extract_timestamp(&parsed);
        }

        let (content, flags) = sentry.scan_and_redact(&content);

        let key = dedupe_key("codex-cli", &session_id, &content);
        if existing.contains(&key) {
            stats.skipped += 1;
            continue;
        }
        existing.insert(key);

        let log = MasterLog {
            event_id: Uuid::new_v4(),
            timestamp: timestamp.unwrap_or_else(Utc::now),
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
