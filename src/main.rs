use rmcp::{transport::stdio, ServiceExt};

use cosh::mcp::server::experimental::VisionServer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let service = VisionServer.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
