//! End-to-end integration tests for `analyze_codebase` + downstream
//! query pipelines.
//!
//! These tests focus on observable behavior at the handler boundary:
//! - Concurrent analyze_codebase: second call gets the single-flight error.
//! - Bad-path errors: nonexistent / file-instead-of-dir / empty-string.
//! - Cache lifecycle: hit on second call, force=true rebuild, stale-mtime
//!   triggers re-parse.
//!
//! They invoke the handler functions directly (not the rmcp router)
//! because Phase 3.7 already covers the wire envelope via the snapshot
//! suite. Direct invocation lets the tests assert against the
//! `CallToolResult` body without juggling stdio/JSON-RPC plumbing.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use code_graph_lang::LanguageRegistry;
use code_graph_lang_cpp::CppParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::handlers::query::get_dependencies;
use code_graph_tools::handlers::structure::detect_cycles;
use code_graph_tools::handlers::symbols::{get_file_symbols, get_symbol_summary};
use code_graph_tools::handlers::NO_BYTE_BUDGET;
use code_graph_tools::server::ServerInner;
use code_graph_tools::CodeGraphServer;
use tempfile::TempDir;

mod common;
use common::{copy_testdata, first_text};

/// Fresh server with the C++ parser plugin registered.
fn fresh_server() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().unwrap()))
        .unwrap();
    CodeGraphServer::new(registry)
}

// -------- end-to-end pipeline -------------------------------------------

#[tokio::test]
async fn analyze_then_query_pipeline() {
    // Use a per-test TempDir copy so the cache write doesn't race the
    // other tests in this file (they all hit `analyze_codebase`).
    let dir = TempDir::new().unwrap();
    copy_testdata(dir.path());
    let path = std::fs::canonicalize(dir.path()).unwrap();

    let server = fresh_server();
    let r = analyze_codebase(
        server.inner.clone(),
        path.to_string_lossy().into_owned(),
        true,
        None,
        None,
    )
    .await;
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "analyze_codebase failed: {r:?}",
    );

    // Indexed flag flips on.
    assert!(server.inner.indexed.load(Ordering::Acquire));

    // get_file_symbols on engine.cpp returns symbols.
    let engine_cpp = path.join("engine.cpp").to_string_lossy().into_owned();
    let sr = get_file_symbols(
        &server.inner.graph,
        &engine_cpp,
        false,
        true,
        None,
        None,
        false,
        NO_BYTE_BUDGET,
    );
    assert!(sr.is_error.is_none() || sr.is_error == Some(false));
    let parsed: serde_json::Value = serde_json::from_str(&first_text(&sr)).unwrap();
    // Phase 3: response is now a Page<SymbolResult> envelope.
    let arr = parsed["results"].as_array().expect("results array");
    assert!(!arr.is_empty(), "engine.cpp has at least one symbol");

    // Summary returns the `Page<SummaryRow>` envelope; assert the envelope
    // shape is present and `results` is non-empty for the indexed fixture.
    let summary = get_symbol_summary(&server.inner.graph, None, None, None, NO_BYTE_BUDGET);
    let parsed: serde_json::Value = serde_json::from_str(&first_text(&summary)).unwrap();
    let results = parsed["results"].as_array().expect("results array");
    assert!(!results.is_empty(), "indexed fixture has at least one row");

    // Dependencies returns engine.h + utils.h for engine.cpp.
    let deps = get_dependencies(&server.inner.graph, &engine_cpp);
    let parsed: serde_json::Value = serde_json::from_str(&first_text(&deps)).unwrap();
    let arr = parsed.as_array().expect("array of paths");
    assert!(
        !arr.is_empty(),
        "engine.cpp has at least one resolved include",
    );

    // detect_cycles surfaces the circular_a/circular_b cycle. Wrapped in
    // the Page<Vec<String>> envelope post-Phase 5 of the deferred-items
    // ship — the cycle is in `results[0]`, count in `total`.
    let cycles = detect_cycles(&server.inner.graph, None, None);
    let parsed: serde_json::Value = serde_json::from_str(&first_text(&cycles)).unwrap();
    let arr = parsed["results"].as_array().expect("results array");
    assert_eq!(
        arr.len(),
        1,
        "testdata/cpp has exactly one circular include cycle (circular_a/b)",
    );
    assert_eq!(
        parsed["total"].as_u64().unwrap(),
        1,
        "total reports the full cycle count",
    );
}

// -------- concurrent analyze single-flight ------------------------------

#[tokio::test]
async fn concurrent_analyze_returns_indexing_in_progress() {
    // Build two servers sharing the same `Arc<ServerInner>` so the
    // index_lock is the actual shared lock under test. (Two distinct
    // servers would each hold their own lock and the test would race
    // its way to a passing pair of Ok results.)
    let dir = TempDir::new().unwrap();
    copy_testdata(dir.path());
    let server = fresh_server();
    let inner: Arc<ServerInner> = server.inner.clone();
    let path = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let inner_a = inner.clone();
    let path_a = path.clone();
    let inner_b = inner.clone();
    let path_b = path.clone();

    // Drive both calls concurrently. The handler holds index_lock across
    // its full async path, so whichever call grabs the lock first
    // succeeds; the other immediately errors.
    let (a, b) = tokio::join!(
        async move { analyze_codebase(inner_a, path_a, true, None, None).await },
        async move { analyze_codebase(inner_b, path_b, true, None, None).await }
    );

    let a_err = a.is_error == Some(true);
    let b_err = b.is_error == Some(true);
    let errored = if a_err { &a } else { &b };
    let succeeded = if a_err { &b } else { &a };

    // Exactly one error and one success. A reverse outcome (both Ok or
    // both errors) means the single-flight gate failed.
    assert!(
        a_err ^ b_err,
        "exactly one call must error; got a_err={a_err} b_err={b_err}",
    );

    // The error must carry the single-flight wording byte-for-byte.
    let body = first_text(errored);
    assert_eq!(body, "indexing already in progress", "got: {body}");

    // The successful call returns a populated AnalyzeResult.
    let body = first_text(succeeded);
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(parsed["files"].as_u64().unwrap() > 0);
}

