use anyhow::Context;
use scrapers::config::ContrailConfig;
use scrapers::history_import;
use scrapers::log_writer::LogWriter;
use scrapers::watchers::Harvester;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::task;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("starting contrail daemon");

    let config = ContrailConfig::from_env()?;

    let contrail_dir = config
        .log_path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| config.log_path.clone());
    fs::create_dir_all(&contrail_dir).context("Failed to create log directory")?;
    info!(path = ?contrail_dir, "ensured log directory exists");

    maybe_import_history(&config);

    let log_writer = LogWriter::new(config.log_path.clone());

    let enable_cursor = config.enable_cursor;
    let enable_codex = config.enable_codex;
    let enable_antigravity = config.enable_antigravity;
    let enable_claude = config.enable_claude;

    let harvester = Arc::new(Harvester::new(log_writer, config));

    let h1 = harvester.clone();
    let cursor_handle = task::spawn(async move {
        if !enable_cursor {
            info!("cursor watcher disabled");
            return;
        }
        loop {
            if let Err(e) = h1.run_cursor_watcher().await {
                error!(err = ?e, "cursor watcher failed");
            }
            warn!("cursor watcher exited; restarting in 2s");
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });

    let h2 = harvester.clone();
    let codex_handle = task::spawn(async move {
        if !enable_codex {
            info!("codex watcher disabled");
            return;
        }
        loop {
            if let Err(e) = h2.run_codex_watcher().await {
                error!(err = ?e, "codex watcher failed");
            }
            warn!("codex watcher exited; restarting in 2s");
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });

    let h3 = harvester.clone();
    let antigravity_handle = task::spawn(async move {
        if !enable_antigravity {
            info!("antigravity watcher disabled");
            return;
        }
        loop {
            if let Err(e) = h3.run_antigravity_watcher().await {
                error!(err = ?e, "antigravity watcher failed");
            }
            warn!("antigravity watcher exited; restarting in 2s");
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });

    let h4 = harvester.clone();
    let claude_handle = task::spawn(async move {
        if !enable_claude {
            info!("claude watcher disabled");
            return;
        }
        loop {
            if let Err(e) = h4.run_claude_watcher().await {
                error!(err = ?e, "claude watcher failed");
            }
            warn!("claude watcher exited; restarting in 2s");
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });

    let h5 = harvester.clone();
    let claude_projects_handle = task::spawn(async move {
        if !enable_claude {
            return;
        }
        loop {
            if let Err(e) = h5.run_claude_projects_watcher().await {
                error!(err = ?e, "claude projects watcher failed");
            }
            warn!("claude projects watcher exited; restarting in 2s");
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });

    if !(enable_cursor || enable_codex || enable_antigravity || enable_claude) {
        warn!("all watchers are disabled; daemon will stay idle until shutdown");
    }

    tokio::signal::ctrl_c().await?;
    info!("received shutdown signal, stopping");

    cursor_handle.abort();
    codex_handle.abort();
    antigravity_handle.abort();
    claude_handle.abort();
    claude_projects_handle.abort();

    // Drop the harvester so its LogWriter sender drops,
    // which closes the channel and lets the background writer flush.
    drop(harvester);

    // Brief pause for the log writer to finish flushing
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    info!("contrail daemon stopped");
    Ok(())
}

fn maybe_import_history(config: &ContrailConfig) {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return,
    };
    let marker_path = home.join(scrapers::config::HISTORY_IMPORT_MARKER_REL);
    if marker_path.exists() {
        return;
    }

    info!("backfilling historical codex/claude logs (one-time)");
    match history_import::import_history(config) {
        Ok(stats) => {
            info!(
                imported = stats.imported,
                skipped = stats.skipped,
                errors = stats.errors,
                "history import complete"
            );
            if let Some(dir) = marker_path.parent() {
                let _ = fs::create_dir_all(dir);
            }
            let completed_at_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs());
            let body = serde_json::json!({
                "completed_at_unix": completed_at_unix,
                "imported": stats.imported,
                "skipped": stats.skipped,
                "errors": stats.errors,
                "log_path": config.log_path,
                "codex_root": config.codex_root,
                "claude_history": config.claude_history,
            });
            let _ = fs::write(
                &marker_path,
                serde_json::to_string_pretty(&body).unwrap_or_default(),
            );
        }
        Err(e) => {
            error!(err = ?e, "history import failed, continuing");
        }
    }
}
