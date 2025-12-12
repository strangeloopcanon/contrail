use crate::parse::{extract_text, parse_timestamp_value};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

#[derive(Debug, Clone)]
pub struct ParsedLine {
    pub role: String,
    pub content: String,
    pub timestamp: Option<DateTime<Utc>>,
    pub project_context: Option<String>,
    pub metadata: Map<String, Value>,
}

pub fn parse_codex_line(raw: &str) -> Option<ParsedLine> {
    let json = serde_json::from_str::<Value>(raw).ok()?;
    let mut metadata = Map::new();

    let project_context = json
        .pointer("/payload/cwd")
        .or_else(|| json.pointer("/turn_context/cwd"))
        .or_else(|| json.pointer("/cwd"))
        .and_then(Value::as_str)
        .map(|s| s.to_string());

    if let Some(cwd) = project_context.as_ref() {
        metadata.insert("cwd".to_string(), Value::String(cwd.clone()));
    }

    if let Some(model) = json.pointer("/payload/model").and_then(Value::as_str) {
        metadata.insert("model".to_string(), Value::String(model.to_string()));
    }

    if let Some(info) = json.pointer("/payload/info") {
        append_usage(&mut metadata, info);
    }
    if let Some(usage) = json.pointer("/payload/usage") {
        append_usage(&mut metadata, usage);
    }
    if let Some(metrics) = json.pointer("/payload/metrics") {
        append_metrics(&mut metadata, metrics);
    }
    if let Some(metrics) = json.pointer("/metrics") {
        append_metrics(&mut metadata, metrics);
    }

    let timestamp = json
        .get("timestamp")
        .or_else(|| json.get("created_at"))
        .or_else(|| json.get("createdAt"))
        .and_then(parse_timestamp_value);
    if let Some(ts) = timestamp.as_ref() {
        metadata.insert(
            "original_timestamp".to_string(),
            Value::String(ts.to_rfc3339()),
        );
    }

    let role = json
        .pointer("/interaction/role")
        .or_else(|| json.pointer("/payload/message/role"))
        .or_else(|| json.pointer("/payload/role"))
        .or_else(|| json.pointer("/role"))
        .and_then(Value::as_str)
        .unwrap_or("assistant")
        .to_string();

    let content_value = json
        .pointer("/interaction/content")
        .or_else(|| json.pointer("/payload/message/content"))
        .or_else(|| json.pointer("/payload/content"))
        .or_else(|| json.pointer("/message/content"))
        .or_else(|| json.get("content"));

    let content = content_value
        .and_then(extract_text)
        .unwrap_or_else(|| raw.to_string());

    if content.trim().is_empty() {
        return None;
    }

    Some(ParsedLine {
        role,
        content,
        timestamp,
        project_context,
        metadata,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_codex_line() {
        let raw = r#"{
            "timestamp": "2025-12-01T10:00:00Z",
            "payload": {
                "cwd": "/tmp/project",
                "model": "gpt-5",
                "message": { "role": "assistant", "content": "hello" },
                "info": { "totalTokens": 10 }
            }
        }"#;

        let parsed = parse_codex_line(raw).expect("should parse");
        assert_eq!(parsed.role, "assistant");
        assert_eq!(parsed.content, "hello");
        assert_eq!(parsed.project_context.as_deref(), Some("/tmp/project"));
        assert!(parsed.timestamp.is_some());
        assert_eq!(
            parsed.metadata.get("model").and_then(Value::as_str),
            Some("gpt-5")
        );
        assert_eq!(
            parsed
                .metadata
                .get("usage_total_tokens")
                .and_then(Value::as_i64),
            Some(10)
        );
    }
}
