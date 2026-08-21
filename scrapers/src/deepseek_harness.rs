use crate::parse::{append_usage, extract_text, parse_timestamp_value, ParsedLine};
use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::fs;
use std::io::Cursor;
use std::path::Path;

pub const SOURCE_TOOL: &str = "deepseek-harness";

pub fn source_instance(root: &Path) -> String {
    let stable_path = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    format!(
        "{:016x}",
        xxhash_rust::xxh3::xxh3_64(stable_path.to_string_lossy().as_bytes())
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DshSessionHeader {
    pub id: String,
    pub cwd: Option<String>,
    pub created_at_ms: i64,
    pub format_version: i64,
    pub parent_session: Option<String>,
    pub seed_length: Option<u64>,
    pub delegation_depth: u64,
    pub origin: Option<String>,
    pub agent_preset: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DshTranscript {
    pub header: DshSessionHeader,
    pub events: Vec<Value>,
}

pub fn read_transcript(path: &Path) -> Result<DshTranscript> {
    let bytes = fs::read(path).with_context(|| format!("read DSH transcript {path:?}"))?;
    let decoded = if path.extension().and_then(|value| value.to_str()) == Some("zstd") {
        decode_complete_zstd_frames(&bytes)
            .with_context(|| format!("decode DSH Zstandard transcript {path:?}"))?
    } else {
        bytes
    };
    let text = String::from_utf8(decoded)
        .with_context(|| format!("DSH transcript is not UTF-8 JSONL: {path:?}"))?;
    parse_transcript(&text)
}

pub fn parse_transcript(raw: &str) -> Result<DshTranscript> {
    let mut lines = raw.lines();
    let header_line = lines.next().context("DSH transcript is empty")?;
    let header_value: Value =
        serde_json::from_str(header_line).context("parse DSH session header")?;
    let header = parse_header(&header_value)?;
    let mut events = lines
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event.get("seq").and_then(Value::as_u64).is_some())
        .collect::<Vec<_>>();
    mark_replaced_usage_chunks(&mut events);
    Ok(DshTranscript { header, events })
}

fn decode_complete_zstd_frames(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut decoded = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let frame_size = match zstd::zstd_safe::find_frame_compressed_size(&bytes[offset..]) {
            Ok(size) if size > 0 => size,
            Ok(_) => bail!("DSH Zstandard frame has zero length"),
            Err(error) if offset == 0 => bail!("invalid first DSH Zstandard frame: {error:?}"),
            Err(_) => break,
        };
        let end = offset
            .checked_add(frame_size)
            .context("DSH Zstandard frame size overflow")?;
        let frame = bytes
            .get(offset..end)
            .context("incomplete DSH Zstandard frame")?;
        decoded.extend(zstd::stream::decode_all(Cursor::new(frame))?);
        offset = end;
    }
    if decoded.is_empty() {
        bail!("DSH Zstandard transcript has no complete frames");
    }
    Ok(decoded)
}

fn mark_replaced_usage_chunks(events: &mut [Value]) {
    let finalized_steps = events
        .iter()
        .filter(|event| event.get("type").and_then(Value::as_str) == Some("assistant/message"))
        .filter(|event| {
            event.pointer("/data/usage").is_some() || event.pointer("/data/message/usage").is_some()
        })
        .filter_map(event_turn_step)
        .collect::<HashSet<_>>();
    for event in events {
        if event.get("type").and_then(Value::as_str) == Some("assistant/chunk")
            && event.pointer("/data/chunk/type").and_then(Value::as_str) == Some("usage")
            && event_turn_step(event).is_some_and(|key| finalized_steps.contains(&key))
        {
            if let Some(object) = event.as_object_mut() {
                object.insert("__contrail_usage_replaced".to_string(), Value::Bool(true));
            }
        }
    }
}

fn event_turn_step(event: &Value) -> Option<(u64, u64)> {
    Some((
        event.pointer("/data/turn")?.as_u64()?,
        event.pointer("/data/step")?.as_u64()?,
    ))
}

pub fn parse_event(header: &DshSessionHeader, event: &Value) -> Option<ParsedLine> {
    let event_type = event.get("type")?.as_str()?;
    let seq = event.get("seq")?.as_u64()?;
    let data = event.get("data")?;

    let (role, content) = match event_type {
        "user/message" => {
            let source_kind = data.pointer("/source/kind").and_then(Value::as_str);
            let role = if source_kind == Some("user") {
                "user"
            } else {
                "system"
            };
            (role, extract_content(data.get("content")?)?)
        }
        "assistant/message" => (
            "assistant",
            extract_content(data.pointer("/message/content")?)?,
        ),
        "assistant/chunk" => {
            if event
                .get("__contrail_usage_replaced")
                .and_then(Value::as_bool)
                == Some(true)
            {
                return None;
            }
            let usage = data.pointer("/chunk/usage")?;
            if data.pointer("/chunk/type").and_then(Value::as_str) != Some("usage") {
                return None;
            }
            let turn = data.get("turn").and_then(Value::as_u64)?;
            let step = data.get("step").and_then(Value::as_u64)?;
            let _ = usage;
            (
                "system",
                format!("DSH usage reported for turn {turn} step {step}"),
            )
        }
        "request/context" => {
            let provider = data.get("provider").and_then(Value::as_str)?;
            let model = data.get("model").and_then(Value::as_str)?;
            (
                "system",
                format!("DSH request routed to {provider}/{model}"),
            )
        }
        "request/header" => {
            let provider = data
                .pointer("/header/config/provider")
                .and_then(Value::as_str)?;
            let model = data
                .pointer("/header/config/model")
                .and_then(Value::as_str)?;
            ("system", format!("DSH request header: {provider}/{model}"))
        }
        "tool/call" => {
            let name = data
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let arguments = data
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or_default();
            ("assistant", format!("[tool_call: {name}]\n{arguments}"))
        }
        "tool/result" => ("tool_result", extract_tool_result(data)?),
        "turn/start" => {
            let turn = data.get("turn").and_then(Value::as_u64)?;
            ("system", format!("DSH turn {turn} started"))
        }
        "turn/end" => {
            let turn = data.get("turn").and_then(Value::as_u64)?;
            let reason = data
                .pointer("/reason/kind")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            ("system", format!("DSH turn {turn} ended: {reason}"))
        }
        _ => return None,
    };

    if content.trim().is_empty() {
        return None;
    }

    let mut metadata = base_metadata(header, event_type, seq);
    copy_u64(data, "turn", &mut metadata, "dsh_turn");
    copy_u64(data, "step", &mut metadata, "dsh_step");
    copy_string(data, "callId", &mut metadata, "dsh_tool_call_id");
    copy_string(data, "name", &mut metadata, "dsh_tool_name");
    append_surface_op(&mut metadata, event.get("surfaceOp"));
    copy_bool(event, "ignorable", &mut metadata, "dsh_ignorable");
    if let Some(value) = event.get("time").and_then(Value::as_i64) {
        metadata.insert("dsh_event_time_ms".to_string(), Value::from(value));
    }
    if let Some(seqs) = event.get("sourceEventSeqs").and_then(Value::as_array) {
        let seqs = seqs
            .iter()
            .filter_map(Value::as_u64)
            .map(Value::from)
            .collect::<Vec<_>>();
        metadata.insert("dsh_source_event_seqs".to_string(), Value::Array(seqs));
    }

    if let Some(message) = data.get("message") {
        copy_string(message, "id", &mut metadata, "dsh_message_id");
        if let Some(source) = message.get("source") {
            copy_string(source, "kind", &mut metadata, "dsh_message_source");
            copy_string(source, "provider", &mut metadata, "provider");
            copy_string(source, "model", &mut metadata, "model");
            copy_string(source, "callId", &mut metadata, "dsh_tool_call_id");
        }
    }
    if let Some(source) = data.get("source") {
        copy_string(source, "kind", &mut metadata, "dsh_message_source");
        copy_string(source, "plugin", &mut metadata, "dsh_source_plugin");
    }
    if let Some(usage) = data.get("usage") {
        append_dsh_usage(&mut metadata, usage);
    }
    if let Some(usage) = data.pointer("/message/usage") {
        append_dsh_usage(&mut metadata, usage);
    }
    if event_type == "assistant/chunk" {
        if let Some(usage) = data.pointer("/chunk/usage") {
            append_dsh_usage(&mut metadata, usage);
            metadata.insert(
                "dsh_usage_record".to_string(),
                Value::String("provider_chunk".to_string()),
            );
        }
    }
    if event_type == "assistant/message" {
        copy_bool(data, "interrupted", &mut metadata, "dsh_interrupted");
        if data.get("usage").is_some() || data.pointer("/message/usage").is_some() {
            metadata.insert(
                "dsh_usage_record".to_string(),
                Value::String("assistant_message_copy".to_string()),
            );
        }
    }
    if event_type == "tool/result" {
        if let Some(result) = data.pointer("/message/content/0") {
            copy_bool(result, "isError", &mut metadata, "dsh_tool_result_is_error");
        }
    }
    if matches!(event_type, "request/context" | "request/header") {
        let config = if event_type == "request/context" {
            data
        } else {
            data.pointer("/header/config")?
        };
        copy_string(config, "provider", &mut metadata, "provider");
        copy_string(config, "model", &mut metadata, "model");
        copy_string(config, "reasoningEffort", &mut metadata, "reasoning_effort");
        if event_type == "request/header" {
            copy_string(data, "reason", &mut metadata, "dsh_request_reason");
        } else if let Some(value) = config.get("contextWindow").and_then(Value::as_u64) {
            metadata.insert("model_context_window".to_string(), Value::from(value));
        }
    }
    if event_type == "turn/end" {
        let reason = data.get("reason").unwrap_or(&Value::Null);
        copy_string(reason, "kind", &mut metadata, "dsh_turn_outcome");
        copy_string(
            reason.get("error").unwrap_or(&Value::Null),
            "code",
            &mut metadata,
            "dsh_turn_error_code",
        );
        copy_u64(
            reason.get("error").unwrap_or(&Value::Null),
            "status",
            &mut metadata,
            "dsh_turn_error_status",
        );
        copy_u64(
            reason.get("error").unwrap_or(&Value::Null),
            "providerRetryAfterMs",
            &mut metadata,
            "dsh_turn_retry_after_ms",
        );
        copy_string(
            reason.get("reason").unwrap_or(&Value::Null),
            "kind",
            &mut metadata,
            "dsh_turn_cancel_cause",
        );
    }

    Some(ParsedLine {
        role: role.to_string(),
        content,
        timestamp: event.get("time").and_then(parse_timestamp_value),
        session_id: Some(header.id.clone()),
        project_context: header.cwd.clone(),
        metadata,
    })
}

fn parse_header(value: &Value) -> Result<DshSessionHeader> {
    if value.get("type").and_then(Value::as_str) != Some("session") {
        bail!("DSH transcript first record is not a session header");
    }
    let id = required_string(value, "id")?;
    let created_at_ms = value
        .get("createdAt")
        .and_then(Value::as_i64)
        .context("DSH session header createdAt missing or invalid")?;
    let format_version = value
        .get("version")
        .and_then(Value::as_i64)
        .context("DSH session header version missing or invalid")?;
    let delegation_depth = value
        .get("delegationDepth")
        .and_then(Value::as_u64)
        .context("DSH session header delegationDepth missing or invalid")?;

    Ok(DshSessionHeader {
        id,
        cwd: optional_string(value, "cwd"),
        created_at_ms,
        format_version,
        parent_session: optional_string(value, "parentSession"),
        seed_length: value.get("seedLength").and_then(Value::as_u64),
        delegation_depth,
        origin: optional_string(value, "origin"),
        agent_preset: optional_string(value, "agentPreset"),
    })
}

fn base_metadata(header: &DshSessionHeader, event_type: &str, seq: u64) -> Map<String, Value> {
    let mut metadata = Map::new();
    metadata.insert(
        "dsh_event_type".to_string(),
        Value::String(event_type.to_string()),
    );
    metadata.insert("dsh_event_seq".to_string(), Value::from(seq));
    metadata.insert(
        "dsh_format_version".to_string(),
        Value::from(header.format_version),
    );
    metadata.insert(
        "dsh_created_at_ms".to_string(),
        Value::from(header.created_at_ms),
    );
    metadata.insert(
        "dsh_delegation_depth".to_string(),
        Value::from(header.delegation_depth),
    );
    if let Some(value) = header.parent_session.as_ref() {
        metadata.insert(
            "dsh_parent_session_id".to_string(),
            Value::String(value.clone()),
        );
    }
    if let Some(value) = header.seed_length {
        metadata.insert("dsh_seed_length".to_string(), Value::from(value));
    }
    if let Some(value) = header.origin.as_ref() {
        metadata.insert("dsh_origin".to_string(), Value::String(value.clone()));
    }
    if let Some(value) = header.agent_preset.as_ref() {
        metadata.insert("dsh_agent_preset".to_string(), Value::String(value.clone()));
    }
    metadata
}

fn extract_content(value: &Value) -> Option<String> {
    let items = value.as_array()?;
    let mut parts = Vec::new();
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    parts.push(text.to_string());
                }
            }
            Some("tool-call") => {
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let arguments = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                parts.push(format!("[tool_call: {name}]\n{arguments}"));
            }
            Some("tool-result") => {
                if let Some(text) = item.get("content").and_then(extract_text) {
                    parts.push(text);
                }
            }
            _ => {}
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn extract_tool_result(data: &Value) -> Option<String> {
    let message = data.get("message")?;
    extract_content(message.get("content")?)
}

