use anyhow::Result;
use scrapers::config::ContrailConfig;
use scrapers::history_import;

fn main() -> Result<()> {
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
