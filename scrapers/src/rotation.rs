use crate::log_index::discover_archives;
use anyhow::Result;
use chrono::Utc;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct RotationResult {
    pub rotated: bool,
    pub archive_path: Option<PathBuf>,
    pub pruned: usize,
}

pub fn rotate_if_needed(
    log_path: &Path,
    max_bytes: u64,
    keep_files: usize,
) -> Result<RotationResult> {
    let mut result = RotationResult::default();
    let Some(dir) = log_path.parent() else {
        return Ok(result);
    };

    fs::create_dir_all(dir)?;

    let Ok(meta) = fs::metadata(log_path) else {
        return Ok(result);
    };

    if meta.len() <= max_bytes {
        return Ok(result);
    }

    let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let archive_path = dir.join(format!("master_log.{timestamp}.jsonl"));
    fs::rename(log_path, &archive_path)?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;

    result.rotated = true;
    result.archive_path = Some(archive_path);

    let mut archives = discover_archives(log_path)?;
    let target_keep = keep_files.max(1);
    if archives.len() > target_keep {
        let to_prune = archives.len() - target_keep;
        archives.truncate(to_prune);
        for path in archives {
            fs::remove_file(path)?;
            result.pruned += 1;
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn rotates_and_prunes_old_archives() {
        let dir = tempdir().expect("tempdir");
        let log_path = dir.path().join("master_log.jsonl");
        fs::write(&log_path, "0123456789").expect("write log");
        fs::write(dir.path().join("master_log.20240101T000000Z.jsonl"), "a").expect("write a1");
        fs::write(dir.path().join("master_log.20240201T000000Z.jsonl"), "b").expect("write a2");

        let res = rotate_if_needed(&log_path, 5, 2).expect("rotate");
        assert!(res.rotated);
        assert!(log_path.exists());
        assert_eq!(res.pruned, 1);

        let archives = discover_archives(&log_path).expect("discover");
        assert_eq!(archives.len(), 2);
    }
}
