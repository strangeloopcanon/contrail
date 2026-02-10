use anyhow::Result;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use std::{ffi::OsStr, process};

/// Greppable search across `.context/sessions/*.md` and `.context/LEARNINGS.md`.
///
/// Output format matches common grep tools:
/// - default: `<path>:<line>:<content>`
/// - `--files`: `<path>` (once per matching file)
///
/// Notes:
/// - This is a literal substring search (not regex) to keep it lightweight.
/// - `--days` only filters session files by mtime; learnings are always searched.
pub fn run_search(
    repo_root: &Path,
    query: &str,
    days: u64,
    limit: usize,
    case_sensitive: bool,
    files: bool,
) -> Result<()> {
    let context_dir = repo_root.join(".context");
    let sessions_dir = context_dir.join("sessions");
    let learnings_path = context_dir.join("LEARNINGS.md");

    if query.is_empty() {
        return Ok(());
    }

    if !sessions_dir.is_dir() && !learnings_path.is_file() {
        println!("No memex context found in this repo.");
        println!("Hint: run `memex init` and `memex sync` (or `memex unlock` if using vault.age).");
        return Ok(());
    }

    let cutoff = cutoff_time(days);
    let query_lower = if case_sensitive {
        None
    } else {
        Some(query.to_lowercase())
    };
    let mut matches = 0usize;

    // Search learnings first (small, curated, always included).
    if learnings_path.is_file() {
        matches += search_file(
            repo_root,
            &learnings_path,
            query,
            query_lower.as_deref(),
            limit.saturating_sub(matches),
            case_sensitive,
            files,
        )?;
    }
    if matches >= limit {
        return Ok(());
    }

    // Search sessions directory.
    if sessions_dir.is_dir() {
        let mut entries: Vec<PathBuf> = fs::read_dir(&sessions_dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file() && p.extension() == Some(OsStr::new("md")))
            .collect();

        // Stable output: sort by filename.
        entries.sort();

        for path in entries {
            if matches >= limit {
                break;
            }

            if let Some(cutoff) = cutoff {
                if let Ok(meta) = fs::metadata(&path) {
                    if let Ok(modified) = meta.modified() {
                        if modified < cutoff {
                            continue;
                        }
                    }
                }
            }

            matches += search_file(
                repo_root,
                &path,
                query,
                query_lower.as_deref(),
                limit.saturating_sub(matches),
                case_sensitive,
                files,
            )?;
        }
    }

    if matches == 0 {
        // Keep output clean/greppable; signal "no matches" via exit code.
        process::exit(1);
    }

    Ok(())
}

fn cutoff_time(days: u64) -> Option<SystemTime> {
    if days == 0 {
        return None;
    }
    let secs = days.saturating_mul(24 * 60 * 60);
    SystemTime::now().checked_sub(Duration::from_secs(secs))
}

fn search_file(
    repo_root: &Path,
    path: &Path,
    query: &str,
    query_lower: Option<&str>,
    limit: usize,
    case_sensitive: bool,
    files: bool,
) -> Result<usize> {
    if limit == 0 {
        return Ok(0);
    }

    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Ok(0),
    };
    let reader = BufReader::new(file);

    let display = repo_relative(repo_root, path);

    let mut count = 0usize;
    for (idx, line) in reader.lines().enumerate() {
        if count >= limit {
            break;
        }
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };

        if !line_matches(&line, query, query_lower, case_sensitive) {
            continue;
        }

        if files {
            println!("{}", display);
            return Ok(1);
        }

        let line_no = idx + 1;
        println!("{}:{}:{}", display, line_no, line);
        count += 1;
    }

    Ok(count)
}

fn line_matches(line: &str, query: &str, query_lower: Option<&str>, case_sensitive: bool) -> bool {
    if case_sensitive {
        return line.contains(query);
    }
    let Some(query_lower) = query_lower else {
        // Defensive fallback; should never happen since we precompute when case_sensitive=false.
        return line.to_lowercase().contains(&query.to_lowercase());
    };
    line.to_lowercase().contains(query_lower)
}

fn repo_relative(repo_root: &Path, path: &Path) -> String {
    path.strip_prefix(repo_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::line_matches;

    #[test]
    fn literal_substring_case_insensitive() {
        assert!(line_matches("Hello World", "world", Some("world"), false));
        assert!(line_matches("Hello World", "WORLD", Some("world"), false));
        assert!(!line_matches(
            "Hello World",
            "planet",
            Some("planet"),
            false
        ));
    }

    #[test]
    fn literal_substring_case_sensitive() {
        assert!(line_matches("Hello World", "World", None, true));
        assert!(!line_matches("Hello World", "world", None, true));
    }
}
