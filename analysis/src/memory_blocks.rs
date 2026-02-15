use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use contrail_types::SecurityFlags;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryBlock {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub label: String,
    pub value: String,
    pub security_flags: SecurityFlags,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

pub fn read_blocks(path: &Path) -> Result<Vec<MemoryBlock>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content =
        fs::read_to_string(path).with_context(|| format!("read memory blocks at {path:?}"))?;
    if content.trim().is_empty() {
        return Ok(Vec::new());
    }
    let blocks =
        serde_json::from_str::<Vec<MemoryBlock>>(&content).context("parse memory blocks json")?;
    Ok(blocks)
}

pub fn write_blocks(path: &Path, blocks: &[MemoryBlock]) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("create dir {dir:?}"))?;
    }

    let tmp_path = tmp_path(path);
    let json = serde_json::to_string_pretty(blocks).context("serialize memory blocks")?;
    fs::write(&tmp_path, json + "\n")
        .with_context(|| format!("write temp blocks at {tmp_path:?}"))?;
    fs::rename(&tmp_path, path)
        .with_context(|| format!("atomic rename {tmp_path:?} -> {path:?}"))?;
    Ok(())
}

pub fn insert_block(path: &Path, mut block: MemoryBlock) -> Result<MemoryBlock> {
    validate_block(&block)?;
    block.updated_at = Utc::now();

    let mut blocks = read_blocks(path)?;
    blocks.push(block.clone());
    write_blocks(path, &blocks)?;
    Ok(block)
}

pub fn update_block(path: &Path, id: Uuid, mut update: MemoryBlockUpdate) -> Result<MemoryBlock> {
    let mut blocks = read_blocks(path)?;
    let now = Utc::now();

    let Some(idx) = blocks.iter().position(|b| b.id == id) else {
        anyhow::bail!("memory block not found");
    };

    let updated = {
        let existing = blocks
            .get_mut(idx)
            .context("memory block index out of bounds")?;

        if let Some(label) = update.label.take() {
            existing.label = label;
        }
        if let Some(value) = update.value.take() {
            existing.value = value;
        }
        if let Some(flags) = update.security_flags.take() {
            existing.security_flags = flags;
        }
        if update.project_context.is_some() {
            existing.project_context = update.project_context.take().flatten();
        }
        if update.source_tool.is_some() {
            existing.source_tool = update.source_tool.take().flatten();
        }
        if update.tags.is_some() {
            existing.tags = update.tags.take().flatten();
        }

        existing.updated_at = now;
        validate_block(existing)?;
        existing.clone()
    };

    write_blocks(path, &blocks)?;
    Ok(updated)
}

pub fn delete_block(path: &Path, id: Uuid) -> Result<()> {
    let mut blocks = read_blocks(path)?;
    let before = blocks.len();
    blocks.retain(|b| b.id != id);
    ensure!(blocks.len() != before, "memory block not found");
    write_blocks(path, &blocks)?;
    Ok(())
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MemoryBlockUpdate {
    pub label: Option<String>,
    pub value: Option<String>,
    pub security_flags: Option<SecurityFlags>,
    pub project_context: Option<Option<String>>,
    pub source_tool: Option<Option<String>>,
    pub tags: Option<Option<Vec<String>>>,
}

fn validate_block(block: &MemoryBlock) -> Result<()> {
    ensure!(!block.label.trim().is_empty(), "label cannot be empty");
    ensure!(!block.value.trim().is_empty(), "value cannot be empty");
    Ok(())
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut tmp = path.to_path_buf();
    let suffix = Uuid::new_v4();
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) if !ext.is_empty() => {
            tmp.set_extension(format!("{ext}.tmp.{suffix}"));
        }
        _ => {
            tmp.set_extension(format!("tmp.{suffix}"));
        }
    }
    tmp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_blocks() -> Result<()> {
        let path =
            std::env::temp_dir().join(format!("contrail_memory_blocks_{}.json", Uuid::new_v4()));
        let blocks = vec![MemoryBlock {
            id: Uuid::new_v4(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            label: "project".to_string(),
            value: "prefer rustfmt".to_string(),
            security_flags: SecurityFlags {
                has_pii: false,
                redacted_secrets: vec![],
            },
            project_context: Some("/tmp/repo".to_string()),
            source_tool: None,
            tags: Some(vec!["style".to_string()]),
        }];

        write_blocks(&path, &blocks)?;
        let loaded = read_blocks(&path)?;
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].label, "project");
        assert_eq!(loaded[0].project_context.as_deref(), Some("/tmp/repo"));

        let _ = fs::remove_file(&path);
        Ok(())
    }
}
