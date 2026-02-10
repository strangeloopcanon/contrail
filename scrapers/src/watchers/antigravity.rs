use super::Harvester;
use anyhow::Result;
use chrono::Utc;
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::Map;
use std::fs;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::sync::mpsc::channel;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, info};

impl Harvester {
    pub async fn run_antigravity_watcher(&self) -> Result<()> {
        info!("starting antigravity watcher");
        let brain_dir = self.config.antigravity_brain.clone();

        loop {
            let mut latest_session = None;
            let mut latest_time = std::time::SystemTime::UNIX_EPOCH;

            if brain_dir.exists() {
                for entry in fs::read_dir(&brain_dir)? {
                    let entry = entry?;
                    if entry.file_type()?.is_dir() {
                        if let Ok(metadata) = entry.metadata() {
                            if let Ok(modified) = metadata.modified() {
                                if modified > latest_time {
                                    latest_time = modified;
                                    latest_session = Some(entry.path());
                                }
                            }
                        }
                    }
                }
            }

            if let Some(session_path) = latest_session {
                let task_md = session_path.join("task.md");
                let plan_md = session_path.join("implementation_plan.md");

                let (tx, rx) = channel();
                let mut watcher = RecommendedWatcher::new(tx, Config::default())?;

                let mut watching = false;
                if task_md.exists() {
                    let _ = watcher.watch(&task_md, RecursiveMode::NonRecursive);
                    watching = true;
                }
                if plan_md.exists() {
                    let _ = watcher.watch(&plan_md, RecursiveMode::NonRecursive);
                    watching = true;
                }

                if watching {
                    info!(path = ?session_path, "watching antigravity session");
                    let mut last_task_pos = fs::metadata(&task_md).map(|m| m.len()).unwrap_or(0);
                    let mut last_plan_pos = fs::metadata(&plan_md).map(|m| m.len()).unwrap_or(0);

                    loop {
                        if let Ok(Ok(_event)) = rx.try_recv() {
                            // Check task.md
                            if let Ok(metadata) = fs::metadata(&task_md) {
                                let current_size = metadata.len();
                                if current_size < last_task_pos {
                                    last_task_pos = 0;
                                }
                                if current_size > last_task_pos {
                                    if let Ok(mut file) = fs::File::open(&task_md) {
                                        let mut reader = BufReader::new(&mut file);
                                        if let Err(e) = reader.seek(SeekFrom::Start(last_task_pos))
                                        {
                                            tracing::warn!(err = %e, "antigravity task.md seek failed");
                                        }
                                        let mut buf = String::new();
                                        if let Err(e) = reader.read_to_string(&mut buf) {
                                            tracing::warn!(err = %e, "antigravity task.md read failed");
                                        }
                                        if !buf.trim().is_empty() {
                                            self.log_interaction_with_metadata(
                                                "antigravity",
                                                session_path.file_name().unwrap().to_str().unwrap(),
                                                "Antigravity Brain",
                                                &buf,
                                                "assistant",
                                                Map::new(),
                                                Some(Utc::now()),
                                            )
                                            .await?;
                                        }
                                    }
                                    last_task_pos = current_size;
                                }
                            }
                            // Check implementation_plan.md
                            if let Ok(metadata) = fs::metadata(&plan_md) {
                                let current_size = metadata.len();
                                if current_size < last_plan_pos {
                                    last_plan_pos = 0;
                                }
                                if current_size > last_plan_pos {
                                    if let Ok(mut file) = fs::File::open(&plan_md) {
                                        let mut reader = BufReader::new(&mut file);
                                        if let Err(e) = reader.seek(SeekFrom::Start(last_plan_pos))
                                        {
                                            tracing::warn!(err = %e, "antigravity plan.md seek failed");
                                        }
                                        let mut buf = String::new();
                                        if let Err(e) = reader.read_to_string(&mut buf) {
                                            tracing::warn!(err = %e, "antigravity plan.md read failed");
                                        }
                                        if !buf.trim().is_empty() {
                                            self.log_interaction_with_metadata(
                                                "antigravity",
                                                session_path.file_name().unwrap().to_str().unwrap(),
                                                "Antigravity Brain",
                                                &buf,
                                                "assistant",
                                                Map::new(),
                                                Some(Utc::now()),
                                            )
                                            .await?;
                                        }
                                    }
                                    last_plan_pos = current_size;
                                }
                            }
                        }
                        sleep(Duration::from_millis(500)).await;

                        // Check for newer sessions occasionally
                        if Utc::now().timestamp() % 10 == 0 {
                            if let Ok(entries) = fs::read_dir(&brain_dir) {
                                let mut found_newer = false;
                                for entry in entries.flatten() {
                                    if let Ok(meta) = entry.metadata() {
                                        if let Ok(mod_time) = meta.modified() {
                                            if mod_time > latest_time {
                                                debug!(
                                                    "found newer antigravity session, switching"
                                                );
                                                found_newer = true;
                                                break;
                                            }
                                        }
                                    }
                                }
                                if found_newer {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            sleep(Duration::from_secs(5)).await;
        }
    }
}
