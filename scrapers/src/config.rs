use anyhow::{Context, Result};
use std::env;
use std::path::PathBuf;

// ── Default path constants (macOS) ──────────────────────────────────────

/// Master log file relative to home.
const DEFAULT_LOG_REL: &str = ".contrail/logs/master_log.jsonl";

/// Cursor workspace storage relative to home.
const DEFAULT_CURSOR_STORAGE_REL: &str = "Library/Application Support/Cursor/User/workspaceStorage";

/// Codex sessions directory relative to home.
const DEFAULT_CODEX_ROOT_REL: &str = ".codex/sessions";

/// Claude global history file relative to home.
const DEFAULT_CLAUDE_HISTORY_REL: &str = ".claude/history.jsonl";

/// Claude per-project session files relative to home.
const DEFAULT_CLAUDE_PROJECTS_REL: &str = ".claude/projects";

/// Antigravity brain directory relative to home.
const DEFAULT_ANTIGRAVITY_BRAIN_REL: &str = ".gemini/antigravity/brain";

/// History import completion marker relative to home.
pub const HISTORY_IMPORT_MARKER_REL: &str = ".contrail/state/history_import_done.json";

// ── Default silence thresholds (seconds) ────────────────────────────────

const DEFAULT_CURSOR_SILENCE_SECS: u64 = 5;
const DEFAULT_CODEX_SILENCE_SECS: u64 = 3;
const DEFAULT_CLAUDE_SILENCE_SECS: u64 = 5;
const DEFAULT_LOG_MAX_BYTES: u64 = 524_288_000;
const DEFAULT_LOG_KEEP_FILES: usize = 5;

// ── Config struct ───────────────────────────────────────────────────────

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
    pub log_max_bytes: u64,
    pub log_keep_files: usize,
}

impl ContrailConfig {
    pub fn from_env() -> Result<Self> {
        let home = dirs::home_dir().context("could not resolve home directory")?;

        Ok(Self {
            log_path: env_path(
                "CONTRAIL_LOG_PATH",
                home.join(DEFAULT_LOG_REL),
                home.as_path(),
            ),
            cursor_storage: env_path(
                "CONTRAIL_CURSOR_STORAGE",
                home.join(DEFAULT_CURSOR_STORAGE_REL),
                home.as_path(),
            ),
            codex_root: env_path(
                "CONTRAIL_CODEX_ROOT",
                home.join(DEFAULT_CODEX_ROOT_REL),
                home.as_path(),
            ),
            claude_history: env_path(
                "CONTRAIL_CLAUDE_HISTORY",
                home.join(DEFAULT_CLAUDE_HISTORY_REL),
                home.as_path(),
            ),
            claude_projects: env_path(
                "CONTRAIL_CLAUDE_PROJECTS",
                home.join(DEFAULT_CLAUDE_PROJECTS_REL),
                home.as_path(),
            ),
            antigravity_brain: env_path(
                "CONTRAIL_ANTIGRAVITY_BRAIN",
                home.join(DEFAULT_ANTIGRAVITY_BRAIN_REL),
                home.as_path(),
            ),
            enable_cursor: env_bool("CONTRAIL_ENABLE_CURSOR", true),
            enable_codex: env_bool("CONTRAIL_ENABLE_CODEX", true),
            enable_claude: env_bool("CONTRAIL_ENABLE_CLAUDE", true),
            enable_antigravity: env_bool("CONTRAIL_ENABLE_ANTIGRAVITY", true),
            cursor_silence_secs: env_u64(
                "CONTRAIL_CURSOR_SILENCE_SECS",
                DEFAULT_CURSOR_SILENCE_SECS,
            ),
            codex_silence_secs: env_u64("CONTRAIL_CODEX_SILENCE_SECS", DEFAULT_CODEX_SILENCE_SECS),
            claude_silence_secs: env_u64(
                "CONTRAIL_CLAUDE_SILENCE_SECS",
                DEFAULT_CLAUDE_SILENCE_SECS,
            ),
            log_max_bytes: env_u64("CONTRAIL_LOG_MAX_BYTES", DEFAULT_LOG_MAX_BYTES),
            log_keep_files: env_usize("CONTRAIL_LOG_KEEP_FILES", DEFAULT_LOG_KEEP_FILES),
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

fn env_usize(key: &str, default: usize) -> usize {
    match env::var(key) {
        Ok(val) => val.parse::<usize>().unwrap_or(default),
        Err(_) => default,
    }
}

fn expand_tilde(input: &str, home: &std::path::Path) -> PathBuf {
    if let Some(rest) = input.strip_prefix("~/") {
        return home.join(rest);
    }
    PathBuf::from(input)
}
