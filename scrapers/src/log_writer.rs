use anyhow::{Context, Result};
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use crate::types::MasterLog;

#[derive(Clone)]
pub struct LogWriter {
    sender: mpsc::UnboundedSender<MasterLog>,
}

impl LogWriter {
    pub fn new(log_path: PathBuf) -> Self {
        let (sender, mut receiver) = mpsc::unbounded_channel::<MasterLog>();

        tokio::spawn(async move {
            if let Err(e) = async move {
                let mut file = tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_path)
                    .await
                    .with_context(|| format!("failed to open log file at {:?}", log_path))?;

                while let Some(log) = receiver.recv().await {
                    let line = serde_json::to_string(&log)?;
                    file.write_all(line.as_bytes()).await?;
                    file.write_all(b"\n").await?;
                }
                Ok::<_, anyhow::Error>(())
            }
            .await
            {
                eprintln!("log writer task failed: {:?}", e);
            }
        });

        Self { sender }
    }

    pub fn write(&self, log: MasterLog) -> Result<()> {
        self.sender
            .send(log)
            .map_err(|_| anyhow::anyhow!("log writer channel closed"))
    }
}
