use crate::{detect, readers};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::Command;

/// A single commit→session linkage record.
#[derive(Debug, Serialize, Deserialize)]
pub struct CommitLink {
    pub sha: String,
    pub short_sha: String,
    pub timestamp: DateTime<Utc>,
    pub branch: String,
    pub message: String,
    /// Session filenames in `.context/sessions/` (as rendered by memex) that were active
    /// around the time of this commit.
    pub active_sessions: Vec<String>,
}

const COMMITS_FILE: &str = ".context/commits.jsonl";

/// Record the current HEAD commit and associate it with recent sessions.
/// Called by the post-commit git hook.
pub fn run_link_commit(repo_root: &Path, quiet: bool) -> Result<()> {
    let context_dir = repo_root.join(".context");
    if !context_dir.is_dir() {
        if !quiet {
            eprintln!(".context/ not found. Run `memex init` first.");
        }
        return Ok(());
    }

    let sha = git_output(repo_root, &["rev-parse", "HEAD"])?;
    let short_sha = if sha.len() >= 7 { &sha[..7] } else { &sha };
    let branch = git_output(repo_root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap_or_else(|_| "detached".to_string());
    let message = git_output(repo_root, &["log", "-1", "--format=%s"]).unwrap_or_default();

    // Prefer the *actual git commit timestamp* over wall-clock time.
    // (Hooks can run slightly after the commit is created.)
    let timestamp = git_commit_timestamp(repo_root, "HEAD").unwrap_or_else(Utc::now);

    // Find sessions active around this commit.
    // This is best-effort: we infer "activeness" from agent transcript timestamps.
    let active_sessions = find_active_sessions(repo_root, timestamp, &branch)?;

    let link = CommitLink {
        sha: sha.clone(),
        short_sha: short_sha.to_string(),
        timestamp,
        branch,
        message,
        active_sessions,
    };

    let commits_path = repo_root.join(COMMITS_FILE);
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&commits_path)
        .with_context(|| format!("open {}", commits_path.display()))?;

    let line = serde_json::to_string(&link)?;
    writeln!(file, "{}", line)?;

    if !quiet {
        println!(
            "Linked commit {} to {} session(s).",
            short_sha,
            link.active_sessions.len()
        );
    }

    Ok(())
}

/// Load all commit links from .context/commits.jsonl.
pub fn load_commit_links(repo_root: &Path) -> Result<Vec<CommitLink>> {
    let commits_path = repo_root.join(COMMITS_FILE);
    if !commits_path.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(&commits_path)
        .with_context(|| format!("read {}", commits_path.display()))?;

    let mut links = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<CommitLink>(line) {
            Ok(link) => links.push(link),
            Err(e) => {
                eprintln!("warning: skipping malformed commit link: {}", e);
            }
        }
    }

    Ok(links)
}

fn git_commit_timestamp(repo_root: &Path, commitish: &str) -> Option<DateTime<Utc>> {
    let raw = git_output(repo_root, &["show", "-s", "--format=%cI", commitish]).ok()?;
    let dt = DateTime::parse_from_rfc3339(&raw).ok()?;
    Some(dt.with_timezone(&Utc))
}

fn find_active_sessions(
    repo_root: &Path,
    commit_ts: DateTime<Utc>,
    commit_branch: &str,
) -> Result<Vec<String>> {
    let agents = detect::detect_agents(repo_root);
    if !agents.any() {
        return Ok(Vec::new());
    }

    // Keep this tight: we only need sessions near the commit time.
    let sessions = readers::read_all_sessions(repo_root, &agents, 3, true);
    let mut selected = select_active_session_filenames(commit_ts, commit_branch, &sessions);

    // Fallback for older memex installs: if we couldn't infer any sessions from agent storage,
    // fall back to `.context/sessions` mtimes.
    if selected.is_empty() {
        selected = select_recent_context_files_by_mtime(repo_root, commit_ts)?;
    }

    Ok(selected)
}

