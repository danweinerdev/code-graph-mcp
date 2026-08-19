//! AC-11 parity suite (phase 7, task 7.3): CLI machine-readable output is
//! byte-identical to the MCP tool payload for one query of each distinct
//! response SHAPE — a plain `Page` envelope, a non-`Page` tree, a
//! flattened envelope with a conditional field (exercised both with and
//! without `suggestions`, which is absent rather than empty), a dual-page
//! response with no top-level `results`, and a non-JSON body.
//!
//! The MCP side runs the REAL adapter path — the same `core::` call the
//! `#[tool]` wrapper makes, through `to_call_tool_result` — and the
//! payload text is extracted from the serialized `CallToolResult` (no
//! rmcp import: the envelope is `Serialize`, and `serde_json::to_value`
//! plus a JSON pointer reads `content[0].text` without naming the type).
//! The CLI side runs the built binary with `--json` against the same
//! cache. A divergence in either serializer fails on bytes, not shapes.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use code_graph_tools::core::{self, ToolResult};
use code_graph_tools::CodeGraphServer;
use tempfile::TempDir;

fn cli() -> &'static str {
    env!("CARGO_BIN_EXE_code-graph")
}

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(cli())
        .arg("--root")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run code-graph")
}

fn machine_stdout(root: &Path, args: &[&str]) -> String {
    let mut full = args.to_vec();
    full.push("--json");
    let output = run(root, &full);
    assert_eq!(
        output.status.code(),
        Some(0),
        "CLI invocation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    stdout
        .strip_suffix('\n')
        .map(str::to_string)
        .unwrap_or(stdout)
}

/// The MCP payload text for a typed core result, via the real adapter.
fn mcp_payload<T: serde::Serialize>(result: ToolResult<T>) -> String {
    let envelope = core::to_call_tool_result(result);
    let value = serde_json::to_value(&envelope).expect("CallToolResult serializes");
    assert_ne!(
        value.pointer("/isError").and_then(|f| f.as_bool()),
        Some(true),
        "parity fixtures must produce successes: {value}"
    );
    value
        .pointer("/content/0/text")
        .and_then(|t| t.as_str())
        .expect("payload text present")
        .to_string()
}

/// Three Rust files: a cross-file call chain (Page/dual-page/diagram
/// shapes) and a trait impl (tree shape).
fn fixture() -> TempDir {
    let dir = TempDir::new().expect("fixture tempdir");
    std::fs::write(
        dir.path().join("alpha.rs"),
        "pub fn entry_point() -> u32 {\n    helper_one() + helper_two()\n}\n\npub fn helper_one() -> u32 {\n    1\n}\n",
    )
    .expect("write alpha.rs");
    std::fs::write(
        dir.path().join("beta.rs"),
        "pub fn helper_two() -> u32 {\n    2\n}\n\npub fn leaf() -> u32 {\n    helper_two()\n}\n",
    )
    .expect("write beta.rs");
    std::fs::write(
        dir.path().join("gamma.rs"),
        "pub trait Base {}\n\npub struct Derived;\n\nimpl Base for Derived {}\n",
    )
    .expect("write gamma.rs");
    dir
}

fn canonical(root: &Path) -> PathBuf {
    code_graph_core::paths::canonicalize(root).expect("canonicalize fixture root")
}

fn symbol_id(root: &Path, file: &str, name: &str) -> String {
    format!("{}:{name}", canonical(root).join(file).to_string_lossy())
}

/// Analyze via the CLI (writes the cache), then load the same cache
/// in-process for the MCP-adapter side. Mirrors the standalone backend's
/// bootstrap: honest `indexed` from the load result, default config.
fn analyzed_pair(dir: &TempDir) -> (CodeGraphServer, usize) {
    let analyze = run(dir.path(), &["analyze-codebase"]);
    assert_eq!(
        analyze.status.code(),
        Some(0),
        "analyze failed: {}",
        String::from_utf8_lossy(&analyze.stderr)
    );

    let mut registry = code_graph_lang::LanguageRegistry::new();
    registry
        .register(Box::new(
            code_graph_lang_rust::RustParser::new().expect("RustParser::new"),
        ))
        .expect("register rust plugin");
    let server = CodeGraphServer::with_vcs_registry(registry, code_graph_vcs::VcsRegistry::new());
    let loaded = server
        .inner
        .graph
        .write()
        .load(&canonical(dir.path()))
        .expect("cache loads");
    assert!(loaded, "the CLI analyze must have written a loadable cache");
    let max_bytes = server.inner.config.read().response.max_bytes;
    (server, max_bytes)
}

/// Shape 1: a plain `Page<T>` envelope.
#[test]
fn parity_page_envelope_get_callers() {
    let dir = fixture();
    let (server, max_bytes) = analyzed_pair(&dir);
    let symbol = symbol_id(dir.path(), "beta.rs", "helper_two");

    let mcp = mcp_payload(core::query::callers_or_callees(
        &server.inner.graph,
        true,
        &symbol,
        None,
        core::query::CallDirection::Callers,
        None,
        None,
        max_bytes,
        None,
    ));
    let cli = machine_stdout(dir.path(), &["get-callers", &symbol]);
    assert_eq!(cli, mcp, "Page envelope diverged");
}

/// Shape 2: a non-`Page` tree.
#[test]
fn parity_tree_get_class_hierarchy() {
    let dir = fixture();
    let (server, _) = analyzed_pair(&dir);

    let mcp = mcp_payload(core::structure::get_class_hierarchy(
        &server.inner.graph,
        true,
        "Base",
        None,
        None,
    ));
    let cli = machine_stdout(dir.path(), &["get-class-hierarchy", "Base"]);
    assert_eq!(cli, mcp, "hierarchy tree diverged");
}

/// Shape 3: a flattened envelope with a conditional field, BOTH arms —
/// `suggestions` populated (anchored query, zero matches) and absent
/// (matching substring query). The field is absent rather than empty, so
/// each arm catches a different renderer/serializer mistake.
#[test]
fn parity_flattened_search_symbols_with_and_without_suggestions() {
    let dir = fixture();
    let (server, max_bytes) = analyzed_pair(&dir);

    let search = |query: &str| {
        mcp_payload(core::symbols::search_symbols(
            &server.inner.graph,
            true,
            core::symbols::SearchInput {
                query: Some(query),
                kind: None,
                namespace: None,
                language: None,
                subtree: None,
                limit: None,
                offset: None,
                brief: true,
                count_only: false,
                near: false,
                max_distance: None,
            },
            max_bytes,
        ))
    };

    let with_suggestions_mcp = search("^helper$");
    assert!(
        with_suggestions_mcp.contains("\"suggestions\""),
        "fixture must exercise the populated arm: {with_suggestions_mcp}"
    );
    let with_suggestions_cli = machine_stdout(dir.path(), &["search-symbols", "^helper$"]);
    assert_eq!(
        with_suggestions_cli, with_suggestions_mcp,
        "flattened envelope (suggestions present) diverged"
    );

    let without_suggestions_mcp = search("helper");
    assert!(
        !without_suggestions_mcp.contains("\"suggestions\""),
        "fixture must exercise the absent arm: {without_suggestions_mcp}"
    );
    let without_suggestions_cli = machine_stdout(dir.path(), &["search-symbols", "helper"]);
    assert_eq!(
        without_suggestions_cli, without_suggestions_mcp,
        "flattened envelope (suggestions absent) diverged"
    );
}

/// Shape 4: a dual-page response with no top-level `results`.
#[test]
fn parity_dual_page_get_coupling_both() {
    let dir = fixture();
    let (server, max_bytes) = analyzed_pair(&dir);
    let file = canonical(dir.path()).join("beta.rs");
    let file = file.to_string_lossy();

    let mcp = mcp_payload(core::structure::get_coupling(
        &server.inner.graph,
        true,
        &file,
        Some("both"),
        None,
        None,
        max_bytes,
    ));
    let cli = machine_stdout(dir.path(), &["get-coupling", &file, "--direction", "both"]);
    assert_eq!(cli, mcp, "dual-page response diverged");
}

/// Shape 5: a non-JSON body (`ToolOk::Text`) — printed verbatim, never
/// JSON-wrapped, in both front-ends.
#[test]
fn parity_non_json_generate_diagram_mermaid() {
    let dir = fixture();
    let (server, _) = analyzed_pair(&dir);
    let symbol = symbol_id(dir.path(), "alpha.rs", "entry_point");

    let mcp = mcp_payload(core::structure::generate_diagram(
        &server.inner.graph,
        true,
        core::structure::DiagramInput {
            symbol: Some(&symbol),
            file: None,
            class: None,
            depth: None,
            max_nodes: None,
            format: Some("mermaid"),
            styled: false,
            direction: None,
            min_confidence: None,
        },
    ));
    assert!(
        mcp.starts_with("graph TD"),
        "the mermaid body is raw text, not JSON: {mcp}"
    );
    let cli = machine_stdout(
        dir.path(),
        &[
            "generate-diagram",
            "--symbol",
            &symbol,
            "--format",
            "mermaid",
        ],
    );
    assert_eq!(cli, mcp, "non-JSON body diverged");
}

/// The human default renders FROM the same payload: spot-pins the
/// absent-vs-present `suggestions` contract the design calls out (a
/// renderer that assumes presence fails on one arm only).
#[test]
fn parity_human_mode_suggestions_footer_tracks_field_presence() {
    let dir = fixture();
    let (_, _) = analyzed_pair(&dir);

    let with_output = run(dir.path(), &["search-symbols", "^helper$"]);
    assert_eq!(with_output.status.code(), Some(0));
    let with_text = String::from_utf8(with_output.stdout).expect("stdout is UTF-8");
    assert!(
        with_text.contains("did you mean:"),
        "populated suggestions render the footer: {with_text}"
    );

    let without_output = run(dir.path(), &["search-symbols", "helper"]);
    assert_eq!(without_output.status.code(), Some(0));
    let without_text = String::from_utf8(without_output.stdout).expect("stdout is UTF-8");
    assert!(
        !without_text.contains("did you mean:"),
        "absent suggestions render no footer: {without_text}"
    );
    assert!(
        without_text.contains("total 2"),
        "page footer carries the envelope fields: {without_text}"
    );
}
