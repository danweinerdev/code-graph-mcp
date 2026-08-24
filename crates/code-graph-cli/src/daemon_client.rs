//! Daemon backend (Designs/CommandLineInterface Decision 3).
//!
//! The client IS the existing proxy, packaged as a child process: the CLI
//! spawns `code-graph-mcp --attach-only` with piped stdio and speaks MCP
//! over it as a byte-level JSON-RPC client built on `serde_json` values —
//! `initialize`, `initialized`, one `tools/call`, EOF. The child performs
//! all attachment work (discovery, admission, auth, identity gating) and
//! serves in-process itself when no compatible live daemon is attachable —
//! it never spawns a contender and never initiates the replacement
//! protocol.
//!
//! **The child's exit status is never consulted.** The proxy deliberately
//! exits 0 on mid-session daemon death; failure detection here is a
//! missing or malformed JSON-RPC response, nothing else.

use std::path::Path;
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;

use crate::CliError;

/// One `tools/call` result from the daemon backend: the payload text
/// (exactly what the MCP adapter serialized) plus the error flag.
pub struct DaemonAnswer {
    pub payload: String,
    pub is_error: bool,
    /// The initialized daemon's package version. Decision 7 treats the
    /// not-indexed wording as package-versioned rather than a cross-version
    /// protocol contract.
    pub server_version: Option<String>,
}

/// Locates the sibling `code-graph-mcp` binary: next to our own executable
/// first (the installed layout), bare name (PATH resolution) second.
fn proxy_binary() -> std::path::PathBuf {
    let name = format!("code-graph-mcp{}", std::env::consts::EXE_SUFFIX);
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            let sibling = dir.join(&name);
            if sibling.exists() {
                return sibling;
            }
        }
    }
    std::path::PathBuf::from(name)
}

/// Calls one tool through the spawned proxy.
///
/// - `Ok(None)`: the child could not be SPAWNED — the caller falls back to
///   the standalone backend with a breadcrumb (design error table).
/// - `Ok(Some(answer))`: the tool ran; `is_error` mirrors the MCP flag.
/// - `Err(Operational)`: the channel was established and then failed —
///   missing/truncated/malformed response (exit 2; retrying standalone
///   could return a DIFFERENT answer than the daemon would have, so a
///   half-dead channel is surfaced, not papered over).
pub async fn call(
    project_root: &Path,
    tool: &str,
    arguments: serde_json::Value,
    quiet: bool,
) -> Result<Option<DaemonAnswer>, CliError> {
    let binary = proxy_binary();
    let mut child = match TokioCommand::new(&binary)
        .arg("--attach-only")
        // The proxy discovers the project root from its cwd, exactly as an
        // MCP host launching it at the workspace root would.
        .current_dir(project_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(if quiet {
            Stdio::null()
        } else {
            Stdio::inherit()
        })
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            if !quiet {
                eprintln!(
                    "code-graph: cannot spawn {} ({error}); answering standalone",
                    binary.display()
                );
            }
            return Ok(None);
        }
    };

    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let mut lines = BufReader::new(stdout).lines();

    let send = |value: serde_json::Value| {
        let mut frame = value.to_string();
        frame.push('\n');
        frame
    };

    let channel_err = |what: &str| CliError::Operational(format!("daemon channel failed: {what}"));

    // MCP initialize handshake (newline-delimited JSON-RPC over stdio).
    stdin
        .write_all(
            send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "code-graph-cli", "version": env!("CARGO_PKG_VERSION") }
                }
            }))
            .as_bytes(),
        )
        .await
        .map_err(|e| channel_err(&format!("write initialize: {e}")))?;

    let initialize = read_response(&mut lines, 1)
        .await?
        .ok_or_else(|| channel_err("no initialize response"))?;
    let server_version = initialize
        .pointer("/result/serverInfo/version")
        .and_then(|version| version.as_str())
        .map(str::to_owned);

    stdin
        .write_all(
            send(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }))
            .as_bytes(),
        )
        .await
        .map_err(|e| channel_err(&format!("write initialized: {e}")))?;

    stdin
        .write_all(
            send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": { "name": tool, "arguments": arguments }
            }))
            .as_bytes(),
        )
        .await
        .map_err(|e| channel_err(&format!("write tools/call: {e}")))?;

    let response = read_response(&mut lines, 2)
        .await?
        .ok_or_else(|| channel_err("no tools/call response"))?;

    // EOF the channel and reap the child; its exit status is meaningless
    // by contract (see module docs).
    drop(stdin);
    let _ = child.wait().await;

    if let Some(error) = response.get("error") {
        // Protocol-level JSON-RPC error (unknown tool, invalid params
        // rejected before the handler ran): the query never executed.
        return Err(CliError::Operational(format!(
            "daemon rejected the call: {error}"
        )));
    }
    let result = response
        .get("result")
        .ok_or_else(|| channel_err("response carries neither result nor error"))?;
    let payload = result
        .pointer("/content/0/text")
        .and_then(|t| t.as_str())
        .ok_or_else(|| channel_err("response carries no text content"))?
        .to_string();
    let is_error = result
        .get("isError")
        .and_then(|f| f.as_bool())
        .unwrap_or(false);

    Ok(Some(DaemonAnswer {
        payload,
        is_error,
        server_version,
    }))
}

/// Reads frames until the response with `id` arrives (skipping requests,
/// notifications, and unrelated responses). `Ok(None)` = EOF first.
async fn read_response(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    id: u64,
) -> Result<Option<serde_json::Value>, CliError> {
    loop {
        let line = lines
            .next_line()
            .await
            .map_err(|e| CliError::Operational(format!("daemon channel failed: read: {e}")))?;
        let Some(line) = line else {
            return Ok(None);
        };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            return Err(CliError::Operational(
                "daemon channel failed: malformed frame".to_string(),
            ));
        };
        if value.get("id").and_then(|v| v.as_u64()) == Some(id)
            && (value.get("result").is_some() || value.get("error").is_some())
        {
            return Ok(Some(value));
        }
    }
}
