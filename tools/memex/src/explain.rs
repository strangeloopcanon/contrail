use crate::link;
use crate::{aliases, detect, readers};
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;

/// Explain a commit: show which agent sessions were active when it was made.
pub fn run_explain(repo_root: &Path, commit_ref: &str) -> Result<()> {
    let links = link::load_commit_links(repo_root)?;

    if links.is_empty() {
        println!("No commit links found.");
        println!("Run `memex init` in this repo to install the post-commit hook,");
        println!("then future commits will be linked to agent sessions automatically.");
        return Ok(());
    }

    // Allow "commit-ish" (HEAD, refs, HEAD~1, etc.), not just SHA prefixes.
    let resolved_sha = git_rev_parse(repo_root, commit_ref);

    // Find matching commit(s) by SHA or SHA prefix.
    let matches: Vec<&link::CommitLink> = match &resolved_sha {
        Some(sha) => {
            let short = short_sha(sha);
            links
                .iter()
                .filter(|l| l.sha == sha.as_str() || l.short_sha == short)
                .collect()
        }
        None => links
            .iter()
            .filter(|l| l.sha.starts_with(commit_ref) || l.short_sha.starts_with(commit_ref))
            .collect(),
    };

    if matches.is_empty() {
        // The commit exists but wasn't linked — try to find sessions by timestamp
        if let Some(sha) = resolved_sha {
            println!("Commit {} not found in .context/commits.jsonl.", sha);
        } else {
            println!("Commit {} not found in .context/commits.jsonl.", commit_ref);
        }
        println!();
        println!("This commit was made before the post-commit hook was installed,");
        println!("or memex wasn't initialized in this repo at the time.");
        return Ok(());
    }

    if matches.len() > 1 {
        println!(
            "Ambiguous prefix '{}' matches {} commits:\n",
            commit_ref,
            matches.len()
        );
        for m in &matches {
            println!("  {} ({}) {}", m.short_sha, m.branch, m.message);
        }
        println!("\nSpecify more characters to disambiguate.");
        return Ok(());
    }

    let link = matches[0];

    // Header
    println!("Commit: {} ({})", link.sha, link.branch);
    println!("Date:   {}", link.timestamp.format("%Y-%m-%d %H:%M:%S UTC"));
    println!("Message: {}", link.message);
    println!();

    if link.active_sessions.is_empty() {
        println!("No agent sessions were active when this commit was made.");
        return Ok(());
    }

    println!("Active sessions ({}):", link.active_sessions.len());
    println!();

    let sessions_dir = repo_root.join(".context/sessions");
    let mut fallback_index: Option<HashMap<String, crate::types::Session>> = None;

    for session_file in &link.active_sessions {
        let path = sessions_dir.join(session_file);
        if path.is_file() {
            print_session_summary_from_file(&path, session_file);
            continue;
        }

        // Fall back to agent storage (local) if we haven't synced/unlocked `.context/sessions/` yet.
        if fallback_index.is_none() {
            fallback_index = Some(load_sessions_index(repo_root));
        }
        let index = fallback_index.as_ref().unwrap();

        if let Some(session) = index.get(session_file) {
            print_session_summary_from_struct(session, session_file);
        } else {
            println!("  --- {} ---", session_file);
            println!("    (not found in .context/sessions/ or local agent storage)");
            println!(
                "    Hint: run `memex sync` (or `memex unlock` if your team shares vault.age)."
            );
            println!();
        }
    }

    Ok(())
}

fn git_rev_parse(repo_root: &Path, commit_ref: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", commit_ref])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn short_sha(full: &str) -> String {
    if full.len() >= 7 {
        full[..7].to_string()
    } else {
        full.to_string()
    }
}

fn load_sessions_index(repo_root: &Path) -> HashMap<String, crate::types::Session> {
    let repo_roots = aliases::ensure_current_repo_roots(repo_root)
        .unwrap_or_else(|_| aliases::load_repo_roots(repo_root));
    let agents = detect::detect_agents(&repo_roots);
    if !agents.any() {
        return HashMap::new();
    }

    // Generous cutoff for ad-hoc explain runs; we only build this index when
    // the linked `.md` files are missing anyway.
    let sessions = readers::read_all_sessions(&repo_roots, &agents, 30, true);
    sessions.into_iter().map(|s| (s.filename(), s)).collect()
}

/// Print a short summary of a session file (first few lines).
fn print_session_summary_from_file(path: &Path, filename: &str) {
    println!("  --- {} ---", filename);

    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => {
            println!("    (file not readable)");
            println!();
            return;
        }
    };

    // Extract header info (first few lines of the markdown)
    let mut header_lines = Vec::new();
    let mut found_turn = false;
    for line in content.lines().take(20) {
        if line.starts_with("## ") && !found_turn {
            found_turn = true;
        }
        if found_turn {
            // Show first user prompt (truncated)
            if !line.starts_with("## ") {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    let display = if trimmed.len() > 120 {
                        format!("{}...", &trimmed[..120])
                    } else {
                        trimmed.to_string()
                    };
                    println!("    First prompt: {}", display);
                    break;
                }
            }
            continue;
        }
        if !line.is_empty() {
            header_lines.push(line);
        }
    }

    for line in &header_lines {
        println!("    {}", line);
    }

    // Show file count
    if let Some(files_line) = content.lines().find(|l| l.starts_with("Files changed:")) {
        println!("    {}", files_line);
    }

    println!("    Path: .context/sessions/{}", filename);
    println!();
}

fn print_session_summary_from_struct(session: &crate::types::Session, filename: &str) {
    println!("  --- {} ---", filename);

    let started = fmt_ts(session.started_at);
    let ended = fmt_ts(session.ended_at);
    let mut meta = vec![format!("Tool: {}", session.tool)];
    if let Some(b) = &session.branch {
        meta.push(format!("Branch: {}", b));
    }
    if started != "unknown" || ended != "unknown" {
        meta.push(format!("Time: {} → {}", started, ended));
    }

    for line in meta {
        println!("    {}", line);
    }

    if let Some(prompt) = first_user_prompt(session) {
        println!("    First prompt: {}", truncate_one_line(prompt, 120));
    }

    println!("    Path: .context/sessions/{}", filename);
    println!();
}

fn first_user_prompt(session: &crate::types::Session) -> Option<&str> {
    session
        .turns
        .iter()
        .find(|t| t.role.eq_ignore_ascii_case("user") || t.role.eq_ignore_ascii_case("human"))
        .map(|t| t.content.as_str())
}

fn truncate_one_line(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.len() > max {
        format!("{}...", &line[..max])
    } else {
        line.to_string()
    }
}

fn fmt_ts(ts: Option<DateTime<Utc>>) -> String {
    ts.map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
