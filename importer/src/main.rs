use anyhow::{Context, Result};
use chrono::Utc;
use glob::glob;
use scrapers::sentry::Sentry;
use scrapers::types::{Interaction, MasterLog};
use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<()> {
    println!("✈️  Contrail History Importer");
    println!("Scanning for historical logs...");

    let home = dirs::home_dir().context("Could not find home directory")?;
    let log_file_path = home.join(".contrail/logs/master_log.jsonl");

    // Open Master Log for appending
    let mut master_log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file_path)?;

    let mut count = 0;

    let sentry = Sentry::new();

    // 1. Import Codex Sessions
    let codex_pattern = home.join(".codex/sessions/**/*.jsonl");
    if let Some(pattern_str) = codex_pattern.to_str() {
        for entry in glob(pattern_str)? {
            match entry {
                Ok(path) => {
                    if let Ok(c) = import_codex_file(&path, &mut master_log, &sentry) {
                        count += c;
                    }
                }
                Err(e) => println!("Error reading glob entry: {:?}", e),
            }
        }
    }

    // 2. Import Claude History
    let claude_path = home.join(".claude/history.jsonl");
    if claude_path.exists() {
        if let Ok(c) = import_claude_file(&claude_path, &mut master_log, &sentry) {
            count += c;
        }
    }

    println!("✅ Import complete! Imported {} events.", count);
    Ok(())
}

fn import_codex_file(path: &Path, writer: &mut fs::File, sentry: &Sentry) -> Result<usize> {
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut count = 0;

    for line in reader.lines() {
        let line = line?;
        let parsed =
            serde_json::from_str::<Value>(&line).unwrap_or_else(|_| Value::String(line.clone()));
        let (content, flags) = sentry.scan_and_redact(&line);
        let timestamp = extract_timestamp(&parsed).unwrap_or_else(Utc::now);

        let mut metadata = serde_json::Map::new();
        metadata.insert("imported".to_string(), serde_json::Value::Bool(true));
        if let Some(model) = parsed
            .get("payload")
            .and_then(|p| p.get("model"))
            .and_then(|m| m.as_str())
        {
            metadata.insert(
                "model".to_string(),
                serde_json::Value::String(model.to_string()),
            );
        }
        if let Some(cwd) = parsed
            .get("payload")
            .and_then(|p| p.get("cwd"))
            .and_then(|c| c.as_str())
        {
            metadata.insert(
                "cwd".to_string(),
                serde_json::Value::String(cwd.to_string()),
            );
        }

        let log = MasterLog {
            event_id: Uuid::new_v4(),
            timestamp,
            source_tool: "codex-cli".to_string(),
            project_context: metadata
                .get("cwd")
                .and_then(|c| c.as_str())
                .unwrap_or("Imported History")
                .to_string(),
            session_id: path.file_name().unwrap().to_str().unwrap().to_string(),
            interaction: Interaction {
                role: "assistant".to_string(),
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

fn import_claude_file(path: &Path, writer: &mut fs::File, sentry: &Sentry) -> Result<usize> {
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut count = 0;

    for line in reader.lines() {
        let line = line?;
        let parsed =
            serde_json::from_str::<Value>(&line).unwrap_or_else(|_| Value::String(line.clone()));
        let (content, flags) = sentry.scan_and_redact(&line);
        let timestamp = extract_timestamp(&parsed).unwrap_or_else(Utc::now);

        let log = MasterLog {
            event_id: Uuid::new_v4(),
            timestamp,
            source_tool: "claude-code".to_string(),
            project_context: parsed
                .get("conversation_id")
                .and_then(|c| c.as_str())
                .unwrap_or("Claude Global")
                .to_string(),
            session_id: "history".to_string(),
            interaction: Interaction {
                role: "user_or_assistant".to_string(),
                content,
                artifacts: None,
            },
            security_flags: flags,
            metadata: serde_json::json!({ "imported": true }),
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

    if let Some(ts) = as_str {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) {
            return Some(dt.with_timezone(&Utc));
        }
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
