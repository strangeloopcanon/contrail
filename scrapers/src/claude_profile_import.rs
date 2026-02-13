use crate::claude::{parse_claude_line, parse_claude_session_line};
use crate::config::ContrailConfig;
use crate::sentry::Sentry;
use crate::types::{Interaction, MasterLog};
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};
use tracing::info;
use uuid::Uuid;
use walkdir::WalkDir;

const MAX_SKILL_CHARS: usize = 120_000;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImportTarget {
    Global,
    Repo { repo_root: PathBuf },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ImportScope {
    Curated,
    Broad,
    Full,
}

impl ImportScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Curated => "curated",
            Self::Broad => "broad",
            Self::Full => "full",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactCategory {
    Instructions,
    Settings,
    Commands,
    Agents,
    History,
    Todos,
    Plugins,
    Other,
}

impl ArtifactCategory {
    fn as_str(self) -> &'static str {
        match self {
            Self::Instructions => "instructions",
            Self::Settings => "settings",
            Self::Commands => "commands",
            Self::Agents => "agents",
            Self::History => "history",
            Self::Todos => "todos",
            Self::Plugins => "plugins",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupRequest {
    pub target: ImportTarget,
    pub source: Option<PathBuf>,
    pub scope: ImportScope,
    #[serde(default)]
    pub include_global: bool,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupReport {
    pub dry_run: bool,
    pub instructions_written: Vec<SetupWrittenItem>,
    pub skills_written: Vec<SetupWrittenItem>,
    pub history_ingested: usize,
    pub history_skipped: usize,
    pub history_errors: usize,
    pub archived: Vec<SetupWrittenItem>,
    pub skipped: Vec<String>,
    pub not_transferred: Vec<String>,
    pub errors: Vec<String>,
    pub agents_md_path: Option<PathBuf>,
    pub skills_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupWrittenItem {
    pub source: String,
    pub destination: PathBuf,
    pub category: String,
}

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct SourceRoot {
    path: PathBuf,
    precedence: usize,
}

#[derive(Debug, Clone)]
struct Candidate {
    category: ArtifactCategory,
    #[allow(dead_code)]
    source_root: PathBuf,
    source_path: PathBuf,
    source_rel_path: String,
}

enum FileClass {
    Include(ArtifactCategory),
    Excluded(String),
}

// ---------------------------------------------------------------------------
// setup_claude_profile -- the main entry point
// ---------------------------------------------------------------------------

/// One-shot migration: scan Claude profile, write directly to live Codex paths,
/// ingest history, and return a report.
pub fn setup_claude_profile(request: &SetupRequest) -> Result<SetupReport> {
    info!(
        scope = request.scope.as_str(),
        include_global = request.include_global,
        dry_run = request.dry_run,
        "starting claude profile setup"
    );

    let roots = resolve_source_roots(
        &request.target,
        request.source.as_deref(),
        request.include_global,
    )?;

    let agents_path = live_agents_md_path(&request.target)?;
    let skills_dir = live_skills_dir(&request.target)?;
    let archive_root = archive_root_for_target(&request.target)?;

    let mut report = SetupReport {
        dry_run: request.dry_run,
        instructions_written: Vec::new(),
        skills_written: Vec::new(),
        history_ingested: 0,
        history_skipped: 0,
        history_errors: 0,
        archived: Vec::new(),
        skipped: Vec::new(),
        not_transferred: Vec::new(),
        errors: Vec::new(),
        agents_md_path: Some(agents_path.clone()),
        skills_dir: Some(skills_dir.clone()),
    };

    // Walk and classify, dedup by precedence
    let mut selected: HashMap<String, (usize, Candidate)> = HashMap::new();
    for root in &roots {
        for entry in WalkDir::new(&root.path).follow_links(false) {
            let entry = match entry {
                Ok(value) => value,
                Err(err) => {
                    report.errors.push(format!("walk error: {err}"));
                    continue;
                }
            };
            if entry.file_type().is_symlink() || !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path().to_path_buf();
            let rel = match path.strip_prefix(&root.path) {
                Ok(value) => value.to_path_buf(),
                Err(_) => continue,
            };
            let rel_str = path_to_slash_string(&rel);
            match classify_file(&rel, request.scope) {
                FileClass::Excluded(reason) => {
                    report.skipped.push(format!("{rel_str}: {reason}"));
                }
                FileClass::Include(category) => {
                    let key = format!("{}::{rel_str}", category.as_str());
                    let candidate = Candidate {
                        category,
                        source_root: root.path.clone(),
                        source_path: path,
                        source_rel_path: rel_str,
                    };
                    match selected.get(&key) {
                        Some((existing_precedence, _))
                            if *existing_precedence > root.precedence => {}
                        _ => {
                            selected.insert(key, (root.precedence, candidate));
                        }
                    }
                }
            }
        }
    }

    // For repo targets, pick up repo-root CLAUDE.md / AGENTS.md
    // but skip the destination AGENTS.md itself to avoid circular import
    if let ImportTarget::Repo { repo_root } = &request.target {
        for name in &["CLAUDE.md", "AGENTS.md"] {
            let path = repo_root.join(name);
            if path.is_file() && path != agents_path {
                let rel_str = name.to_string();
                let category = ArtifactCategory::Instructions;
                let key = format!("{}::{rel_str}", category.as_str());
                selected.insert(
                    key,
                    (
                        10,
                        Candidate {
                            category,
                            source_root: repo_root.clone(),
                            source_path: path,
                            source_rel_path: rel_str,
                        },
                    ),
                );
            }
        }
    }

    if selected.is_empty() {
        info!("no Claude profile sources found");
        return Ok(SetupReport {
            dry_run: request.dry_run,
            instructions_written: Vec::new(),
            skills_written: Vec::new(),
            history_ingested: 0,
            history_skipped: 0,
            history_errors: 0,
            archived: Vec::new(),
            skipped: Vec::new(),
            not_transferred: vec!["no importable Claude artifacts found".to_string()],
            errors: Vec::new(),
            agents_md_path: None,
            skills_dir: None,
        });
    }

    // Initialize history ingest state lazily
    let has_history = selected
        .values()
        .any(|(_, c)| c.category == ArtifactCategory::History);
    let mut history_state = if has_history && !request.dry_run {
        match HistoryIngestState::new() {
            Ok(state) => Some(state),
            Err(err) => {
                report
                    .errors
                    .push(format!("failed to init history ingest: {err}"));
                None
            }
        }
    } else {
        None
    };

    // Process each artifact by category
    let mut sorted: Vec<_> = selected.into_values().collect();
    sorted.sort_by(|a, b| a.1.source_rel_path.cmp(&b.1.source_rel_path));

    for (_, candidate) in &sorted {
        let source_text = || -> Result<String> {
            let raw = fs::read(&candidate.source_path)
                .with_context(|| format!("read {}", candidate.source_path.display()))?;
            String::from_utf8(raw)
                .with_context(|| format!("utf-8 error: {}", candidate.source_path.display()))
        };

        match candidate.category {
            ArtifactCategory::Instructions => {
                let text = match source_text() {
                    Ok(t) => t,
                    Err(err) => {
                        report.errors.push(format!(
                            "read {} failed: {err}",
                            candidate.source_path.display()
                        ));
                        continue;
                    }
                };
                let rendered = render_instructions_doc(
                    &candidate.source_path,
                    &candidate.source_rel_path,
                    &text,
                );

                if request.dry_run {
                    report.instructions_written.push(SetupWrittenItem {
                        source: candidate.source_rel_path.clone(),
                        destination: agents_path.clone(),
                        category: "instructions".to_string(),
                    });
                } else {
                    match append_to_agents_md(&agents_path, &candidate.source_rel_path, &rendered) {
                        Ok(changed) => {
                            if changed {
                                info!(
                                    src = %candidate.source_rel_path,
                                    dest = %agents_path.display(),
                                    "appended instructions to AGENTS.md"
                                );
                            }
                            report.instructions_written.push(SetupWrittenItem {
                                source: candidate.source_rel_path.clone(),
                                destination: agents_path.clone(),
                                category: "instructions".to_string(),
                            });
                        }
                        Err(err) => {
                            report.errors.push(format!(
                                "append {} to AGENTS.md failed: {err}",
                                candidate.source_rel_path
                            ));
                        }
                    }
                }
            }

            ArtifactCategory::Commands | ArtifactCategory::Agents => {
                let text = match source_text() {
                    Ok(t) => t,
                    Err(err) => {
                        report.errors.push(format!(
                            "read {} failed: {err}",
                            candidate.source_path.display()
                        ));
                        continue;
                    }
                };
                let rendered = render_skill_doc(
                    candidate.category,
                    &candidate.source_path,
                    &candidate.source_rel_path,
                    &text,
                );
                let slug = skill_slug(&candidate.source_rel_path);
                let prefix = if candidate.category == ArtifactCategory::Commands {
                    "claude-cmd"
                } else {
                    "claude-agent"
                };
                let dest = skills_dir.join(format!("{prefix}-{slug}")).join("SKILL.md");

                if request.dry_run {
                    report.skills_written.push(SetupWrittenItem {
                        source: candidate.source_rel_path.clone(),
                        destination: dest,
                        category: candidate.category.as_str().to_string(),
                    });
                } else {
                    if let Some(parent) = dest.parent() {
                        fs::create_dir_all(parent)
                            .with_context(|| format!("create {}", parent.display()))?;
                    }
                    match fs::write(&dest, &rendered) {
                        Ok(()) => {
                            info!(
                                src = %candidate.source_rel_path,
                                dest = %dest.display(),
                                "wrote skill"
                            );
                            report.skills_written.push(SetupWrittenItem {
                                source: candidate.source_rel_path.clone(),
                                destination: dest,
                                category: candidate.category.as_str().to_string(),
                            });
                        }
                        Err(err) => {
                            report
                                .errors
                                .push(format!("write skill {} failed: {err}", dest.display()));
                        }
                    }
                }
            }

            ArtifactCategory::History => {
                if request.dry_run {
                    report.not_transferred.push(format!(
                        "{}: would ingest into master log",
                        candidate.source_rel_path
                    ));
                } else if let Some(state) = history_state.as_mut() {
                    match state.ingest_file(&candidate.source_path) {
                        Ok(stats) => {
                            report.history_ingested += stats.imported;
                            report.history_skipped += stats.skipped;
                            report.history_errors += stats.errors;
                        }
                        Err(err) => {
                            report.history_errors += 1;
                            report.errors.push(format!(
                                "history ingest {}: {err}",
                                candidate.source_rel_path
                            ));
                        }
                    }
                }
            }

            ArtifactCategory::Settings | ArtifactCategory::Todos | ArtifactCategory::Plugins => {
                // Archive these; they can't be auto-applied to Codex
                let slug = skill_slug(&candidate.source_rel_path);
                let dest = archive_root
                    .join(candidate.category.as_str())
                    .join(format!("{slug}.archived"));

                if request.dry_run {
                    report.archived.push(SetupWrittenItem {
                        source: candidate.source_rel_path.clone(),
                        destination: dest,
                        category: candidate.category.as_str().to_string(),
                    });
                } else {
                    if let Some(parent) = dest.parent() {
                        fs::create_dir_all(parent)
                            .with_context(|| format!("create {}", parent.display()))?;
                    }
                    match fs::copy(&candidate.source_path, &dest) {
                        Ok(_) => {
                            info!(
                                src = %candidate.source_rel_path,
                                dest = %dest.display(),
                                "archived"
                            );
                            report.archived.push(SetupWrittenItem {
                                source: candidate.source_rel_path.clone(),
                                destination: dest,
                                category: candidate.category.as_str().to_string(),
                            });
                        }
                        Err(err) => {
                            report.errors.push(format!(
                                "archive {} failed: {err}",
                                candidate.source_rel_path
                            ));
                        }
                    }
                }
                report.not_transferred.push(format!(
                    "{}: archived (not auto-applied to Codex)",
                    candidate.source_rel_path
                ));
            }

            ArtifactCategory::Other => {
                report
                    .skipped
                    .push(format!("{}: other/unclassified", candidate.source_rel_path));
            }
        }
    }

    if let Some(state) = history_state.as_mut() {
        state.flush()?;
    }

    if !request.dry_run && matches!(request.target, ImportTarget::Global) {
        write_setup_marker(&report)?;
    }

    info!(
        instructions = report.instructions_written.len(),
        skills = report.skills_written.len(),
        history_ingested = report.history_ingested,
        archived = report.archived.len(),
        errors = report.errors.len(),
        "claude profile setup complete"
    );

    Ok(report)
}

// ---------------------------------------------------------------------------
// Destination path helpers
// ---------------------------------------------------------------------------

/// Path to the live AGENTS.md that Codex actually reads.
/// Global: ~/AGENTS.md (Codex walks up from CWD; home is the ceiling).
/// Repo: <repo>/AGENTS.md.
pub fn live_agents_md_path(target: &ImportTarget) -> Result<PathBuf> {
    match target {
        ImportTarget::Global => {
            let home = dirs::home_dir().context("could not resolve home directory")?;
            Ok(home.join("AGENTS.md"))
        }
        ImportTarget::Repo { repo_root } => Ok(repo_root.join("AGENTS.md")),
    }
}

/// Directory where live Codex skills should be written.
/// Global: ~/.agents/skills/ (USER scope in Codex skill resolution).
/// Repo: <repo>/.agents/skills/ (REPO scope).
pub fn live_skills_dir(target: &ImportTarget) -> Result<PathBuf> {
    match target {
        ImportTarget::Global => {
            let home = dirs::home_dir().context("could not resolve home directory")?;
            Ok(home.join(".agents/skills"))
        }
        ImportTarget::Repo { repo_root } => Ok(repo_root.join(".agents/skills")),
    }
}

/// Archive root for categories not directly wired into Codex (settings, todos, plugins).
fn archive_root_for_target(target: &ImportTarget) -> Result<PathBuf> {
    match target {
        ImportTarget::Global => {
            let home = dirs::home_dir().context("could not resolve home directory")?;
            Ok(home.join(".codex/imports/claude"))
        }
        ImportTarget::Repo { repo_root } => Ok(repo_root.join(".codex/imports/claude")),
    }
}

fn default_claude_root() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not resolve home directory")?;
    Ok(home.join(".claude"))
}

fn resolve_source_roots(
    target: &ImportTarget,
    source_override: Option<&Path>,
    include_global: bool,
) -> Result<Vec<SourceRoot>> {
    let mut roots = Vec::new();

    let want_global = match target {
        ImportTarget::Global => true,
        ImportTarget::Repo { .. } => include_global,
    };

    if want_global {
        let base = match source_override {
            Some(path) => normalize_existing_path(path)?,
            None => default_claude_root()?,
        };
        if base.exists() {
            roots.push(SourceRoot {
                path: base,
                precedence: 0,
            });
        }
    }

    if let ImportTarget::Repo { repo_root } = target {
        let local = repo_root.join(".claude");
        if local.exists() {
            roots.push(SourceRoot {
                path: normalize_existing_path(&local)?,
                precedence: 1,
            });
        }
    }

    Ok(roots)
}

// ---------------------------------------------------------------------------
// AGENTS.md append with idempotent markers
// ---------------------------------------------------------------------------

const MARKER_PREFIX: &str = "<!-- BEGIN contrail:claude-import src=";
const MARKER_SUFFIX: &str = " -->";
const MARKER_END_PREFIX: &str = "<!-- END contrail:claude-import src=";

fn sanitize_marker_value(s: &str) -> String {
    s.replace("-->", "—>")
}

fn begin_marker(source_rel: &str) -> String {
    format!(
        "{MARKER_PREFIX}{}{MARKER_SUFFIX}",
        sanitize_marker_value(source_rel)
    )
}

fn end_marker(source_rel: &str) -> String {
    format!(
        "{MARKER_END_PREFIX}{}{MARKER_SUFFIX}",
        sanitize_marker_value(source_rel)
    )
}

/// Append (or replace) a delimited section in an AGENTS.md file.
///
/// If the file already contains markers for `source_rel`, the existing section
/// is replaced in place. Otherwise the section is appended at the end.
/// Returns `true` if the content actually changed.
pub fn append_to_agents_md(
    agents_md_path: &Path,
    source_rel: &str,
    rendered_content: &str,
) -> Result<bool> {
    let begin = begin_marker(source_rel);
    let end = end_marker(source_rel);

    let mut section = String::new();
    section.push_str(&begin);
    section.push('\n');
    section.push_str(rendered_content);
    if !rendered_content.ends_with('\n') {
        section.push('\n');
    }
    section.push_str(&end);
    section.push('\n');

    let existing = if agents_md_path.exists() {
        fs::read_to_string(agents_md_path)
            .with_context(|| format!("read {}", agents_md_path.display()))?
    } else {
        String::new()
    };

    // Check if markers already exist for this source
    if let (Some(start_pos), Some(end_pos)) = (existing.find(&begin), existing.find(&end)) {
        let end_line_end = existing[end_pos..]
            .find('\n')
            .map(|i| end_pos + i + 1)
            .unwrap_or(existing.len());

        if end_pos > start_pos {
            let old_section = &existing[start_pos..end_line_end];
            if old_section == section {
                return Ok(false); // identical, nothing to do
            }
            // Replace the existing section
            let mut updated = String::with_capacity(existing.len());
            updated.push_str(&existing[..start_pos]);
            updated.push_str(&section);
            updated.push_str(&existing[end_line_end..]);

            if let Some(parent) = agents_md_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
            fs::write(agents_md_path, &updated)
                .with_context(|| format!("write {}", agents_md_path.display()))?;
            return Ok(true);
        }
    }

    // Append to end
    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.is_empty() {
        out.push('\n'); // blank line before new section
    }
    out.push_str(&section);

    if let Some(parent) = agents_md_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(agents_md_path, &out)
        .with_context(|| format!("write {}", agents_md_path.display()))?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// File classification
// ---------------------------------------------------------------------------

fn classify_file(rel: &Path, scope: ImportScope) -> FileClass {
    let rel_str = path_to_slash_string(rel);
    let lower = rel_str.to_lowercase();
    let first = rel
        .components()
        .next()
        .and_then(component_str)
        .unwrap_or("");

    if lower.ends_with(".lock")
        || lower.ends_with(".tmp")
        || lower.ends_with(".swp")
        || lower.ends_with('~')
    {
        return FileClass::Excluded("transient lock/temp file".to_string());
    }

    if lower.contains("/.git/") || lower.starts_with(".git/") {
        return FileClass::Excluded("git metadata ignored".to_string());
    }

    if lower.starts_with("debug/")
        || lower.starts_with("statsig/")
        || lower.starts_with("telemetry/")
        || lower.starts_with("cache/")
    {
        return match scope {
            ImportScope::Full => FileClass::Include(ArtifactCategory::Other),
            _ => FileClass::Excluded("runtime telemetry/cache path".to_string()),
        };
    }

    let category = if lower == "claude.md"
        || lower == ".clauderc"
        || lower == "instructions.md"
        || lower == "agents.md"
    {
        ArtifactCategory::Instructions
    } else if lower == "history.jsonl"
        || (lower.starts_with("projects/") && lower.ends_with(".jsonl"))
    {
        ArtifactCategory::History
    } else if lower.starts_with("agents/") {
        ArtifactCategory::Agents
    } else if lower.starts_with("commands/") {
        ArtifactCategory::Commands
    } else if lower.starts_with("todos/") {
        ArtifactCategory::Todos
    } else if lower.starts_with("plugins/") {
        ArtifactCategory::Plugins
    } else if lower.starts_with("settings/")
        || lower == "config.json"
        || lower == "preferences.json"
        || lower == "settings.json"
    {
        ArtifactCategory::Settings
    } else {
        ArtifactCategory::Other
    };

    match scope {
        ImportScope::Full => FileClass::Include(category),
        ImportScope::Broad => {
            if first == "ide" && !lower.ends_with(".json") {
                FileClass::Excluded("ide runtime artifact".to_string())
            } else {
                FileClass::Include(category)
            }
        }
        ImportScope::Curated => match category {
            ArtifactCategory::Instructions
            | ArtifactCategory::Settings
            | ArtifactCategory::Commands
            | ArtifactCategory::Agents
            | ArtifactCategory::History
            | ArtifactCategory::Todos
            | ArtifactCategory::Plugins => FileClass::Include(category),
            ArtifactCategory::Other => FileClass::Excluded("excluded by curated scope".to_string()),
        },
    }
}

// ---------------------------------------------------------------------------
// Renderers
// ---------------------------------------------------------------------------

fn render_instructions_doc(source_path: &Path, source_rel_path: &str, text: &str) -> String {
    let (content, truncated) = truncate_chars(text, MAX_SKILL_CHARS);
    let mut out = String::new();
    out.push_str(&format!(
        "<!-- Imported from Claude: {} via contrail import-claude -->\n\n",
        source_rel_path
    ));
    out.push_str(&content);
    if !content.ends_with('\n') {
        out.push('\n');
    }
    if truncated {
        out.push_str(&format!(
            "\n<!-- Truncated during import from {} -->\n",
            source_path.display()
        ));
    }
    out
}

fn render_skill_doc(
    category: ArtifactCategory,
    source_path: &Path,
    source_rel_path: &str,
    text: &str,
) -> String {
    let (frontmatter, body) = parse_frontmatter(text);

    let file_stem = Path::new(source_rel_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("imported");
    let skill_name = frontmatter
        .get("name")
        .cloned()
        .unwrap_or_else(|| file_stem.to_string());
    let description = frontmatter.get("description").cloned().unwrap_or_else(|| {
        format!(
            "Imported Claude {} from {}",
            category.as_str(),
            source_rel_path
        )
    });

    let (content, truncated) = truncate_chars(body.trim(), MAX_SKILL_CHARS);

    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("name: {skill_name}\n"));
    out.push_str(&format!("description: {description}\n"));
    out.push_str("---\n\n");

    out.push_str(&format!(
        "<!-- Imported from Claude {}: {} via contrail import-claude -->\n\n",
        category.as_str(),
        source_path.display()
    ));

    if matches!(
        category,
        ArtifactCategory::Commands | ArtifactCategory::Agents
    ) {
        out.push_str(&content);
        if !content.ends_with('\n') {
            out.push('\n');
        }
    } else {
        let lang = code_fence_lang(source_path);
        out.push_str(&format!("```{lang}\n"));
        out.push_str(&content);
        if !content.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("```\n");
    }

    if truncated {
        out.push_str("\n<!-- Content truncated during import normalization -->\n");
    }
    out
}

/// Parse optional YAML frontmatter delimited by `---` lines.
fn parse_frontmatter(text: &str) -> (HashMap<String, String>, &str) {
    let trimmed = text.trim_start();
    if !trimmed.starts_with("---") {
        return (HashMap::new(), text);
    }
    let after_open = &trimmed[3..];
    let after_open = after_open.strip_prefix('\n').unwrap_or(after_open);
    if let Some(close_pos) = after_open.find("\n---") {
        let fm_block = &after_open[..close_pos];
        let body_start = close_pos + 4;
        let body = after_open[body_start..]
            .strip_prefix('\n')
            .unwrap_or(&after_open[body_start..]);

        let mut map = HashMap::new();
        for line in fm_block.lines() {
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim().to_lowercase();
                let value = value.trim().to_string();
                if !key.is_empty() && !value.is_empty() {
                    map.insert(key, value);
                }
            }
        }
        (map, body)
    } else {
        (HashMap::new(), text)
    }
}

fn code_fence_lang(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
    {
        "json" => "json",
        "jsonl" => "json",
        "toml" => "toml",
        "md" => "markdown",
        "yaml" | "yml" => "yaml",
        "sh" => "bash",
        _ => "text",
    }
}

// ---------------------------------------------------------------------------
// Utility helpers
// ---------------------------------------------------------------------------

fn truncate_chars(input: &str, max_chars: usize) -> (String, bool) {
    let mut end = 0usize;
    let mut hit_limit = false;
    for (count, (idx, ch)) in input.char_indices().enumerate() {
        if count >= max_chars {
            hit_limit = true;
            break;
        }
        end = idx + ch.len_utf8();
    }
    if hit_limit {
        (format!("{}...[truncated]", &input[..end]), true)
    } else {
        (input.to_string(), false)
    }
}

fn skill_slug(rel: &str) -> String {
    let mut slug = String::new();
    for byte in rel.as_bytes() {
        if byte.is_ascii_alphanumeric() {
            slug.push(byte.to_ascii_lowercase() as char);
        } else {
            slug.push('_');
            slug.push('x');
            slug.push_str(&format!("{:x}", byte));
            slug.push('_');
        }
    }
    slug.trim_matches('_').to_string()
}

fn path_to_slash_string(path: &Path) -> String {
    let mut pieces = Vec::new();
    for component in path.components() {
        if let Component::Normal(value) = component {
            pieces.push(value.to_string_lossy().to_string());
        }
    }
    pieces.join("/")
}

fn component_str(component: Component<'_>) -> Option<&str> {
    match component {
        Component::Normal(value) => value.to_str(),
        _ => None,
    }
}

fn normalize_existing_path(path: &Path) -> Result<PathBuf> {
    let expanded = if let Some(raw) = path.to_str() {
        if raw.starts_with("~/") {
            expand_tilde(raw)?
        } else {
            path.to_path_buf()
        }
    } else {
        path.to_path_buf()
    };
    if expanded.exists() {
        fs::canonicalize(&expanded).with_context(|| format!("canonicalize {}", expanded.display()))
    } else {
        Ok(expanded)
    }
}

fn expand_tilde(raw: &str) -> Result<PathBuf> {
    if let Some(rest) = raw.strip_prefix("~/") {
        let home = dirs::home_dir().context("could not resolve home directory")?;
        return Ok(home.join(rest));
    }
    Ok(PathBuf::from(raw))
}

fn write_setup_marker(report: &SetupReport) -> Result<()> {
    let home = dirs::home_dir().context("could not resolve home directory")?;
    let marker_path = home.join(".contrail/state/claude_codex_setup.json");
    if let Some(parent) = marker_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let payload = serde_json::json!({
        "completed_at": Utc::now().to_rfc3339(),
        "instructions": report.instructions_written.len(),
        "skills": report.skills_written.len(),
        "history_ingested": report.history_ingested,
        "archived": report.archived.len(),
    });
    let marker_json = serde_json::to_string_pretty(&payload).context("serialize setup marker")?;
    fs::write(&marker_path, marker_json)
        .with_context(|| format!("write {}", marker_path.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// History ingest
// ---------------------------------------------------------------------------

#[derive(Default)]
struct HistoryIngestStats {
    imported: usize,
    skipped: usize,
    errors: usize,
}

struct HistoryIngestState {
    sentry: Sentry,
    existing: HashSet<u64>,
    writer: std::io::BufWriter<File>,
}

impl HistoryIngestState {
    fn new() -> Result<Self> {
        let config = ContrailConfig::from_env()?;
        if let Some(parent) = config.log_path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        let existing = load_existing_history_keys(&config.log_path)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&config.log_path)
            .with_context(|| format!("open {}", config.log_path.display()))?;
        Ok(Self {
            sentry: Sentry::new(),
            existing,
            writer: std::io::BufWriter::new(file),
        })
    }

    fn ingest_file(&mut self, path: &Path) -> Result<HistoryIngestStats> {
        let mut stats = HistoryIngestStats::default();
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let reader = BufReader::new(file);
        let is_session_file = path_to_slash_string(path).contains("/projects/");
        let fallback_session = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        for line in reader.lines() {
            let line = match line {
                Ok(value) => value,
                Err(_) => {
                    stats.errors += 1;
                    continue;
                }
            };
            let parsed = if is_session_file {
                parse_claude_session_line(&line).or_else(|| parse_claude_line(&line))
            } else {
                parse_claude_line(&line).or_else(|| parse_claude_session_line(&line))
            };
            let Some(parsed) = parsed else {
                continue;
            };

            // Redact *before* computing the dedup key, so the key matches
            // what load_existing_history_keys sees when reading back from disk.
            let (content, security_flags) = self.sentry.scan_and_redact(&parsed.content);

            let key = dedupe_key(
                "claude-code",
                parsed.session_id.as_deref().unwrap_or(&fallback_session),
                &content,
            );
            if self.existing.contains(&key) {
                stats.skipped += 1;
                continue;
            }
            self.existing.insert(key);
            let timestamp = parsed.timestamp.unwrap_or_else(Utc::now);
            let session_id = parsed
                .session_id
                .unwrap_or_else(|| fallback_session.clone());
            let project_context = parsed
                .project_context
                .unwrap_or_else(|| "Imported Claude Profile".to_string());

            let mut metadata = parsed.metadata;
            metadata.insert("imported".to_string(), Value::Bool(true));
            metadata.insert("claude_profile_import".to_string(), Value::Bool(true));
            metadata.insert(
                "source_file".to_string(),
                Value::String(path.to_string_lossy().to_string()),
            );

            let event = MasterLog {
                event_id: Uuid::new_v4(),
                timestamp,
                source_tool: "claude-code".to_string(),
                project_context,
                session_id,
                interaction: Interaction {
                    role: parsed.role,
                    content,
                    artifacts: None,
                },
                security_flags,
                metadata: Value::Object(metadata),
            };
            if event.validate_schema().is_err() {
                stats.errors += 1;
                continue;
            }

            writeln!(
                self.writer,
                "{}",
                serde_json::to_string(&event).context("serialize history event")?
            )
            .context("write history event")?;
            stats.imported += 1;
        }

        Ok(stats)
    }

    fn flush(&mut self) -> Result<()> {
        self.writer.flush().context("flush history writer")
    }
}

fn load_existing_history_keys(log_path: &Path) -> Result<HashSet<u64>> {
    let mut out = HashSet::new();
    if !log_path.exists() {
        return Ok(out);
    }
    let file = File::open(log_path).with_context(|| format!("open {}", log_path.display()))?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = match line {
            Ok(value) => value,
            Err(_) => continue,
        };
        let json: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let source = json
            .get("source_tool")
            .and_then(Value::as_str)
            .unwrap_or("");
        let session = json.get("session_id").and_then(Value::as_str).unwrap_or("");
        let content = json
            .pointer("/interaction/content")
            .and_then(Value::as_str)
            .unwrap_or("");
        out.insert(dedupe_key(source, session, content));
    }
    Ok(out)
}

fn dedupe_key(source: &str, session: &str, content: &str) -> u64 {
    let mut combined = String::with_capacity(source.len() + session.len() + content.len() + 2);
    combined.push_str(source);
    combined.push('\0');
    combined.push_str(session);
    combined.push('\0');
    combined.push_str(content);
    xxhash_rust::xxh3::xxh3_64(combined.as_bytes())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn curated_scope_includes_plugins_and_todos() {
        assert!(matches!(
            classify_file(Path::new("plugins/foo/plugin.toml"), ImportScope::Curated),
            FileClass::Include(ArtifactCategory::Plugins)
        ));
        assert!(matches!(
            classify_file(Path::new("todos/task.json"), ImportScope::Curated),
            FileClass::Include(ArtifactCategory::Todos)
        ));
        assert!(matches!(
            classify_file(Path::new("debug/log.txt"), ImportScope::Curated),
            FileClass::Excluded(_)
        ));
    }

    #[test]
    fn full_scope_keeps_runtime_dirs() {
        assert!(matches!(
            classify_file(Path::new("debug/log.txt"), ImportScope::Full),
            FileClass::Include(ArtifactCategory::Other)
        ));
    }

    #[test]
    fn curated_scope_includes_instructions() {
        assert!(matches!(
            classify_file(Path::new("CLAUDE.md"), ImportScope::Curated),
            FileClass::Include(ArtifactCategory::Instructions)
        ));
        assert!(matches!(
            classify_file(Path::new(".clauderc"), ImportScope::Curated),
            FileClass::Include(ArtifactCategory::Instructions)
        ));
        assert!(matches!(
            classify_file(Path::new("instructions.md"), ImportScope::Curated),
            FileClass::Include(ArtifactCategory::Instructions)
        ));
    }

    #[test]
    fn skill_doc_parses_claude_frontmatter() {
        let input = "---\ndescription: Review code changes\nallowed-tools: Bash, Read\n---\nReview the code in $ARGUMENTS.\n";
        let output = render_skill_doc(
            ArtifactCategory::Commands,
            Path::new("commands/review.md"),
            "commands/review.md",
            input,
        );
        assert!(output.starts_with("---\n"));
        assert!(output.contains("name: review\n"));
        assert!(output.contains("description: Review code changes\n"));
        assert!(output.contains("Review the code in $ARGUMENTS."));
        assert!(!output.contains("```text"));
    }

    #[test]
    fn skill_doc_wraps_settings_in_code_fence() {
        let input = r#"{"model": "claude-sonnet-4-5"}"#;
        let output = render_skill_doc(
            ArtifactCategory::Settings,
            Path::new("settings.json"),
            "settings.json",
            input,
        );
        assert!(output.starts_with("---\n"));
        assert!(output.contains("```json"));
    }

    #[test]
    fn setup_writes_to_live_agents_md() -> Result<()> {
        let temp = TempDir::new()?;
        let repo = temp.path().join("repo");
        fs::create_dir_all(repo.join(".claude"))?;
        fs::write(repo.join("CLAUDE.md"), "# My instructions\nDo the thing.\n")?;

        let request = SetupRequest {
            target: ImportTarget::Repo {
                repo_root: repo.clone(),
            },
            source: None,
            scope: ImportScope::Curated,
            include_global: false,
            dry_run: false,
        };
        let report = setup_claude_profile(&request)?;
        assert!(!report.instructions_written.is_empty());

        let agents_md = repo.join("AGENTS.md");
        assert!(agents_md.exists());
        let content = fs::read_to_string(&agents_md)?;
        assert!(content.contains("BEGIN contrail:claude-import src=CLAUDE.md"));
        assert!(content.contains("Do the thing."));
        assert!(content.contains("END contrail:claude-import src=CLAUDE.md"));
        Ok(())
    }

    #[test]
    fn setup_appends_idempotently() -> Result<()> {
        let temp = TempDir::new()?;
        let repo = temp.path().join("repo");
        fs::create_dir_all(repo.join(".claude"))?;
        fs::write(repo.join("CLAUDE.md"), "# Instructions\n")?;

        let request = SetupRequest {
            target: ImportTarget::Repo {
                repo_root: repo.clone(),
            },
            source: None,
            scope: ImportScope::Curated,
            include_global: false,
            dry_run: false,
        };
        setup_claude_profile(&request)?;
        let first = fs::read_to_string(repo.join("AGENTS.md"))?;

        setup_claude_profile(&request)?;
        let second = fs::read_to_string(repo.join("AGENTS.md"))?;

        assert_eq!(first, second);
        Ok(())
    }

    #[test]
    fn setup_dry_run_writes_nothing() -> Result<()> {
        let temp = TempDir::new()?;
        let repo = temp.path().join("repo");
        fs::create_dir_all(repo.join(".claude/commands"))?;
        fs::write(repo.join("CLAUDE.md"), "# Instr\n")?;
        fs::write(repo.join(".claude/commands/build.md"), "run cargo build\n")?;

        let request = SetupRequest {
            target: ImportTarget::Repo {
                repo_root: repo.clone(),
            },
            source: None,
            scope: ImportScope::Curated,
            include_global: false,
            dry_run: true,
        };
        let report = setup_claude_profile(&request)?;
        assert!(report.dry_run);
        assert!(!report.instructions_written.is_empty());
        assert!(!report.skills_written.is_empty());

        assert!(!repo.join("AGENTS.md").exists());
        assert!(!repo.join(".agents/skills").exists());
        Ok(())
    }

    #[test]
    fn skill_slug_avoids_collisions() {
        assert_ne!(
            skill_slug("commands/foo-bar.md"),
            skill_slug("commands/foo_bar.md")
        );
        assert_ne!(skill_slug("commands/a/b.md"), skill_slug("commands/a_b.md"));
    }

    #[test]
    fn setup_skills_written_to_live_dir() -> Result<()> {
        let temp = TempDir::new()?;
        let repo = temp.path().join("repo");
        fs::create_dir_all(repo.join(".claude/commands"))?;
        fs::create_dir_all(repo.join(".claude/agents"))?;
        fs::write(
            repo.join(".claude/commands/build.md"),
            "---\ndescription: Build the project\n---\ncargo build\n",
        )?;
        fs::write(
            repo.join(".claude/agents/reviewer.md"),
            "---\ndescription: Code reviewer\n---\nReview code.\n",
        )?;

        let request = SetupRequest {
            target: ImportTarget::Repo {
                repo_root: repo.clone(),
            },
            source: None,
            scope: ImportScope::Curated,
            include_global: false,
            dry_run: false,
        };
        let report = setup_claude_profile(&request)?;

        let cmd_items: Vec<_> = report
            .skills_written
            .iter()
            .filter(|s| s.category == "commands")
            .collect();
        let agent_items: Vec<_> = report
            .skills_written
            .iter()
            .filter(|s| s.category == "agents")
            .collect();
        assert_eq!(cmd_items.len(), 1);
        assert_eq!(agent_items.len(), 1);

        assert!(cmd_items[0]
            .destination
            .starts_with(repo.join(".agents/skills")));
        assert!(cmd_items[0]
            .destination
            .to_string_lossy()
            .contains("claude-cmd-"));
        assert!(cmd_items[0].destination.ends_with("SKILL.md"));
        assert!(cmd_items[0].destination.exists());

        assert!(agent_items[0]
            .destination
            .to_string_lossy()
            .contains("claude-agent-"));
        assert!(agent_items[0].destination.exists());

        let skill_content = fs::read_to_string(&cmd_items[0].destination)?;
        assert!(skill_content.contains("name:"));
        assert!(skill_content.contains("description: Build the project"));
        Ok(())
    }

    #[test]
    fn setup_repo_with_only_claude_md_no_dot_claude_dir() -> Result<()> {
        let temp = TempDir::new()?;
        let repo = temp.path().join("repo");
        fs::create_dir_all(&repo)?;
        fs::write(
            repo.join("CLAUDE.md"),
            "# My repo rules\nAlways run tests.\n",
        )?;

        let request = SetupRequest {
            target: ImportTarget::Repo {
                repo_root: repo.clone(),
            },
            source: None,
            scope: ImportScope::Curated,
            include_global: false,
            dry_run: false,
        };
        let report = setup_claude_profile(&request)?;
        assert_eq!(report.instructions_written.len(), 1);
        assert!(report.errors.is_empty());

        let agents_md = repo.join("AGENTS.md");
        assert!(agents_md.exists());
        let content = fs::read_to_string(&agents_md)?;
        assert!(content.contains("Always run tests."));
        Ok(())
    }
}
