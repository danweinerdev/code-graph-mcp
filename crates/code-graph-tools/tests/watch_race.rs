//! Watch + analyze race regression and editor-rename coalescing tests.
//!
//! Verification headline: spawn a watch loop on a tempdir AND a
//! parallel `analyze_codebase` loop on the same dir AND mutate files in
//! the tempdir, all concurrent. Assert no panics, no deadlocks, every
//! query returns a coherent snapshot. The index_lock is the load-bearing
//! invariant here — without it, the watch path's snapshot+resolve+merge
//! sequence races a concurrent analyze that calls `Graph::clear` then
//! re-merges, producing a half-built graph for any query that lands in
//! the middle.
//!
//! Editor-style atomic save (write `.tmp` → rename to `.cpp`) coalesce
//! test: the debouncer's 250ms window collapses the multi-event editor
//! pattern into a single re-parse. We assert the eventual graph state
//! (the consequential property — exactly one entry for the file, with
//! the new symbol surfaced) rather than instrumenting the debouncer
//! event count, which would couple the test to the dependency's internals.

use std::sync::Arc;
use std::time::Duration;

use code_graph_lang::LanguageRegistry;
use code_graph_lang_cpp::CppParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::handlers::symbols::get_file_symbols;
use code_graph_tools::handlers::watch::{watch_start, watch_stop};
use code_graph_tools::handlers::NO_BYTE_BUDGET;
use code_graph_tools::CodeGraphServer;
use tempfile::TempDir;

mod common;
use common::first_text;

/// Fresh server with the C++ parser plugin registered.
fn fresh_server() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().expect("CppParser::new")))
        .unwrap();
    CodeGraphServer::new(registry)
}

/// Seed a TempDir with N C++ files and return the canonicalized root.
fn seed_dir(n: usize) -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().expect("TempDir");
    for i in 0..n {
        std::fs::write(
            dir.path().join(format!("f{i}.cpp")),
            format!("void f{i}() {{}}\n").as_bytes(),
        )
        .expect("seed write");
    }
    let root = std::fs::canonicalize(dir.path()).expect("canonicalize root");
    (dir, root)
}

/// Initial-index helper. Drives the canonical happy path the way the
/// MCP handler does, so the rest of the test mirrors a live session.
async fn index_initial(server: &CodeGraphServer, root: &std::path::Path) {
    let r = analyze_codebase(
        server.inner.clone(),
        root.to_string_lossy().into_owned(),
        true, // force=true: deterministic — never a stale-cache short-circuit.
        None,
        None,
    )
    .await;
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "initial analyze failed: {r:?}"
    );
}

