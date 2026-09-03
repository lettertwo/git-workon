//! `git-workon-mcp`: MCP server for the git-workon suite (stdio transport), reached as
//! `git workon mcp` via `git-workon`'s external-subcommand PATH dispatch. This binary is
//! never a dependency of the published `git-workon` crate — see ADR-040's publish-blocker
//! reasoning (carried over from ADR-039, which first identified it).
//!
//! Tool routes live under [`tools`], grouped by domain; [`server::WorkonServer`] wires them
//! into the `ServerHandler` rmcp dispatches against.

mod server;
mod tools;

use rmcp::transport::io::stdio;
use rmcp::ServiceExt;

use server::WorkonServer;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let service = WorkonServer::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
