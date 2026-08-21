use super::Harvester;
use crate::deepseek_harness::{parse_event, read_transcript, source_instance};
use crate::parse::ParsedLine;
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tokio::time::sleep;
use tracing::{debug, info, warn};
use walkdir::WalkDir;

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileRevision {
    len: u64,
    modified: Option<SystemTime>,
}

#[derive(Debug, Clone, Default)]
struct FileCursor {
    revision: Option<FileRevision>,
    last_seq: Option<u64>,
}

#[derive(Debug)]
struct PendingEvent {
    seq: u64,
    parsed: ParsedLine,
}

#[derive(Debug)]
struct PendingBatch {
    path: PathBuf,
    revision: FileRevision,
    last_seq: Option<u64>,
    events: Vec<PendingEvent>,
}

#[derive(Default)]
struct DshCollector {
    files: HashMap<PathBuf, FileCursor>,
    watermarks: HashMap<String, u64>,
}

impl DshCollector {
    fn new(watermarks: HashMap<String, u64>) -> Self {
        Self {
            files: HashMap::new(),
            watermarks,
        }
    }

    fn collect(&self, root: &Path) -> Vec<PendingBatch> {
        if !root.exists() {
            return Vec::new();
        }
        let instance = source_instance(root);
        let mut paths = WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.path().to_path_buf())
            .filter(|path| is_dsh_transcript(path))
            .collect::<Vec<_>>();
        paths.sort();

        let mut batches = Vec::new();
        for path in paths {
            let Ok(metadata) = fs::metadata(&path) else {
                continue;
            };
            let revision = FileRevision {
                len: metadata.len(),
                modified: metadata.modified().ok(),
            };
            if self
                .files
                .get(&path)
                .and_then(|cursor| cursor.revision.as_ref())
                == Some(&revision)
            {
                continue;
            }
            let transcript = match read_transcript(&path) {
                Ok(transcript) => transcript,
                Err(error) => {
                    debug!(path = ?path, err = %error, "DSH transcript not yet readable");
                    continue;
                }
            };
            let prior_seq = self
                .files
                .get(&path)
                .and_then(|cursor| cursor.last_seq)
                .or_else(|| self.watermarks.get(&transcript.header.id).copied());
            let last_seq = transcript
                .events
                .iter()
                .filter_map(|event| event.get("seq").and_then(Value::as_u64))
                .max();
            let events = transcript
                .events
                .into_iter()
                .filter_map(|event| {
                    let seq = event.get("seq").and_then(Value::as_u64)?;
                    if prior_seq.is_some_and(|prior| seq <= prior) {
                        return None;
                    }
                    let mut parsed = parse_event(&transcript.header, &event)?;
                    parsed.metadata.insert(
                        "dsh_source_instance".to_string(),
                        Value::String(instance.clone()),
                    );
                    Some(PendingEvent { seq, parsed })
                })
                .collect();
            batches.push(PendingBatch {
                path,
                revision,
                last_seq,
                events,
            });
        }
        batches
    }

    fn commit_seq(&mut self, path: &Path, seq: u64) {
        let cursor = self.files.entry(path.to_path_buf()).or_default();
        cursor.last_seq = Some(cursor.last_seq.map_or(seq, |prior| prior.max(seq)));
        cursor.revision = None;
    }

    fn commit_batch(&mut self, batch: &PendingBatch) {
        let cursor = self.files.entry(batch.path.clone()).or_default();
        cursor.last_seq = match (cursor.last_seq, batch.last_seq) {
            (Some(prior), Some(last)) => Some(prior.max(last)),
            (prior, last) => prior.or(last),
        };
        cursor.revision = Some(batch.revision.clone());
    }
}

fn is_dsh_transcript(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|value| value.to_str()),
        Some("session.jsonl" | "session.jsonl.zstd")
    )
}

fn load_dsh_watermarks(log_path: &Path, instance: &str) -> Result<HashMap<String, u64>> {
    let mut watermarks: HashMap<String, u64> = HashMap::new();
    for path in crate::log_index::discover_logs(log_path)? {
        for line in BufReader::new(fs::File::open(path)?).lines() {
            let line = line?;
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if value.get("source_tool").and_then(Value::as_str)
                != Some(crate::deepseek_harness::SOURCE_TOOL)
                || value
                    .pointer("/metadata/dsh_source_instance")
                    .and_then(Value::as_str)
                    != Some(instance)
            {
                continue;
            }
            let Some(session) = value.get("session_id").and_then(Value::as_str) else {
                continue;
            };
            let Some(seq) = value
                .pointer("/metadata/dsh_event_seq")
                .and_then(Value::as_u64)
            else {
                continue;
            };
            watermarks
                .entry(session.to_string())
                .and_modify(|prior| *prior = (*prior).max(seq))
                .or_insert(seq);
        }
    }
    Ok(watermarks)
}

