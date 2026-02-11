//! Cross-machine master log export and merge.
//!
//! `export_log` writes a (optionally filtered) copy of the local master log to a file.
//! `merge_log` imports events from an external log, deduplicating by event_id UUID first,
//! then by a content fingerprint to catch the same underlying event ingested independently
//! on two machines (which would have different UUIDs).

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::borrow::Cow;
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use uuid::Uuid;

// ── Public types ────────────────────────────────────────────────────────

/// Filters for `export_log`. All fields are optional; unset filters match everything.
#[derive(Debug, Default)]
pub struct ExportFilters {
    pub after: Option<DateTime<Utc>>,
    pub before: Option<DateTime<Utc>>,
    pub project: Option<String>,
    pub tool: Option<String>,
    pub hostname: Option<String>,
}

#[derive(Debug, Default)]
pub struct ExportStats {
    pub exported: usize,
    pub skipped: usize,
    pub errors: usize,
}

#[derive(Debug, Default)]
pub struct MergeStats {
    pub merged: usize,
    pub skipped_uuid: usize,
    pub skipped_fingerprint: usize,
    pub errors: usize,
}

// ── Export ───────────────────────────────────────────────────────────────

/// Read the master log at `log_path`, apply `filters`, write matching lines to `output`.
pub fn export_log(log_path: &Path, filters: &ExportFilters, output: &Path) -> Result<ExportStats> {
    let file = File::open(log_path)
        .with_context(|| format!("open master log at {}", log_path.display()))?;
    let reader = BufReader::new(file);

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut writer =
        File::create(output).with_context(|| format!("create output {}", output.display()))?;

    let mut stats = ExportStats::default();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => {
                stats.errors += 1;
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }

        let json: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                stats.errors += 1;
                continue;
            }
        };

        if !matches_filters(&json, filters) {
            stats.skipped += 1;
            continue;
        }

        // Write the original line verbatim (lossless).
        write_jsonl_line(&mut writer, &line)?;
        stats.exported += 1;
    }

    Ok(stats)
}

fn matches_filters(json: &Value, f: &ExportFilters) -> bool {
    if let Some(ref after) = f.after {
        if let Some(ts) = parse_timestamp(json) {
            if ts < *after {
                return false;
            }
        }
    }
    if let Some(ref before) = f.before {
        if let Some(ts) = parse_timestamp(json) {
            if ts >= *before {
                return false;
            }
        }
    }
    if let Some(ref project) = f.project {
        let ctx = json
            .get("project_context")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !ctx.starts_with(project.as_str()) {
            return false;
        }
    }
    if let Some(ref tool) = f.tool {
        let src = json
            .get("source_tool")
            .and_then(Value::as_str)
            .unwrap_or("");
        if src != tool.as_str() {
            return false;
        }
    }
    if let Some(ref hostname) = f.hostname {
        let hn = json
            .pointer("/metadata/hostname")
            .and_then(Value::as_str)
            .unwrap_or("");
        if hn != hostname.as_str() {
            return false;
        }
    }
    true
}

