use anyhow::Result;
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use scrapers::claude_profile_import::{
    ImportScope, ImportTarget, SetupRequest, setup_claude_profile,
};
use scrapers::config::ContrailConfig;
use scrapers::history_import;
use scrapers::merge::{self, ExportFilters};
use std::path::PathBuf;
use std::process::Command;

#[derive(Parser)]
#[command(
    name = "importer",
    about = "Contrail log tools: history import, profile import, cross-machine export/merge"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Import historical logs from Codex, Claude, Cursor, and Antigravity native storage (one-time backfill).
    ImportHistory,

    /// Export the master log (or a filtered subset) to a portable JSONL file.
    ExportLog {
        /// Output file path.
        #[arg(short, long)]
        output: PathBuf,

        /// Only include events after this timestamp (RFC 3339, e.g. 2026-01-01T00:00:00Z).
        #[arg(long)]
        after: Option<String>,

        /// Only include events before this timestamp (RFC 3339).
        #[arg(long)]
        before: Option<String>,

        /// Filter by project path prefix (e.g. /Users/rohit/myproject).
        #[arg(long)]
        project: Option<String>,

        /// Filter by source tool (cursor, codex-cli, claude-code, antigravity).
        #[arg(long)]
        tool: Option<String>,

        /// Filter by hostname (only reliable for live-captured events, not history imports).
        #[arg(long)]
        hostname: Option<String>,
    },

    /// Merge events from an external JSONL file into the local master log.
    ///
    /// Deduplicates by event_id UUID first, then by content fingerprint to catch
    /// the same underlying event ingested independently on two machines.
    ///
    /// Stop the contrail daemon before running this to avoid partial-line interleaving.
    MergeLog {
        /// Path to the JSONL file to merge in.
        file: PathBuf,
    },

    /// Migrate Claude Code profile (instructions, commands, agents, history) to Codex.
    ImportClaude {
        /// Repo root (makes this a repo-scoped migration; omit for global).
        #[arg(long)]
        repo_root: Option<PathBuf>,

        /// Also include global ~/.claude profile when doing a repo migration.
        #[arg(long, default_value_t = false)]
        include_global: bool,

        /// Optional source override (default: ~/.claude).
        #[arg(long)]
        source: Option<PathBuf>,

        /// Scan scope policy.
        #[arg(long, value_enum, default_value = "curated")]
        scope: CliImportScope,

        /// Preview what would happen without writing anything.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },
}

#[derive(Clone, Debug, ValueEnum)]
enum CliImportScope {
    Curated,
    Broad,
    Full,
}

impl From<CliImportScope> for ImportScope {
    fn from(value: CliImportScope) -> Self {
        match value {
            CliImportScope::Curated => Self::Curated,
            CliImportScope::Broad => Self::Broad,
            CliImportScope::Full => Self::Full,
        }
    }
}

pub fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        None | Some(Commands::ImportHistory) => run_import_history(),
        Some(Commands::ExportLog {
            output,
            after,
            before,
            project,
            tool,
            hostname,
        }) => run_export(output, after, before, project, tool, hostname),
        Some(Commands::MergeLog { file }) => run_merge(file),
        Some(Commands::ImportClaude {
            repo_root,
            include_global,
            source,
            scope,
            dry_run,
        }) => run_import_claude(repo_root, include_global, source, scope, dry_run),
    }
}

fn run_import_history() -> Result<()> {
    println!("Contrail History Importer");
    println!("Scanning for historical logs (Codex, Claude, Cursor, Antigravity)...");

    let config = ContrailConfig::from_env()?;
    let stats = history_import::import_history(&config)?;
    println!(
        "Import complete: imported={} skipped={} errors={}",
        stats.imported, stats.skipped, stats.errors
    );
    Ok(())
}

fn run_export(
    output: PathBuf,
    after: Option<String>,
    before: Option<String>,
    project: Option<String>,
    tool: Option<String>,
    hostname: Option<String>,
) -> Result<()> {
    let config = ContrailConfig::from_env()?;

    let filters = ExportFilters {
        after: parse_optional_ts(after.as_deref(), "--after")?,
        before: parse_optional_ts(before.as_deref(), "--before")?,
        project,
        tool,
        hostname,
    };

    let stats = merge::export_log(&config.log_path, &filters, &output)?;
    println!(
        "Exported {} events to {} (skipped={}, errors={})",
        stats.exported,
        output.display(),
        stats.skipped,
        stats.errors,
    );
    Ok(())
}

fn run_merge(file: PathBuf) -> Result<()> {
    let config = ContrailConfig::from_env()?;

    if is_contrail_daemon_running() {
        anyhow::bail!("com.contrail.daemon is running; stop it before merge");
    }

    println!(
        "Merging {} into {}",
        file.display(),
        config.log_path.display()
    );

    let stats = merge::merge_log(&config.log_path, &file)?;
    println!(
        "Merge complete: merged={} skipped_uuid={} skipped_fingerprint={} errors={}",
        stats.merged, stats.skipped_uuid, stats.skipped_fingerprint, stats.errors,
    );
    Ok(())
}

