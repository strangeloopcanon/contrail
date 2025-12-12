use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;

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
