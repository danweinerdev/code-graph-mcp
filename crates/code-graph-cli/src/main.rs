//! `code-graph` — command-line front-end over the typed core
//! (Designs/CommandLineInterface; phase 7 of GraphPlatformExpansion).
//!
//! Every subcommand executes the same `core::` function the MCP adapter
//! executes (FR-17); machine output (`--json`) is the MCP payload
//! byte-for-byte (FR-19); exit status carries the outcome class (FR-20):
//! `0` success (including success-shaped negatives), `1` tool error,
//! `2` operational failure. Attaches to a running repository daemon via a
//! spawned `code-graph-mcp --attach-only` child when `daemon.json` is
//! published, answers standalone from the on-disk cache when not (FR-18).

#![forbid(unsafe_code)]

mod args;
mod daemon_client;
mod exec;
mod render;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use args::Cli;

/// A successful tool outcome: the payload in its wire form.
pub enum Outcome {
    /// A `ToolOk::Value` success — the compact JSON payload text.
    Json(String),
    /// A `ToolOk::Text` success (mermaid diagram, advisory) — verbatim.
    Text(String),
}

/// The two failure classes of the exit-status contract (FR-20).
pub enum CliError {
    /// User-visible tool error (`ToolError` / MCP `isError`): exit 1.
    Tool(String),
    /// The query never ran or the channel died: exit 2.
    Operational(String),
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let json_mode = cli.json;
    match run(cli).await {
        Ok(outcome) => {
            render(outcome, json_mode);
            ExitCode::SUCCESS
        }
        Err(CliError::Tool(message)) => {
            eprintln!("{message}");
            ExitCode::from(1)
        }
        Err(CliError::Operational(message)) => {
            eprintln!("code-graph: {message}");
            ExitCode::from(2)
        }
    }
}

async fn run(cli: Cli) -> Result<Outcome, CliError> {
    let invocation_root = match cli.root {
        Some(root) => root,
        None => std::env::current_dir()
            .map_err(|e| CliError::Operational(format!("cannot resolve current directory: {e}")))?,
    };
    let root = code_graph_core::paths::canonicalize(&invocation_root).map_err(|e| {
        CliError::Operational(format!(
            "cannot resolve root {}: {e}",
            invocation_root.display()
        ))
    })?;
    // Project root = config discovery root; also where daemon.json lives.
    let (_, project_root) = code_graph_core::RootConfig::load(&root)
        .map_err(|e| CliError::Operational(format!("configuration error: {e}")))?;

    // Backend choice by the documented discovery surface (FR-40): a
    // published daemon.json selects the daemon backend; the child then
    // revalidates liveness itself.
    if daemon_metadata_path(&project_root).exists() {
        let (tool, arguments) = cli.command.tool_call(&root.to_string_lossy());
        match daemon_client::call(&project_root, tool, arguments, cli.quiet).await? {
            Some(answer) if answer.is_error => {
                // Decision 7: a daemon that holds no graph does not hide
                // the cache. Byte-exact match on the core guard's own
                // message — computed, not duplicated.
                let not_indexed = code_graph_tools::core::require_indexed(false)
                    .expect_err("require_indexed(false) is always an error")
                    .0;
                if answer.payload == not_indexed && cli.command.unindexed_fallback_eligible() {
                    if !cli.quiet {
                        eprintln!(
                            "code-graph: daemon holds no graph yet; answering from the on-disk cache"
                        );
                    }
                    // fall through to standalone below
                } else {
                    return Err(CliError::Tool(answer.payload));
                }
            }
            Some(answer) => {
                // The daemon's payload is already the MCP wire text; keep
                // it verbatim (Decision 4). JSON-vs-text is decided by
                // whether it parses — mermaid/advisory bodies do not.
                return Ok(
                    match serde_json::from_str::<serde_json::Value>(&answer.payload) {
                        Ok(_) => Outcome::Json(answer.payload),
                        Err(_) => Outcome::Text(answer.payload),
                    },
                );
            }
            // Spawn failure: breadcrumb already printed; fall through.
            None => {}
        }
    }

    let load_cache = !matches!(cli.command, args::Command::AnalyzeCodebase { .. });
    let app = exec::bootstrap(&root, load_cache)?;
    exec::run(&app, &cli.command).await
}

fn daemon_metadata_path(project_root: &std::path::Path) -> PathBuf {
    project_root.join(".code-graph").join("daemon.json")
}

/// Machine mode prints the payload verbatim; human mode renders FROM the
/// same payload JSON through the per-shape renderer (Decision 4 — one
/// payload, one renderer; `ToolOk::Text` bodies pass through verbatim in
/// both modes).
fn render(outcome: Outcome, json_mode: bool) {
    match outcome {
        Outcome::Json(payload) => {
            if json_mode {
                println!("{payload}");
            } else {
                match serde_json::from_str::<serde_json::Value>(&payload) {
                    Ok(value) => print!("{}", render::human(&value)),
                    Err(_) => println!("{payload}"),
                }
            }
        }
        Outcome::Text(text) => println!("{text}"),
    }
}
