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

/// Parse a line from Claude Code's project session files (e.g. ~/.claude/projects/*/*.jsonl).
/// These contain richer data including token usage in message.usage.
pub fn parse_claude_session_line(raw: &str) -> Option<ParsedLine> {
    let json = serde_json::from_str::<Value>(raw).ok()?;
    let mut metadata = Map::new();

    // Session files have a "type" field: "user", "assistant", or "file-history-snapshot"
    let msg_type = json.get("type").and_then(Value::as_str)?;

    // Skip file-history-snapshot entries - they don't contain conversation data
    if msg_type == "file-history-snapshot" {
        return None;
    }

    // Check if this is a real user message vs tool result
    // Real user messages have "userType": "external", tool results don't
    let is_external_user = json.get("userType").and_then(Value::as_str) == Some("external");

    // Tool results have role="user" but no userType="external"
    // Mark them distinctly so wrapup can filter them out
    let role = if msg_type == "user" && !is_external_user {
        "tool_result".to_string()
    } else {
        msg_type.to_string()
    };

    // Extract session ID
    let session_id = json
        .get("sessionId")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    if let Some(id) = session_id.as_ref() {
        metadata.insert("session_id".to_string(), Value::String(id.clone()));
    }

    // Extract project context from cwd
    let project_context = json
        .get("cwd")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    if let Some(cwd) = project_context.as_ref() {
        metadata.insert("cwd".to_string(), Value::String(cwd.clone()));
    }

    // Extract timestamp
    let timestamp = json.get("timestamp").and_then(parse_timestamp_value);
    if let Some(ts) = timestamp.as_ref() {
        metadata.insert(
            "original_timestamp".to_string(),
            Value::String(ts.to_rfc3339()),
        );
    }

    // Extract model from message object
    if let Some(model) = json.pointer("/message/model").and_then(Value::as_str) {
        metadata.insert("model".to_string(), Value::String(model.to_string()));
    }

    // Extract token usage from message.usage
    if let Some(usage) = json.pointer("/message/usage") {
        append_usage(&mut metadata, usage);
    }

    // Extract git branch if available
    if let Some(branch) = json.get("gitBranch").and_then(Value::as_str) {
        if !branch.is_empty() {
            metadata.insert("git_branch".to_string(), Value::String(branch.to_string()));
        }
    }

    // Extract content from message.content array
    let content = extract_message_content(&json).unwrap_or_default();

    // Skip empty content
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

/// Extract text content from Claude session message.content array
fn extract_message_content(json: &Value) -> Option<String> {
    let content_array = json.pointer("/message/content")?.as_array()?;

    let mut texts = Vec::new();
    for item in content_array {
        if let Some(text) = item.get("text").and_then(Value::as_str) {
            // Truncate very long text content
            let truncated = if text.len() > 2000 {
                // Find the nearest character boundary to avoid panicking on multi-byte UTF-8
                let boundary = text
                    .char_indices()
                    .take_while(|(i, _)| *i < 2000)
                    .last()
                    .map(|(i, c)| i + c.len_utf8())
                    .unwrap_or(0);
                format!("{}...[truncated]", &text[..boundary])
            } else {
                text.to_string()
            };
            texts.push(truncated);
        } else if let Some(tool_name) = item.get("name").and_then(Value::as_str) {
            // Tool use - just note the tool name
            texts.push(format!("[tool_use: {}]", tool_name));
        } else if item.get("tool_use_id").is_some() {
            // Tool result - skip or summarize
            texts.push("[tool_result]".to_string());
        }
    }

    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n"))
    }
}

fn append_usage(meta: &mut Map<String, Value>, value: &Value) {
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

    #[test]
    fn parses_claude_session_line_with_tokens() {
        let raw = r#"{
            "type": "assistant",
            "timestamp": "2025-11-10T02:52:43.237Z",
            "sessionId": "7109a899-3331-4a49-99f1-0eab6ce5282b",
            "cwd": "/Users/test/project",
            "gitBranch": "main",
            "message": {
                "model": "claude-sonnet-4-5-20250929",
                "usage": {
                    "input_tokens": 19491,
                    "output_tokens": 281,
                    "cache_read_input_tokens": 1000
                },
                "content": [{"type": "text", "text": "Hello, I can help you."}]
            }
        }"#;

        let parsed = parse_claude_session_line(raw).expect("should parse");
        assert_eq!(parsed.role, "assistant");
        assert_eq!(parsed.content, "Hello, I can help you.");
        assert_eq!(
            parsed.session_id.as_deref(),
            Some("7109a899-3331-4a49-99f1-0eab6ce5282b")
        );
        assert_eq!(
            parsed.project_context.as_deref(),
            Some("/Users/test/project")
        );
        assert!(parsed.timestamp.is_some());
        assert_eq!(
            parsed
                .metadata
                .get("usage_prompt_tokens")
                .and_then(Value::as_i64),
            Some(19491)
        );
        assert_eq!(
            parsed
                .metadata
                .get("usage_completion_tokens")
                .and_then(Value::as_i64),
            Some(281)
        );
        assert_eq!(
            parsed
                .metadata
                .get("usage_cached_input_tokens")
                .and_then(Value::as_i64),
            Some(1000)
        );
        assert_eq!(
            parsed.metadata.get("model").and_then(Value::as_str),
            Some("claude-sonnet-4-5-20250929")
        );
        assert_eq!(
            parsed.metadata.get("git_branch").and_then(Value::as_str),
            Some("main")
        );
    }

    #[test]
    fn skips_file_history_snapshot() {
        let raw = r#"{
            "type": "file-history-snapshot",
            "messageId": "aa7b7ee1-c8cb-4179-9578-59be4c059803"
        }"#;

        let result = parse_claude_session_line(raw);
        assert!(result.is_none());
    }

    #[test]
    fn truncates_utf8_safely() {
        // Create content with multi-byte chars that would panic at byte 2000
        // Using em-dash (─) which is 3 bytes each
        let long_text = "─".repeat(700); // 3 bytes × 700 = 2100 bytes
        let raw = format!(
            r#"{{
            "type": "assistant",
            "timestamp": "2025-12-01T00:00:00Z",
            "sessionId": "test-utf8",
            "message": {{ "content": [{{"type": "text", "text": "{}"}}] }}
        }}"#,
            long_text
        );
        // Should not panic and should parse successfully
        let result = parse_claude_session_line(&raw);
        assert!(result.is_some());
        let parsed = result.unwrap();
        assert!(parsed.content.contains("...[truncated]"));
    }

    #[test]
    fn distinguishes_tool_result_from_real_user() {
        // Real user message has userType: "external"
        let real_user = r#"{
            "type": "user",
            "userType": "external",
            "timestamp": "2025-12-01T00:00:00Z",
            "sessionId": "test",
            "message": { "content": [{"type": "text", "text": "Hello"}] }
        }"#;
        let parsed = parse_claude_session_line(real_user).expect("should parse");
        assert_eq!(parsed.role, "user");

        // Tool result has type: "user" but no userType
        let tool_result = r#"{
            "type": "user",
            "timestamp": "2025-12-01T00:00:00Z",
            "sessionId": "test",
            "message": { "content": [{"type": "tool_result", "tool_use_id": "123", "content": "ok"}] }
        }"#;
        let parsed = parse_claude_session_line(tool_result).expect("should parse");
        assert_eq!(parsed.role, "tool_result");
    }
}
