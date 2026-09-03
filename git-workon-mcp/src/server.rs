//! [`WorkonServer`]: the MCP `ServerHandler` for the whole suite. Tool routes are added by
//! domain module under [`crate::tools`] (today: `annotations`; future: worktrees, stack —
//! see `docs/rfc/agent-integration.md` Model C); this file only owns the router field and
//! `get_info`.

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{Implementation, ProtocolVersion, ServerCapabilities, ServerInfo};
use rmcp::{tool_handler, ServerHandler};

#[derive(Debug, Clone)]
pub struct WorkonServer {
    pub(crate) tool_router: ToolRouter<Self>,
}

impl WorkonServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for WorkonServer {
    fn get_info(&self) -> ServerInfo {
        // `ServerInfo` and `Implementation` are `#[non_exhaustive]` in rmcp 3.2, so they
        // can't be built with a struct literal; mutate defaults instead.
        let mut server_info = Implementation::from_build_env();
        // `Implementation::from_build_env()` reads `env!("CARGO_CRATE_NAME")` at the call
        // site inside rmcp itself, so it would report "rmcp" here, not this binary — name
        // it explicitly instead.
        server_info.name = "git-workon-mcp".to_string();
        server_info.version = env!("CARGO_PKG_VERSION").to_string();

        let mut info = ServerInfo::default();
        info.protocol_version = ProtocolVersion::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = server_info;
        info.instructions = Some(
            "Read and write git-workon-review's annotation store: line comments, \
             replies, and explain-diff-style walkthroughs. All tools take an optional \
             `repo_path`, defaulting to discovery from the current directory."
                .to_string(),
        );
        info
    }
}
