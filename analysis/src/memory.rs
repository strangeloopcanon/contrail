use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use uuid::Uuid;

use crate::models::ProbeMatch;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MemoryRecord {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub query: String,
    pub day: Option<String>,
    pub matches: Vec<ProbeMatch>,
    pub prompt: Option<String>,
    pub llm_response: Option<serde_json::Value>,
}

pub fn append_memory(path: &Path, record: &MemoryRecord) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open memory file at {:?}", path))?;
    serde_json::to_writer(&mut file, record)?;
    file.write_all(b"\n")?;
    Ok(())
}

pub fn read_memories(path: &Path) -> Result<Vec<MemoryRecord>> {
    let mut records = Vec::new();
    if !path.exists() {
        return Ok(records);
    }
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line?;
        match serde_json::from_str::<MemoryRecord>(&line) {
            Ok(r) => records.push(r),
            Err(e) => eprintln!("skip invalid memory record: {e}"),
        }
    }
    Ok(records)
}
