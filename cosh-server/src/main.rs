use rmcp::{ServiceExt, transport::stdio};

use cosh_server::mcp::experimental::VisionServer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let service = VisionServer.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
