//! Phase 6.5 watch-mode reindex regression test for the Go parser.
//!
//! Mirrors `watch_rust_reindex.rs` (Phase 5.5) and `watch_dangling_edges.rs`
//! (Phase 4.2) but drives the Go plugin instead of Rust / C++. The point
//! is to confirm:
//!
//!   1. The watch path's `try_reindex_file` works end-to-end against
//!      real `.go` source — same `index_lock` + parse + reconstruct +
//!      merge pipeline that ships in Phase 4.2.
//!   2. `Graph::prune_dangling_edges` (the invariant that closed the
//!      Phase 4.2 dangling-edge bug) is exercised by Go changes the
//!      same way it is by C++ and Rust changes — when a method is
//!      removed from a file by a re-parse, no `adj`/`radj` entries
//!      continue to point at the removed method's symbol ID.
//!
//! The test calls `try_reindex_file` directly rather than going through
//! the live debouncer — same rationale as `watch_dangling_edges.rs` and
//! `watch_rust_reindex.rs`: deterministic assertion, no debounce-window
//! flakiness. The `watch_start`/`watch_stop` half of the brief is
//! exercised by a second test (`watch_start_stop_against_go_temp_project`)
//! so the lifecycle gets exercised even though the per-edit assertion
//! path is the deterministic one.

use std::path::PathBuf;

use code_graph_lang::LanguageRegistry;
use code_graph_lang_go::GoParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::handlers::query::{callers_or_callees, Direction};
use code_graph_tools::handlers::symbols::{get_file_symbols, get_symbol_detail};
use code_graph_tools::handlers::watch::{
    try_reindex_file, watch_start, watch_stop, ReindexOutcome,
};
use code_graph_tools::CodeGraphServer;
use tempfile::TempDir;

mod common;
use common::first_text;

/// Fresh server with the Go parser plugin registered. Mirrors
/// `fresh_server` in `watch_rust_reindex.rs` but with `GoParser`.
fn fresh_server() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(GoParser::new().expect("GoParser::new")))
        .unwrap();
    CodeGraphServer::new(registry)
}

/// Seed a temp Go project: `srv.go` with `package srv` declaring three
/// methods on `*Server` — `Alpha`, `Beta`, and `Caller` (which calls
/// `Beta` via a selector on a Server value, anchoring a cross-method
/// Calls edge so the dangling-edge invariant has something to assert
/// against). Returns the dir handle (kept alive by the test) and the
/// canonicalized srv.go path.
///
/// The directory has no `go.mod` because `analyze_codebase` walks
/// files via the `ignore` crate and parses `.go` files directly — the
/// module system is not consulted by the discovery layer.
fn seed_go_project_with_alpha_beta_caller() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("TempDir");
    std::fs::write(
        dir.path().join("srv.go"),
        b"package srv\n\
          type Server struct{}\n\
          func (s *Server) Alpha() {}\n\
          func (s *Server) Beta() {}\n\
          func (s *Server) Caller() { s.Beta() }\n",
    )
    .unwrap();
    let srv = std::fs::canonicalize(dir.path().join("srv.go")).unwrap();
    (dir, srv)
}

