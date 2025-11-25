use anyhow::{Context, Result};
use chrono::DateTime;
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

fn main() -> Result<()> {
    let home = dirs::home_dir().context("Could not find home directory")?;
    let input = home.join(".contrail/logs/master_log.jsonl");
    let output = PathBuf::from("export/curated_dataset.jsonl");

    std::fs::create_dir_all(output.parent().unwrap())?;
    let reader = BufReader::new(File::open(&input)?);
    let mut writer = File::create(&output)?;
    let mut kept = 0usize;
    let mut seen_sessions = std::collections::HashSet::new();

    for line in reader.lines() {
        let line = line?;
        let Ok(mut json) = serde_json::from_str::<Value>(&line) else {
            continue;
        };

        if !matches!(
            json.get("source_tool").and_then(Value::as_str),
            Some("codex-cli" | "cursor" | "claude-code" | "antigravity")
        ) {
            continue;
        }

        // Drop giant content to avoid blowing up training data
        if let Some(content) = json.pointer("/interaction/content").and_then(Value::as_str) {
            if content.len() > 10_000 {
                continue;
            }
        }

        // Truncate metadata blobs (e.g., function_call_output)
        if let Some(obj) = json.get_mut("metadata").and_then(Value::as_object_mut) {
            if let Some(Value::String(s)) = obj.get_mut("function_call_output") {
                if s.len() > 2000 {
                    s.truncate(2000);
                }
            }
        }

        // Ensure timestamp is RFC3339
        if let Some(ts) = json.get("timestamp").and_then(Value::as_str) {
            if DateTime::parse_from_rfc3339(ts).is_err() {
                json.as_object_mut().map(|o| o.remove("timestamp"));
            }
        }

        // Deduplicate identical session_id + content hashes
        let session = json
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let content_hash = json
            .get("interaction")
            .and_then(|i| i.get("content"))
            .and_then(Value::as_str)
            .map(|c| xxhash_rust::xxh3::xxh3_64(c.as_bytes()))
            .unwrap_or(0);
        let key = format!("{session}:{content_hash}");
        if seen_sessions.contains(&key) {
            continue;
        }
        seen_sessions.insert(key);

        serde_json::to_writer(&mut writer, &json)?;
        writer.write_all(b"\n")?;
        kept += 1;
    }

    println!("Exported {} curated entries to {:?}", kept, output);
    Ok(())
}
