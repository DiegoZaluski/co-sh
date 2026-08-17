use rmcp::{ServiceExt, transport::stdio};

use cosh_server::mcp::tools::Server;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let service = Server::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