fn parse_timestamp(json: &Value) -> Option<DateTime<Utc>> {
    json.get("timestamp")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

// ── Merge ───────────────────────────────────────────────────────────────

/// Merge events from `input` into the master log at `log_path`.
///
/// Dedup strategy:
/// 1. Primary: skip events whose `event_id` UUID already exists locally.
/// 2. Fallback: skip events whose content fingerprint already exists locally.
///    This catches the same underlying event ingested on two machines with different UUIDs
///    (e.g. both ran `import-history` independently).
///
/// **Important**: this should run with the contrail daemon stopped.
/// We write each entry in a single append call to reduce interleaving risk,
/// but this function does not coordinate a cross-process lock.
pub fn merge_log(log_path: &Path, input: &Path) -> Result<MergeStats> {
    let (existing_uuids, existing_fps) = load_existing_keys(log_path)?;

    let file =
        File::open(input).with_context(|| format!("open import file {}", input.display()))?;
    let reader = BufReader::new(file);

    let mut writer = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("open master log for append at {}", log_path.display()))?;

    let mut stats = MergeStats::default();
    let mut seen_uuids = existing_uuids;
    let mut seen_fps = existing_fps;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => {
                stats.errors += 1;
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }

        let json: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                stats.errors += 1;
                continue;
            }
        };

        // Primary dedup: event_id UUID.
        if let Some(uuid) = extract_uuid(&json) {
            if seen_uuids.contains(&uuid) {
                stats.skipped_uuid += 1;
                continue;
            }
            seen_uuids.insert(uuid);
        }

        // Fallback dedup: content fingerprint.
        let fp = fingerprint(&json);
        if seen_fps.contains(&fp) {
            stats.skipped_fingerprint += 1;
            continue;
        }
        seen_fps.insert(fp);

        write_jsonl_line(&mut writer, &line)?;
        stats.merged += 1;
    }

    Ok(stats)
}

/// Build sets of existing UUIDs and fingerprints from the local master log.
fn load_existing_keys(log_path: &Path) -> Result<(HashSet<Uuid>, HashSet<u64>)> {
    let mut uuids = HashSet::new();
    let mut fps = HashSet::new();

    if !log_path.exists() {
        return Ok((uuids, fps));
    }

    let file = File::open(log_path)?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let json: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if let Some(uuid) = extract_uuid(&json) {
            uuids.insert(uuid);
        }
        fps.insert(fingerprint(&json));
    }

    Ok((uuids, fps))
}

fn extract_uuid(json: &Value) -> Option<Uuid> {
    json.get("event_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
}

/// Content fingerprint: hash of (source_tool, project_context, session_id, timestamp, role, content).
/// Timestamps are canonicalized to UTC epoch-millis when parseable so equivalent
/// RFC3339 representations dedupe consistently.
/// Uses `std::hash::DefaultHasher` — not cryptographic, but sufficient for dedup.
fn fingerprint(json: &Value) -> u64 {
    use std::hash::{Hash, Hasher};

    let source = json
        .get("source_tool")
        .and_then(Value::as_str)
        .unwrap_or("");
    let project = json
        .get("project_context")
        .and_then(Value::as_str)
        .unwrap_or("");
    let session = json.get("session_id").and_then(Value::as_str).unwrap_or("");
    let timestamp =
        canonical_timestamp_repr(json.get("timestamp").and_then(Value::as_str).unwrap_or(""));
    let role = json
        .pointer("/interaction/role")
        .and_then(Value::as_str)
        .unwrap_or("");
    let content = json
        .pointer("/interaction/content")
        .and_then(Value::as_str)
        .unwrap_or("");

    let mut h = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut h);
    project.hash(&mut h);
    session.hash(&mut h);
    timestamp.hash(&mut h);
    role.hash(&mut h);
    content.hash(&mut h);
    h.finish()
}

fn canonical_timestamp_repr(raw: &str) -> Cow<'_, str> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Cow::Owned(dt.with_timezone(&Utc).timestamp_millis().to_string());
    }
    Cow::Borrowed(raw)
}

