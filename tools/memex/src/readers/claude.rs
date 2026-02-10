use crate::types::{Session, Turn};
use anyhow::Result;
use chrono::{DateTime, Utc};
use scrapers::claude::{parse_claude_line, parse_claude_session_line};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Read Claude Code sessions for the given repo.
/// Checks both ~/.claude/projects/ (per-project session files) and
/// ~/.claude/history.jsonl (global history).
pub fn read_sessions(
    repo_root: &Path,
    cutoff: &DateTime<Utc>,
    _quiet: bool,
) -> Result<Vec<Session>> {
    let mut sessions: HashMap<String, Session> = HashMap::new();
    let repo_str = repo_root.to_string_lossy().to_string();

    // 1. Read per-project session files from ~/.claude/projects/
    if let Some(projects_dir) = crate::detect::claude_projects_dir() {
        if projects_dir.is_dir() {
            read_projects_dir(&projects_dir, &repo_str, cutoff, &mut sessions)?;
        }
    }

    // 2. Read global history as fallback
    if let Some(history_path) = crate::detect::claude_history_path() {
        if history_path.is_file() {
            read_history_file(&history_path, &repo_str, cutoff, &mut sessions)?;
        }
    }

    Ok(sessions.into_values().collect())
}

fn read_projects_dir(
    projects_dir: &Path,
    repo_str: &str,
    cutoff: &DateTime<Utc>,
    sessions: &mut HashMap<String, Session>,
) -> Result<()> {
    let entries = std::fs::read_dir(projects_dir)?;
    for entry in entries.flatten() {
        let project_dir = entry.path();
        if !project_dir.is_dir() {
            continue;
        }
        // Read all JSONL files in the project directory
        let files = std::fs::read_dir(&project_dir)?;
        for file_entry in files.flatten() {
            let path = file_entry.path();
            if path.extension().is_none_or(|e| e != "jsonl") {
                continue;
            }
            read_session_jsonl(&path, repo_str, cutoff, sessions)?;
        }
    }
    Ok(())
}

fn read_session_jsonl(
    path: &Path,
    repo_str: &str,
    cutoff: &DateTime<Utc>,
    sessions: &mut HashMap<String, Session>,
) -> Result<()> {
    // Fast path: skip reading old session files entirely based on mtime.
    // The JSONL content can be large, and we don't need to parse historical
    // sessions when syncing or linking recent work.
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

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let parsed = match parse_claude_session_line(&line) {
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

        // Skip tool results for cleaner transcripts
        if parsed.role == "tool_result" {
            continue;
        }

        let session_id = parsed.session_id.unwrap_or_else(|| "unknown".to_string());

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
            .entry(format!("claude-code_{}", session_id))
            .or_insert_with(|| Session {
                tool: "claude-code".to_string(),
                session_id: session_id.clone(),
                project_path: cwd.clone(),
                branch: branch.clone(),
                started_at: parsed.timestamp,
                ended_at: parsed.timestamp,
                turns: Vec::new(),
                files_changed: Vec::new(),
            });

        // Update time bounds
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

fn read_history_file(
    path: &Path,
    repo_str: &str,
    cutoff: &DateTime<Utc>,
    sessions: &mut HashMap<String, Session>,
) -> Result<()> {
    // Fast path: skip the global history file if it hasn't been touched since cutoff.
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

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let parsed = match parse_claude_line(&line) {
            Some(p) => p,
            None => continue,
        };

        let cwd = match &parsed.project_context {
            Some(c) if c.starts_with(repo_str) => c.clone(),
            _ => continue,
        };

        if let Some(ts) = parsed.timestamp {
            if ts < *cutoff {
                continue;
            }
        }

        let session_id = parsed.session_id.unwrap_or_else(|| "unknown".to_string());

        let key = format!("claude-code_{}", session_id);
        // Don't duplicate sessions already found in project files
        if sessions.contains_key(&key) {
            continue;
        }

        let turn = Turn {
            role: parsed.role,
            content: parsed.content,
            timestamp: parsed.timestamp,
        };

        let session = sessions.entry(key).or_insert_with(|| Session {
            tool: "claude-code".to_string(),
            session_id: session_id.clone(),
            project_path: cwd,
            branch: None,
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

        session.turns.push(turn);
    }
    Ok(())
}