fn append_dsh_usage(metadata: &mut Map<String, Value>, usage: &Value) {
    append_usage(metadata, usage);
    if let Some(value) = usage.get("inputTokens") {
        crate::parse::insert_scalar(metadata, "usage_prompt_tokens", value);
    }
    if let Some(value) = usage.get("outputTokens") {
        crate::parse::insert_scalar(metadata, "usage_completion_tokens", value);
    }
    if let Some(value) = usage.get("cacheReadTokens") {
        crate::parse::insert_scalar(metadata, "usage_cached_input_tokens", value);
    }
    if let Some(value) = usage.get("cacheWriteTokens") {
        crate::parse::insert_scalar(metadata, "usage_cache_creation_tokens", value);
    }
    if let Some(value) = usage.get("reasoningTokens") {
        crate::parse::insert_scalar(metadata, "usage_reasoning_tokens", value);
    }
}

fn append_surface_op(metadata: &mut Map<String, Value>, surface_op: Option<&Value>) {
    let Some(surface_op) = surface_op else {
        return;
    };
    if let Some(op) = surface_op.as_str() {
        metadata.insert("dsh_surface_op".to_string(), Value::String(op.to_string()));
        return;
    }
    let Some(object) = surface_op.as_object() else {
        return;
    };
    if let Some(op) = object.get("op").and_then(Value::as_str) {
        metadata.insert("dsh_surface_op".to_string(), Value::String(op.to_string()));
    }
    if let Some(start) = object.get("start").and_then(Value::as_u64) {
        metadata.insert("dsh_surface_replace_start".to_string(), Value::from(start));
    }
    if let Some(end) = object.get("end").and_then(Value::as_u64) {
        metadata.insert("dsh_surface_replace_end".to_string(), Value::from(end));
    }
}

