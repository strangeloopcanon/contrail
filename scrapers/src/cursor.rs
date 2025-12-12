use crate::parse::parse_timestamp_value;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde_json::{Map, Value};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;
use uuid::Uuid;

const MAX_CONTENT_CHARS: usize = 4000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorMessage {
    pub role: String,
    pub content: String,
    pub metadata: Map<String, Value>,
}

pub fn timestamp_from_metadata(meta: &Map<String, Value>) -> Option<DateTime<Utc>> {
    for key in ["timestamp", "createdAt", "updatedAt"] {
        if let Some(v) = meta.get(key) {
            if let Some(ts) = parse_timestamp_value(v) {
                return Some(ts);
            }
        }
    }
    None
}

pub fn read_cursor_messages(db_path: &Path) -> Result<Vec<CursorMessage>> {
    let temp_path = std::env::temp_dir().join(format!("cursor_dump_{}.db", Uuid::new_v4()));
    fs::copy(db_path, &temp_path).context("failed to copy Cursor DB")?;

    let conn = Connection::open(&temp_path).context("failed to open Cursor DB snapshot")?;
    let mut stmt = conn.prepare(
        "SELECT key, value FROM ItemTable WHERE key LIKE '%chat%' OR key LIKE '%composer%'",
    )?;
    let rows = stmt.query_map([], |row| {
        let key: String = row.get(0)?;
        let raw: Vec<u8> = row
            .get(1)
            .or_else(|_| row.get::<_, String>(1).map(|s| s.into_bytes()))?;
        Ok((key, raw))
    })?;

    let mut messages = Vec::new();
    for row in rows {
        let (key, raw) = row?;
        if raw.is_empty() {
            continue;
        }

        let raw_string = String::from_utf8_lossy(&raw).into_owned();

        if let Ok(value) = serde_json::from_str::<Value>(&raw_string) {
            let parsed = parse_cursor_value(&value);
            if !parsed.is_empty() {
                messages.extend(parsed);
                continue;
            }
        }

        let trimmed = trim_content(&raw_string);
        if !trimmed.is_empty() {
            messages.push(CursorMessage {
                role: "assistant".to_string(),
                content: format!("{key}: {trimmed}"),
                metadata: Map::new(),
            });
        }
    }

    let _ = fs::remove_file(&temp_path);
    Ok(messages)
}

fn parse_cursor_value(value: &Value) -> Vec<CursorMessage> {
    let mut messages = Vec::new();

    match value {
        Value::Array(items) => {
            for item in items {
                if let Some(message) = parse_message(item) {
                    messages.push(message);
                } else {
                    messages.extend(parse_cursor_value(item));
                }
            }
        }
        Value::Object(obj) => {
            if let Some(message_value) = obj.get("messages") {
                messages.extend(parse_cursor_value(message_value));
            } else if obj.contains_key("role") || obj.contains_key("content") {
                if let Some(message) = parse_message(value) {
                    messages.push(message);
                }
            }
        }
        _ => {}
    }

    messages
}

fn parse_message(value: &Value) -> Option<CursorMessage> {
    let obj = value.as_object()?;
    let role = obj
        .get("role")
        .and_then(|r| r.as_str())
        .unwrap_or("assistant")
        .to_string();
    let metadata = extract_metadata(obj);

    if let Some(content) = obj.get("content") {
        if let Some(text) = extract_text_from_content(content) {
            return Some(CursorMessage {
                role,
                content: trim_content(&text),
                metadata,
            });
        }
    }

    if let Some(text) = obj.get("text").and_then(|t| t.as_str()) {
        return Some(CursorMessage {
            role,
            content: trim_content(text),
            metadata,
        });
    }

    None
}

fn extract_text_from_content(content: &Value) -> Option<String> {
    match content {
        Value::String(s) => Some(s.to_string()),
        Value::Array(items) => {
            let mut parts = Vec::new();
            for item in items {
                if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                    parts.push(text.to_string());
                } else if let Some(text) = item.as_str() {
                    parts.push(text.to_string());
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(""))
            }
        }
        Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(|t| t.as_str()) {
                return Some(text.to_string());
            }
            if let Some(nested) = map.get("content") {
                return extract_text_from_content(nested);
            }
            None
        }
        _ => None,
    }
}

fn trim_content(content: &str) -> String {
    let mut trimmed = String::new();
    for c in content.chars().take(MAX_CONTENT_CHARS) {
        trimmed.push(c);
    }
    trimmed
}

pub fn fingerprint(messages: &[CursorMessage]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for message in messages {
        message.role.hash(&mut hasher);
        message.content.hash(&mut hasher);
    }
    hasher.finish()
}

fn extract_metadata(obj: &Map<String, Value>) -> Map<String, Value> {
    let mut meta = Map::new();
    let allowed_scalar_keys = [
        "id",
        "messageId",
        "createdAt",
        "updatedAt",
        "timestamp",
        "model",
        "provider",
        "source",
        "temperature",
        "topP",
        "stopReason",
        "finishReason",
        "parentId",
    ];

    for key in allowed_scalar_keys {
        if let Some(value) = obj.get(key) {
            insert_scalar(&mut meta, key, value);
        }
    }

    extract_usage(obj, &mut meta);
    extract_metrics(obj, &mut meta);
    extract_tools(obj, &mut meta);

    meta
}

