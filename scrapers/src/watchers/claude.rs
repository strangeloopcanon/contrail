use super::Harvester;
use crate::claude::{parse_claude_line, parse_claude_session_line};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::Map;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::{sleep, Instant};
use tracing::{debug, info, warn};

impl Harvester {
    pub async fn run_claude_watcher(&self) -> Result<()> {
        info!("starting claude watcher");
        let claude_history = self.config.claude_history.clone();

        if claude_history.exists() {
            info!(path = ?claude_history, "watching claude history");
            let file = fs::File::open(&claude_history)?;
            let mut reader = BufReader::new(file);
            let mut pos = reader.seek(SeekFrom::End(0))?;

            let mut last_activity = Instant::now();
            let mut generating = false;
            let mut cwd_cache: HashMap<String, String> = HashMap::new();

            loop {
                let current_len = fs::metadata(&claude_history)?.len();
                if current_len < pos {
                    pos = 0;
                }
                if current_len > pos {
                    reader.seek(SeekFrom::Start(pos))?;
                    let mut line = String::new();
                    while reader.read_line(&mut line)? > 0 {
                        debug!("new claude line");
                        let mut metadata = Map::new();
                        let mut project_context = "Claude Global".to_string();
                        let mut role = "user_or_assistant".to_string();
                        let mut session_id = "history".to_string();
                        let mut content = line.clone();
                        let mut timestamp: Option<DateTime<Utc>> = None;

                        if let Some(parsed) = parse_claude_line(&line) {
                            role = parsed.role;
                            content = parsed.content;
                            timestamp = parsed.timestamp;
                            if let Some(id) = parsed.session_id {
                                session_id = id.clone();
                            }
                            if let Some(cwd) = parsed.project_context {
                                project_context = cwd.clone();
                                cwd_cache.insert(session_id.clone(), cwd);
                            } else if let Some(cached) = cwd_cache.get(&session_id) {
                                project_context = cached.clone();
                            }
                            for (k, v) in parsed.metadata {
                                metadata.insert(k, v);
                            }
                        }

                        self.log_interaction_with_metadata(
                            "claude-code",
                            &session_id,
                            &project_context,
                            &content,
                            &role,
                            metadata,
                            timestamp,
                        )
                        .await?;

                        pos += line.len() as u64;
                        line.clear();
                        last_activity = Instant::now();
                        if !generating {
                            generating = true;
                        }
                    }
                }

                if generating
                    && last_activity.elapsed()
                        > Duration::from_secs(self.config.claude_silence_secs)
                {
                    generating = false;
                    self.notifier
                        .send_notification("AI Task Complete", "Claude Code finished.");
                }

                sleep(Duration::from_millis(500)).await;
            }
        } else {
            warn!(path = ?claude_history, "claude history not found");
        }
        Ok(())
    }

    /// Watch Claude Code's project session files for detailed token usage data.
    /// These files are located in ~/.claude/projects/*/*.jsonl
    pub async fn run_claude_projects_watcher(&self) -> Result<()> {
        info!("starting claude projects watcher");
        let claude_projects = self.config.claude_projects.clone();

        if !claude_projects.exists() {
            warn!(path = ?claude_projects, "claude projects directory not found");
            return Ok(());
        }

        info!(path = ?claude_projects, "watching claude projects");

        let mut file_positions: HashMap<PathBuf, u64> = HashMap::new();
        let mut last_activity = Instant::now();
        let mut generating = false;

        loop {
            if let Ok(project_dirs) = fs::read_dir(&claude_projects) {
                for project_entry in project_dirs.flatten() {
                    let project_path = project_entry.path();
                    if !project_path.is_dir() {
                        continue;
                    }

                    if let Ok(session_files) = fs::read_dir(&project_path) {
                        for session_entry in session_files.flatten() {
                            let session_path = session_entry.path();
                            if session_path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                                continue;
                            }

                            let pos = file_positions.entry(session_path.clone()).or_insert(0);

                            if let Ok(file) = fs::File::open(&session_path) {
                                let mut reader = BufReader::new(file);
                                let current_len =
                                    reader.get_ref().metadata().map(|m| m.len()).unwrap_or(0);

                                if current_len < *pos {
                                    *pos = 0;
                                }

                                if current_len > *pos {
                                    if reader.seek(SeekFrom::Start(*pos)).is_err() {
                                        continue;
                                    }

                                    let mut line = String::new();
                                    while reader.read_line(&mut line).unwrap_or(0) > 0 {
                                        let len = line.len() as u64;

                                        if let Some(parsed) = parse_claude_session_line(&line) {
                                            let project_context = parsed
                                                .project_context
                                                .clone()
                                                .unwrap_or_else(|| "Claude Session".to_string());

                                            let session_id =
                                                parsed.session_id.clone().unwrap_or_else(|| {
                                                    session_path
                                                        .file_stem()
                                                        .and_then(|s| s.to_str())
                                                        .unwrap_or("unknown")
                                                        .to_string()
                                                });

                                            self.log_interaction_with_metadata(
                                                "claude-code",
                                                &session_id,
                                                &project_context,
                                                &parsed.content,
                                                &parsed.role,
                                                parsed.metadata,
                                                parsed.timestamp,
                                            )
                                            .await?;

                                            last_activity = Instant::now();
                                            if !generating {
                                                generating = true;
                                                info!(project = %project_context, "claude code active");
                                            }
                                        }

                                        *pos += len;
                                        line.clear();
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Session end detection
            if generating
                && last_activity.elapsed() > Duration::from_secs(self.config.claude_silence_secs)
            {
                generating = false;
                self.notifier
                    .send_notification("AI Task Complete", "Claude Code finished.");
            }

            sleep(Duration::from_secs(2)).await;
        }
    }
}