fn run_import_claude(
    repo_root: Option<PathBuf>,
    include_global: bool,
    source: Option<PathBuf>,
    scope: CliImportScope,
    dry_run: bool,
) -> Result<()> {
    let target = if let Some(root) = repo_root {
        ImportTarget::Repo { repo_root: root }
    } else {
        ImportTarget::Global
    };

    let request = SetupRequest {
        target,
        source,
        scope: scope.into(),
        include_global,
        dry_run,
    };

    let report = setup_claude_profile(&request)?;

    if report.dry_run {
        println!("Claude -> Codex migration (dry run, nothing written)");
    } else {
        println!("Claude -> Codex migration complete");
    }
    println!();

    if !report.instructions_written.is_empty() {
        let dest = report
            .agents_md_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "AGENTS.md".to_string());
        println!(
            "  Instructions:  {} appended to {}",
            report.instructions_written.len(),
            dest
        );
    }

    if !report.skills_written.is_empty() {
        let cmd_count = report
            .skills_written
            .iter()
            .filter(|s| s.category == "commands")
            .count();
        let agent_count = report
            .skills_written
            .iter()
            .filter(|s| s.category == "agents")
            .count();
        let dest = report
            .skills_dir
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "skills/".to_string());
        println!(
            "  Skills:        {} written ({} commands, {} agents) -> {}",
            report.skills_written.len(),
            cmd_count,
            agent_count,
            dest
        );
    }

    if report.history_ingested > 0 || report.history_skipped > 0 {
        println!(
            "  History:       {} events ingested ({} skipped as duplicates)",
            report.history_ingested, report.history_skipped
        );
    }

    if !report.archived.is_empty() {
        println!("  Archived:      {} files", report.archived.len());
        for item in &report.archived {
            println!(
                "                   {} -> {}",
                item.source,
                item.destination.display()
            );
        }
    }

    if !report.errors.is_empty() {
        println!();
        println!("  Errors ({}):", report.errors.len());
        for err in &report.errors {
            println!("    - {err}");
        }
    }

    if !report.not_transferred.is_empty() {
        println!();
        println!("  Manual review needed:");
        for note in &report.not_transferred {
            println!("    - {note}");
        }
    }

    if let Some(agents) = &report.agents_md_path
        && !report.instructions_written.is_empty()
        && !report.dry_run
    {
        println!();
        println!("  Verify imported instructions: {}", agents.display());
    }

    Ok(())
}

fn parse_optional_ts(value: Option<&str>, flag_name: &str) -> Result<Option<DateTime<Utc>>> {
    match value {
        None => Ok(None),
        Some(s) => {
            let dt = s
                .parse::<DateTime<Utc>>()
                .map_err(|e| anyhow::anyhow!("invalid {flag_name} timestamp '{s}': {e}"))?;
            Ok(Some(dt))
        }
    }
}

#[cfg(target_os = "macos")]
fn is_contrail_daemon_running() -> bool {
    Command::new("launchctl")
        .arg("list")
        .arg("com.contrail.daemon")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
fn is_contrail_daemon_running() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_optional_ts_accepts_rfc3339() {
        let ts = parse_optional_ts(Some("2026-01-01T00:00:00Z"), "--after").unwrap();
        assert!(ts.is_some());
    }

    #[test]
    fn parse_optional_ts_rejects_invalid() {
        let err = parse_optional_ts(Some("not-a-timestamp"), "--after").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("invalid --after timestamp"));
    }

    #[test]
    fn export_log_requires_output_path() {
        let parsed = Cli::try_parse_from(["importer", "export-log"]);
        assert!(parsed.is_err());
    }

    #[test]
    fn merge_log_requires_only_input_path() {
        let parsed = Cli::try_parse_from(["importer", "merge-log", "/tmp/input.jsonl"]).unwrap();
        let Some(Commands::MergeLog { .. }) = parsed.command else {
            panic!("expected merge-log subcommand");
        };
    }

    #[test]
    fn import_claude_parses_global() {
        let parsed = Cli::try_parse_from(["importer", "import-claude"]).unwrap();
        assert!(matches!(
            parsed.command,
            Some(Commands::ImportClaude { .. })
        ));
    }

    #[test]
    fn import_claude_parses_repo() {
        let parsed =
            Cli::try_parse_from(["importer", "import-claude", "--repo-root", "/tmp/repo"]).unwrap();
        let Some(Commands::ImportClaude { repo_root, .. }) = parsed.command else {
            panic!("expected import-claude");
        };
        assert_eq!(repo_root, Some(PathBuf::from("/tmp/repo")));
    }

    #[test]
    fn import_claude_dry_run() {
        let parsed = Cli::try_parse_from(["importer", "import-claude", "--dry-run"]).unwrap();
        let Some(Commands::ImportClaude { dry_run, .. }) = parsed.command else {
            panic!("expected import-claude");
        };
        assert!(dry_run);
    }
}
