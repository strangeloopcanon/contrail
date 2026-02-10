use chrono::{DateTime, TimeZone, Utc};
use serde_json::{Map, Value};

/// Unified parsed-line type shared by all source parsers (Claude, Codex, etc.).
#[derive(Debug, Clone)]
pub struct ParsedLine {
    pub role: String,
    pub content: String,
    pub timestamp: Option<DateTime<Utc>>,
    pub session_id: Option<String>,
    pub project_context: Option<String>,
    pub metadata: Map<String, Value>,
}

// ── Shared metadata helpers ─────────────────────────────────────────────

/// Merge usage-related keys from a JSON object into flat `usage_*` metadata fields.
/// Handles aliases across Claude, Codex, and OpenAI response shapes.
pub fn append_usage(meta: &mut Map<String, Value>, value: &Value) {
    if let Some(obj) = value.as_object() {
        for (k, v) in obj {
            match k.as_str() {
                "total" | "total_tokens" | "totalTokens" => {
                    insert_scalar(meta, "usage_total_tokens", v)
                }
                "prompt" | "prompt_tokens" | "promptTokens" | "input" | "input_tokens" => {
                    insert_scalar(meta, "usage_prompt_tokens", v)
                }
                "completion" | "completion_tokens" | "completionTokens" | "output"
                | "output_tokens" => insert_scalar(meta, "usage_completion_tokens", v),
                "cache_read_input_tokens" | "cached_tokens" => {
                    insert_scalar(meta, "usage_cached_input_tokens", v)
                }
                "cache_creation_input_tokens" => {
                    insert_scalar(meta, "usage_cache_creation_tokens", v)
                }
                _ => {}
            }
        }
    }
}

/// Merge latency / duration metrics into flat metadata fields.
pub fn append_metrics(meta: &mut Map<String, Value>, value: &Value) {
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

/// Insert a scalar JSON value (string / number / bool) into a metadata map.
pub fn insert_scalar(meta: &mut Map<String, Value>, key: &str, value: &Value) {
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

// ── Text extraction ─────────────────────────────────────────────────────

pub fn extract_text(value: &Value) -> Option<String> {
    extract_text_depth(value, 0)
}

fn extract_text_depth(value: &Value, depth: usize) -> Option<String> {
    if depth > 6 {
        return None;
    }
    match value {
        Value::String(s) => Some(s.to_string()),
        Value::Array(items) => {
            let mut parts = Vec::new();
            for item in items {
                if let Some(text) = extract_text_depth(item, depth + 1) {
                    if !text.trim().is_empty() {
                        parts.push(text);
                    }
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(""))
            }
        }
        Value::Object(map) => {
            for key in [
                "content",
                "text",
                "message",
                "delta",
                "completion",
                "prompt",
            ] {
                if let Some(v) = map.get(key) {
                    if let Some(text) = extract_text_depth(v, depth + 1) {
                        if !text.trim().is_empty() {
                            return Some(text);
                        }
                    }
                }
            }
            None
        }
        _ => None,
    }
}

pub fn parse_timestamp_value(value: &Value) -> Option<DateTime<Utc>> {
    match value {
        Value::String(s) => parse_timestamp_str(s),
        Value::Number(n) => n.as_i64().and_then(parse_timestamp_i64),
        _ => None,
    }
}

pub fn parse_timestamp_str(raw: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(num) = raw.parse::<i64>() {
        return parse_timestamp_i64(num);
    }
    None
}

pub fn parse_timestamp_i64(num: i64) -> Option<DateTime<Utc>> {
    if num <= 0 {
        return None;
    }
    // Heuristic: treat values over ~year 2286 seconds as milliseconds.
    if num > 10_000_000_000 {
        let secs = num / 1000;
        let nsec = ((num % 1000) * 1_000_000) as u32;
        return Utc.timestamp_opt(secs, nsec).single();
    }
    Utc.timestamp_opt(num, 0).single()
}
