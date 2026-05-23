//! Phase 7.6 watch-mode reindex regression test for the Python parser.
//!
//! Mirrors `watch_go_reindex.rs` and `watch_rust_reindex.rs`
//! but drives the Python plugin instead of Go / Rust. The
//! point is to confirm:
//!
//!   1. The watch path's `try_reindex_file` works end-to-end against
//!      real `.py` source — same `index_lock` + parse + reconstruct +
//!      merge pipeline the watch reindex uses.
//!   2. `Graph::prune_dangling_edges` (the invariant that prevents
//!      dangling edges after a re-parse) is exercised by Python changes for
//!      BOTH edge kinds — `Inherits` AND `Calls`. When `Beta` is removed
//!      from `models.py` by a re-parse, no `adj`/`radj` entries continue
//!      to point at the removed `Beta` symbol's ID (the dangling `Calls`
//!      edge from `Delta::use_beta`), and no `Inherits` edge from `Beta`
//!      survives in `class_hierarchy("Alpha")`.
//!
//! The test calls `try_reindex_file` directly rather than going through
//! the live debouncer — same rationale as the Go/Rust watch tests:
//! deterministic assertion, no debounce-window flakiness. The
//! `watch_start`/`watch_stop` half is exercised by a second test
//! (`watch_start_stop_against_python_temp_project`) so the lifecycle is
//! covered without coupling to the per-edit debouncer timing.

use std::path::PathBuf;

use code_graph_lang::LanguageRegistry;
use code_graph_lang_python::PythonParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::handlers::query::{callers_or_callees, Direction};
use code_graph_tools::handlers::structure::get_class_hierarchy;
use code_graph_tools::handlers::symbols::{get_file_symbols, get_symbol_detail};
use code_graph_tools::handlers::watch::{
    try_reindex_file, watch_start, watch_stop, ReindexOutcome,
};
use code_graph_tools::handlers::NO_BYTE_BUDGET;
use code_graph_tools::CodeGraphServer;
use tempfile::TempDir;

mod common;
use common::first_text;

/// Fresh server with the Python parser plugin registered. Mirrors
/// `fresh_server` in `watch_go_reindex.rs` but with `PythonParser`.
fn fresh_server() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(PythonParser::new().expect("PythonParser::new")))
        .unwrap();
    CodeGraphServer::new(registry)
}

/// Seed a temp Python project: `models.py` declaring `Alpha` (base
/// class), `Beta` (single-inheritance derivative — anchors the `Inherits`
/// edge from `Beta` to `Alpha`), and `Delta` (whose `use_beta` method
/// calls `Beta()` — anchors the `Calls` edge from `Delta::use_beta` to
/// `Beta`). Returns the dir handle (kept alive by the test) and the
/// canonicalized models.py path.
///
/// The two anchor edges (`Beta -> Alpha` Inherits, `Delta::use_beta ->
/// Beta` Calls) are the load-bearing dangling-edge targets for the
/// post-edit assertions — both edge kinds must flow through
/// `Graph::prune_dangling_edges` when `Beta` is removed.
fn seed_python_project_with_alpha_beta_delta() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("TempDir");
    std::fs::write(
        dir.path().join("models.py"),
        "class Alpha:\n    def m(self): pass\n\nclass Beta(Alpha):\n    def m(self): pass\n\nclass Delta:\n    def use_beta(self):\n        Beta()\n",
    )
    .unwrap();
    let models = std::fs::canonicalize(dir.path().join("models.py")).unwrap();
    (dir, models)
}

/// Pull symbol names out of a `get_file_symbols` JSON response body.
/// The response is a `Page<SymbolResult>` envelope with the rows
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

