pub mod claude;
pub mod codex;
pub mod cursor;

use crate::types::Session;

/// Read sessions from all available agents for a given repo.
pub fn read_all_sessions(
    repo_roots: &[String],
    agents: &crate::types::DetectedAgents,
    max_age_days: u64,
    quiet: bool,
) -> Vec<Session> {
    let cutoff = chrono::Utc::now() - chrono::Duration::days(max_age_days as i64);
    let mut sessions = Vec::new();

    if agents.gemini && !quiet {
        eprintln!(
            "warning: gemini detected but reader is not implemented; skipping gemini sessions"
        );
    }

    if agents.claude {
        match claude::read_sessions(repo_roots, &cutoff, quiet) {
            Ok(s) => sessions.extend(s),
            Err(e) => {
                if !quiet {
                    eprintln!("warning: claude reader: {e}");
                }
            }
        }
    }

    if agents.codex {
        match codex::read_sessions(repo_roots, &cutoff, quiet) {
            Ok(s) => sessions.extend(s),
            Err(e) => {
                if !quiet {
                    eprintln!("warning: codex reader: {e}");
                }
            }
        }
    }

    if agents.cursor {
        match cursor::read_sessions(repo_roots, &cutoff, quiet) {
            Ok(s) => sessions.extend(s),
            Err(e) => {
                if !quiet {
                    eprintln!("warning: cursor reader: {e}");
                }
            }
        }
    }

    // Sort by start time, oldest first
    sessions.sort_by_key(|s| s.started_at);
    sessions
}
