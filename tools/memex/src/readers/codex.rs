use crate::types::{Session, Turn};
use anyhow::Result;
use chrono::{DateTime, Utc};
use scrapers::codex::parse_codex_line;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Read Codex CLI/Desktop sessions for the given repo.
/// Scans ~/.codex/sessions/YYYY/MM/DD/*.jsonl, filters by cwd.
pub fn read_sessions(
    repo_root: &Path,
    cutoff: &DateTime<Utc>,
    _quiet: bool,
) -> Result<Vec<Session>> {
    let sessions_root = match crate::detect::codex_sessions_root() {
        Some(p) if p.is_dir() => p,
        _ => return Ok(Vec::new()),
    };

    let repo_str = repo_root.to_string_lossy().to_string();
    let mut sessions: HashMap<String, Session> = HashMap::new();

    // Walk YYYY/MM/DD structure
    walk_sessions_dir(&sessions_root, &repo_str, cutoff, &mut sessions)?;

    Ok(sessions.into_values().collect())
}

fn walk_sessions_dir(
    dir: &Path,
    repo_str: &str,
    cutoff: &DateTime<Utc>,
    sessions: &mut HashMap<String, Session>,
) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_sessions_dir(&path, repo_str, cutoff, sessions)?;
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            read_codex_jsonl(&path, repo_str, cutoff, sessions)?;
        }
    }
    Ok(())
}

fn read_codex_jsonl(
    path: &Path,
    repo_str: &str,
    cutoff: &DateTime<Utc>,
    sessions: &mut HashMap<String, Session>,
) -> Result<()> {
    // Fast path: skip reading old session files entirely. This keeps `memex sync`
    // and post-commit linking snappy even with large ~/.codex/sessions archives.
    if let Ok(meta) = std::fs::metadata(path) {
        if let Ok(modified) = meta.modified() {
            let mod_time: DateTime<Utc> = modified.into();
            if mod_time < *cutoff {
                return Ok(());
            }
        }
    }

    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);

    // Use the filename stem as a fallback session ID
    let file_session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let parsed = match parse_codex_line(&line) {
            Some(p) => p,
            None => continue,
        };

        // Filter by repo
        let cwd = match &parsed.project_context {
            Some(c) if c.starts_with(repo_str) => c.clone(),
            _ => continue,
        };

        // Filter by cutoff
        if let Some(ts) = parsed.timestamp {
            if ts < *cutoff {
                continue;
            }
        }

        // Skip system/metadata entries for cleaner transcripts
        if parsed.role == "system" {
            continue;
        }

        let session_id = parsed.session_id.unwrap_or_else(|| file_session_id.clone());

        let branch = parsed
            .metadata
            .get("git_branch")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let turn = Turn {
            role: parsed.role,
            content: parsed.content,
            timestamp: parsed.timestamp,
        };

        let session = sessions
            .entry(format!("codex-cli_{}", session_id))
            .or_insert_with(|| Session {
                tool: "codex-cli".to_string(),
                session_id: session_id.clone(),
                project_path: cwd.clone(),
                branch: branch.clone(),
                started_at: parsed.timestamp,
                ended_at: parsed.timestamp,
                turns: Vec::new(),
                files_changed: Vec::new(),
            });

        if let Some(ts) = parsed.timestamp {
            if session.started_at.is_none() || session.started_at.is_some_and(|s| ts < s) {
                session.started_at = Some(ts);
            }
            if session.ended_at.is_none() || session.ended_at.is_some_and(|e| ts > e) {
                session.ended_at = Some(ts);
            }
        }
        if branch.is_some() && session.branch.is_none() {
            session.branch = branch;
        }

        session.turns.push(turn);
    }
    Ok(())
}