/// Pull derived class names out of a `get_class_hierarchy` JSON response
/// body. The response is wrapped: `{hierarchy: {name, bases, derived},
/// truncated, max_nodes, total_nodes_seen}`. The tree itself lives under
/// `parsed["hierarchy"]`; `derived` is a list of `HierarchyNode` objects
/// (each with its own `name` field), not bare strings. `bases` and
/// `derived` are `omitempty` (Vec::is_empty), so leaf nodes serialize as
/// just `{ "name": ... }`.
fn derived_from(body: &str) -> Vec<String> {
    let parsed: serde_json::Value =
        serde_json::from_str(body).expect("get_class_hierarchy body must be JSON");
    parsed["hierarchy"]["derived"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|n| n["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// CRITICAL: a watch-driven reindex of a `.py`
/// file that removes a class (and removes the only call to its
/// constructor) must:
///   1. Drop the removed class symbol AND its method from the graph.
///   2. Surface the new class symbol on subsequent queries.
///   3. NOT leave any dangling `Inherits` edge with `from = "Beta"`
///      (this is the inheritance half of `Graph::prune_dangling_edges`
///      — pruning must hold for Python the same way it
///      does for C++/Rust/Go).
///   4. NOT leave any dangling `Calls` edge from `Delta::use_beta` to
///      `Beta` (the calls half — both edge kinds flow through the same
///      pruner; this asserts both halves in the same regression).
#[tokio::test]
async fn watch_python_reindex_drops_removed_class_and_no_dangling_edges() {
    let (dir, models_path) = seed_python_project_with_alpha_beta_delta();
    let server = fresh_server();

    // Initial index. `force = true` so a stale cache cannot mask a
    // regression. Same convention as the Go and Rust watch tests.
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

    let models_str = models_path.to_string_lossy().into_owned();
    let beta_id = format!("{models_str}:Beta");
    let delta_use_beta_id = format!("{models_str}:Delta::use_beta");

    // Pre-edit sanity: file symbols list contains all three classes
    // plus their methods; class_hierarchy on Alpha includes Beta as
    // derived.
    let r = get_file_symbols(
        &server.inner.graph,
        &models_str,
        false,
        true,
        None,
        None,
        false,
        NO_BYTE_BUDGET,
    );
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "pre-edit get_file_symbols must succeed: {r:?}"
    );
    let pre_names = symbol_names_from(&first_text(&r));
    for want in ["Alpha", "Beta", "Delta", "use_beta"] {
        assert!(
            pre_names.iter().any(|n| n == want),
            "pre-edit models.py must contain {want:?}; got {pre_names:?}"
        );
    }

    let r = get_class_hierarchy(&server.inner.graph, "Alpha", Some(1), None);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "pre-edit class hierarchy for Alpha must succeed: {r:?}"
    );
    let pre_derived = derived_from(&first_text(&r));
    assert!(
        pre_derived.iter().any(|n| n == "Beta"),
        "pre-edit class_hierarchy(Alpha) must include Beta as derived; \
         got {pre_derived:?}"
    );

    // Pre-edit Calls-edge sanity (7.6 carry-over): confirm that
    // `Delta::use_beta` actually has a `Calls` edge to `Beta` BEFORE we
    // remove Beta. Without this, a regression where the call was never
    // captured at all would silently pass the post-edit "no dangling Beta
    // callee" assertion below — both halves would trivially hold for the
    // wrong reason. Mirrors the Go watch test
    // (`watch_go_reindex.rs:136-151`).
    let r = callers_or_callees(
        &server.inner.graph,
        &delta_use_beta_id,
        Some(1),
        Direction::Callees,
        None,
        None,
        NO_BYTE_BUDGET,
        None,
    );
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "pre-edit get_callees(Delta::use_beta) must succeed: {r:?}"
    );
    let pre_callee_body: serde_json::Value =
        serde_json::from_str(&first_text(&r)).expect("get_callees response is JSON");
    // The callees response is a Page<CallChain> envelope.
    let pre_callee_ids: Vec<String> = pre_callee_body["results"]
        .as_array()
        .expect("results array")
        .iter()
        .filter_map(|c| c["symbol_id"].as_str().map(String::from))
        .collect();
    assert!(
        pre_callee_ids.iter().any(|t| t == &beta_id),
        "pre-edit Delta::use_beta's callees must include {beta_id} \
         (anchors the dangling-Calls-edge assertion below); got \
         {pre_callee_ids:?}"
    );

    // Edit: remove Beta entirely; remove Delta and its use_beta method;
    // add Gamma(Alpha). Alpha is left untouched. The post-edit shape
    // must:
    //   - keep Alpha (untouched)
    //   - drop Beta (and its m method)
    //   - drop Delta (and its use_beta method)
    //   - add Gamma (which inherits from Alpha)
    std::fs::write(
        &models_path,
        "class Alpha:\n    def m(self): pass\n\nclass Gamma(Alpha):\n    def m(self): pass\n",
    )
    .unwrap();

    // Drive the watch reindex directly (no debouncer wait) — same
    // determinism rationale as the Go/Rust watch tests.
    let outcome = try_reindex_file(&server.inner, &models_path, false).await;
    match outcome {
        ReindexOutcome::Reindexed => {}
        other => panic!("expected Reindexed, got {other:?}"),
    }

    // Post-edit: file symbols must contain Alpha + Gamma (and their m
    // methods), and must NOT contain Beta, Delta, or Delta::use_beta.
    let r = get_file_symbols(
        &server.inner.graph,
        &models_str,
        false,
        true,
        None,
        None,
        false,
        NO_BYTE_BUDGET,
    );
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "post-edit get_file_symbols must succeed: {r:?}"
    );
    let post_names = symbol_names_from(&first_text(&r));
    for want in ["Alpha", "Gamma"] {
        assert!(
            post_names.iter().any(|n| n == want),
            "post-edit models.py must contain {want:?}; got {post_names:?}"
        );
    }
    for forbidden in ["Beta", "Delta", "use_beta"] {
        assert!(
            !post_names.iter().any(|n| n == forbidden),
            "post-edit models.py must NOT contain {forbidden:?}; got \
             {post_names:?}"
        );
    }

    // Inheritance dangling-edge invariant: class_hierarchy(Alpha) must
    // surface Gamma as derived AND must NOT surface Beta. This is the
    // load-bearing assertion for the Inherits-edge half of the pruner.
    let r = get_class_hierarchy(&server.inner.graph, "Alpha", Some(1), None);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "post-edit class_hierarchy(Alpha) must succeed: {r:?}"
    );
    let post_derived = derived_from(&first_text(&r));
    assert!(
        post_derived.iter().any(|n| n == "Gamma"),
        "post-edit class_hierarchy(Alpha) must include Gamma; got \
         {post_derived:?}"
    );
    assert!(
        !post_derived.iter().any(|n| n == "Beta"),
        "post-edit class_hierarchy(Alpha) must NOT include the dangling \
         Beta; got {post_derived:?}"
    );

    // Inherits dangling-edge invariant — the graph-walk shape:
    // class_hierarchy("Beta") is the agent-visible probe for "is there
    // any structure pointing at Beta?". Pre-fix, Beta would survive in
    // the adjacency map (radj["Alpha"] still contains a Beta entry,
    // adj["Beta"] still contains an Alpha entry) and class_hierarchy
    // would either return Beta as a node (if `nodes` lookup succeeds)
    // or surface a stale list. Post-fix, both Beta's `nodes` entry and
    // every adj/radj entry referencing Beta are pruned, so
    // class_hierarchy("Beta") must report not-found.
    let r = get_class_hierarchy(&server.inner.graph, "Beta", Some(1), None);
    assert_eq!(
        r.is_error,
        Some(true),
        "post-edit class_hierarchy(Beta) must report not-found (Beta and \
         all its inherits-edge entries were pruned); got: {r:?}"
    );
    assert!(
        first_text(&r).starts_with("class not found: \"Beta\""),
        "expected 'class not found: \"Beta\"' wording; got {:?}",
        first_text(&r)
    );

    // Calls dangling-edge invariant — the agent-visible probe:
    // get_callees(Delta::use_beta) must NOT report a callee with
    // symbol_id = Beta. Pre-fix, the call edge survived in adj
    // (Delta::use_beta was deleted; the edge entry remained; the
    // callees query used to walk those orphaned entries). Post-fix,
    // the entire `from` symbol is gone and the public API surfaces
    // not-found for it.
    let r = callers_or_callees(
        &server.inner.graph,
        &delta_use_beta_id,
        Some(1),
        Direction::Callees,
        None,
        None,
        NO_BYTE_BUDGET,
        None,
    );
    if r.is_error == Some(true) {
        // Canonical post-fix shape: Delta::use_beta itself was deleted
        // by the reindex, so the symbol-id lookup at the start of
        // callers_or_callees fails with the standard not-found message.
        let body = first_text(&r);
        assert!(
            body.starts_with(&format!("symbol not found: {delta_use_beta_id:?}")),
            "expected 'symbol not found' for deleted Delta::use_beta; \
             got {body}"
        );
    } else {
        // Defensive branch — in case the deleted-from symbol survives
        // somehow, we still must not see Beta as a callee.
        let parsed: serde_json::Value = serde_json::from_str(&first_text(&r)).unwrap();
        // The callees response is a Page<CallChain> envelope.
        let post_callee_ids: Vec<String> = parsed["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["symbol_id"].as_str().map(String::from))
            .collect();
        assert!(
            !post_callee_ids.contains(&beta_id),
            "post-edit callees of Delta::use_beta must NOT include the \
             dangling {beta_id}; got {post_callee_ids:?}"
        );
    }

    // get_symbol_detail on the removed Beta ID must return the canonical
    // not-found wording. This is the agent-visible half of the dangling
    // bug — without the dangling-node sweep, this lookup would return a
    // result for a node that no longer existed in the index.
    let r = get_symbol_detail(&server.inner.graph, &beta_id);
    assert_eq!(r.is_error, Some(true));
    let body = first_text(&r);
    assert!(
        body.starts_with(&format!("symbol not found: {beta_id:?}")),
        "expected 'symbol not found: …' wording for removed Beta; got {body}"
    );

    // Same for the deleted Delta::use_beta method ID — agent-visible
    // confirmation that the Calls-edge `from` symbol was scrubbed.
    let r = get_symbol_detail(&server.inner.graph, &delta_use_beta_id);
    assert_eq!(r.is_error, Some(true));
    let body = first_text(&r);
    assert!(
        body.starts_with(&format!("symbol not found: {delta_use_beta_id:?}")),
        "expected 'symbol not found: …' wording for removed \
         Delta::use_beta; got {body}"
    );

    // Belt-and-suspenders: Alpha and Gamma both lookup-able post-edit.
    for id in [format!("{models_str}:Alpha"), format!("{models_str}:Gamma")] {
        let r = get_symbol_detail(&server.inner.graph, &id);
        assert!(
            r.is_error.is_none() || r.is_error == Some(false),
            "post-edit symbol detail for {id} must succeed: {r:?}"
        );
    }

    drop(dir);
}

/// Lifecycle test: `watch_start` against a Python temp project
/// must succeed, `watch_stop` must clean up. Distinct from the
/// deterministic-edit test above so a watcher-construction or shutdown
/// regression is not masked by the per-edit pipeline.
///
/// We don't drive an edit through the live debouncer here — the per-edit
/// path is exercised deterministically by
/// `watch_python_reindex_drops_removed_class_and_no_dangling_edges`
/// above. This test exists strictly to confirm `watch_start`/`watch_stop`
/// can hand off a Python-only indexed root without panicking.
#[tokio::test]
async fn watch_start_stop_against_python_temp_project() {
    let (dir, _models_path) = seed_python_project_with_alpha_beta_delta();
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
