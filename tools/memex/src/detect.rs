use crate::types::DetectedAgents;
use std::path::{Path, PathBuf};

/// Detect which agents have been used in the given repo by checking their
/// native storage locations for sessions referencing this repo path.
pub fn detect_agents(repo_roots: &[String]) -> DetectedAgents {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return DetectedAgents::default(),
    };

    DetectedAgents {
        cursor: detect_cursor(&home, repo_roots),
        codex: detect_codex(&home, repo_roots),
        claude: detect_claude(&home, repo_roots),
        gemini: detect_gemini(&home, repo_roots),
    }
}

fn detect_cursor(home: &Path, repo_roots: &[String]) -> bool {
    let ws_storage = home.join("Library/Application Support/Cursor/User/workspaceStorage");
    if !ws_storage.is_dir() {
        return false;
    }
    let entries = match std::fs::read_dir(&ws_storage) {
        Ok(e) => e,
        Err(_) => return false,
    };
    for entry in entries.flatten() {
        let workspace_json = entry.path().join("workspace.json");
        if let Ok(content) = std::fs::read_to_string(&workspace_json) {
            if repo_roots
                .iter()
                .any(|r| contains_repo_reference(&content, r))
            {
                return true;
            }
        }
    }
    false
}

fn detect_codex(home: &Path, repo_roots: &[String]) -> bool {
    for sessions_root in codex_sessions_roots_from_home(home) {
        if scan_jsonl_dir_for_repo(&sessions_root, repo_roots, 500) {
            return true;
        }
    }
    false
}

fn detect_claude(home: &Path, repo_roots: &[String]) -> bool {
    let projects_dir = home.join(".claude/projects");
    if projects_dir.is_dir() {
        // Claude Code stores per-project dirs; check if any reference this repo
        if let Ok(entries) = std::fs::read_dir(&projects_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                // The directory name is often a hash, but session files inside contain cwd
                if scan_jsonl_dir_for_repo(&path, repo_roots, 200) {
                    return true;
                }
            }
        }
    }

    // Also check the global history file
    let history = home.join(".claude/history.jsonl");
    if history.is_file() && scan_jsonl_file_for_repo(&history, repo_roots, 500) {
        return true;
    }

    false
}

fn detect_gemini(home: &Path, repo_roots: &[String]) -> bool {
    let brain = home.join(".gemini/antigravity/brain");
    if !brain.is_dir() {
        return false;
    }
    // Check if any session dirs reference the repo in their content
    if let Ok(entries) = std::fs::read_dir(&brain) {
        for entry in entries.flatten() {
            let task_md = entry.path().join("task.md");
            if let Ok(content) = std::fs::read_to_string(&task_md) {
                if repo_roots.iter().any(|r| content.contains(r)) {
                    return true;
                }
            }
        }
    }
    false
}

/// Scan JSONL files in a directory (recursively) for lines containing the repo path.
fn scan_jsonl_dir_for_repo(dir: &Path, repo_roots: &[String], max_files: usize) -> bool {
    let mut checked = 0usize;
    scan_jsonl_dir_recursive(dir, repo_roots, max_files, &mut checked)
}

fn scan_jsonl_dir_recursive(
    dir: &Path,
    repo_roots: &[String],
    max_files: usize,
    checked: &mut usize,
) -> bool {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return false,
    };
    for entry in entries.flatten() {
        if *checked >= max_files {
            return false;
        }
        let path = entry.path();
        if path.is_dir() {
            if scan_jsonl_dir_recursive(&path, repo_roots, max_files, checked) {
                return true;
            }
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            *checked += 1;
            if scan_jsonl_file_for_repo(&path, repo_roots, 100) {
                return true;
            }
        }
    }
    false
}

/// Check if a JSONL file contains lines referencing the repo path.
fn scan_jsonl_file_for_repo(path: &Path, repo_roots: &[String], max_lines: usize) -> bool {
    use std::io::{BufRead, BufReader};
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let reader = BufReader::new(file);
    for (i, line) in reader.lines().enumerate() {
        if i >= max_lines {
            break;
        }
        if let Ok(line) = line {
            if repo_roots.iter().any(|r| contains_repo_reference(&line, r)) {
                return true;
            }
        }
    }
    false
}

fn contains_repo_reference(haystack: &str, repo_root: &str) -> bool {
    if repo_root.is_empty() {
        return false;
    }

    let mut start = 0usize;
    while start < haystack.len() {
        let Some(rel_idx) = haystack[start..].find(repo_root) else {
            break;
        };
        let idx = start + rel_idx;
        let before = haystack[..idx].chars().next_back();
        let after_idx = idx + repo_root.len();
        let after = haystack[after_idx..].chars().next();
        if is_left_boundary(before) && is_right_boundary(after) {
            return true;
        }
        start = idx + 1;
    }
    false
}

fn is_left_boundary(c: Option<char>) -> bool {
    c.is_none_or(|ch| !is_path_char(ch))
}

fn is_right_boundary(c: Option<char>) -> bool {
    c.is_none_or(|ch| ch == '/' || ch == '\\' || !is_path_char(ch))
}

fn is_path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')
}

fn codex_sessions_roots_from_home(home: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();

    let cli_sessions = home.join(".codex/sessions");
    if cli_sessions.is_dir() {
        roots.push(cli_sessions);
    }

    let desktop_sessions = home.join("Library/Application Support/codex-desktop/codex/sessions");
    if desktop_sessions.is_dir() {
        roots.push(desktop_sessions);
    }

    roots
}

/// Get standard storage paths for reference.
pub fn cursor_workspace_storage() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join("Library/Application Support/Cursor/User/workspaceStorage"))
}

pub fn codex_sessions_root() -> Option<PathBuf> {
    codex_sessions_roots().into_iter().next()
}

pub fn codex_sessions_roots() -> Vec<PathBuf> {
    dirs::home_dir()
        .map(|h| codex_sessions_roots_from_home(&h))
        .unwrap_or_default()
}

pub fn claude_projects_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude/projects"))
}

pub fn claude_history_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude/history.jsonl"))
}

#[cfg(test)]
mod tests {
    use super::contains_repo_reference;

    #[test]
    fn contains_repo_reference_matches_exact_or_child_path() {
        let root = "/Users/alice/project";
        assert!(contains_repo_reference("cwd:/Users/alice/project", root));
        assert!(contains_repo_reference(
            "cwd:/Users/alice/project/src/main.rs",
            root
        ));
    }

    #[test]
    fn contains_repo_reference_rejects_prefix_overlap() {
        let root = "/Users/alice/project";
        assert!(!contains_repo_reference(
            "cwd:/Users/alice/project-extra",
            root
        ));
    }
}
