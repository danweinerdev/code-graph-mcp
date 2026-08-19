//! `code-graph-mcp` binary entry point.
//!
//! Builds a [`LanguageRegistry`] with all six shipped language plugins —
//! C++, Rust, Go, Python, C#, and Java — constructs a [`CodeGraphServer`],
//! and serves stdio MCP by default. `--serve` starts the repository-local
//! daemon transport used by the later proxy mode.

mod daemon;

use anyhow::Context;
use code_graph_core::RootConfig;
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
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let no_daemon = args.iter().any(|arg| arg == "--no-daemon");
    let serve_daemon = !no_daemon && args.iter().any(|arg| arg == "--serve");
    // Attach-only proxy mode (Designs/CommandLineInterface Decision 3):
    // attach to a published, compatible, live daemon or serve in-process —
    // never spawn a contender, never initiate the replacement protocol.
    // `--no-daemon` and `--serve` both take precedence.
    let attach_only = !no_daemon && !serve_daemon && args.iter().any(|arg| arg == "--attach-only");

    if !serve_daemon && !no_daemon {
        // Proxy selection must happen before constructing the MCP transport:
        // when enabled, stdin/stdout are byte-pumped directly to the daemon.
        if let Ok(cwd) = std::env::current_dir() {
            // Must match daemon::run's canonicalization: both sides derive
            // DaemonPaths from this root, and a `\\?\`-verbatim client path
            // against a dunce-stripped daemon path would split the runtime
            // directory and defeat attach on Windows.
            if let Ok(cwd) = code_graph_core::paths::canonicalize(&cwd) {
                match RootConfig::load(&cwd) {
                    Ok((config, root)) if config.daemon.enabled => {
                        let attempt = if attach_only {
                            daemon::proxy_attach_only(root.clone()).await
                        } else {
                            daemon::proxy(root.clone()).await
                        };
                        match attempt {
                            // `tokio::io::stdin` uses a blocking reader. After the daemon
                            // closes first, its cancelled read can keep runtime teardown
                            // waiting even though the byte pump has ended. The proxy has
                            // already drained stdout, so exit without waiting for that
                            // irrelevant stdin worker.
                            Ok(()) => std::process::exit(0),
                            Err(error) => eprintln!(
                                "code-graph-mcp: daemon unavailable for {}; falling back to in-process stdio ({error})",
                                root.display()
                            ),
                        }
                    }
                    Ok(_) => {}
                    // A malformed configuration did not affect binary startup before
                    // daemon opt-in. Preserve that direct-stdio behavior; the normal
                    // analyze path continues to surface the configuration error.
                    Err(_) => {}
                }
            }
        }
    }

    let server = make_server()?;

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

fn make_server() -> anyhow::Result<CodeGraphServer> {
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

    Ok(CodeGraphServer::with_vcs_registry(registry, vcs_registry()))
}

/// Version-control providers for the history tools. Binding happens here in
/// the binary so `code-graph-tools` depends on the trait crate only (AC-23).
///
/// The git provider binds to the working tree discovered from the process
/// working directory — the daemon runs with its project root as cwd, and
/// direct stdio servers are launched at the workspace root by MCP hosts. A
/// cwd outside any git repository simply registers no provider, and the
/// history tools report unavailability as a success (FR-36).
fn vcs_registry() -> code_graph_vcs::VcsRegistry {
    let mut vcs = code_graph_vcs::VcsRegistry::new();
    let Ok(cwd) = std::env::current_dir() else {
        return vcs;
    };
    let Ok(root) = code_graph_core::paths::canonicalize(&cwd) else {
        return vcs;
    };
    match code_graph_vcs_git::GitProvider::open(&root) {
        Ok(provider) => {
            if let Err(error) = vcs.register(Box::new(provider)) {
                eprintln!("code-graph-mcp: register git provider: {error}");
            }
        }
        // A cwd outside any repository is the ordinary no-VCS case and
        // stays silent (FR-36 reports it per call); anything else is a
        // real failure worth a breadcrumb rather than a silent absence.
        Err(code_graph_vcs::VcsError::Unavailable(_)) => {}
        Err(error) => eprintln!(
            "code-graph-mcp: git provider unavailable at {}: {error}",
            root.display()
        ),
    }
    vcs
}
