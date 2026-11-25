mod tools;

use std::error::Error;

use rmcp::{ServiceExt, transport::stdio};
use tools::SysPulseServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init()?;

    tracing::info!("Starting sys-pulse-mcp over stdio");
    let service = SysPulseServer::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
