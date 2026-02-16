#[tokio::main]
async fn main() -> anyhow::Result<()> {
    core_daemon::run().await
}