/// Pull symbol names out of a `get_file_symbols` JSON response body.
/// Phase 3: response is now a `Page<SymbolResult>` envelope with the rows
/// under `results`.
fn symbol_names_from(body: &str) -> Vec<String> {
    let parsed: serde_json::Value =
        serde_json::from_str(body).expect("get_file_symbols body must be JSON");
    parsed["results"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// CRITICAL — Phase 6.5 verification: a watch-driven reindex of a `.go`
/// file that removes a method (and removes the only call to it) must:
///   1. Drop the removed method symbol from the graph.
///   2. Surface the new method symbol on subsequent queries.
///   3. NOT leave any dangling `Calls` edge pointing at the removed
///      method's symbol ID (this is the `Graph::prune_dangling_edges`
///      invariant from Phase 4.2 — it must hold for the Go plugin the
///      same way it does for C++ and Rust).
#[tokio::test]
async fn watch_go_reindex_drops_removed_method_and_no_dangling_edge() {
    let (dir, srv_path) = seed_go_project_with_alpha_beta_caller();
    let server = fresh_server();

    // Initial index. `force = true` so a stale cache cannot mask a
    // regression. Same convention as the Rust watch test.
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

    let srv_str = srv_path.to_string_lossy().into_owned();
    let alpha_id = format!("{srv_str}:Server::Alpha");
    let beta_id = format!("{srv_str}:Server::Beta");
    let caller_id = format!("{srv_str}:Server::Caller");
    let gamma_id = format!("{srv_str}:Server::Gamma");

    // Pre-edit sanity: file symbols list contains all three methods (plus
    // the Server struct); Caller's callees include Beta.
    let r = get_file_symbols(&server.inner.graph, &srv_str, false, true, None, None);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "pre-edit get_file_symbols must succeed: {r:?}"
    );
    let pre_names = symbol_names_from(&first_text(&r));
    for want in ["Alpha", "Beta", "Caller", "Server"] {
        assert!(
            pre_names.iter().any(|n| n == want),
            "pre-edit srv.go must contain {want:?}; got {pre_names:?}"
        );
    }

    let r = callers_or_callees(
        &server.inner.graph,
        &caller_id,
        Some(1),
        Direction::Callees,
        None,
        None,
    );
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "pre-edit callees must succeed: {r:?}"
    );
    let body: serde_json::Value = serde_json::from_str(&first_text(&r)).unwrap();
    // Phase 3: callees response is now a Page<CallChain> envelope.
    let pre_callee_ids: Vec<String> = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["symbol_id"].as_str().map(String::from))
        .collect();
    assert!(
        pre_callee_ids.iter().any(|t| t == &beta_id),
        "pre-edit Caller's callees must include {beta_id}; got {pre_callee_ids:?}"
    );

    // Edit: remove Beta entirely; remove the call from Caller; add Gamma.
    // Alpha is left untouched. Caller becomes a no-op body so it has no
    // callees post-edit.
    std::fs::write(
        &srv_path,
        b"package srv\n\
          type Server struct{}\n\
          func (s *Server) Alpha() {}\n\
          func (s *Server) Caller() {}\n\
          func (s *Server) Gamma() {}\n",
    )
    .unwrap();

    // Drive the watch reindex directly (no debouncer wait) — same
    // determinism rationale as the Rust watch test.
    let outcome = try_reindex_file(&server.inner, &srv_path, false).await;
    match outcome {
        ReindexOutcome::Reindexed => {}
        other => panic!("expected Reindexed, got {other:?}"),
    }

    // Post-edit: file symbols must contain Alpha + Caller + Gamma + Server,
    // and must NOT contain Beta.
    let r = get_file_symbols(&server.inner.graph, &srv_str, false, true, None, None);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "post-edit get_file_symbols must succeed: {r:?}"
    );
    let post_names = symbol_names_from(&first_text(&r));
    for want in ["Alpha", "Caller", "Gamma", "Server"] {
        assert!(
            post_names.iter().any(|n| n == want),
            "post-edit srv.go must contain {want:?}; got {post_names:?}"
        );
    }
    assert!(
        !post_names.iter().any(|n| n == "Beta"),
        "post-edit srv.go must NOT contain `Beta`; got {post_names:?}"
    );

    // Dangling-edge invariant: Caller's callees must NOT include the
    // removed Beta ID. Acceptable: empty list (Caller now has no body
    // calls) — anything else (notably `Beta` returning) is a regression.
    let r = callers_or_callees(
        &server.inner.graph,
        &caller_id,
        Some(1),
        Direction::Callees,
        None,
        None,
    );
    let post_text = first_text(&r);
    if r.is_error.is_none() || r.is_error == Some(false) {
        let parsed: serde_json::Value = serde_json::from_str(&post_text).unwrap();
        // Phase 3: callees response is now a Page<CallChain> envelope.
        let post_callee_ids: Vec<String> = parsed["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["symbol_id"].as_str().map(String::from))
            .collect();
        assert!(
            !post_callee_ids.contains(&beta_id),
            "post-edit Caller's callees must NOT include the dangling \
             {beta_id}; got {post_callee_ids:?}"
        );
        // Caller's body is now empty after the edit, so the canonical
        // post-edit shape is "no callees" — but we don't enforce that
        // here because the wire format of an empty list has historically
        // taken two shapes (empty array vs. not-found error envelope).
        // The dangling-edge assertion above is the load-bearing check.
    } else {
        // get_callees returns an error envelope when the caller_id is
        // not found in the graph at all. That would mean we over-pruned
        // (Caller's symbol was scrubbed unnecessarily), which is a
        // different bug than the dangling-edge one we're guarding.
        panic!(
            "Caller went missing post-edit — over-pruned? response: \
             is_error={:?}, body={post_text}",
            r.is_error
        );
    }

    // get_symbol_detail on the removed Beta ID must return the canonical
    // not-found wording. This is the agent-visible half of the dangling
    // bug — pre-Phase-4.2 this returned a result for a node that no
    // longer existed in the index.
    let r = get_symbol_detail(&server.inner.graph, &beta_id);
    assert_eq!(r.is_error, Some(true));
    let body = first_text(&r);
    assert!(
        body.starts_with(&format!("symbol not found: {beta_id:?}")),
        "expected 'symbol not found: …' wording for removed Beta; got {body}"
    );

    // Alpha and Gamma both lookup-able. Caller is too. Belt-and-suspenders
    // for the over-prune check above.
    for id in [&alpha_id, &gamma_id, &caller_id] {
        let r = get_symbol_detail(&server.inner.graph, id);
        assert!(
            r.is_error.is_none() || r.is_error == Some(false),
            "post-edit symbol detail for {id} must succeed: {r:?}"
        );
    }

    drop(dir);
}

/// Phase 6.5 lifecycle test: `watch_start` against a Go temp project
/// must succeed, `watch_stop` must clean up. Distinct from the
/// deterministic-edit test above so a watcher-construction or shutdown
/// regression is not masked by the per-edit pipeline.
///
/// We don't drive an edit through the live debouncer here — the per-edit
/// path is exercised deterministically by
/// `watch_go_reindex_drops_removed_method_and_no_dangling_edge` above.
/// This test exists strictly to confirm `watch_start`/`watch_stop` can
/// hand off a Go-only indexed root without panicking.
#[tokio::test]
async fn watch_start_stop_against_go_temp_project() {
    let (dir, _srv_path) = seed_go_project_with_alpha_beta_caller();
    let server = fresh_server();

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

    let r = watch_start(&server.inner);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "watch_start failed: {r:?}"
    );

    let r = watch_stop(&server.inner);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "watch_stop failed: {r:?}"
    );

    // Calling watch_stop a second time must surface the canonical
    // "watch mode is not active" envelope rather than silently
    // succeeding — confirms the cleanup actually tore down the handle.
    let r = watch_stop(&server.inner);
    assert_eq!(
        r.is_error,
        Some(true),
        "second watch_stop must report error envelope: {r:?}"
    );
    assert!(
        first_text(&r).contains("watch mode is not active"),
        "expected 'watch mode is not active' wording; got {:?}",
        first_text(&r)
    );

    drop(dir);
}
