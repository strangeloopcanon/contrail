use anyhow::Result;
use chrono::Utc;
use glob::glob;
use scrapers::claude::parse_claude_line;
use scrapers::codex::parse_codex_line;
use scrapers::config::ContrailConfig;
use scrapers::sentry::Sentry;
use scrapers::types::{Interaction, MasterLog};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use uuid::Uuid;
use xxhash_rust::xxh3::xxh3_64;

#[tokio::main]
async fn main() -> Result<()> {
    println!("✈️  Contrail History Importer");
    println!("Scanning for historical logs...");

    let config = ContrailConfig::from_env()?;
    let log_file_path = config.log_path.clone();

    // Open Master Log for appending
    let mut master_log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file_path)?;

    let mut count = 0;

    let sentry = Sentry::new();
    let mut existing_keys = load_existing_keys(&log_file_path)?;

    // 1. Import Codex Sessions
    let codex_pattern = config.codex_root.join("**/*.jsonl");
    if let Some(pattern_str) = codex_pattern.to_str() {
        for entry in glob(pattern_str)? {
            match entry {
                Ok(path) => {
                    if let Ok(c) =
                        import_codex_file(&path, &mut master_log, &sentry, &mut existing_keys)
                    {
                        count += c;
                    }
                }
                Err(e) => println!("Error reading glob entry: {:?}", e),
            }
        }
    }

    // 2. Import Claude History
    let claude_path = config.claude_history.clone();
    if claude_path.exists()
        && let Ok(c) =
            import_claude_file(&claude_path, &mut master_log, &sentry, &mut existing_keys)
    {
        count += c;
    }

    println!("✅ Import complete! Imported {} events.", count);
    Ok(())
}

fn import_codex_file(
    path: &Path,
    writer: &mut fs::File,
    sentry: &Sentry,
    existing: &mut HashSet<String>,
) -> Result<usize> {
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut count = 0;

    for line in reader.lines() {
        let line = line?;
        let mut metadata = serde_json::Map::new();
        metadata.insert("imported".to_string(), serde_json::Value::Bool(true));

        let mut role = "assistant".to_string();
        let mut content = line.clone();
        let mut project_context = "Imported History".to_string();
        let mut timestamp = None;

        if let Some(parsed) = parse_codex_line(&line) {
            role = parsed.role;
            content = parsed.content;
            timestamp = parsed.timestamp;
            if let Some(ctx) = parsed.project_context {
                project_context = ctx.clone();
            }
            for (k, v) in parsed.metadata {
                metadata.insert(k, v);
            }
        } else if let Ok(parsed) = serde_json::from_str::<Value>(&line)
            && let Some(ts) = extract_timestamp(&parsed)
        {
            timestamp = Some(ts);
        }

        let (content, flags) = sentry.scan_and_redact(&content);

        let session_id = path.file_name().unwrap().to_str().unwrap().to_string();
        let key = dedupe_key("codex-cli", &session_id, &content);
        if existing.contains(&key) {
            continue;
        }
        existing.insert(key);

        let log = MasterLog {
            event_id: Uuid::new_v4(),
            timestamp: timestamp.unwrap_or_else(Utc::now),
            source_tool: "codex-cli".to_string(),
            project_context,
            session_id,
            interaction: Interaction {
                role,
                content,
                artifacts: None,
            },
            security_flags: flags,
            metadata: serde_json::Value::Object(metadata),
        };

        if log.validate_schema().is_ok() {
            writeln!(writer, "{}", serde_json::to_string(&log)?)?;
            count += 1;
        }
    }
    Ok(count)
}

fn import_claude_file(
    path: &Path,
    writer: &mut fs::File,
    sentry: &Sentry,
    existing: &mut HashSet<String>,
) -> Result<usize> {
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut count = 0;

    for line in reader.lines() {
        let line = line?;
        let mut metadata = serde_json::Map::new();
        metadata.insert("imported".to_string(), serde_json::Value::Bool(true));

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
        } else if let Ok(parsed) = serde_json::from_str::<Value>(&line)
            && let Some(ts) = extract_timestamp(&parsed)
        {
            timestamp = Some(ts);
        }

        let (content, flags) = sentry.scan_and_redact(&content);
        let key = dedupe_key("claude-code", &session_id, &content);
        if existing.contains(&key) {
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
            metadata: serde_json::Value::Object(metadata),
        };

        if log.validate_schema().is_ok() {
            writeln!(writer, "{}", serde_json::to_string(&log)?)?;
            count += 1;
        }
    }
    Ok(count)
}

fn extract_timestamp(value: &Value) -> Option<chrono::DateTime<Utc>> {
    let as_str = value
        .get("timestamp")
        .or_else(|| value.get("created_at"))
        .or_else(|| value.get("createdAt"))
        .and_then(|v| v.as_str());

    if let Some(ts) = as_str
        && let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts)
    {
        return Some(dt.with_timezone(&Utc));
    }

    if let Some(ts) = value
        .get("created_at")
        .or_else(|| value.get("createdAt"))
        .or_else(|| value.get("timestamp"))
        .and_then(|v| v.as_i64())
    {
        return chrono::DateTime::<Utc>::from_timestamp(ts, 0);
    }

    None
}

fn load_existing_keys(path: &Path) -> Result<HashSet<String>> {
    let mut keys = HashSet::new();
    if !path.exists() {
        return Ok(keys);
    }
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line?;
        if let Ok(json) = serde_json::from_str::<Value>(&line) {
            let source = json
                .get("source_tool")
                .and_then(Value::as_str)
                .unwrap_or("");
            let session = json.get("session_id").and_then(Value::as_str).unwrap_or("");
            let content = json
                .pointer("/interaction/content")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !source.is_empty() && !session.is_empty() {
                keys.insert(dedupe_key(source, session, content));
            }
        }
    }
    Ok(keys)
}

fn dedupe_key(source: &str, session: &str, content: &str) -> String {
    let hash = xxh3_64(content.as_bytes());
    format!("{source}:{session}:{hash}")
}
