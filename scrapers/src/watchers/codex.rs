use super::Harvester;
use crate::codex::parse_codex_line;
use anyhow::Result;
use chrono::{DateTime, Datelike, Local, Utc};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::{sleep, Instant};
use tracing::info;

impl Harvester {
    pub async fn run_codex_watcher(&self) -> Result<()> {
        info!("starting codex watcher");
        let codex_root = self.config.codex_root.clone();
        let mut file_positions: HashMap<PathBuf, u64> = HashMap::new();
        let mut file_activity: HashMap<PathBuf, Instant> = HashMap::new();
        let mut file_generating: HashMap<PathBuf, bool> = HashMap::new();
        let mut file_saw_token_count: HashMap<PathBuf, bool> = HashMap::new();
        let mut file_session_ids: HashMap<PathBuf, String> = HashMap::new();
        let mut file_project_context: HashMap<PathBuf, String> = HashMap::new();

        loop {
            let now = Local::now();
            let date_path = codex_root.join(format!(
                "{}/{:02}/{:02}",
                now.year(),
                now.month(),
                now.day()
            ));

            let mut candidates: HashSet<PathBuf> = HashSet::new();
            if date_path.exists() {
                for entry in fs::read_dir(&date_path)? {
                    let entry = entry?;
                    let path = entry.path();
                    if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                        candidates.insert(path);
                    }
                }
            }
            if codex_root.exists() {
                for entry in fs::read_dir(&codex_root)? {
                    let entry = entry?;
                    let path = entry.path();
                    if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("jsonl")
                    {
                        candidates.insert(path);
                    }
                }
            }
            for path in candidates {
                file_positions.entry(path.clone()).or_insert_with(|| {
                    fs::File::open(&path)
                        .ok()
                        .and_then(|f| {
                            let mut r = BufReader::new(f);
                            r.seek(SeekFrom::End(0)).ok()
                        })
                        .unwrap_or(0)
                });
            }

            if date_path.exists() || codex_root.exists() {
                let mut to_remove = Vec::new();
                for (path, pos) in file_positions.iter_mut() {
                    if let Ok(file) = fs::File::open(path) {
                        let mut reader = BufReader::new(file);
                        let current_len = reader.get_ref().metadata().map(|m| m.len()).unwrap_or(0);
                        if current_len < *pos {
                            *pos = 0;
                        }
                        reader.seek(SeekFrom::Start(*pos))?;
                        let mut line = String::new();
                        let mut saw_token_count = *file_saw_token_count.get(path).unwrap_or(&false);
                        let mut session_id =
                            file_session_ids.get(path).cloned().unwrap_or_else(|| {
                                path.file_name()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("unknown")
                                    .to_string()
                            });
                        let mut default_project_context = file_project_context
                            .get(path)
                            .cloned()
                            .unwrap_or_else(|| "Codex Session".to_string());

                        while reader.read_line(&mut line)? > 0 {
                            let len = line.len() as u64;
                            let mut project_context = "Codex Session".to_string();
                            let mut extra_metadata = Map::new();
                            let mut role = "assistant".to_string();
                            let mut content = line.clone();
                            let mut timestamp: Option<DateTime<Utc>> = None;

                            if let Some(parsed) = parse_codex_line(&line) {
                                if let Some(id) = parsed.session_id {
                                    session_id = id;
                                }
                                if let Some(cwd) = parsed.project_context {
                                    project_context = cwd.clone();
                                    default_project_context = cwd;
                                } else {
                                    project_context = default_project_context.clone();
                                }
                                role = parsed.role;
                                content = parsed.content;
                                timestamp = parsed.timestamp;
                                for (k, v) in parsed.metadata {
                                    if k.starts_with("usage_") {
                                        saw_token_count = true;
                                    }
                                    extra_metadata.insert(k, v);
                                }
                            }

                            self.log_interaction_with_metadata(
                                "codex-cli",
                                &session_id,
                                &project_context,
                                &content,
                                &role,
                                extra_metadata,
                                timestamp,
                            )
                            .await?;

                            *pos += len;
                            line.clear();
                            file_generating.insert(path.clone(), true);
                            file_activity.insert(path.clone(), Instant::now());
                        }

                        file_session_ids.insert(path.clone(), session_id);
                        file_project_context.insert(path.clone(), default_project_context);
                        file_saw_token_count.insert(path.clone(), saw_token_count);
                    } else {
                        to_remove.push(path.clone());
                    }
                }

                // Session end detection across iterations
                for (path, last) in file_activity.clone() {
                    if !file_generating.get(&path).copied().unwrap_or(false) {
                        continue;
                    }
                    if last.elapsed() > Duration::from_secs(self.config.codex_silence_secs) {
                        let saw_tokens = file_saw_token_count.get(&path).copied().unwrap_or(false);
                        let session_id =
                            file_session_ids.get(&path).cloned().unwrap_or_else(|| {
                                path.file_name()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("unknown")
                                    .to_string()
                            });
                        let project_context = file_project_context
                            .get(&path)
                            .cloned()
                            .unwrap_or_else(|| "Codex Session".to_string());
                        let mut completion_metadata = Map::new();
                        completion_metadata
                            .insert("interrupted".to_string(), Value::Bool(!saw_tokens));
                        self.notifier
                            .send_notification("AI Task Complete", "Codex CLI finished.");
                        self.log_interaction_with_metadata(
                            "codex-cli",
                            &session_id,
                            &project_context,
                            "Session Ended",
                            "system",
                            completion_metadata,
                            Some(Utc::now()),
                        )
                        .await?;
                        file_generating.insert(path.clone(), false);
                        file_saw_token_count.insert(path.clone(), false);
                    }
                }

                for path in to_remove {
                    file_positions.remove(&path);
                    file_activity.remove(&path);
                    file_generating.remove(&path);
                    file_saw_token_count.remove(&path);
                    file_session_ids.remove(&path);
                    file_project_context.remove(&path);
                }
            }

            sleep(Duration::from_secs(2)).await;
        }
    }
}