fn required_string(value: &Value, key: &str) -> Result<String> {
    optional_string(value, key).with_context(|| format!("DSH session header {key} missing"))
}

fn optional_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn copy_string(
    source: &Value,
    source_key: &str,
    target: &mut Map<String, Value>,
    target_key: &str,
) {
    if let Some(value) = source.get(source_key).and_then(Value::as_str) {
        target.insert(target_key.to_string(), Value::String(value.to_string()));
    }
}

fn copy_u64(source: &Value, source_key: &str, target: &mut Map<String, Value>, target_key: &str) {
    if let Some(value) = source.get(source_key).and_then(Value::as_u64) {
        target.insert(target_key.to_string(), Value::from(value));
    }
}

fn copy_bool(source: &Value, source_key: &str, target: &mut Map<String, Value>, target_key: &str) {
    if let Some(value) = source.get(source_key).and_then(Value::as_bool) {
        target.insert(target_key.to_string(), Value::Bool(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = r#"{"type":"session","version":0,"id":"session-1","createdAt":1783600629539,"cwd":"/tmp/project","parentSession":"parent-1","seedLength":9,"origin":"subagent","delegationDepth":1,"agentPreset":"worker"}"#;

    #[test]
    fn parses_exact_rc8_provenance_and_usage() -> Result<()> {
        let raw = format!(
            "{HEADER}\n{}\n{}\n",
            r#"{"type":"request/context","seq":8,"time":1785730415288,"data":{"provider":"deepseek-official","model":"deepseek-v4-flash","turn":1,"step":1}}"#,
            r#"{"type":"assistant/message","seq":37,"time":1785730415298,"data":{"turn":1,"step":1,"message":{"role":"assistant","content":[{"type":"reasoning","text":"private chain"},{"type":"text","text":"PONG"}],"source":{"kind":"model","provider":"deepseek-official","model":"deepseek-v4-flash"},"id":"message-1"},"usage":{"inputTokens":3091,"outputTokens":23,"cacheReadTokens":7,"reasoningTokens":20}},"surfaceOp":"append"}"#,
        );
        let transcript = parse_transcript(&raw)?;
        assert_eq!(
            transcript.header.parent_session.as_deref(),
            Some("parent-1")
        );
        let request = parse_event(&transcript.header, &transcript.events[0]).unwrap();
        assert_eq!(request.metadata["provider"], "deepseek-official");
        assert_eq!(request.metadata["model"], "deepseek-v4-flash");
        let assistant = parse_event(&transcript.header, &transcript.events[1]).unwrap();
        assert_eq!(assistant.content, "PONG");
        assert!(!assistant.content.contains("private chain"));
        assert_eq!(assistant.metadata["dsh_event_seq"], 37);
        assert_eq!(assistant.metadata["dsh_message_id"], "message-1");
        assert_eq!(assistant.metadata["dsh_surface_op"], "append");
        assert_eq!(assistant.metadata["dsh_event_time_ms"], 1785730415298_i64);
        assert_eq!(assistant.metadata["dsh_seed_length"], 9);
        assert_eq!(assistant.metadata["usage_prompt_tokens"], 3091);
        assert_eq!(assistant.metadata["usage_completion_tokens"], 23);
        assert_eq!(assistant.metadata["usage_cached_input_tokens"], 7);
        assert_eq!(assistant.metadata["usage_reasoning_tokens"], 20);
        Ok(())
    }

    #[test]
    fn maps_tool_actions_results_and_turn_outcomes() -> Result<()> {
        let raw = format!(
            "{HEADER}\n{}\n{}\n{}\n",
            r#"{"type":"tool/call","seq":64,"time":1785730417568,"data":{"turn":1,"step":1,"callId":"call-1","name":"bash","arguments":"{\"command\":\"echo ok\"}"}}"#,
            r#"{"type":"tool/result","seq":65,"time":1785730417585,"data":{"turn":1,"step":1,"message":{"source":{"kind":"tool","callId":"call-1"},"content":[{"type":"tool-result","toolCallId":"call-1","content":[{"type":"text","text":"ok\\n"}],"isError":false}],"role":"user","id":"result-1"}}}"#,
            r#"{"type":"turn/end","seq":66,"time":1785730417586,"data":{"turn":1,"reason":{"kind":"completed"}}}"#,
        );
        let transcript = parse_transcript(&raw)?;
        let call = parse_event(&transcript.header, &transcript.events[0]).unwrap();
        assert_eq!(call.metadata["dsh_tool_call_id"], "call-1");
        assert!(call.content.contains("echo ok"));
        let result = parse_event(&transcript.header, &transcript.events[1]).unwrap();
        assert_eq!(result.role, "tool_result");
        assert_eq!(result.metadata["dsh_tool_call_id"], "call-1");
        let end = parse_event(&transcript.header, &transcript.events[2]).unwrap();
        assert_eq!(end.metadata["dsh_turn_outcome"], "completed");
        Ok(())
    }

    #[test]
    fn keeps_structured_failure_facts_without_raw_error_text() -> Result<()> {
        let raw = format!(
            "{HEADER}\n{}\n{}\n",
            r#"{"type":"turn/end","seq":70,"time":1785730417586,"data":{"turn":1,"reason":{"kind":"error","error":{"message":"secret provider detail user@example.com","code":"RATE_LIMITED","status":429,"providerRetryAfterMs":1000,"requestId":"opaque-request"}}}}"#,
            r#"{"type":"turn/end","seq":71,"time":1785730417587,"data":{"turn":2,"reason":{"kind":"aborted","reason":{"kind":"hook","reason":"private hook detail"}}}}"#,
        );
        let transcript = parse_transcript(&raw)?;
        let failed = parse_event(&transcript.header, &transcript.events[0]).unwrap();
        assert_eq!(failed.metadata["dsh_turn_outcome"], "error");
        assert_eq!(failed.metadata["dsh_turn_error_code"], "RATE_LIMITED");
        assert_eq!(failed.metadata["dsh_turn_error_status"], 429);
        assert_eq!(failed.metadata["dsh_turn_retry_after_ms"], 1000);
        assert!(!failed.metadata.values().any(|value| {
            value.as_str().is_some_and(|text| {
                text.contains("user@example.com") || text.contains("opaque-request")
            })
        }));
        let aborted = parse_event(&transcript.header, &transcript.events[1]).unwrap();
        assert_eq!(aborted.metadata["dsh_turn_cancel_cause"], "hook");
        assert!(!aborted
            .metadata
            .values()
            .any(|value| value.as_str() == Some("private hook detail")));
        Ok(())
    }

    #[test]
    fn ignores_packed_chunks_and_inbox_duplicates() -> Result<()> {
        let raw = format!(
            "{HEADER}\n{}\n{}\n",
            r#"{"type":"reasoning-chunks","seq0":10,"time0":1,"data":{"texts":["secret"]}}"#,
            r#"{"type":"agent/inbox/spliced","seq":0,"time":1,"data":{"inserted":[]}}"#,
        );
        let transcript = parse_transcript(&raw)?;
        assert_eq!(transcript.events.len(), 1);
        assert!(parse_event(&transcript.header, &transcript.events[0]).is_none());
        Ok(())
    }

    #[test]
    fn preserves_replace_operations_and_failed_request_usage() -> Result<()> {
        let raw = format!(
            "{HEADER}\n{}\n{}\n",
            r#"{"type":"assistant/chunk","seq":40,"time":1785730415299,"data":{"turn":2,"step":3,"chunk":{"type":"usage","usage":{"inputTokens":11,"outputTokens":2,"cacheReadTokens":5}}}}"#,
            r#"{"type":"user/message","seq":41,"time":1785730415300,"data":{"source":{"kind":"plugin","plugin":"compaction"},"content":[{"type":"text","text":"summary"}]},"surfaceOp":{"op":"replace","start":10,"end":39}}"#,
        );
        let transcript = parse_transcript(&raw)?;
        let usage = parse_event(&transcript.header, &transcript.events[0]).unwrap();
        assert_eq!(usage.metadata["usage_prompt_tokens"], 11);
        assert_eq!(usage.metadata["usage_completion_tokens"], 2);
        assert_eq!(usage.metadata["usage_cached_input_tokens"], 5);
        assert_eq!(usage.metadata["dsh_usage_record"], "provider_chunk");

        let replacement = parse_event(&transcript.header, &transcript.events[1]).unwrap();
        assert_eq!(replacement.metadata["dsh_surface_op"], "replace");
        assert_eq!(replacement.metadata["dsh_surface_replace_start"], 10);
        assert_eq!(replacement.metadata["dsh_surface_replace_end"], 39);
        Ok(())
    }

    #[test]
    fn finalized_usage_replaces_the_earlier_chunk_sample() -> Result<()> {
        let raw = format!(
            "{HEADER}\n{}\n{}\n",
            r#"{"type":"assistant/chunk","seq":20,"time":1785730415299,"data":{"turn":2,"step":3,"chunk":{"type":"usage","usage":{"inputTokens":11,"outputTokens":2}}}}"#,
            r#"{"type":"assistant/message","seq":22,"time":1785730415300,"data":{"turn":2,"step":3,"message":{"role":"assistant","content":[{"type":"text","text":"done"}],"source":{"kind":"model","provider":"deepseek-official","model":"deepseek-v4-flash"}},"usage":{"inputTokens":12,"outputTokens":3}}}"#,
        );
        let transcript = parse_transcript(&raw)?;
        let chunk = parse_event(&transcript.header, &transcript.events[0]);
        let message = parse_event(&transcript.header, &transcript.events[1]).unwrap();
        assert!(chunk.is_none());
        assert_eq!(message.metadata["usage_prompt_tokens"], 12);
        assert_eq!(message.metadata["usage_completion_tokens"], 3);
        assert_eq!(
            message.metadata["dsh_usage_record"],
            "assistant_message_copy"
        );
        Ok(())
    }

    #[test]
    fn reads_rc8_style_concatenated_zstd_frames() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("session.jsonl.zstd");
        let header_frame =
            zstd::stream::encode_all(Cursor::new(format!("{HEADER}\n").into_bytes()), 0)?;
        let event = concat!(
            "{\"type\":\"turn/end\",\"seq\":0,\"time\":1785730417586,",
            "\"data\":{\"turn\":1,\"reason\":{\"kind\":\"completed\"}}}\n"
        );
        let event_frame = zstd::stream::encode_all(Cursor::new(event.as_bytes()), 0)?;
        let mut bytes = header_frame;
        bytes.extend(event_frame);
        fs::write(&path, bytes)?;

        let transcript = read_transcript(&path)?;
        assert_eq!(transcript.header.id, "session-1");
        assert_eq!(transcript.events.len(), 1);
        assert_eq!(transcript.events[0]["seq"], 0);
        Ok(())
    }

    #[test]
    fn reads_only_complete_zstd_frames_after_torn_append() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("session.jsonl.zstd");
        let header_frame =
            zstd::stream::encode_all(Cursor::new(format!("{HEADER}\n").into_bytes()), 0)?;
        let event_frame = zstd::stream::encode_all(
            Cursor::new(
                b"{\"type\":\"turn/start\",\"seq\":0,\"time\":1785730417586,\"data\":{\"turn\":1}}\n",
            ),
            0,
        )?;
        let torn_frame = zstd::stream::encode_all(
            Cursor::new(
                b"{\"type\":\"turn/end\",\"seq\":1,\"time\":1785730417587,\"data\":{\"turn\":1,\"reason\":{\"kind\":\"completed\"}}}\n",
            ),
            0,
        )?;
        let mut bytes = header_frame;
        bytes.extend(event_frame);
        bytes.extend(&torn_frame[..torn_frame.len() / 2]);
        fs::write(&path, bytes)?;

        let transcript = read_transcript(&path)?;
        assert_eq!(transcript.events.len(), 1);
        assert_eq!(transcript.events[0]["seq"], 0);
        Ok(())
    }
}
