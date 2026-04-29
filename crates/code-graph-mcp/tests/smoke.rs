//! End-to-end stdio smoke test.
//!
//! Spawns the freshly-built `code-graph-mcp` debug binary, completes the
//! `initialize` handshake, sends a `tools/list` request, and asserts that
//! the response advertises 15 tools. Phase 3.7 expands this into a full
//! wire-format snapshot suite; today the assertion is a coarse
//! compile-and-handshake gate.
//!
//! This complements the unit-level `tool_router_registers_fifteen_tools`
//! test in `codegraph-tools::server` — that test never starts the IO loop,
//! so it can't catch a regression where the macro generates 15 routes but
//! `ServerHandler::list_tools` filters them. Running both gives us
//! belt-and-braces coverage without depending on an external MCP client.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};

/// Read one line from `reader`, returning an error if EOF / IO error / the
/// stream closes before we get a newline. The rmcp stdio loop is
/// line-delimited JSON.
fn read_response(reader: &mut BufReader<std::process::ChildStdout>) -> std::io::Result<String> {
    let mut buf = String::new();
    let n = reader.read_line(&mut buf)?;
    if n == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "child closed stdout before responding",
        ));
    }
    Ok(buf)
}

#[test]
fn binary_advertises_fifteen_tools() {
    // Prefer the cargo-built path. `CARGO_BIN_EXE_<name>` is set by cargo
    // for integration tests targeting binary crates; if it's unset (e.g.
    // running this file via `rust-analyzer` directly) we fail fast with a
    // clear diagnostic rather than a generic "command not found".
    let bin = env!("CARGO_BIN_EXE_code-graph-mcp");

    let mut child = Command::new(bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn code-graph-mcp");

    let mut stdin = child.stdin.take().expect("child stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("child stdout"));

    // 1. initialize
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "smoke-test", "version": "0.1.0" }
        }
    });
    writeln!(stdin, "{init}").expect("write initialize");
    let _init_resp = read_response(&mut stdout).expect("read initialize response");

    // The MCP spec requires a notifications/initialized after the client
    // receives the initialize response.
    let initialized = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    writeln!(stdin, "{initialized}").expect("write initialized notif");

    // 2. tools/list
    let list = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    });
    writeln!(stdin, "{list}").expect("write tools/list");
    let list_resp_line = read_response(&mut stdout).expect("read tools/list response");
    let list_resp: Value =
        serde_json::from_str(&list_resp_line).expect("tools/list response is valid JSON");

    // Tear the child down before asserting so a hung child doesn't leave
    // an orphan when the assertion fails.
    drop(stdin);
    let _ = child.wait_timeout_or_kill(Duration::from_secs(2));

    let tools = list_resp
        .pointer("/result/tools")
        .and_then(|v| v.as_array())
        .expect("tools/list response has /result/tools array");

    assert_eq!(
        tools.len(),
        15,
        "tools/list must advertise 15 tools, got {}: {tools:?}",
        tools.len(),
    );

    // Names sanity-check: every expected tool name appears.
    let names: std::collections::HashSet<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|v| v.as_str()))
        .collect();
    for expected in [
        "analyze_codebase",
        "get_file_symbols",
        "search_symbols",
        "get_symbol_detail",
        "get_symbol_summary",
        "get_callers",
        "get_callees",
        "get_dependencies",
        "detect_cycles",
        "get_orphans",
        "get_class_hierarchy",
        "get_coupling",
        "generate_diagram",
        "watch_start",
        "watch_stop",
    ] {
        assert!(
            names.contains(expected),
            "tool {expected} missing from tools/list response; have {names:?}",
        );
    }
}

/// Tiny extension trait so the test can bound how long it waits on the
/// child without pulling in an extra dependency. Five-line implementation
/// keeps the smoke test self-contained.
trait WaitTimeout {
    fn wait_timeout_or_kill(&mut self, timeout: Duration) -> std::io::Result<()>;
}

impl WaitTimeout for std::process::Child {
    fn wait_timeout_or_kill(&mut self, timeout: Duration) -> std::io::Result<()> {
        let start = std::time::Instant::now();
        loop {
            match self.try_wait()? {
                Some(_) => return Ok(()),
                None => {
                    if start.elapsed() >= timeout {
                        let _ = self.kill();
                        let _ = self.wait();
                        return Ok(());
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        }
    }
}