fn select_active_session_filenames(
    commit_ts: DateTime<Utc>,
    commit_branch: &str,
    sessions: &[crate::types::Session],
) -> Vec<String> {
    // "Active" is approximate: treat sessions as relevant if their time range overlaps
    // a short window around the commit time.
    let window_start = commit_ts - chrono::Duration::hours(2);
    let window_end = commit_ts + chrono::Duration::minutes(5);

    let prefer_branch = commit_branch != "detached" && commit_branch != "HEAD";

    let mut candidates: Vec<(bool, DateTime<Utc>, String)> = Vec::new();

    for s in sessions {
        let (start, end) = match (s.started_at, s.ended_at) {
            (Some(a), Some(b)) => (a, b),
            (Some(a), None) => (a, a),
            (None, Some(b)) => (b, b),
            (None, None) => continue,
        };

        // Window overlap check.
        if end < window_start || start > window_end {
            continue;
        }

        let branch_match = prefer_branch && s.branch.as_deref() == Some(commit_branch);
        let rank_time = s.ended_at.or(s.started_at).unwrap_or(end);
        candidates.push((branch_match, rank_time, s.filename()));
    }

    // Prefer sessions on the same branch, then by recency.
    candidates.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| b.2.cmp(&a.2))
    });

    // Deduplicate, cap output to keep `memex explain` readable.
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (_, _, fname) in candidates {
        if seen.insert(fname.clone()) {
            out.push(fname);
            if out.len() >= 8 {
                break;
            }
        }
    }
    out
}

fn select_recent_context_files_by_mtime(
    repo_root: &Path,
    commit_ts: DateTime<Utc>,
) -> Result<Vec<String>> {
    let sessions_dir = repo_root.join(".context/sessions");
    if !sessions_dir.is_dir() {
        return Ok(Vec::new());
    }

    let cutoff = commit_ts - chrono::Duration::hours(2);
    let mut recent = Vec::new();

    for entry in fs::read_dir(&sessions_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".md") || name == ".gitkeep" {
            continue;
        }

        if let Ok(meta) = entry.metadata() {
            if let Ok(modified) = meta.modified() {
                let mod_time: DateTime<Utc> = modified.into();
                if mod_time > cutoff {
                    recent.push(name);
                }
            }
        }
    }

    recent.sort();
    recent.reverse();
    if recent.len() > 8 {
        recent.truncate(8);
    }
    Ok(recent)
}

fn git_output(repo_root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .with_context(|| format!("run git {}", args.join(" ")))?;

    if !output.status.success() {
        anyhow::bail!("git {} failed", args.join(" "));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::select_active_session_filenames;
    use crate::types::{Session, Turn};
    use chrono::{TimeZone, Utc};

    fn mk_session(
        tool: &str,
        session_id: &str,
        branch: Option<&str>,
        started_at: Option<chrono::DateTime<Utc>>,
        ended_at: Option<chrono::DateTime<Utc>>,
    ) -> Session {
        Session {
            tool: tool.to_string(),
            session_id: session_id.to_string(),
            project_path: "/repo".to_string(),
            branch: branch.map(|s| s.to_string()),
            started_at,
            ended_at,
            turns: vec![Turn {
                role: "user".to_string(),
                content: "hi".to_string(),
                timestamp: started_at,
            }],
            files_changed: Vec::new(),
        }
    }

    #[test]
    fn selects_only_sessions_overlapping_time_window() {
        let commit_ts = Utc.with_ymd_and_hms(2026, 2, 10, 12, 0, 0).unwrap();

        let in_window = mk_session(
            "codex-cli",
            "s1",
            Some("feat"),
            Some(commit_ts - chrono::Duration::hours(1)),
            Some(commit_ts - chrono::Duration::minutes(5)),
        );
        let too_old = mk_session(
            "codex-cli",
            "s2",
            Some("feat"),
            Some(commit_ts - chrono::Duration::hours(5)),
            Some(commit_ts - chrono::Duration::hours(3)),
        );
        let too_new = mk_session(
            "codex-cli",
            "s3",
            Some("feat"),
            Some(commit_ts + chrono::Duration::hours(1)),
            Some(commit_ts + chrono::Duration::hours(2)),
        );

        let sessions = vec![too_old.clone(), in_window.clone(), too_new.clone()];
        let out = select_active_session_filenames(commit_ts, "feat", &sessions);

        assert_eq!(out.len(), 1);
        assert_eq!(out[0], in_window.filename());
    }

    #[test]
    fn prefers_branch_match_over_recency() {
        let commit_ts = Utc.with_ymd_and_hms(2026, 2, 10, 12, 0, 0).unwrap();

        let branch_match = mk_session(
            "claude-code",
            "s1",
            Some("feat"),
            Some(commit_ts - chrono::Duration::minutes(50)),
            Some(commit_ts - chrono::Duration::minutes(40)),
        );
        let other_branch_more_recent = mk_session(
            "claude-code",
            "s2",
            Some("main"),
            Some(commit_ts - chrono::Duration::minutes(10)),
            Some(commit_ts - chrono::Duration::minutes(1)),
        );

        let sessions = vec![other_branch_more_recent.clone(), branch_match.clone()];
        let out = select_active_session_filenames(commit_ts, "feat", &sessions);

        assert_eq!(out.len(), 2);
        assert_eq!(out[0], branch_match.filename());
        assert_eq!(out[1], other_branch_more_recent.filename());
    }
}