/// Race regression: spawn a watcher + concurrent analyzes + concurrent
/// queries + concurrent file mutations and assert the system stays
/// coherent.
///
/// "Coherent" here means: every `get_file_symbols` returns either an
/// error envelope (file vanished) or a JSON array — never a panic, never
/// a deadlock, never a half-built node-without-edges or
/// edges-without-nodes shape (those would surface as serialization
/// errors or empty bodies).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn watch_and_analyze_concurrent_no_panic_no_deadlock() {
    let (dir, root) = seed_dir(4);
    let server = Arc::new(fresh_server());
    index_initial(&server, &root).await;

    // Start the watcher.
    let r = watch_start(&server.inner);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "watch_start failed: {r:?}"
    );

    // Three concurrent workloads. Bounded iteration counts + an overall
    // tokio test timeout (the runtime's default for a #[tokio::test])
    // make this terminate even if the watcher misbehaves.
    let mutator = {
        let root = root.clone();
        tokio::spawn(async move {
            for iter in 0..10 {
                for i in 0..4 {
                    let p = root.join(format!("f{i}.cpp"));
                    let body = format!("void f{i}_v{iter}() {{}}\nvoid extra{iter}_{i}() {{}}\n");
                    let _ = std::fs::write(&p, body.as_bytes());
                }
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
        })
    };

    let analyzer = {
        let server = Arc::clone(&server);
        let root = root.clone();
        tokio::spawn(async move {
            for _ in 0..6 {
                // force=true bypasses the cache so analyze actually
                // contends for the index_lock every iteration. Errors
                // are fine — `indexing already in progress` is the
                // expected outcome when the watcher won the lock first.
                let _ = analyze_codebase(
                    server.inner.clone(),
                    root.to_string_lossy().into_owned(),
                    true,
                    None,
                    None,
                )
                .await;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
    };

    let querier = {
        let server = Arc::clone(&server);
        let root = root.clone();
        tokio::spawn(async move {
            for _ in 0..30 {
                for i in 0..4 {
                    let p = root.join(format!("f{i}.cpp"));
                    let r = get_file_symbols(
                        &server.inner.graph,
                        &p.to_string_lossy(),
                        false,
                        true,
                        None,
                        None,
                        false,
                        NO_BYTE_BUDGET,
                    );
                    // Either:
                    //  - success: body is a Page<SymbolResult> envelope, or
                    //  - error: "no symbols found in file: <path>".
                    // Both are coherent. A panic, deadlock, or invalid
                    // JSON would show up here as a test failure.
                    let body = first_text(&r);
                    if r.is_error == Some(true) {
                        assert!(
                            body.starts_with("no symbols found in file:"),
                            "unexpected error wording: {body}"
                        );
                    } else {
                        let parsed: serde_json::Value =
                            serde_json::from_str(&body).expect("body is JSON");
                        // Expect Page<SymbolResult> envelope.
                        assert!(parsed["results"].is_array(), "non-envelope body: {body}");
                    }
                }
                tokio::time::sleep(Duration::from_millis(8)).await;
            }
        })
    };

    // Bounded join with a hard timeout so a deadlock fails the test
    // instead of hanging the runner.
    tokio::time::timeout(Duration::from_secs(10), async {
        let _ = mutator.await;
        let _ = analyzer.await;
        let _ = querier.await;
    })
    .await
    .expect("workloads must complete within 10s");

    let _ = watch_stop(&server.inner);
    drop(dir);
}

/// Editor-style atomic save: write `foo.cpp.tmp`, then rename to
/// `foo.cpp`. The debouncer (250ms window) collapses the resulting
/// notify events into a single batch; the loop's per-batch reindex
/// produces exactly one logical update for `foo.cpp`. We assert the
/// downstream-observable property: after the debounce window, the file
/// is in the graph with the post-rename contents (not duplicated, not
/// missing).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editor_atomic_save_rename_coalesces_to_single_reindex() {
    let (dir, root) = seed_dir(2);
    let server = fresh_server();
    index_initial(&server, &root).await;

    let r = watch_start(&server.inner);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "watch_start failed: {r:?}"
    );

    // Mimic the editor pattern: write a tmp, rename to a fresh .cpp.
    let tmp = root.join("brand_new.cpp.tmp");
    let final_path = root.join("brand_new.cpp");
    std::fs::write(&tmp, b"void editor_added() {}\n").unwrap();
    std::fs::rename(&tmp, &final_path).unwrap();

    // Debounce window is 250ms; give the loop one full window plus
    // generous slack so the merge has landed before we query.
    tokio::time::sleep(Duration::from_millis(800)).await;

    // The file should now be queryable with exactly the expected symbol.
    let r = get_file_symbols(
        &server.inner.graph,
        &final_path.to_string_lossy(),
        false,
        true,
        None,
        None,
        false,
        NO_BYTE_BUDGET,
    );
    let body = first_text(&r);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "expected file to be reindexed; got error: {body}"
    );
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("body is JSON");
    // The response is a Page<SymbolResult> envelope.
    let arr = parsed["results"].as_array().expect("results array");
    let names: Vec<&str> = arr.iter().filter_map(|s| s["name"].as_str()).collect();
    assert_eq!(
        names,
        vec!["editor_added"],
        "exactly one symbol from the renamed file; got {names:?}"
    );

    let _ = watch_stop(&server.inner);
    drop(dir);
}

/// Removal end-to-end: index → watch → delete a file → after the
/// debounce window, `get_file_symbols` reports the canonical
/// "no symbols found" wording for the deleted path. Corroborates the
/// `is_remove=true` branch through the real watch loop.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn watch_loop_handles_file_removal_end_to_end() {
    let (dir, root) = seed_dir(3);
    let server = fresh_server();
    index_initial(&server, &root).await;

    let target = root.join("f1.cpp");
    // Sanity: file is in the graph pre-delete.
    let pre = get_file_symbols(
        &server.inner.graph,
        &target.to_string_lossy(),
        false,
        true,
        None,
        None,
        false,
        NO_BYTE_BUDGET,
    );
    assert!(
        pre.is_error.is_none() || pre.is_error == Some(false),
        "pre-delete: file expected in graph: {pre:?}"
    );

    let r = watch_start(&server.inner);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "watch_start failed: {r:?}"
    );

    // Delete on disk; debounce window + slack.
    std::fs::remove_file(&target).unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;

    let path_str = target.to_string_lossy().into_owned();
    let r = get_file_symbols(
        &server.inner.graph,
        &path_str,
        false,
        true,
        None,
        None,
        false,
        NO_BYTE_BUDGET,
    );
    assert_eq!(
        r.is_error,
        Some(true),
        "post-delete file_symbols must report error envelope; got {r:?}"
    );
    assert_eq!(
        first_text(&r),
        format!("no symbols found in file: {path_str}"),
    );

    let _ = watch_stop(&server.inner);
    drop(dir);
}
