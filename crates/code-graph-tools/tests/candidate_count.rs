//! AC-57 (FR-48, D-0007): every edge-reporting tool exposes how many
//! same-named candidates competed for each edge's target, so a caller can
//! tell an unambiguous edge (1) from a contested one (N) in a single
//! response — no second query, no reasoning from the one-bit confidence
//! projection.
//!
//! One fixture drives all four surfaces: `caller` invokes `unique_helper`
//! (one candidate anywhere) and `duplicate_name` (defined in TWO files, so
//! the scope rule picks one of 2).

mod common;

use std::path::Path;

use code_graph_lang::LanguageRegistry;
use code_graph_lang_rust::RustParser;
use code_graph_tools::core;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::CodeGraphServer;
use common::first_text;
use tempfile::TempDir;

fn fixture() -> TempDir {
    let dir = TempDir::new().expect("fixture tempdir");
    std::fs::write(
        dir.path().join("main.rs"),
        "pub fn unique_helper() -> u32 {\n    1\n}\n\npub fn caller() -> u32 {\n    unique_helper() + duplicate_name()\n}\n",
    )
    .expect("write main.rs");
    std::fs::write(
        dir.path().join("a.rs"),
        "pub fn duplicate_name() -> u32 {\n    10\n}\n",
    )
    .expect("write a.rs");
    std::fs::write(
        dir.path().join("b.rs"),
        "pub fn duplicate_name() -> u32 {\n    20\n}\n",
    )
    .expect("write b.rs");
    dir
}

async fn analyzed_server(root: &Path) -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(RustParser::new().expect("RustParser::new")))
        .unwrap();
    let server = CodeGraphServer::with_vcs_registry(registry, code_graph_vcs::VcsRegistry::new());
    let r = analyze_codebase(
        server.inner.clone(),
        root.to_string_lossy().into_owned(),
        true,
        None,
        None,
    )
    .await;
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "analyze_codebase failed: {}",
        first_text(&r)
    );
    server
}

fn symbol_id(root: &Path, file: &str, name: &str) -> String {
    let root = code_graph_core::paths::canonicalize(root).expect("canonicalize fixture root");
    format!("{}:{name}", root.join(file).to_string_lossy())
}

fn payload<T: serde::Serialize>(result: core::ToolResult<T>) -> serde_json::Value {
    match result {
        Ok(core::ToolOk::Value(v)) => serde_json::to_value(&v).expect("serializes"),
        other => panic!(
            "expected a Value success, got {:?}",
            other.map(|_| "Text").map_err(|e| e.0)
        ),
    }
}

/// `get_callees` (and by the same struct, `get_callers`): the contested
/// hop reports 2, the unambiguous hop reports 1, in ONE response.
#[tokio::test]
async fn callees_distinguish_contested_from_unambiguous_hops() {
    let dir = fixture();
    let server = analyzed_server(dir.path()).await;
    let caller = symbol_id(dir.path(), "main.rs", "caller");

    let body = payload(core::query::callers_or_callees(
        &server.inner.graph,
        true,
        &caller,
        None,
        core::query::CallDirection::Callees,
        None,
        None,
        usize::MAX,
        None,
    ));
    let results = body["results"].as_array().expect("results array");
    let count_for = |name: &str| -> u64 {
        results
            .iter()
            .find(|r| r["symbol_id"].as_str().unwrap_or("").ends_with(name))
            .unwrap_or_else(|| panic!("hop for {name} present: {body}"))["candidates"]
            .as_u64()
            .expect("candidates is a number")
    };
    assert_eq!(count_for("unique_helper"), 1, "sole candidate: {body}");
    assert_eq!(
        count_for("duplicate_name"),
        2,
        "two same-named definitions competed (AC-57): {body}"
    );
}

/// `get_callers` on the contested target: the inbound hop carries the
/// same count the resolver stamped on the edge.
#[tokio::test]
async fn callers_carry_the_contested_count() {
    let dir = fixture();
    let server = analyzed_server(dir.path()).await;
    let caller = symbol_id(dir.path(), "main.rs", "caller");

    // Learn which duplicate the scope rule picked, then ask for its callers.
    let callees = payload(core::query::callers_or_callees(
        &server.inner.graph,
        true,
        &caller,
        None,
        core::query::CallDirection::Callees,
        None,
        None,
        usize::MAX,
        None,
    ));
    let chosen = callees["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| {
            r["symbol_id"]
                .as_str()
                .unwrap_or("")
                .ends_with("duplicate_name")
        })
        .expect("contested hop present")["symbol_id"]
        .as_str()
        .unwrap()
        .to_string();

    let body = payload(core::query::callers_or_callees(
        &server.inner.graph,
        true,
        &chosen,
        None,
        core::query::CallDirection::Callers,
        None,
        None,
        usize::MAX,
        None,
    ));
    assert_eq!(
        body["results"][0]["candidates"],
        serde_json::json!(2),
        "the reverse-adjacency copy carries the same contested count: {body}"
    );
}

/// `find_path`: hops mirror `entered_by`'s convention — `null` for the
/// source (no edge reached it), the traversed edge's count for every
/// later hop.
#[tokio::test]
async fn find_path_hops_carry_the_count() {
    let dir = fixture();
    let server = analyzed_server(dir.path()).await;
    let caller = symbol_id(dir.path(), "main.rs", "caller");
    let unique = symbol_id(dir.path(), "main.rs", "unique_helper");

    let body = payload(core::query::find_path(
        &server.inner.graph,
        true,
        &caller,
        &unique,
        None,
        None,
    ));
    assert_eq!(body["found"], serde_json::json!(true));
    assert_eq!(
        body["hops"][0]["candidates"],
        serde_json::Value::Null,
        "the source hop was reached by no edge: {body}"
    );
    assert_eq!(
        body["hops"][1]["candidates"],
        serde_json::json!(1),
        "the traversed edge was unambiguous: {body}"
    );
}

/// `generate_diagram` (symbol mode, edges format): call edges carry the
/// count; the caller separates 1 from 2 without leaving the response.
#[tokio::test]
async fn diagram_edges_carry_the_count() {
    let dir = fixture();
    let server = analyzed_server(dir.path()).await;
    let caller = symbol_id(dir.path(), "main.rs", "caller");

    let body = payload(core::structure::generate_diagram(
        &server.inner.graph,
        true,
        core::structure::DiagramInput {
            symbol: Some(&caller),
            file: None,
            class: None,
            depth: None,
            max_nodes: None,
            format: Some("edges"),
            styled: false,
            direction: Some("callees"),
            min_confidence: None,
        },
    ));
    let edges = body.as_array().expect("edges array");
    let count_for = |to: &str| -> u64 {
        edges
            .iter()
            .find(|e| e["to"].as_str() == Some(to))
            .unwrap_or_else(|| panic!("edge to {to} present: {body}"))["candidates"]
            .as_u64()
            .expect("candidates is a number")
    };
    assert_eq!(count_for("unique_helper"), 1, "sole candidate: {body}");
    assert_eq!(
        count_for("duplicate_name"),
        2,
        "contested pick reports the real N (AC-57): {body}"
    );
}
