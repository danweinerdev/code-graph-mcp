//! Regression test for the watch-mode dangling-edge bug.
//!
//! Scenario: file `A` defines `old_fn`; file `B` calls `old_fn`. The user
//! renames `old_fn` → `new_fn` in `A` and saves. Pre-fix, the watch
//! reindex of `A` deletes `nodes["A:old_fn"]` but leaves
//! `adj["B:caller"] → "A:old_fn"` because `Graph::remove_file_unsafe`
//! only scrubs edges whose `file` equals the removed path — and B's
//! call edge was stored with `file = B`. Post-fix, `try_reindex_file`
//! computes the truly-removed-ID set (pre-existing IDs in A minus IDs
//! still produced by the freshly-parsed A) and calls
//! `Graph::prune_dangling_edges` to scrub adj/radj entries pointing at
//! those IDs.
//!
//! The test calls `try_reindex_file` directly rather than going through
//! the debouncer so the assertion is deterministic — a debounce window
//! would make this probabilistic and flaky.
//!
//! Inbound re-resolution (rebinding B's call to `A:new_fn`) is
//! intentionally out of scope — that requires
//! re-parsing B, which the watch event for A does not warrant. The
//! test accepts either:
//! - `get_callees(B::caller)` returns an empty list, or
//! - `get_callees(B::caller)` returns `A:new_fn`
//!
//! …both of which are coherent. The pre-fix shape — returning
//! `A:old_fn`, an ID no longer in `nodes` — is rejected.

use code_graph_lang::LanguageRegistry;
use code_graph_lang_cpp::CppParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::handlers::query::{callers_or_callees, Direction};
use code_graph_tools::handlers::symbols::get_symbol_detail;
use code_graph_tools::handlers::watch::{try_reindex_file, ReindexOutcome};
use code_graph_tools::handlers::NO_BYTE_BUDGET;
use code_graph_tools::CodeGraphServer;
use tempfile::TempDir;

mod common;
use common::first_text;

fn fresh_server() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().expect("CppParser::new")))
        .unwrap();
    CodeGraphServer::new(registry)
}

/// Seed a tempdir with two C++ files: `a.cpp` defines `old_fn`, `b.cpp`
/// defines `caller` which calls `old_fn`. Returns the dir handle (kept
/// alive by the test) and the canonicalized paths.
fn seed_two_file_call_graph() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = TempDir::new().expect("TempDir");
    std::fs::write(dir.path().join("a.cpp"), b"void old_fn() {}\n").unwrap();
    // B includes A's header-style declaration so the call resolves; we
    // inline a forward declaration in the same TU since the C++ plugin's
    // call resolution is name-based across the symbol index.
    std::fs::write(
        dir.path().join("b.cpp"),
        b"void old_fn();\nvoid caller() { old_fn(); }\n",
    )
    .unwrap();
    let a = std::fs::canonicalize(dir.path().join("a.cpp")).unwrap();
    let b = std::fs::canonicalize(dir.path().join("b.cpp")).unwrap();
    (dir, a, b)
}

#[tokio::test]
async fn watch_reindex_does_not_leave_dangling_cross_file_edge_after_rename() {
    let (dir, a_path, b_path) = seed_two_file_call_graph();
    let server = fresh_server();

    // Initial index. force=true keeps it deterministic.
    let r = analyze_codebase(
        server.inner.clone(),
        dir.path().to_string_lossy().into_owned(),
        true,
        None,
        None,
    )
    .await;
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "initial analyze must succeed: {r:?}"
    );

    let caller_id = format!("{}:caller", b_path.display());
    let old_fn_id = format!("{}:old_fn", a_path.display());

    // Pre-rename sanity: B's caller has A:old_fn as a callee.
    let r = callers_or_callees(
        &server.inner.graph,
        &caller_id,
        Some(1),
        Direction::Callees,
        None,
        None,
        NO_BYTE_BUDGET,
        None,
    );
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "pre-rename callees must succeed: {r:?}"
    );
    let body: serde_json::Value = serde_json::from_str(&first_text(&r)).unwrap();
    // The callees response is a Page<CallChain> envelope.
    let pre_targets: Vec<&str> = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["symbol_id"].as_str())
        .collect();
    assert!(
        pre_targets.iter().any(|t| *t == old_fn_id),
        "sanity: B::caller should call A:old_fn before rename; got {pre_targets:?}"
    );

    // Rename old_fn → new_fn in A (definition only — B is not touched).
    std::fs::write(&a_path, b"void new_fn() {}\n").unwrap();

    // Drive the watch reindex directly (no debouncer wait).
    let outcome = try_reindex_file(&server.inner, &a_path, false).await;
    match outcome {
        ReindexOutcome::Reindexed => {}
        other => panic!("expected Reindexed, got {other:?}"),
    }

    // Post-rename: B::caller's callees must NOT contain the dangling
    // A:old_fn ID. Acceptable: empty list (inbound re-resolution is out
    // of scope) OR rebound to A:new_fn (no-op forward-compatible).
    let r = callers_or_callees(
        &server.inner.graph,
        &caller_id,
        Some(1),
        Direction::Callees,
        None,
        None,
        NO_BYTE_BUDGET,
        None,
    );
    let post_text = first_text(&r);
    if r.is_error.is_none() || r.is_error == Some(false) {
        let parsed: serde_json::Value = serde_json::from_str(&post_text).unwrap();
        // The callees response is a Page<CallChain> envelope.
        let post_targets: Vec<String> = parsed["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["symbol_id"].as_str().map(String::from))
            .collect();
        assert!(
            !post_targets.contains(&old_fn_id),
            "post-rename callees of B::caller must NOT include the dangling \
             {old_fn_id}; got {post_targets:?}"
        );
    } else {
        // get_callees can also return a "symbol not found" error if the
        // caller_id itself was scrubbed — that would mean we over-pruned,
        // which is a different bug. Assert we didn't.
        panic!(
            "B::caller went missing post-rename — over-pruned? response: \
             is_error={:?}, body={post_text}",
            r.is_error
        );
    }

    // get_symbol_detail on the dangling ID must return the canonical
    // not-found wording. Pre-fix this test still passes (the node is
    // gone), but it's the agent-visible half of the bug — the dangling
    // edge promised a symbol that detail can't deliver.
    let r = get_symbol_detail(&server.inner.graph, &old_fn_id);
    assert_eq!(r.is_error, Some(true));
    let body = first_text(&r);
    assert!(
        body.starts_with(&format!("symbol not found: {old_fn_id:?}")),
        "expected 'symbol not found: …' wording for the gone ID; got {body}"
    );

    drop(dir);
}
