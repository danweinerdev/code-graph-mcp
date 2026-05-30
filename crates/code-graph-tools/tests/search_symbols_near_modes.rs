//! `search_symbols` near-mode composition tests (Finding 4).
//!
//! Two regressions, both in the `near=true` branch:
//!
//! 1. `near_search` filtered by kind/language/namespace but NOT by
//!    `subtree`, so a fuzzy search advertised as subtree-scoped returned
//!    hits from anywhere in the graph. The handler didn't even pass the
//!    resolved subtree into `near_search`.
//! 2. The `count_only` short-circuit ran BEFORE the near branch, so
//!    `near=true,count_only=true` silently returned a regex/substring count
//!    instead of the documented incompatibility error (the tool schema says
//!    "Incompatible with count_only", and the near-branch comment claimed a
//!    rejection that no code actually performed).

use std::sync::Arc;

use code_graph_lang::LanguageRegistry;
use code_graph_lang_cpp::CppParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::handlers::symbols::{search_symbols, SearchSymbolsInput};
use code_graph_tools::handlers::NO_BYTE_BUDGET;
use code_graph_tools::server::ServerInner;
use code_graph_tools::CodeGraphServer;
use tempfile::TempDir;

mod common;
use common::first_text;

/// Index a TempDir holding `a/afile.cpp` and `b/bfile.cpp`, each defining a
/// `widget` free function. Returns the indexed `ServerInner` plus the
/// canonical root so tests can build absolute subtree prefixes.
async fn fixture_two_subtrees() -> (Arc<ServerInner>, std::path::PathBuf, TempDir) {
    let dir = TempDir::new().expect("TempDir");
    std::fs::create_dir(dir.path().join("a")).unwrap();
    std::fs::create_dir(dir.path().join("b")).unwrap();
    std::fs::write(dir.path().join("a/afile.cpp"), "void widget() {}\n").unwrap();
    std::fs::write(dir.path().join("b/bfile.cpp"), "void widget() {}\n").unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();

    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().expect("CppParser::new")))
        .unwrap();
    let server = CodeGraphServer::new(registry);
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
        "analyze must succeed: {r:?}"
    );
    (server.inner.clone(), root, dir)
}

/// Files recovered from a `Page<SymbolResult>` body via each record's `id`.
fn result_files(body: &str) -> Vec<String> {
    let parsed: serde_json::Value = serde_json::from_str(body).expect("search returns JSON");
    parsed["results"]
        .as_array()
        .expect("results is an array")
        .iter()
        .map(|r| {
            let id = r["id"].as_str().expect("each result carries an id");
            code_graph_core::id_to_file(id).to_string()
        })
        .collect()
}

#[tokio::test]
async fn near_search_without_subtree_finds_both_widgets() {
    let (inner, _root, _dir) = fixture_two_subtrees().await;
    // Baseline: a fuzzy query with no subtree sees both widgets. This pins
    // that the two-symbol fixture really is fuzzy-reachable, so the scoped
    // test below is measuring the subtree filter and not an empty result.
    let r = search_symbols(
        &inner.graph,
        SearchSymbolsInput {
            query: Some("widgrt"), // 1 edit from "widget"
            near: true,
            brief: true,
            ..Default::default()
        },
        NO_BYTE_BUDGET,
    );
    let files = result_files(&first_text(&r));
    assert_eq!(
        files.len(),
        2,
        "fuzzy 'widgrt' must match both widgets without a subtree; got: {files:?}"
    );
}

#[tokio::test]
async fn near_search_honors_subtree_filter() {
    let (inner, root, _dir) = fixture_two_subtrees().await;
    let subtree = root.join("a");
    let subtree_str = subtree.to_string_lossy().into_owned();
    let r = search_symbols(
        &inner.graph,
        SearchSymbolsInput {
            query: Some("widgrt"),
            near: true,
            subtree: Some(&subtree_str),
            brief: true,
            ..Default::default()
        },
        NO_BYTE_BUDGET,
    );
    let files = result_files(&first_text(&r));
    assert_eq!(
        files.len(),
        1,
        "near + subtree=<root>/a must return only the widget under /a; got: {files:?}"
    );
    assert!(
        files[0].contains(&format!(
            "{}a{}afile.cpp",
            std::path::MAIN_SEPARATOR,
            std::path::MAIN_SEPARATOR
        )),
        "the surviving hit must be the /a widget; got: {files:?}"
    );
}

#[tokio::test]
async fn near_with_count_only_is_rejected() {
    let (inner, _root, _dir) = fixture_two_subtrees().await;
    let r = search_symbols(
        &inner.graph,
        SearchSymbolsInput {
            query: Some("widget"),
            near: true,
            count_only: true,
            brief: true,
            ..Default::default()
        },
        NO_BYTE_BUDGET,
    );
    assert_eq!(
        r.is_error,
        Some(true),
        "near + count_only must be a tool error (schema: 'Incompatible with count_only')"
    );
    let body = first_text(&r);
    assert!(
        body.contains("count_only"),
        "the error must name the offending combination; got: {body:?}"
    );
}
