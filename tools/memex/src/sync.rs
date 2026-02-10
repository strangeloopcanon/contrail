use crate::detect;
use crate::readers;
use crate::render;
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

/// Sync recent sessions from agent storage into .context/sessions/.
pub fn run_sync(repo_root: &Path, max_age_days: u64, quiet: bool) -> Result<()> {
    let sessions_dir = repo_root.join(".context/sessions");
    if !sessions_dir.is_dir() {
        if quiet {
            return Ok(());
        }
        anyhow::bail!(".context/sessions/ not found. Run `memex init` first.");
    }

    let agents = detect::detect_agents(repo_root);
    if !agents.any() {
        if !quiet {
            println!("No agent sessions found for this repo.");
        }
        return Ok(());
    }

    // Collect existing session filenames to avoid duplicates
    let mut existing = list_existing_sessions(&sessions_dir)?;

    // Read sessions from all detected agents
    let sessions = readers::read_all_sessions(repo_root, &agents, max_age_days, quiet);

    let mut written = 0usize;
    let mut skipped = 0usize;

    for session in &sessions {
        if session.turns.is_empty() {
            skipped += 1;
            continue;
        }

        let rendered = render::render_session(session);
        let base_filename = session.filename();

        let filename = if existing.contains(&base_filename) {
            let existing_path = sessions_dir.join(&base_filename);
            if let Ok(existing_content) = fs::read_to_string(&existing_path) {
                if existing_content == rendered {
                    skipped += 1;
                    continue;
                }
            }
            allocate_unique_filename(&base_filename, &existing)
        } else {
            base_filename
        };

        let out_path = sessions_dir.join(&filename);
        fs::write(&out_path, &rendered).with_context(|| format!("write {}", out_path.display()))?;
        existing.insert(filename);
        written += 1;
    }

    if !quiet {
        println!("Synced {} new session(s) ({} skipped).", written, skipped);
    }
    Ok(())
}

fn allocate_unique_filename(base: &str, existing: &HashSet<String>) -> String {
    if !existing.contains(base) {
        return base.to_string();
    }
    let stem = base.strip_suffix(".md").unwrap_or(base);
    for i in 1u32.. {
        let candidate = format!("{stem}__{i}.md");
        if !existing.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!("exhausted filename suffix space")
}

fn list_existing_sessions(dir: &Path) -> Result<HashSet<String>> {
    let mut names = HashSet::new();
    if !dir.is_dir() {
        return Ok(names);
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if let Some(name) = entry.file_name().to_str() {
            if name.ends_with(".md") {
                names.insert(name.to_string());
            }
        }
    }
    Ok(names)
}
