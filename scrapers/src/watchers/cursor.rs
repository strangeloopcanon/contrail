use super::Harvester;
use crate::cursor::{fingerprint, read_cursor_messages, timestamp_from_metadata};
use anyhow::Result;
use chrono::Utc;
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::time::Duration;
use tokio::time::{sleep, Instant};
use tracing::{debug, error, info, warn};

impl Harvester {
    pub async fn run_cursor_watcher(&self) -> Result<()> {
        info!("starting cursor watcher");
        let cursor_base = self.config.cursor_storage.clone();

        if !cursor_base.exists() {
            warn!("cursor workspaceStorage not found");
            return Ok(());
        }

        let (tx, rx) = channel();
        let mut watcher = RecommendedWatcher::new(tx, Config::default())?;
        if let Err(e) = watcher.watch(&cursor_base, RecursiveMode::Recursive) {
            error!(err = ?e, "failed to watch cursor DB");
            return Ok(());
        }

        info!(path = ?cursor_base, "watching all cursor workspaces");

        let mut last_activity = Instant::now();
        let mut generating = false;
        let mut active_project = "Unknown".to_string();
        let mut workspace_hash = "unknown_hash".to_string();
        let mut last_state_path: Option<PathBuf> = None;
        let mut last_cursor_snapshot: Option<u64> = None;

        loop {
            if let Ok(res) = rx.try_recv() {
                match res {
                    Ok(event) => {
                        let mut is_db_change = false;

                        for path in event.paths {
                            if path.file_name().and_then(|s| s.to_str()) == Some("state.vscdb") {
                                is_db_change = true;
                                last_state_path = Some(path.clone());
                                if let Some(parent) = path.parent() {
                                    if let Some(hash) = parent.file_name().and_then(|s| s.to_str())
                                    {
                                        workspace_hash = hash.to_string();
                                    }

                                    let workspace_json = parent.join("workspace.json");
                                    if let Ok(content) = fs::read_to_string(&workspace_json) {
                                        if let Ok(json) =
                                            serde_json::from_str::<serde_json::Value>(&content)
                                        {
                                            if let Some(folder) =
                                                json.get("folder").and_then(|v| v.as_str())
                                            {
                                                active_project =
                                                    folder.replace("file://", "").to_string();
                                                active_project = active_project.replace("%20", " ");
                                            } else if let Some(name) =
                                                json.get("name").and_then(|v| v.as_str())
                                            {
                                                active_project = name.to_string();
                                            }
                                        }
                                    }
                                }
                                break;
                            }
                        }

                        if is_db_change {
                            last_activity = Instant::now();
                            if !generating {
                                generating = true;
                                info!(project = %active_project, hash = %workspace_hash, "cursor active");
                            }
                        }
                    }
                    Err(e) => warn!(err = ?e, "cursor watch error"),
                }
            }

            // Check silence
            if generating
                && last_activity.elapsed() > Duration::from_secs(self.config.cursor_silence_secs)
            {
                generating = false;
                info!(project = %active_project, "cursor finished generating");
                self.notifier.send_notification(
                    "AI Task Complete",
                    &format!("Cursor finished in {}", active_project),
                );

                let mut extra_metadata = serde_json::Map::new();

                if let Some(db_path) = last_state_path.as_ref() {
                    match read_cursor_messages(db_path) {
                        Ok(messages) if !messages.is_empty() => {
                            let message_count = messages.len();
                            let snapshot = fingerprint(&messages);
                            if Some(snapshot) != last_cursor_snapshot {
                                for message in messages {
                                    let ts = timestamp_from_metadata(&message.metadata);
                                    self.log_interaction_with_metadata(
                                        "cursor",
                                        &workspace_hash,
                                        &active_project,
                                        &message.content,
                                        &message.role,
                                        message.metadata,
                                        ts,
                                    )
                                    .await?;
                                }
                                extra_metadata.insert(
                                    "cursor_message_count".to_string(),
                                    serde_json::json!(message_count),
                                );
                                last_cursor_snapshot = Some(snapshot);
                            } else {
                                debug!("cursor snapshot unchanged, skipping");
                            }
                        }
                        Ok(_) => {
                            debug!("cursor state snapshot contained no chat messages");
                        }
                        Err(e) => {
                            error!(err = ?e, "failed to read cursor state");
                        }
                    }
                }

                // Capture git context & effects
                if let Ok(repo) = std::process::Command::new("git")
                    .arg("-C")
                    .arg(&active_project)
                    .arg("rev-parse")
                    .arg("--abbrev-ref")
                    .arg("HEAD")
                    .output()
                {
                    if let Ok(branch) = String::from_utf8(repo.stdout) {
                        extra_metadata.insert(
                            "git_branch".to_string(),
                            serde_json::Value::String(branch.trim().to_string()),
                        );
                    }
                }

                if let Ok(status) = std::process::Command::new("git")
                    .arg("-C")
                    .arg(&active_project)
                    .arg("status")
                    .arg("--short")
                    .output()
                {
                    if let Ok(changes) = String::from_utf8(status.stdout) {
                        let effects: Vec<String> = changes.lines().map(|s| s.to_string()).collect();
                        if !effects.is_empty() {
                            extra_metadata
                                .insert("file_effects".to_string(), serde_json::json!(effects));
                        }
                    }
                }

                self.log_interaction_with_metadata(
                    "cursor",
                    &workspace_hash,
                    &active_project,
                    "Session Ended",
                    "system",
                    extra_metadata,
                    Some(Utc::now()),
                )
                .await?;
            }

            sleep(Duration::from_millis(100)).await;
        }
    }
}