fn extract_usage(obj: &Map<String, Value>, meta: &mut Map<String, Value>) {
    let candidates = ["usage", "tokenCount", "token_count"];
    for key in candidates {
        if let Some(Value::Object(usage)) = obj.get(key) {
            for (k, v) in usage {
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
}

fn extract_metrics(obj: &Map<String, Value>, meta: &mut Map<String, Value>) {
    let candidates = ["metrics", "stats"];
    for key in candidates {
        if let Some(Value::Object(metrics)) = obj.get(key) {
            for (k, v) in metrics {
                match k.as_str() {
                    "latencyMs" | "latency" => insert_scalar(meta, "latency_ms", v),
                    "durationMs" | "duration" => insert_scalar(meta, "duration_ms", v),
                    "wallTimeMs" | "wallTime" => insert_scalar(meta, "wall_time_ms", v),
                    _ => {}
                }
            }
        }
    }
}

fn extract_tools(obj: &Map<String, Value>, meta: &mut Map<String, Value>) {
    let candidates = ["toolCalls", "tool_calls"];
    for key in candidates {
        if let Some(Value::Array(calls)) = obj.get(key) {
            meta.insert(
                "tool_call_count".to_string(),
                Value::Number((calls.len() as u64).into()),
            );
            if let Some(Value::Object(first)) = calls.first() {
                if let Some(name) = first
                    .get("name")
                    .or_else(|| first.get("toolName"))
                    .and_then(|v| v.as_str())
                {
                    insert_scalar(
                        meta,
                        "tool_call_first_name",
                        &Value::String(name.to_string()),
                    );
                }
            }
        }
    }
}

fn insert_scalar(meta: &mut Map<String, Value>, key: &str, value: &Value) {
    match value {
        Value::String(s) => {
            meta.insert(key.to_string(), Value::String(trim_metadata_str(s)));
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

fn trim_metadata_str(s: &str) -> String {
    let max = 256;
    if s.chars().count() > max {
        s.chars().take(max).collect()
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn parses_messages_from_state_db() -> Result<()> {
        let path = std::env::temp_dir().join(format!("cursor_state_test_{}.db", Uuid::new_v4()));
        let conn = Connection::open(&path)?;
        conn.execute(
            "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value BLOB)",
            [],
        )?;

        let payload = r#"
        {
            "messages": [
                {"role": "user", "content": [{"text": "build me a widget"}]},
                {"role": "assistant", "content": "here is the widget"}
            ]
        }"#;
        conn.execute(
            "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
            ("chatSessions", payload),
        )?;

        let messages = read_cursor_messages(&path)?;
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert!(messages[0].content.contains("build me a widget"));
        assert_eq!(messages[1].role, "assistant");
        assert!(messages[1].content.contains("here is the widget"));

        let _ = fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn fingerprint_is_stable() {
        let messages = vec![
            CursorMessage {
                role: "user".to_string(),
                content: "hi".to_string(),
                metadata: Map::new(),
            },
            CursorMessage {
                role: "assistant".to_string(),
                content: "hello".to_string(),
                metadata: Map::new(),
            },
        ];

        let first = fingerprint(&messages);
        let second = fingerprint(&messages);
        assert_eq!(first, second);
    }

    #[test]
    fn extracts_metadata_fields() -> Result<()> {
        let value = serde_json::json!({
            "role": "assistant",
            "content": "reply",
            "model": "gpt-5",
            "provider": "openai",
            "createdAt": 1712345678,
            "usage": { "totalTokens": 1234, "promptTokens": 234, "completionTokens": 1000 },
            "metrics": { "latencyMs": 250 },
            "toolCalls": [ { "name": "bash", "arguments": "{}" } ],
            "temperature": 0.2
        });

        let parsed = parse_cursor_value(&value);
        assert_eq!(parsed.len(), 1);
        let meta = &parsed[0].metadata;
        assert_eq!(meta.get("model").and_then(|v| v.as_str()), Some("gpt-5"));
        assert_eq!(
            meta.get("provider").and_then(|v| v.as_str()),
            Some("openai")
        );
        assert_eq!(
            meta.get("createdAt").and_then(|v| v.as_i64()),
            Some(1712345678)
        );
        assert_eq!(
            meta.get("usage_total_tokens").and_then(|v| v.as_i64()),
            Some(1234)
        );
        assert_eq!(
            meta.get("usage_prompt_tokens").and_then(|v| v.as_i64()),
            Some(234)
        );
        assert_eq!(
            meta.get("usage_completion_tokens").and_then(|v| v.as_i64()),
            Some(1000)
        );
        assert_eq!(meta.get("latency_ms").and_then(|v| v.as_i64()), Some(250));
        assert_eq!(
            meta.get("tool_call_count").and_then(|v| v.as_u64()),
            Some(1)
        );
        assert_eq!(
            meta.get("tool_call_first_name").and_then(|v| v.as_str()),
            Some("bash")
        );
        assert_eq!(meta.get("temperature").and_then(|v| v.as_f64()), Some(0.2));
        Ok(())
    }
}
