use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

fn is_archive_name(name: &str) -> bool {
    name.starts_with("master_log.") && name.ends_with(".jsonl") && name != "master_log.jsonl"
}

pub fn discover_archives(log_path: &Path) -> Result<Vec<PathBuf>> {
    let Some(dir) = log_path.parent() else {
        return Ok(Vec::new());
    };

    let mut archives = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if is_archive_name(name) {
            archives.push(path);
        }
    }

    archives.sort_by_key(|p| p.file_name().map(|n| n.to_os_string()));
    Ok(archives)
}

pub fn discover_logs(log_path: &Path) -> Result<Vec<PathBuf>> {
    let mut files = discover_archives(log_path)?;
    if log_path.is_file() {
        files.push(log_path.to_path_buf());
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn archives_are_sorted_and_current_is_last() {
        let dir = tempdir().expect("tempdir");
        let log_path = dir.path().join("master_log.jsonl");
        fs::write(&log_path, "").expect("write log");
        fs::write(dir.path().join("master_log.20240101T000000Z.jsonl"), "").expect("write a1");
        fs::write(dir.path().join("master_log.20240201T000000Z.jsonl"), "").expect("write a2");

        let files = discover_logs(&log_path).expect("discover");
        assert_eq!(files.len(), 3);
        assert!(files[0].ends_with("master_log.20240101T000000Z.jsonl"));
        assert!(files[1].ends_with("master_log.20240201T000000Z.jsonl"));
        assert!(files[2].ends_with("master_log.jsonl"));
    }
}
