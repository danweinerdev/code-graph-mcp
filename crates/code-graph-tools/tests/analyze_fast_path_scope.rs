//! Regression tests for the cache fast-path's new-file blindness.
//!
//! The fast-path probe short-circuits `analyze_codebase(force=false)` when
//! every CACHED in-scope file is mtime-fresh. Its staleness universe is the
//! cache itself, so before the fix it never noticed files that exist on disk
//! but were never cached — two user-visible failures found by dogfooding:
//!
//! 1. A file ADDED after a full index was silently never indexed by a
//!    subsequent non-force analyze (the everyday incremental case).
//! 2. A root-scope analyze over a cache built by a SUBTREE-scoped run
//!    returned only the subtree and never indexed the rest of the tree.
//!
//! The fix makes the fast path run a discovery-only walk and fall through to
//! the slow path when any discovered file is missing from the cache. These
//! tests pin both failures end-to-end through the handler pipeline; without
//! the walk they fail with `files == 1`.

use code_graph_lang::LanguageRegistry;
use code_graph_lang_cpp::CppParser;
use code_graph_tools::handlers::analyze::{analyze_codebase, AnalyzeResult};
use code_graph_tools::CodeGraphServer;
use std::path::Path;
use tempfile::TempDir;

fn fresh_server() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().expect("CppParser::new")))
        .unwrap();
    CodeGraphServer::new(registry)
}

fn parse_result(r: &rmcp::model::CallToolResult) -> AnalyzeResult {
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "analyze_codebase returned an error: {r:?}",
    );
    let body = r
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.to_string())
        .expect("result must carry a text body");
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("AnalyzeResult must serialize as valid JSON");
    AnalyzeResult {
        files: parsed["files"].as_u64().unwrap() as u32,
        symbols: parsed["symbols"].as_u64().unwrap() as u32,
        edges: parsed["edges"].as_u64().unwrap() as u32,
        root_path: parsed["root_path"].as_str().unwrap().to_string(),
        warnings: parsed["warnings"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    }
}

async fn run_analyze(server: &CodeGraphServer, path: &Path, force: bool) -> AnalyzeResult {
    let r = analyze_codebase(
        server.inner.clone(),
        path.to_string_lossy().into_owned(),
        force,
        None,
        None,
    )
    .await;
    parse_result(&r)
}

/// Failure 1: the everyday incremental case. Index a directory, add a new
/// file, re-analyze WITHOUT force — the new file must be indexed. Before the
/// fix the fast path saw one fresh cached file, short-circuited, and the new
/// file never entered the graph.
#[tokio::test]
async fn new_file_after_full_index_is_indexed_without_force() {
    let dir = TempDir::new().unwrap();
    let root = code_graph_core::paths::canonicalize(dir.path()).unwrap();
    std::fs::write(root.join("first.cpp"), "void first() {}\n").unwrap();

    let server = fresh_server();
    let initial = run_analyze(&server, &root, false).await;
    assert_eq!(initial.files, 1, "baseline index sees the first file");

    std::fs::write(root.join("second.cpp"), "void second() {}\n").unwrap();

    let incremental = run_analyze(&server, &root, false).await;
    assert_eq!(
        incremental.files, 2,
        "a file added after indexing must be picked up by a non-force \
         analyze (fast path must fall through on uncached files)"
    );
}

/// Failure 2: the scope-expansion case (the dogfood repro). Build the cache
/// with a subtree-scoped analyze, then analyze the project root WITHOUT
/// force — the sibling subtree must be indexed. Before the fix the fast path
/// saw the cached subtree file fresh-in-scope and returned the subtree-only
/// graph as the root answer.
#[tokio::test]
async fn root_scope_expands_a_subtree_scoped_cache_without_force() {
    let dir = TempDir::new().unwrap();
    let root = code_graph_core::paths::canonicalize(dir.path()).unwrap();
    // Anchor the project root above the scoped invocation, mirroring a repo
    // whose .code-graph.toml sits at the top.
    std::fs::write(root.join(".code-graph.toml"), "[discovery]\n").unwrap();
    std::fs::create_dir(root.join("a")).unwrap();
    std::fs::create_dir(root.join("b")).unwrap();
    std::fs::write(root.join("a").join("one.cpp"), "void alpha() {}\n").unwrap();
    std::fs::write(root.join("b").join("two.cpp"), "void beta() {}\n").unwrap();

    let server = fresh_server();
    let scoped = run_analyze(&server, &root.join("a"), false).await;
    assert_eq!(scoped.files, 1, "subtree-scoped run indexes only a/");

    let widened = run_analyze(&server, &root, false).await;
    assert_eq!(
        widened.files, 2,
        "a root-scope non-force analyze over a subtree-scoped cache must \
         discover and index the sibling subtree, not echo the cached scope"
    );
}
