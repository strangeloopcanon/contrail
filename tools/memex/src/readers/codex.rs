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
    repo_roots: &[String],
    cutoff: &DateTime<Utc>,
    _quiet: bool,
) -> Result<Vec<Session>> {
    let session_roots = crate::detect::codex_sessions_roots();
    if session_roots.is_empty() {
        return Ok(Vec::new());
    }

    let mut sessions: HashMap<String, Session> = HashMap::new();

    // Walk YYYY/MM/DD structure (and legacy flat roots) for each known location.
    for sessions_root in session_roots {
        walk_sessions_dir(&sessions_root, repo_roots, cutoff, &mut sessions)?;
    }

    Ok(sessions.into_values().collect())
}

fn walk_sessions_dir(
    dir: &Path,
    repo_roots: &[String],
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
            walk_sessions_dir(&path, repo_roots, cutoff, sessions)?;
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            read_codex_jsonl(&path, repo_roots, cutoff, sessions)?;
        }
    }
    Ok(())
}

fn read_codex_jsonl(
    path: &Path,
    repo_roots: &[String],
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
    let mut file_repo_context: Option<String> = None;
    let mut session_repo_context: HashMap<String, String> = HashMap::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let parsed = match parse_codex_line(&line) {
            Some(p) => p,
            None => continue,
        };

        // Preserve repo context at line/session/file scope. Codex often emits cwd
        // only in session_meta / turn_context records, while user/assistant message
        // lines omit it. Without this, real chat turns get dropped.
        let session_id = parsed
            .session_id
            .clone()
            .unwrap_or_else(|| file_session_id.clone());

        let line_repo_context = parsed
            .project_context
            .as_deref()
            .filter(|cwd| crate::aliases::matches_any_root(cwd, repo_roots))
            .map(str::to_string);

        if let Some(cwd) = line_repo_context.as_ref() {
            if file_repo_context.is_none() {
                file_repo_context = Some(cwd.clone());
            }
            session_repo_context.insert(session_id.clone(), cwd.clone());
        }

        let cwd = line_repo_context
            .or_else(|| session_repo_context.get(&session_id).cloned())
            .or_else(|| file_repo_context.clone());
        let Some(cwd) = cwd else {
            continue;
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
