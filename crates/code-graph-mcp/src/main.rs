//! `code-graph-mcp` binary entry point.
//!
//! Builds a [`LanguageRegistry`] with all six shipped language plugins —
//! C++, Rust, Go, Python, C#, and Java — constructs a [`CodeGraphServer`],
//! and serves stdio MCP by default. `--serve` starts the repository-local
//! daemon transport used by the later proxy mode.

mod daemon;

use anyhow::Context;
use code_graph_lang::LanguageRegistry;
use code_graph_lang_cpp::CppParser;
use code_graph_lang_csharp::CSharpParser;
use code_graph_lang_go::GoParser;
use code_graph_lang_java::JavaParser;
use code_graph_lang_python::PythonParser;
use code_graph_lang_rust::RustParser;
use code_graph_tools::CodeGraphServer;
use rmcp::transport::io::stdio;
use rmcp::ServiceExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let serve_daemon = std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "--serve");
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
    registry
        .register(Box::new(
            CSharpParser::new().context("initialize C# language plugin")?,
        ))
        .context("register C# language plugin")?;
    registry
        .register(Box::new(
            JavaParser::new().context("initialize Java language plugin")?,
        ))
        .context("register Java language plugin")?;

    let server = CodeGraphServer::new(registry);

    if serve_daemon {
        return daemon::run(server).await;
    }

    let service = server
        .serve(stdio())
        .await
        .context("rmcp stdio handshake")?;

    service.waiting().await.context("mcp service loop")?;

    Ok(())
}
