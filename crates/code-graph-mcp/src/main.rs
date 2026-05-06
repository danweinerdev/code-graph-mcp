//! `code-graph-mcp` binary entry point.
//!
//! Builds a [`LanguageRegistry`] (C++, Rust, and Go are live as of Phase 6;
//! Phase 7 adds Python), constructs a [`CodeGraphServer`], and serves
//! stdio MCP via rmcp's [`ServiceExt::serve`] / [`RunningService::waiting`].
//!
//! Tool handlers are stubs in Phase 3.1; Phase 3.4 / 3.5 fill them in.

use anyhow::Context;
use codegraph_lang::LanguageRegistry;
use codegraph_lang_cpp::CppParser;
use codegraph_lang_go::GoParser;
use codegraph_lang_rust::RustParser;
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
    registry
        .register(Box::new(
            RustParser::new().context("initialize Rust language plugin")?,
        ))
        .context("register Rust language plugin")?;
    registry
        .register(Box::new(
            GoParser::new().context("initialize Go language plugin")?,
        ))
        .context("register Go language plugin")?;

    let server = CodeGraphServer::new(registry);

    let service = server
        .serve(stdio())
        .await
        .context("rmcp stdio handshake")?;

    service.waiting().await.context("mcp service loop")?;

    Ok(())
}
