use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const LOCAL_DIR: &str = ".context/.memex";
const ROOTS_FILE: &str = ".context/.memex/repo_roots.txt";

/// Load repo-root aliases for matching against agent-native logs.
///
/// Returns a de-duplicated list that always includes the current repo root
/// (and, when available, its canonical path).
pub fn load_repo_roots(repo_root: &Path) -> Vec<String> {
    let mut roots = Vec::new();

    // Existing aliases (local-only)
    let path = roots_file(repo_root);
    if let Ok(content) = fs::read_to_string(&path) {
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            roots.push(normalize_root(line));
        }
    }

    // Always include current roots (even if the alias file doesn't exist yet).
    roots.extend(current_roots(repo_root));

    dedupe_preserve_order(roots)
}

/// Ensure the local-only alias store exists, and auto-add the current repo root
/// if it's missing. This supports repo renames/moves without user intervention.
pub fn ensure_current_repo_roots(repo_root: &Path) -> Result<Vec<String>> {
    let context_dir = repo_root.join(".context");
    if !context_dir.is_dir() {
        // Don't create `.context/` implicitly.
        return Ok(current_roots(repo_root));
    }

    fs::create_dir_all(repo_root.join(LOCAL_DIR))
        .with_context(|| format!("create {}", repo_root.join(LOCAL_DIR).display()))?;

    // Keep aliases local-only (not committed).
    let _ = ensure_git_info_exclude(repo_root, ".context/.memex/");

    let path = roots_file(repo_root);
    let mut existing = Vec::new();
    if let Ok(content) = fs::read_to_string(&path) {
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            existing.push(normalize_root(line));
        }
    }

    let mut merged = existing.clone();
    let mut changed = false;
    for r in current_roots(repo_root) {
        if !merged.iter().any(|e| e == &r) {
            merged.push(r);
            changed = true;
        }
    }

    let merged = dedupe_preserve_order(merged);
    if !path.is_file() || changed {
        let mut out = String::new();
        out.push_str("# memex repo root aliases (local-only)\n");
        out.push_str("# Used to match agent-native logs across repo renames/moves.\n");
        for r in &merged {
            out.push_str(r);
            out.push('\n');
        }
        fs::write(&path, out).with_context(|| format!("write {}", path.display()))?;
    }

    Ok(merged)
}

pub fn matches_any_root(path: &str, roots: &[String]) -> bool {
    roots.iter().any(|r| is_under_root(path, r))
}

fn roots_file(repo_root: &Path) -> PathBuf {
    repo_root.join(ROOTS_FILE)
}

fn current_roots(repo_root: &Path) -> Vec<String> {
    let mut out = Vec::new();

    // Prefer the value from git rev-parse (repo_root passed in is already that),
    // but normalize to reduce accidental duplicates.
    let raw = repo_root.to_string_lossy().to_string();
    out.push(normalize_root(&raw));

    if let Ok(canon) = fs::canonicalize(repo_root) {
        let canon = canon.to_string_lossy().to_string();
        let canon = normalize_root(&canon);
        if !out.iter().any(|e| e == &canon) {
            out.push(canon);
        }
    }

    out
}

fn normalize_root(root: &str) -> String {
    let mut s = root.trim().to_string();
    while s.len() > 1 && (s.ends_with('/') || s.ends_with('\\')) {
        s.pop();
    }
    s
}

fn is_under_root(path: &str, root: &str) -> bool {
    if path == root {
        return true;
    }
    if !path.starts_with(root) {
        return false;
    }
    // Boundary check: root "/foo/bar" should match "/foo/bar/..." but not "/foo/bar2".
    matches!(path.as_bytes().get(root.len()), Some(b'/') | Some(b'\\'))
}

fn dedupe_preserve_order(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for v in values {
        if v.is_empty() {
            continue;
        }
        if seen.insert(v.clone()) {
            out.push(v);
        }
    }
    out
}

fn ensure_git_info_exclude(repo_root: &Path, pattern: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-path", "info/exclude"])
        .current_dir(repo_root)
        .output()
        .context("run git rev-parse --git-path info/exclude")?;
    if !output.status.success() {
        return Ok(());
    }
    let rel = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if rel.is_empty() {
        return Ok(());
    }

    let p = PathBuf::from(rel);
    let exclude_path = if p.is_absolute() {
        p
    } else {
        repo_root.join(p)
    };

    let mut existing = fs::read_to_string(&exclude_path).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == pattern.trim()) {
        return Ok(());
    }
    if !existing.ends_with('\n') && !existing.is_empty() {
        existing.push('\n');
    }
    existing.push_str(pattern.trim());
    existing.push('\n');
    fs::write(&exclude_path, existing)
        .with_context(|| format!("write {}", exclude_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{is_under_root, normalize_root};

    #[test]
    fn root_matching_is_boundary_aware() {
        assert!(is_under_root("/a/b", "/a/b"));
        assert!(is_under_root("/a/b/c", "/a/b"));
        assert!(!is_under_root("/a/b2", "/a/b"));
    }

    #[test]
    fn normalizes_trailing_slash() {
        assert_eq!(normalize_root("/a/b/"), "/a/b");
    }
}
