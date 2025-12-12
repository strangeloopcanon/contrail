use anyhow::Context;
use scrapers::config::ContrailConfig;
use scrapers::harvester::Harvester;
use scrapers::log_writer::LogWriter;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::task;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("Starting Contrail Daemon...");

    let config = ContrailConfig::from_env()?;

    // Phase 1: Create directory structure
    let contrail_dir = config
        .log_path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| config.log_path.clone());
    fs::create_dir_all(&contrail_dir).context("Failed to create log directory")?;
    println!(
        "Ensured Contrail log directory exists at {:?}",
        contrail_dir
    );

    let log_writer = LogWriter::new(config.log_path.clone());

    let enable_cursor = config.enable_cursor;
    let enable_codex = config.enable_codex;
    let enable_antigravity = config.enable_antigravity;
    let enable_claude = config.enable_claude;

    let harvester = Arc::new(Harvester::new(log_writer, config));

    let h1 = harvester.clone();
    let cursor_handle = task::spawn(async move {
        if enable_cursor {
            if let Err(e) = h1.run_cursor_watcher().await {
                eprintln!("Cursor Watcher failed: {:?}", e);
            }
        }
    });

    let h2 = harvester.clone();
    let codex_handle = task::spawn(async move {
        if enable_codex {
            if let Err(e) = h2.run_codex_watcher().await {
                eprintln!("Codex Watcher failed: {:?}", e);
            }
        }
    });

    let h3 = harvester.clone();
    let antigravity_handle = task::spawn(async move {
        if enable_antigravity {
            if let Err(e) = h3.run_antigravity_watcher().await {
                eprintln!("Antigravity Watcher failed: {:?}", e);
            }
        }
    });

    let h4 = harvester.clone();
    let claude_handle = task::spawn(async move {
        if enable_claude {
            if let Err(e) = h4.run_claude_watcher().await {
                eprintln!("Claude Watcher failed: {:?}", e);
            }
        }
    });

    // Wait for tasks (they shouldn't finish unless error)
    let _ = tokio::join!(
        cursor_handle,
        codex_handle,
        antigravity_handle,
        claude_handle
    );

    Ok(())
}
