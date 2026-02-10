mod detect;
mod init;
mod readers;
mod render;
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
    /// Encrypt sessions + learnings into .context/vault.age for sharing via git
    Share {
        /// Passphrase (prompted interactively if omitted)
        #[arg(long)]
        passphrase: Option<String>,
    },
    /// Decrypt .context/vault.age back into sessions + learnings
    Unlock {
        /// Passphrase (prompted interactively if omitted)
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
        Commands::Share { passphrase } => share::run_share(&repo_root, passphrase),
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
