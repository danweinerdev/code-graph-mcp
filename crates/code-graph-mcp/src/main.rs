//! `code-graph-mcp` binary entry point.
//!
//! Builds a [`LanguageRegistry`] (currently C++ only — Phases 5/6/7 add
//! Rust, Go, Python), constructs a [`CodeGraphServer`], and serves stdio
//! MCP via rmcp's [`ServiceExt::serve`] / [`RunningService::waiting`].
//!
//! Tool handlers are stubs in Phase 3.1; Phase 3.4 / 3.5 fill them in.

use anyhow::Context;
use codegraph_lang::LanguageRegistry;
use codegraph_lang_cpp::CppParser;
use codegraph_tools::CodeGraphServer;
use rmcp::transport::io::stdio;
use rmcp::ServiceExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(
            CppParser::new().context("initialize C++ language plugin")?,
        ))
        .context("register C++ language plugin")?;

    let server = CodeGraphServer::new(registry);

    let service = server
        .serve(stdio())
        .await
        .context("rmcp stdio handshake")?;

    service.waiting().await.context("mcp service loop")?;

    Ok(())
}
