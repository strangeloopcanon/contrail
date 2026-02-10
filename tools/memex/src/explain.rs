use crate::link;
use anyhow::Result;
use std::fs;
use std::path::Path;

/// Explain a commit: show which agent sessions were active when it was made.
pub fn run_explain(repo_root: &Path, commit_ref: &str) -> Result<()> {
    let links = link::load_commit_links(repo_root)?;

    if links.is_empty() {
        println!("No commit links found.");
        println!("Run `memex init` in this repo to install the post-commit hook,");
        println!("then future commits will be linked to agent sessions automatically.");
        return Ok(());
    }

    // Find matching commit(s) by SHA prefix
    let matches: Vec<&link::CommitLink> = links
        .iter()
        .filter(|l| l.sha.starts_with(commit_ref) || l.short_sha.starts_with(commit_ref))
        .collect();

    if matches.is_empty() {
        // The commit exists but wasn't linked — try to find sessions by timestamp
        println!("Commit {} not found in .context/commits.jsonl.", commit_ref);
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

    for session_file in &link.active_sessions {
        let path = sessions_dir.join(session_file);
        print_session_summary(&path, session_file);
    }

    Ok(())
}

/// Print a short summary of a session file (first few lines).
fn print_session_summary(path: &Path, filename: &str) {
    println!("  --- {} ---", filename);

    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => {
            println!("    (file not found — may have been cleaned up)");
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

    println!();
}
