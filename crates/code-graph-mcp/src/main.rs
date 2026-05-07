//! `code-graph-mcp` binary entry point.
//!
//! Builds a [`LanguageRegistry`] with all four shipped language plugins —
//! C++, Rust, Go, and Python — constructs a [`CodeGraphServer`], and
//! serves stdio MCP via rmcp's [`ServiceExt::serve`] /
//! [`RunningService::waiting`].

use anyhow::Context;
use codegraph_lang::LanguageRegistry;
use codegraph_lang_cpp::CppParser;
use codegraph_lang_go::GoParser;
use codegraph_lang_python::PythonParser;
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
    registry
        .register(Box::new(
            PythonParser::new().context("initialize Python language plugin")?,
        ))
        .context("register Python language plugin")?;

    let server = CodeGraphServer::new(registry);

    let service = server
        .serve(stdio())
        .await
        .context("rmcp stdio handshake")?;

    service.waiting().await.context("mcp service loop")?;

    Ok(())
}
