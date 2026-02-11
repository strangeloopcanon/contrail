use crate::types::{Session, Turn};
use anyhow::Result;
use chrono::{DateTime, Utc};
use scrapers::cursor::{read_cursor_messages, timestamp_from_metadata};
use std::path::Path;

/// Read Cursor sessions for the given repo.
/// Finds the workspaceStorage directories that reference this repo, then
/// extracts conversations from state.vscdb.
pub fn read_sessions(
    repo_roots: &[String],
    cutoff: &DateTime<Utc>,
    quiet: bool,
) -> Result<Vec<Session>> {
    let ws_storage = match crate::detect::cursor_workspace_storage() {
        Some(p) if p.is_dir() => p,
        _ => return Ok(Vec::new()),
    };

    let mut sessions = Vec::new();

    let entries = std::fs::read_dir(&ws_storage)?;
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }

        // Check workspace.json for repo match
        let workspace_json = dir.join("workspace.json");
        let matched_root = match std::fs::read_to_string(&workspace_json) {
            Ok(content) => repo_roots.iter().find(|r| content.contains(*r)).cloned(),
            Err(_) => None,
        };
        let Some(repo_str) = matched_root else {
            continue;
        };

        // Read state.vscdb
        let db_path = dir.join("state.vscdb");
        if !db_path.is_file() {
            continue;
        }

        match read_workspace_sessions(&db_path, &repo_str, cutoff) {
            Ok(s) => sessions.extend(s),
            Err(e) => {
                if !quiet {
                    eprintln!("warning: cursor db {:?}: {e}", db_path);
                }
            }
        }
    }

    Ok(sessions)
}

fn read_workspace_sessions(
    db_path: &Path,
    repo_str: &str,
    cutoff: &DateTime<Utc>,
) -> Result<Vec<Session>> {
    let messages = read_cursor_messages(db_path)?;
    if messages.is_empty() {
        return Ok(Vec::new());
    }

    // Group messages into conversation chunks.
    // Cursor doesn't have explicit session IDs, so we split on gaps > 30 min
    // or when we see a "user" message after an "assistant" with a big time jump.
    let mut conversations: Vec<Vec<(Turn, Option<DateTime<Utc>>)>> = Vec::new();
    let mut current: Vec<(Turn, Option<DateTime<Utc>>)> = Vec::new();

    for msg in &messages {
        let ts = timestamp_from_metadata(&msg.metadata);

        // Check for session boundary: gap > 30 minutes
        if let Some(last) = current.last() {
            if let (Some(last_ts), Some(this_ts)) = (last.1, ts) {
                let gap = this_ts.signed_duration_since(last_ts);
                if (gap.num_minutes() > 30 || gap.num_minutes() < -30) && !current.is_empty() {
                    conversations.push(std::mem::take(&mut current));
                }
            }
        }

        current.push((
            Turn {
                role: msg.role.clone(),
                content: msg.content.clone(),
                timestamp: ts,
            },
            ts,
        ));
    }
    if !current.is_empty() {
        conversations.push(current);
    }

    let mut sessions = Vec::new();
    for (i, conv) in conversations.into_iter().enumerate() {
        // Filter by cutoff: skip if latest turn is before cutoff
        let latest = conv.iter().filter_map(|(_, ts)| *ts).max();
        if let Some(latest_ts) = latest {
            if latest_ts < *cutoff {
                continue;
            }
        }

        let earliest = conv.iter().filter_map(|(_, ts)| *ts).min();
        let turns: Vec<Turn> = conv.into_iter().map(|(t, _)| t).collect();

        if turns.is_empty() {
            continue;
        }

        // Use the workspace dir hash + index as session ID
        let session_id = format!("cursor_{:x}_{}", fxhash(repo_str.as_bytes()), i);

        sessions.push(Session {
            tool: "cursor".to_string(),
            session_id,
            project_path: repo_str.to_string(),
            branch: None,
            started_at: earliest,
            ended_at: latest,
            turns,
            files_changed: Vec::new(),
        });
    }

    Ok(sessions)
}

fn fxhash(data: &[u8]) -> u64 {
    let mut hash: u64 = 0;
    for &byte in data {
        hash = hash.wrapping_mul(0x100000001b3).wrapping_add(byte as u64);
    }
    hash
}
