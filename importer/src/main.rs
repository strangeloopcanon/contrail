use anyhow::Result;
use scrapers::config::ContrailConfig;
use scrapers::history_import;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    println!("✈️  Contrail History Importer");
    println!("Scanning for historical logs (Codex, Claude, Cursor, Antigravity)...");

    let config = ContrailConfig::from_env()?;
    let stats = history_import::import_history(&config)?;
    println!(
        "✅ Import complete! imported={} skipped={} errors={}",
        stats.imported, stats.skipped, stats.errors
    );
    Ok(())
}
