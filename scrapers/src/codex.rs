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

    let _ = append_token_count_usage(&mut metadata, &json);

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

    let role = derive_role_override(&json).unwrap_or(role);

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

fn derive_role_override(json: &Value) -> Option<String> {
    let record_type = json.get("type").and_then(Value::as_str)?;
    if record_type.eq_ignore_ascii_case("event_msg") {
        let payload_type = json.pointer("/payload/type").and_then(Value::as_str)?;
        if payload_type.eq_ignore_ascii_case("user_message") {
            return Some("user".to_string());
        }
        if payload_type.eq_ignore_ascii_case("agent_message") {
            return Some("assistant".to_string());
        }
        if payload_type.eq_ignore_ascii_case("token_count") {
            return Some("system".to_string());
        }
    }
    None
}

fn append_token_count_usage(meta: &mut Map<String, Value>, json: &Value) -> Option<String> {
    let record_type = json.get("type").and_then(Value::as_str)?;
    if !record_type.eq_ignore_ascii_case("event_msg") {
        return None;
    }
    let payload_type = json.pointer("/payload/type").and_then(Value::as_str)?;
    if !payload_type.eq_ignore_ascii_case("token_count") {
        return None;
    }

    meta.insert(
        "codex_event_type".to_string(),
        Value::String("token_count".to_string()),
    );

    if let Some(window) = json
        .pointer("/payload/info/model_context_window")
        .and_then(Value::as_i64)
    {
        meta.insert(
            "model_context_window".to_string(),
            Value::Number(window.into()),
        );
    }

    let total_usage = json.pointer("/payload/info/total_token_usage");
    let last_usage = json.pointer("/payload/info/last_token_usage");

    let total_total = total_usage
        .and_then(|v| v.get("total_tokens"))
        .and_then(Value::as_i64);
    let total_input = total_usage
        .and_then(|v| v.get("input_tokens"))
        .and_then(Value::as_i64);
    let total_output = total_usage
        .and_then(|v| v.get("output_tokens"))
        .and_then(Value::as_i64);
    let total_cached_input = total_usage
        .and_then(|v| v.get("cached_input_tokens"))
        .and_then(Value::as_i64);
    let total_reasoning = total_usage
        .and_then(|v| v.get("reasoning_output_tokens"))
        .and_then(Value::as_i64);

    if let Some(n) = total_total {
        meta.insert(
            "usage_cumulative_total_tokens".to_string(),
            Value::Number(n.into()),
        );
    }
    if let Some(n) = total_input {
        meta.insert(
            "usage_cumulative_prompt_tokens".to_string(),
            Value::Number(n.into()),
        );
    }
    if let Some(n) = total_output {
        meta.insert(
            "usage_cumulative_completion_tokens".to_string(),
            Value::Number(n.into()),
        );
    }
    if let Some(n) = total_cached_input {
        meta.insert(
            "usage_cumulative_cached_input_tokens".to_string(),
            Value::Number(n.into()),
        );
    }
    if let Some(n) = total_reasoning {
        meta.insert(
            "usage_cumulative_reasoning_output_tokens".to_string(),
            Value::Number(n.into()),
        );
    }

    let last_total = last_usage
        .and_then(|v| v.get("total_tokens"))
        .and_then(Value::as_i64);
    let last_input = last_usage
        .and_then(|v| v.get("input_tokens"))
        .and_then(Value::as_i64);
    let last_output = last_usage
        .and_then(|v| v.get("output_tokens"))
        .and_then(Value::as_i64);
    let last_cached_input = last_usage
        .and_then(|v| v.get("cached_input_tokens"))
        .and_then(Value::as_i64);
    let last_reasoning = last_usage
        .and_then(|v| v.get("reasoning_output_tokens"))
        .and_then(Value::as_i64);

    if let Some(n) = last_total {
        meta.insert("usage_total_tokens".to_string(), Value::Number(n.into()));
    }
    if let Some(n) = last_input {
        meta.insert("usage_prompt_tokens".to_string(), Value::Number(n.into()));
    }
    if let Some(n) = last_output {
        meta.insert(
            "usage_completion_tokens".to_string(),
            Value::Number(n.into()),
        );
    }
    if let Some(n) = last_cached_input {
        meta.insert(
            "usage_cached_input_tokens".to_string(),
            Value::Number(n.into()),
        );
    }
    if let Some(n) = last_reasoning {
        meta.insert(
            "usage_reasoning_output_tokens".to_string(),
            Value::Number(n.into()),
        );
    }

    Some(format!(
        "Token count: last_total={:?}, cumulative_total={:?}",
        last_total, total_total
    ))
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

    #[test]
    fn parses_event_msg_user_message_as_user() {
        let raw = r#"{
            "timestamp": "2025-12-01T10:00:00Z",
            "type": "event_msg",
            "payload": {
                "type": "user_message",
                "message": "hello from user"
            }
        }"#;

        let parsed = parse_codex_line(raw).expect("should parse");
        assert_eq!(parsed.role, "user");
        assert!(parsed.content.contains("\"user_message\""));
    }

    #[test]
    fn parses_token_count_event_into_usage_metadata() {
        let raw = r#"{
            "timestamp": "2025-12-15T06:10:28.257Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "total_token_usage": { "input_tokens": 100, "output_tokens": 50, "total_tokens": 150 },
                    "last_token_usage": { "input_tokens": 10, "output_tokens": 5, "total_tokens": 15 },
                    "model_context_window": 258400
                }
            }
        }"#;

        let parsed = parse_codex_line(raw).expect("should parse");
        assert_eq!(parsed.role, "system");
        assert!(parsed.content.contains("\"token_count\""));
        assert_eq!(
            parsed
                .metadata
                .get("usage_total_tokens")
                .and_then(Value::as_i64),
            Some(15)
        );
        assert_eq!(
            parsed
                .metadata
                .get("usage_cumulative_total_tokens")
                .and_then(Value::as_i64),
            Some(150)
        );
        assert_eq!(
            parsed
                .metadata
                .get("model_context_window")
                .and_then(Value::as_i64),
            Some(258400)
        );
    }
}
