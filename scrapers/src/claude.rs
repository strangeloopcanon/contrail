use crate::parse::{extract_text, parse_timestamp_value};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

#[derive(Debug, Clone)]
pub struct ParsedLine {
    pub role: String,
    pub content: String,
    pub timestamp: Option<DateTime<Utc>>,
    pub session_id: Option<String>,
    pub project_context: Option<String>,
    pub metadata: Map<String, Value>,
}

pub fn parse_claude_line(raw: &str) -> Option<ParsedLine> {
    let json = serde_json::from_str::<Value>(raw).ok()?;
    let mut metadata = Map::new();

    let session_id = json
        .get("conversation_id")
        .or_else(|| json.get("conversationId"))
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    if let Some(id) = session_id.as_ref() {
        metadata.insert("conversation_id".to_string(), Value::String(id.clone()));
    }

    if let Some(model) = json.get("model").and_then(Value::as_str) {
        metadata.insert("model".to_string(), Value::String(model.to_string()));
    }

    if let Some(usage) = json.get("usage") {
        append_usage(&mut metadata, usage);
    }
    if let Some(metrics) = json.get("metrics") {
        append_metrics(&mut metadata, metrics);
    }

    let project_context = extract_cwd(&json);
    if let Some(cwd) = project_context.as_ref() {
        metadata.insert("cwd".to_string(), Value::String(cwd.clone()));
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
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("user_or_assistant")
        .to_string();

    let content_value = json
        .get("content")
        .or_else(|| json.pointer("/message/content"))
        .or_else(|| json.pointer("/payload/content"));

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
        session_id,
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

fn extract_cwd(json: &Value) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_claude_line() {
        let raw = r#"{
            "created_at": "2025-12-01T10:00:00Z",
            "conversation_id": "conv-1",
            "role": "user",
            "content": "hi",
            "cwd": "/tmp/project",
            "usage": { "totalTokens": 3 }
        }"#;

        let parsed = parse_claude_line(raw).expect("should parse");
        assert_eq!(parsed.role, "user");
        assert_eq!(parsed.content, "hi");
        assert_eq!(parsed.session_id.as_deref(), Some("conv-1"));
        assert_eq!(parsed.project_context.as_deref(), Some("/tmp/project"));
        assert!(parsed.timestamp.is_some());
        assert_eq!(
            parsed
                .metadata
                .get("usage_total_tokens")
                .and_then(Value::as_i64),
            Some(3)
        );
    }
}
