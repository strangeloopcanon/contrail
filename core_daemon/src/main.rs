use anyhow::Context;
use scrapers::harvester::Harvester;
use scrapers::log_writer::LogWriter;
use std::fs;
use std::sync::Arc;
use tokio::task;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("Starting Contrail Daemon...");

    // Phase 1: Create directory structure
    let home = dirs::home_dir().context("Could not find home directory")?;
    let contrail_dir = home.join(".contrail/logs");
    fs::create_dir_all(&contrail_dir).context("Failed to create .contrail directory")?;
    println!("Ensured .contrail directory exists at {:?}", contrail_dir);

    let log_file = contrail_dir.join("master_log.jsonl");
    let log_writer = LogWriter::new(log_file);

    let harvester = Arc::new(Harvester::new(log_writer));

    let h1 = harvester.clone();
    let cursor_handle = task::spawn(async move {
        if let Err(e) = h1.run_cursor_watcher().await {
            eprintln!("Cursor Watcher failed: {:?}", e);
        }
    });

    let h2 = harvester.clone();
    let codex_handle = task::spawn(async move {
        if let Err(e) = h2.run_codex_watcher().await {
            eprintln!("Codex Watcher failed: {:?}", e);
        }
    });

    let h3 = harvester.clone();
    let antigravity_handle = task::spawn(async move {
        if let Err(e) = h3.run_antigravity_watcher().await {
            eprintln!("Antigravity Watcher failed: {:?}", e);
        }
    });

    let h4 = harvester.clone();
    let claude_handle = task::spawn(async move {
        if let Err(e) = h4.run_claude_watcher().await {
            eprintln!("Claude Watcher failed: {:?}", e);
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
