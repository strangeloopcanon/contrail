use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
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
    /// Session filenames in .context/sessions/ that existed at commit time.
    /// The most recently modified files are the most likely active sessions.
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

    // Find sessions active around now — recently modified .md files in .context/sessions/
    let active_sessions = find_recent_sessions(repo_root)?;

    let link = CommitLink {
        sha: sha.clone(),
        short_sha: short_sha.to_string(),
        timestamp: Utc::now(),
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

/// Find session files modified in the last 2 hours (likely active during this commit).
fn find_recent_sessions(repo_root: &Path) -> Result<Vec<String>> {
    let sessions_dir = repo_root.join(".context/sessions");
    if !sessions_dir.is_dir() {
        return Ok(Vec::new());
    }

    let cutoff = Utc::now() - chrono::Duration::hours(2);
    let mut recent = Vec::new();

    for entry in fs::read_dir(&sessions_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".md") || name == ".gitkeep" {
            continue;
        }

        // Check modification time
        if let Ok(meta) = entry.metadata() {
            if let Ok(modified) = meta.modified() {
                let mod_time: DateTime<Utc> = modified.into();
                if mod_time > cutoff {
                    recent.push(name);
                }
            }
        }
    }

    // Sort most recent first
    recent.sort();
    recent.reverse();

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
