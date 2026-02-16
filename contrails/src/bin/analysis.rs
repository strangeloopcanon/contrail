#[tokio::main]
async fn main() -> anyhow::Result<()> {
    analysis::run().await
}
