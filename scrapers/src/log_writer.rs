use anyhow::{Context, Result};
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use crate::types::MasterLog;

const CHANNEL_CAPACITY: usize = 1024;

#[derive(Clone)]
pub struct LogWriter {
    sender: mpsc::Sender<MasterLog>,
}

impl LogWriter {
    pub fn new(log_path: PathBuf) -> Self {
        let (sender, mut receiver) = mpsc::channel::<MasterLog>(CHANNEL_CAPACITY);

        tokio::spawn(async move {
            if let Err(e) = async move {
                let mut file = tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_path)
                    .await
                    .with_context(|| format!("failed to open log file at {:?}", log_path))?;

                while let Some(log) = receiver.recv().await {
                    let mut line = serde_json::to_vec(&log)?;
                    line.push(b'\n');
                    file.write_all(&line).await?;
                }
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
            .send(log)
            .await
            .map_err(|_| anyhow::anyhow!("log writer channel closed"))
    }
}