// -------- bad-path errors ------------------------------------------------

#[tokio::test]
async fn analyze_bad_path_returns_directory_not_found() {
    let server = fresh_server();
    let r = analyze_codebase(
        server.inner.clone(),
        "/this/does/not/exist/abc123xyz".to_string(),
        false,
        None,
        None,
    )
    .await;
    assert_eq!(r.is_error, Some(true));
    let body = first_text(&r);
    assert!(
        body.starts_with("directory does not exist:"),
        "expected 'directory does not exist:' wording, got: {body}",
    );
}

#[tokio::test]
async fn analyze_path_is_file_returns_not_a_directory() {
    // Deliberate Rust-side divergence (Phase 3.4 carry-forward): Go
    // collapses this into "directory does not exist", but Rust's
    // canonicalize already distinguishes "missing" from "file" so we
    // surface the richer wording. This test locks the wording in.
    let server = fresh_server();
    let file = common::testdata_cpp_path().join("engine.cpp");
    let r = analyze_codebase(
        server.inner.clone(),
        file.to_string_lossy().into_owned(),
        false,
        None,
        None,
    )
    .await;
    assert_eq!(r.is_error, Some(true));
    let body = first_text(&r);
    assert!(
        body.starts_with("path is not a directory:"),
        "expected 'path is not a directory:' wording, got: {body}",
    );
}

#[tokio::test]
async fn analyze_empty_path_returns_path_required() {
    let server = fresh_server();
    let r = analyze_codebase(server.inner.clone(), String::new(), false, None, None).await;
    assert_eq!(r.is_error, Some(true));
    assert_eq!(first_text(&r), "'path' is required");
}

// -------- cache lifecycle ------------------------------------------------

#[tokio::test]
async fn cache_hit_on_second_analyze_no_force() {
    // Use a temp copy so we don't pollute the shared testdata cache.
    let dir = TempDir::new().unwrap();
    copy_testdata(dir.path());

    let server = fresh_server();
    let path = dir.path().to_string_lossy().into_owned();

    // First call: full re-index (cache absent); writes the cache file.
    let _ = analyze_codebase(server.inner.clone(), path.clone(), false, None, None).await;
    let cache = dir.path().join(".code-graph-cache.json");
    assert!(cache.exists(), "first analyze must write the cache");
    let mtime_after_first = std::fs::metadata(&cache).unwrap().modified().unwrap();

    // Sleep > 1s so a re-write would change the modified-time at the
    // resolution most filesystems support. (No-op if the cache fast
    // path takes the cached graph and skips the save.)
    std::thread::sleep(Duration::from_millis(1100));

    // Second call: cache hit, no force, no stale paths → must NOT
    // rewrite the cache file.
    let r2 = analyze_codebase(server.inner.clone(), path.clone(), false, None, None).await;
    assert!(r2.is_error.is_none() || r2.is_error == Some(false));

    let mtime_after_second = std::fs::metadata(&cache).unwrap().modified().unwrap();
    assert_eq!(
        mtime_after_first, mtime_after_second,
        "cache file must not be rewritten on a clean cache hit",
    );

    // The result body must still report a populated graph.
    let body = first_text(&r2);
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(parsed["files"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn force_skips_cache_load() {
    let dir = TempDir::new().unwrap();
    copy_testdata(dir.path());

    let server = fresh_server();
    let path = dir.path().to_string_lossy().into_owned();

    let _ = analyze_codebase(server.inner.clone(), path.clone(), false, None, None).await;
    let r2 = analyze_codebase(server.inner.clone(), path, true, None, None).await;
    assert!(
        r2.is_error.is_none() || r2.is_error == Some(false),
        "force=true must succeed: {r2:?}",
    );
    let body = first_text(&r2);
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    // Even with force=true, the result body shape is the same.
    assert!(parsed["files"].as_u64().unwrap() > 0);
    assert!(parsed["symbols"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn stale_mtime_invalidates_cached_file() {
    // Stale-mtime test: after analyze writes the cache, bump a tracked
    // file's mtime. The next analyze (no force) must take the
    // stale-paths branch and re-index without erroring.
    let dir = TempDir::new().unwrap();
    copy_testdata(dir.path());

    let server = fresh_server();
    let path = dir.path().to_string_lossy().into_owned();
    let _ = analyze_codebase(server.inner.clone(), path.clone(), false, None, None).await;

    // Bump engine.cpp's mtime to ~2 seconds in the future. File::set_modified
    // sidesteps low-resolution fs timestamps deterministically.
    let target = dir.path().join("engine.cpp");
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(&target)
        .unwrap();
    let bumped = SystemTime::now()
        .checked_add(Duration::from_secs(2))
        .unwrap();
    f.set_modified(bumped).unwrap();
    drop(f);

    let r2 = analyze_codebase(server.inner.clone(), path, false, None, None).await;
    assert!(
        r2.is_error.is_none() || r2.is_error == Some(false),
        "analyze must succeed after stale-mtime invalidation: {r2:?}",
    );
    let body = first_text(&r2);
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    // Counts must match the testdata baseline (8 files, 18 symbols).
    assert_eq!(parsed["files"], serde_json::json!(8));
    assert_eq!(parsed["symbols"], serde_json::json!(18));
}