fn write_jsonl_line<W: Write>(writer: &mut W, line: &str) -> Result<()> {
    let mut buf = Vec::with_capacity(line.len() + 1);
    buf.extend_from_slice(line.as_bytes());
    buf.push(b'\n');
    writer.write_all(&buf)?;
    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;
    use std::io::Write;
    use tempfile::NamedTempFile;
    use uuid::Uuid;

    fn make_event(
        event_id: Uuid,
        source: &str,
        session: &str,
        content: &str,
        hostname: &str,
    ) -> Value {
        json!({
            "event_id": event_id.to_string(),
            "timestamp": Utc::now().to_rfc3339(),
            "source_tool": source,
            "project_context": "/tmp/project",
            "session_id": session,
            "interaction": { "role": "assistant", "content": content },
            "security_flags": { "has_pii": false, "redacted_secrets": [] },
            "metadata": { "hostname": hostname, "user": "testuser" }
        })
    }

    fn write_events(events: &[Value]) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        for e in events {
            serde_json::to_writer(&mut f, e).unwrap();
            f.write_all(b"\n").unwrap();
        }
        f.flush().unwrap();
        f
    }

    #[test]
    fn merge_appends_new_events() {
        let local_event = make_event(Uuid::new_v4(), "cursor", "s1", "hello", "macA");
        let remote_event = make_event(Uuid::new_v4(), "codex-cli", "s2", "world", "macB");

        let local_file = write_events(&[local_event]);
        let remote_file = write_events(&[remote_event]);

        let stats = merge_log(local_file.path(), remote_file.path()).unwrap();
        assert_eq!(stats.merged, 1);
        assert_eq!(stats.skipped_uuid, 0);
        assert_eq!(stats.skipped_fingerprint, 0);

        // Local log should now have 2 lines.
        let contents = fs::read_to_string(local_file.path()).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn merge_deduplicates_by_uuid() {
        let id = Uuid::new_v4();
        let event = make_event(id, "cursor", "s1", "hello", "macA");

        let local_file = write_events(std::slice::from_ref(&event));
        let remote_file = write_events(std::slice::from_ref(&event));

        let stats = merge_log(local_file.path(), remote_file.path()).unwrap();
        assert_eq!(stats.merged, 0);
        assert_eq!(stats.skipped_uuid, 1);

        let contents = fs::read_to_string(local_file.path()).unwrap();
        assert_eq!(contents.lines().count(), 1);
    }

    #[test]
    fn merge_deduplicates_by_fingerprint() {
        // Same content, different UUIDs — simulates both machines importing the same history.
        let ts = Utc::now().to_rfc3339();
        let mut event_a = make_event(Uuid::new_v4(), "codex-cli", "s1", "same content", "macA");
        let mut event_b = make_event(Uuid::new_v4(), "codex-cli", "s1", "same content", "macB");

        // Force identical timestamps so fingerprints match.
        event_a["timestamp"] = json!(ts);
        event_b["timestamp"] = json!(ts);
        // Force identical roles.
        event_a["interaction"]["role"] = json!("user");
        event_b["interaction"]["role"] = json!("user");

        let local_file = write_events(&[event_a]);
        let remote_file = write_events(&[event_b]);

        let stats = merge_log(local_file.path(), remote_file.path()).unwrap();
        assert_eq!(stats.merged, 0);
        assert_eq!(stats.skipped_fingerprint, 1);

        let contents = fs::read_to_string(local_file.path()).unwrap();
        assert_eq!(contents.lines().count(), 1);
    }

    #[test]
    fn merge_deduplicates_by_fingerprint_when_timestamp_format_differs() {
        let mut event_a = make_event(Uuid::new_v4(), "codex-cli", "s1", "same content", "macA");
        let mut event_b = make_event(Uuid::new_v4(), "codex-cli", "s1", "same content", "macB");

        event_a["timestamp"] = json!("2026-06-01T00:00:00Z");
        event_b["timestamp"] = json!("2026-06-01T00:00:00.000+00:00");

        let local_file = write_events(&[event_a]);
        let remote_file = write_events(&[event_b]);

        let stats = merge_log(local_file.path(), remote_file.path()).unwrap();
        assert_eq!(stats.merged, 0);
        assert_eq!(stats.skipped_fingerprint, 1);
    }

    #[test]
    fn merge_is_idempotent() {
        let event = make_event(Uuid::new_v4(), "cursor", "s1", "hello", "macA");
        let local_file = write_events(&[]);
        let remote_file = write_events(&[event]);

        // First merge.
        let stats1 = merge_log(local_file.path(), remote_file.path()).unwrap();
        assert_eq!(stats1.merged, 1);

        // Second merge of the same file.
        let stats2 = merge_log(local_file.path(), remote_file.path()).unwrap();
        assert_eq!(stats2.merged, 0);
        assert_eq!(stats2.skipped_uuid, 1);

        let contents = fs::read_to_string(local_file.path()).unwrap();
        assert_eq!(contents.lines().count(), 1);
    }

    #[test]
    fn merge_handles_malformed_lines() {
        let event = make_event(Uuid::new_v4(), "cursor", "s1", "hello", "macA");
        let local_file = write_events(&[]);

        let mut remote = NamedTempFile::new().unwrap();
        writeln!(remote, "not json at all").unwrap();
        serde_json::to_writer(&mut remote, &event).unwrap();
        writeln!(remote).unwrap();
        writeln!(remote, "{{broken").unwrap();
        remote.flush().unwrap();

        let stats = merge_log(local_file.path(), remote.path()).unwrap();
        assert_eq!(stats.merged, 1);
        assert_eq!(stats.errors, 2);
    }

    #[test]
    fn merge_into_nonexistent_log_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("new_log.jsonl");
        let event = make_event(Uuid::new_v4(), "cursor", "s1", "hello", "macA");
        let remote_file = write_events(&[event]);

        let stats = merge_log(&log_path, remote_file.path()).unwrap();
        assert_eq!(stats.merged, 1);
        assert!(log_path.exists());
    }

    #[test]
    fn export_filters_by_tool() {
        let e1 = make_event(Uuid::new_v4(), "cursor", "s1", "a", "macA");
        let e2 = make_event(Uuid::new_v4(), "codex-cli", "s2", "b", "macA");
        let log_file = write_events(&[e1, e2]);

        let output = tempfile::NamedTempFile::new().unwrap();
        let filters = ExportFilters {
            tool: Some("cursor".to_string()),
            ..Default::default()
        };
        let stats = export_log(log_file.path(), &filters, output.path()).unwrap();
        assert_eq!(stats.exported, 1);
        assert_eq!(stats.skipped, 1);
    }

    #[test]
    fn export_filters_by_date_range() {
        let ts_old = "2024-01-01T00:00:00Z";
        let ts_new = "2026-06-01T00:00:00Z";

        let mut e1 = make_event(Uuid::new_v4(), "cursor", "s1", "old", "macA");
        e1["timestamp"] = json!(ts_old);
        let mut e2 = make_event(Uuid::new_v4(), "cursor", "s2", "new", "macA");
        e2["timestamp"] = json!(ts_new);

        let log_file = write_events(&[e1, e2]);
        let output = tempfile::NamedTempFile::new().unwrap();
        let filters = ExportFilters {
            after: Some("2025-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap()),
            ..Default::default()
        };
        let stats = export_log(log_file.path(), &filters, output.path()).unwrap();
        assert_eq!(stats.exported, 1);
        assert_eq!(stats.skipped, 1);
    }

    #[test]
    fn export_round_trip() {
        let e1 = make_event(Uuid::new_v4(), "cursor", "s1", "a", "macA");
        let e2 = make_event(Uuid::new_v4(), "codex-cli", "s2", "b", "macB");
        let log_file = write_events(&[e1, e2]);

        let exported = tempfile::NamedTempFile::new().unwrap();
        let filters = ExportFilters::default();
        export_log(log_file.path(), &filters, exported.path()).unwrap();

        // Merge exported into an empty log.
        let dir = tempfile::tempdir().unwrap();
        let new_log = dir.path().join("merged.jsonl");
        let stats = merge_log(&new_log, exported.path()).unwrap();
        assert_eq!(stats.merged, 2);

        let original = fs::read_to_string(log_file.path()).unwrap();
        let restored = fs::read_to_string(&new_log).unwrap();
        assert_eq!(original.lines().count(), restored.lines().count());
    }
}
