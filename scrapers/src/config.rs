use anyhow::{Context, Result};
use std::env;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct ContrailConfig {
    pub log_path: PathBuf,
    pub cursor_storage: PathBuf,
    pub codex_root: PathBuf,
    pub claude_history: PathBuf,
    pub claude_projects: PathBuf,
    pub antigravity_brain: PathBuf,
    pub enable_cursor: bool,
    pub enable_codex: bool,
    pub enable_claude: bool,
    pub enable_antigravity: bool,
    pub cursor_silence_secs: u64,
    pub codex_silence_secs: u64,
    pub claude_silence_secs: u64,
}

impl ContrailConfig {
    pub fn from_env() -> Result<Self> {
        let home = dirs::home_dir().context("could not resolve home directory")?;
        let log_default = home.join(".contrail/logs/master_log.jsonl");

        Ok(Self {
            log_path: env_path("CONTRAIL_LOG_PATH", log_default, home.as_path()),
            cursor_storage: env_path(
                "CONTRAIL_CURSOR_STORAGE",
                home.join("Library/Application Support/Cursor/User/workspaceStorage"),
                home.as_path(),
            ),
            codex_root: env_path(
                "CONTRAIL_CODEX_ROOT",
                home.join(".codex/sessions"),
                home.as_path(),
            ),
            claude_history: env_path(
                "CONTRAIL_CLAUDE_HISTORY",
                home.join(".claude/history.jsonl"),
                home.as_path(),
            ),
            claude_projects: env_path(
                "CONTRAIL_CLAUDE_PROJECTS",
                home.join(".claude/projects"),
                home.as_path(),
            ),
            antigravity_brain: env_path(
                "CONTRAIL_ANTIGRAVITY_BRAIN",
                home.join(".gemini/antigravity/brain"),
                home.as_path(),
            ),
            enable_cursor: env_bool("CONTRAIL_ENABLE_CURSOR", true),
            enable_codex: env_bool("CONTRAIL_ENABLE_CODEX", true),
            enable_claude: env_bool("CONTRAIL_ENABLE_CLAUDE", true),
            enable_antigravity: env_bool("CONTRAIL_ENABLE_ANTIGRAVITY", true),
            cursor_silence_secs: env_u64("CONTRAIL_CURSOR_SILENCE_SECS", 5),
            codex_silence_secs: env_u64("CONTRAIL_CODEX_SILENCE_SECS", 3),
            claude_silence_secs: env_u64("CONTRAIL_CLAUDE_SILENCE_SECS", 5),
        })
    }
}

fn env_path(key: &str, default: PathBuf, home: &std::path::Path) -> PathBuf {
    match env::var(key) {
        Ok(val) if !val.trim().is_empty() => expand_tilde(&val, home),
        _ => default,
    }
}

fn env_bool(key: &str, default: bool) -> bool {
    match env::var(key) {
        Ok(val) => matches!(val.to_lowercase().as_str(), "1" | "true" | "yes" | "on"),
        Err(_) => default,
    }
}

fn env_u64(key: &str, default: u64) -> u64 {
    match env::var(key) {
        Ok(val) => val.parse::<u64>().unwrap_or(default),
        Err(_) => default,
    }
}

fn expand_tilde(input: &str, home: &std::path::Path) -> PathBuf {
    if let Some(rest) = input.strip_prefix("~/") {
        return home.join(rest);
    }
    PathBuf::from(input)
}
