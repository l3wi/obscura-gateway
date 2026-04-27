#[tokio::main]
async fn main() -> anyhow::Result<()> {
    obscura_gateway::run().await
}
