use anyhow::{Context, Result};
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot};

use crate::types::MasterLog;

const CHANNEL_CAPACITY: usize = 1024;

#[derive(Clone)]
pub struct LogWriter {
    sender: mpsc::Sender<LogWriterCommand>,
}

enum LogWriterCommand {
    Write(Box<MasterLog>),
    Flush(oneshot::Sender<Result<(), String>>),
}

impl LogWriter {
    pub fn new(log_path: PathBuf) -> Self {
        let (sender, mut receiver) = mpsc::channel::<LogWriterCommand>(CHANNEL_CAPACITY);

        tokio::spawn(async move {
            if let Err(e) = async move {
                let mut file = tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_path)
                    .await
                    .with_context(|| format!("failed to open log file at {:?}", log_path))?;

                while let Some(command) = receiver.recv().await {
                    match command {
                        LogWriterCommand::Write(log) => {
                            let mut line = serde_json::to_vec(&log)?;
                            line.push(b'\n');
                            file.write_all(&line).await?;
                        }
                        LogWriterCommand::Flush(done) => {
                            let result = file.flush().await.map_err(|e| e.to_string());
                            let _ = done.send(result);
                        }
                    }
                }
                file.flush().await?;
                Ok::<_, anyhow::Error>(())
            }
            .await
            {
                tracing::error!(err = ?e, "log writer task failed");
            }
        });

        Self { sender }
    }

    pub async fn write(&self, log: MasterLog) -> Result<()> {
        self.sender
            .send(LogWriterCommand::Write(Box::new(log)))
            .await
            .map_err(|_| anyhow::anyhow!("log writer channel closed"))
    }

    pub async fn flush(&self) -> Result<()> {
        let (done_tx, done_rx) = oneshot::channel();
        self.sender
            .send(LogWriterCommand::Flush(done_tx))
            .await
            .map_err(|_| anyhow::anyhow!("log writer channel closed"))?;

        match done_rx.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(anyhow::anyhow!("log writer flush failed: {e}")),
            Err(_) => Err(anyhow::anyhow!("log writer flush was cancelled")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Interaction, MasterLog};
    use chrono::Utc;
    use serde_json::json;
    use uuid::Uuid;

    #[tokio::test]
    async fn flush_waits_for_queued_writes() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let log_path = dir.path().join("master_log.jsonl");
        let writer = LogWriter::new(log_path.clone());

        writer.write(test_log("hello")).await?;
        writer.flush().await?;

        let content = tokio::fs::read_to_string(&log_path).await?;
        assert!(content.contains("hello"));
        Ok(())
    }

    fn test_log(content: &str) -> MasterLog {
        MasterLog {
            event_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            source_tool: "test".to_string(),
            project_context: "/tmp/project".to_string(),
            session_id: "session".to_string(),
            interaction: Interaction {
                role: "user".to_string(),
                content: content.to_string(),
                artifacts: None,
            },
            security_flags: contrail_types::SecurityFlags {
                has_pii: false,
                redacted_secrets: Vec::new(),
            },
            metadata: json!({}),
        }
    }
}
