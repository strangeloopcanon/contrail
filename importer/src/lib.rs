use anyhow::Result;
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use scrapers::config::ContrailConfig;
use scrapers::history_import;
use scrapers::merge::{self, ExportFilters};
use std::path::PathBuf;
use std::process::Command;

#[derive(Parser)]
#[command(
    name = "importer",
    about = "Contrail log tools: history import, cross-machine export/merge"
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
}
