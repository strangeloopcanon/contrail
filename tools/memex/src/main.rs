mod aliases;
mod bundle;
mod detect;
mod explain;
mod init;
mod link;
mod readers;
mod render;
mod search;
mod share;
mod sync;
mod types;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::Command;

#[derive(Parser)]
#[command(name = "memex", about = "Self-managed context layer for coding agents")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize .context/ in the current repo and wire up detected agents
    Init,
    /// Sync recent session transcripts from agent storage into .context/sessions/
    Sync {
        /// How many days of history to sync (default: 30)
        #[arg(long, default_value_t = 30)]
        days: u64,
        /// Suppress output (for use in git hooks)
        #[arg(long, default_value_t = false)]
        quiet: bool,
    },
    /// Record a link between the current HEAD commit and active agent sessions
    LinkCommit {
        /// Suppress output (for use in git hooks)
        #[arg(long, default_value_t = false)]
        quiet: bool,
    },
    /// Show which agent sessions were active when a commit was made
    Explain {
        /// Commit SHA or prefix to look up
        commit: String,
    },
    /// Greppable search across synced sessions + learnings
    Search {
        /// Literal text query (substring match, not regex)
        query: String,
        /// Only search session files modified in the last N days (learnings always searched)
        #[arg(long, default_value_t = 30)]
        days: u64,
        /// Maximum number of matches to print (default: 200)
        #[arg(long, default_value_t = 200)]
        limit: usize,
        /// Case-sensitive search (default: false)
        #[arg(long, default_value_t = false)]
        case_sensitive: bool,
        /// Only print matching filenames (like `rg -l`)
        #[arg(long, default_value_t = false)]
        files: bool,
    },
    /// Encrypt sessions + learnings into .context/vault.age for sharing via git
    Share {
        /// Passphrase (required)
        #[arg(long)]
        passphrase: Option<String>,
    },
    /// Encrypt a single session transcript into a portable bundle under .context/bundles/
    ShareSession {
        /// Session filename under .context/sessions/ (e.g. 2026-02-10T12-00-00_codex-cli_abc123.md)
        session: String,
        /// Passphrase (required)
        #[arg(long)]
        passphrase: Option<String>,
    },
    /// Import a shared session bundle by ID (resolves from working tree first, then git history)
    Import {
        /// Bundle ID (the filename stem under .context/bundles/, without extension)
        id: String,
        /// Passphrase (required)
        #[arg(long)]
        passphrase: Option<String>,
    },
    /// Decrypt .context/vault.age back into sessions + learnings
    Unlock {
        /// Passphrase (required)
        #[arg(long)]
        passphrase: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo_root = find_repo_root()?;

    match cli.command {
        Commands::Init => init::run_init(&repo_root),
        Commands::Sync { days, quiet } => sync::run_sync(&repo_root, days, quiet),
        Commands::LinkCommit { quiet } => link::run_link_commit(&repo_root, quiet),
        Commands::Explain { commit } => explain::run_explain(&repo_root, &commit),
        Commands::Search {
            query,
            days,
            limit,
            case_sensitive,
            files,
        } => search::run_search(&repo_root, &query, days, limit, case_sensitive, files),
        Commands::Share { passphrase } => share::run_share(&repo_root, passphrase),
        Commands::ShareSession {
            session,
            passphrase,
        } => bundle::run_share_session(&repo_root, &session, passphrase),
        Commands::Import { id, passphrase } => bundle::run_import(&repo_root, &id, passphrase),
        Commands::Unlock { passphrase } => share::run_unlock(&repo_root, passphrase),
    }
}

fn find_repo_root() -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("failed to run git rev-parse")?;

    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(PathBuf::from(path))
    } else {
        // Fall back to current directory if not in a git repo
        std::env::current_dir().context("failed to get current directory")
    }
}