impl Harvester {
    pub async fn run_deepseek_harness_watcher(&self) -> Result<()> {
        let root = self.config.dsh_sessions.clone();
        info!(path = ?root, "starting DeepSeek Harness watcher");
        let instance = source_instance(&root);
        let mut collector =
            DshCollector::new(load_dsh_watermarks(&self.config.log_path, &instance)?);

        loop {
            for batch in collector.collect(&root) {
                let mut failed = false;
                for event in &batch.events {
                    let parsed = &event.parsed;
                    if let Err(error) = self
                        .log_interaction_with_metadata(
                            crate::deepseek_harness::SOURCE_TOOL,
                            parsed.session_id.as_deref().unwrap_or("unknown"),
                            parsed
                                .project_context
                                .as_deref()
                                .unwrap_or("DeepSeek Harness Session"),
                            &parsed.content,
                            &parsed.role,
                            parsed.metadata.clone(),
                            parsed.timestamp,
                        )
                        .await
                    {
                        warn!(err = %error, "write DeepSeek Harness event failed; will retry");
                        failed = true;
                        break;
                    }
                    collector.commit_seq(&batch.path, event.seq);
                }
                if !failed {
                    collector.commit_batch(&batch);
                }
            }
            sleep(Duration::from_millis(500)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn append(path: &Path, line: &str) -> Result<()> {
        let mut file = fs::OpenOptions::new().append(true).open(path)?;
        writeln!(file, "{line}")?;
        file.sync_all()?;
        Ok(())
    }

    fn write_master_event(path: &Path, instance: &str, seq: u64) -> Result<()> {
        let value = serde_json::json!({
            "source_tool": crate::deepseek_harness::SOURCE_TOOL,
            "session_id": "session-1",
            "metadata": { "dsh_source_instance": instance, "dsh_event_seq": seq }
        });
        fs::write(path, format!("{value}\n"))?;
        Ok(())
    }

    #[test]
    fn resumes_after_watermark_and_retries_only_uncommitted_sequences() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let sessions = temp.path().join("sessions");
        let session_dir = sessions.join("--tmp-project--/session-1");
        fs::create_dir_all(&session_dir)?;
        let path = session_dir.join("session.jsonl");
        fs::write(&path, concat!(
            "{\"type\":\"session\",\"version\":0,\"id\":\"session-1\",\"createdAt\":1783600629539,\"cwd\":\"/tmp/project\",\"delegationDepth\":0}\n",
            "{\"type\":\"turn/start\",\"seq\":0,\"time\":1783600629540,\"data\":{\"turn\":1}}\n",
            "{\"type\":\"request/context\",\"seq\":1,\"time\":1783600629541,\"data\":{\"provider\":\"deepseek-official\",\"model\":\"deepseek-v4-flash\"}}\n",
            "{\"type\":\"turn/end\",\"seq\":2,\"time\":1783600629542,\"data\":{\"turn\":1,\"reason\":{\"kind\":\"completed\"}}}\n"
        ))?;

        let mut collector = DshCollector::new(HashMap::from([("session-1".to_string(), 0)]));
        let batches = collector.collect(&sessions);
        assert_eq!(
            batches[0]
                .events
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        collector.commit_seq(&batches[0].path, 1);
        let retry = collector.collect(&sessions);
        assert_eq!(
            retry[0]
                .events
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            vec![2]
        );
        collector.commit_seq(&retry[0].path, 2);
        collector.commit_batch(&retry[0]);
        assert!(collector.collect(&sessions).is_empty());

        append(
            &path,
            r#"{"type":"turn/start","seq":3,"time":1783600629543,"data":{"turn":2}}"#,
        )?;
        let next = collector.collect(&sessions);
        assert_eq!(next[0].events[0].seq, 3);
        Ok(())
    }

    #[test]
    fn watermarks_include_rotated_logs() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let log_path = temp.path().join("master_log.jsonl");
        write_master_event(
            &temp.path().join("master_log.20260820T010000Z.jsonl"),
            "instance-1",
            8,
        )?;
        write_master_event(&log_path, "instance-1", 3)?;
        assert_eq!(
            load_dsh_watermarks(&log_path, "instance-1")?["session-1"],
            8
        );
        Ok(())
    }

    #[test]
    fn captures_external_rc8_root_when_requested() {
        let Ok(root) = std::env::var("CONTRAIL_DSH_PROOF_ROOT") else {
            return;
        };
        let events = DshCollector::default()
            .collect(Path::new(&root))
            .into_iter()
            .flat_map(|batch| batch.events)
            .map(|event| event.parsed)
            .collect::<Vec<_>>();
        assert!(events.iter().any(|event| {
            event.metadata.get("dsh_event_type")
                == Some(&Value::String("request/context".to_string()))
                && event.metadata.get("provider")
                    == Some(&Value::String("deepseek-official".to_string()))
                && event.metadata.get("model")
                    == Some(&Value::String("deepseek-v4-flash".to_string()))
        }));
        assert!(events
            .iter()
            .any(|event| event.metadata.get("dsh_event_type")
                == Some(&Value::String("assistant/message".to_string()))));
        assert!(events
            .iter()
            .any(|event| event.metadata.get("dsh_turn_outcome")
                == Some(&Value::String("completed".to_string()))));
        assert!(events.iter().any(|event| {
            event.metadata.get("dsh_usage_record")
                == Some(&Value::String("assistant_message_copy".to_string()))
        }));
        assert_eq!(
            events
                .iter()
                .filter(|event| event.metadata.contains_key("usage_prompt_tokens"))
                .count(),
            1
        );
        eprintln!("captured {} normalized DSH events", events.len());
    }
}
