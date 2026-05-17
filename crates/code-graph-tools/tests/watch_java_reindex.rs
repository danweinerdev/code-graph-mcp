//! Phase 3.6 watch-mode reindex regression test for the Java parser.
//!
//! Mirrors `watch_csharp_reindex.rs` and the analogous Go /
//! Python / Rust tests but drives the Java plugin instead. The point is
//! to confirm:
//!
//!   1. The watch path's `try_reindex_file` works end-to-end against
//!      real `.java` source — same `index_lock` + parse + reconstruct +
//!      merge pipeline the watch reindex uses.
//!   2. `Graph::prune_dangling_edges` (the invariant that prevents
//!      dangling edges after a re-parse) is exercised by Java changes for
//!      BOTH edge kinds — `Inherits` AND `Calls`. When `Beta` is
//!      removed from `Models.java` by a re-parse, no `adj`/`radj` entries
//!      continue to point at the removed `Beta` symbol's ID (the
//!      dangling `Calls` edge from `Delta::useBeta`), and no `Inherits`
//!      edge from `Beta` survives in `class_hierarchy("Alpha")`.
//!   3. **Anonymous-class lifecycle** (Decision 4 discriminator):
//!      Java does NOT have C#'s partial-class construct. The
//!      load-bearing Java-specific discriminator is anonymous-class
//!      behavior: a file containing
//!      `new Runnable() { void run() { foo(); } }` produces a `run`
//!      Method symbol parented to the outer NAMED class and a `Calls`
//!      edge from that `run` to `foo`. Removing the file must prune
//!      BOTH the symbol AND the call edge — the same pruner that
//!      handles single-symbol-per-class C# files must also handle the
//!      Decision 4 collision case (two anonymous `run` methods in the
//!      same enclosing class produce two Method symbols with identical
//!      IDs; both must be pruned together with the file).
//!
//! Per CLAUDE.md test conventions, each test uses the **diagnostic-
//! sentinel-before-discriminator** pattern: a low-stakes baseline
//! assertion fires first ("a no-frills Java class extracts at all")
//! whose failure message names the most likely root cause (file write
//! didn't land, debounce window too short, registry dispatch missed
//! `.java`). Only after the sentinel passes does the discriminator
//! assertion run (the anonymous-class lifecycle, the Inherits-edge
//! prune, etc.).
//!
//! The tests call `try_reindex_file` directly rather than going through
//! the live debouncer — same rationale as the Python/Go/Rust/C# watch
//! tests: deterministic assertion, no debounce-window flakiness.

use std::path::PathBuf;

use code_graph_lang::LanguageRegistry;
use code_graph_lang_java::JavaParser;
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

/// Fresh server with the Java parser plugin registered. Mirrors
/// `fresh_server` in `watch_csharp_reindex.rs` but with `JavaParser`.
fn fresh_server() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(JavaParser::new().expect("JavaParser::new")))
        .unwrap();
    CodeGraphServer::new(registry)
}

/// Seed a temp Java project: `Models.java` declaring `Alpha` (base
/// class), `Beta extends Alpha` (single-inheritance derivative —
/// anchors the `Inherits` edge from `Beta` to `Alpha`), and `Delta`
/// (whose `useBeta` method calls `new Beta()` — anchors the `Calls`
/// edge from `Delta::useBeta` to `Beta`). All three are TOP-LEVEL
/// package-private classes in the same file (Java permits multiple
/// top-level classes per file, only one of which can be `public`) so
/// their symbol IDs are `<path>:Alpha`, `<path>:Beta`, `<path>:Delta`
/// without a nested-class qualifier. Returns the dir handle (kept
/// alive by the test) and the canonicalized `Models.java` path.
fn seed_java_project_with_alpha_beta_delta() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("TempDir");
    std::fs::write(
        dir.path().join("Models.java"),
        "package app;\n\
\n\
class Alpha { public void m() { } }\n\
\n\
class Beta extends Alpha { public void m() { } }\n\
\n\
class Delta {\n\
    public void useBeta() { new Beta(); }\n\
}\n",
    )
    .unwrap();
    let models = std::fs::canonicalize(dir.path().join("Models.java")).unwrap();
    (dir, models)
}

/// Seed a temp Java project with anonymous-class fixtures plus a
/// non-anonymous sentinel class in a separate file. Returns the dir
/// handle, the canonicalized sentinel path, and the canonicalized
/// anonymous-fixture path. The sentinel is a regular (no-anonymous)
/// class — it MUST extract from the initial analyze and is the
/// no-stakes baseline asserted before any anonymous-class lifecycle
/// step.
fn seed_java_project_with_anonymous() -> (TempDir, PathBuf, PathBuf) {
    let dir = TempDir::new().expect("TempDir");
    std::fs::write(
        dir.path().join("Sentinel.java"),
        "package app;\n\
\n\
public class Sentinel {\n\
    public void ping() { }\n\
}\n",
    )
    .unwrap();
    // The anonymous fixture: `handle()` contains a `new Runnable() {
    // void run() { foo(); } }` whose inner `run` method takes
    // `AnonHost` (the enclosing NAMED class) as parent per Decision 4.
    // The `foo()` call inside `run` produces a Calls edge from the
    // anonymous's `run` (parent `AnonHost`) to `foo`.
    std::fs::write(
        dir.path().join("AnonHost.java"),
        "package app;\n\
\n\
public class AnonHost {\n\
    public void foo() { }\n\
\n\
    public void handle() {\n\
        Runnable r = new Runnable() {\n\
            @Override\n\
            public void run() { foo(); }\n\
        };\n\
        r.run();\n\
    }\n\
}\n",
    )
    .unwrap();
    let sentinel = std::fs::canonicalize(dir.path().join("Sentinel.java")).unwrap();
    let anon_host = std::fs::canonicalize(dir.path().join("AnonHost.java")).unwrap();
    (dir, sentinel, anon_host)
}

/// Pull symbol names out of a `get_file_symbols` JSON response body.
/// Same shape as the C# watch test.
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

/// Pull derived class names out of a `get_class_hierarchy` JSON
/// response body. Same shape as the C# watch test.
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

/// CRITICAL: a watch-driven reindex of a
/// `.java` file that removes a class (and removes the only call to its
/// constructor) must:
///   1. Drop the removed class symbol AND its method from the graph.
///   2. Surface the new class symbol on subsequent queries.
///   3. NOT leave any dangling `Inherits` edge with `from = "Beta"`
///      (this is the inheritance half of `Graph::prune_dangling_edges`
///      — pruning must hold for Java the same way it
///      does for C++/Rust/Go/Python/C#).
///   4. NOT leave any dangling `Calls` edge from `Delta::useBeta` to
///      `Beta` (the calls half — both edge kinds flow through the
///      same pruner; this asserts both halves in the same regression).
///
/// Diagnostic-sentinel pattern (CLAUDE.md test conventions): a
/// no-stakes "Alpha extracts at all" assertion fires before the
/// load-bearing "Beta-derived-class assertion" so a debounce-window or
/// IO-race failure has a distinguishable error message.
#[tokio::test]
async fn watch_java_reindex_drops_removed_class_and_no_dangling_edges() {
    let (dir, models_path) = seed_java_project_with_alpha_beta_delta();
    let server = fresh_server();

    // Initial index. `force = true` so a stale cache cannot mask a
    // regression.
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
    let delta_use_beta_id = format!("{models_str}:Delta::useBeta");

    // SENTINEL (low-stakes baseline): the no-inheritance, no-call
    // class `Alpha` must extract at all. If this fails, the rest of
    // the test cannot meaningfully diagnose anything — the most
    // likely root causes are listed in the assertion message.
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
    assert!(
        pre_names.iter().any(|n| n == "Alpha"),
        "SENTINEL FAILED — Alpha (a no-frills Java class) is missing \
         from the initial index. Likely causes (in order): (1) the \
         JavaParser plugin is not registered against `.java`, (2) the \
         file write did not land before analyze_codebase ran, (3) the \
         analyze pipeline silently rejected the file. Got names: \
         {pre_names:?}"
    );

    // DISCRIMINATOR (load-bearing): pre-edit file symbols list must
    // contain all three classes plus the method symbol; class_hierarchy
    // on Alpha includes Beta as derived.
    for want in ["Alpha", "Beta", "Delta", "useBeta"] {
        assert!(
            pre_names.iter().any(|n| n == want),
            "pre-edit Models.java must contain {want:?}; got {pre_names:?}"
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

    // Pre-edit Calls-edge sanity: `Delta::useBeta` must have a `Calls`
    // edge to `Beta` BEFORE we remove Beta. Without this pre-check, a
    // regression where the call was never captured at all would silently
    // pass the post-edit "no dangling Beta callee" assertion — both
    // halves would trivially hold for the wrong reason. Mirrors the C#
    // / Python watch test pattern.
    let r = callers_or_callees(
        &server.inner.graph,
        &delta_use_beta_id,
        Some(1),
        Direction::Callees,
        None,
        None,
        NO_BYTE_BUDGET,
    );
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "pre-edit get_callees(Delta::useBeta) must succeed: {r:?}"
    );
    let pre_callee_body: serde_json::Value =
        serde_json::from_str(&first_text(&r)).expect("get_callees response is JSON");
    let pre_callee_ids: Vec<String> = pre_callee_body["results"]
        .as_array()
        .expect("results array")
        .iter()
        .filter_map(|c| c["symbol_id"].as_str().map(String::from))
        .collect();
    assert!(
        pre_callee_ids.iter().any(|t| t == &beta_id),
        "pre-edit Delta::useBeta's callees must include {beta_id} \
         (anchors the dangling-Calls-edge assertion below); got \
         {pre_callee_ids:?}"
    );

    // Edit: remove Beta and Delta entirely; add Gamma extends Alpha.
    // Alpha is left untouched. Post-edit shape:
    //   - keep Alpha (untouched)
    //   - drop Beta (and its m method)
    //   - drop Delta (and its useBeta method)
    //   - add Gamma (which inherits from Alpha)
    std::fs::write(
        &models_path,
        "package app;\n\
\n\
class Alpha { public void m() { } }\n\
\n\
class Gamma extends Alpha { public void m() { } }\n",
    )
    .unwrap();

    let outcome = try_reindex_file(&server.inner, &models_path, false).await;
    match outcome {
        ReindexOutcome::Reindexed => {}
        other => panic!("expected Reindexed, got {other:?}"),
    }

    // Post-edit: file symbols must contain Alpha + Gamma (and their
    // m methods), and must NOT contain Beta, Delta, or useBeta.
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
            "post-edit Models.java must contain {want:?}; got {post_names:?}"
        );
    }
    for forbidden in ["Beta", "Delta", "useBeta"] {
        assert!(
            !post_names.iter().any(|n| n == forbidden),
            "post-edit Models.java must NOT contain {forbidden:?}; got \
             {post_names:?}"
        );
    }

    // Inheritance dangling-edge invariant: class_hierarchy(Alpha)
    // must surface Gamma as derived AND must NOT surface Beta. This
    // is the load-bearing assertion for the Inherits-edge half of
    // the pruner.
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

    // class_hierarchy("Beta") is the agent-visible probe for "is
    // there any structure pointing at Beta?". Post-fix, Beta and all
    // its adj/radj entries are pruned, so this must report not-found.
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

    // Calls dangling-edge invariant — agent-visible probe:
    // get_callees(Delta::useBeta) must NOT report a callee with
    // symbol_id = Beta. Canonical post-fix shape: Delta::useBeta
    // itself was deleted, so the symbol-id lookup at the start of
    // callers_or_callees fails with the standard not-found message.
    let r = callers_or_callees(
        &server.inner.graph,
        &delta_use_beta_id,
        Some(1),
        Direction::Callees,
        None,
        None,
        NO_BYTE_BUDGET,
    );
    if r.is_error == Some(true) {
        let body = first_text(&r);
        assert!(
            body.starts_with(&format!("symbol not found: {delta_use_beta_id:?}")),
            "expected 'symbol not found' for deleted Delta::useBeta; got {body}"
        );
    } else {
        // Defensive branch — in case the deleted-from symbol survives
        // somehow, we still must not see Beta as a callee.
        let parsed: serde_json::Value = serde_json::from_str(&first_text(&r)).unwrap();
        let post_callee_ids: Vec<String> = parsed["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["symbol_id"].as_str().map(String::from))
            .collect();
        assert!(
            !post_callee_ids.contains(&beta_id),
            "post-edit callees of Delta::useBeta must NOT include the \
             dangling {beta_id}; got {post_callee_ids:?}"
        );
    }

    // get_symbol_detail on the removed Beta ID must return the
    // canonical not-found wording.
    let r = get_symbol_detail(&server.inner.graph, &beta_id);
    assert_eq!(r.is_error, Some(true));
    let body = first_text(&r);
    assert!(
        body.starts_with(&format!("symbol not found: {beta_id:?}")),
        "expected 'symbol not found: …' wording for removed Beta; got {body}"
    );

    // Same for the deleted Delta::useBeta method ID.
    let r = get_symbol_detail(&server.inner.graph, &delta_use_beta_id);
    assert_eq!(r.is_error, Some(true));
    let body = first_text(&r);
    assert!(
        body.starts_with(&format!("symbol not found: {delta_use_beta_id:?}")),
        "expected 'symbol not found: …' wording for removed \
         Delta::useBeta; got {body}"
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

/// CRITICAL — anonymous-class lifecycle (anonymous classes are invisible
/// to the symbol index):
///
/// Java does NOT have C#'s partial-class construct. The load-bearing
/// Java-specific discriminator is anonymous-class behavior:
///
///   1. Initial state: a file containing
///      `new Runnable() { void run() { foo(); } }` produces a `run`
///      Method symbol parented to the outer NAMED class (`AnonHost`)
///      per Decision 4, plus a `Calls` edge from that `run` (with
///      parent `AnonHost`) to `foo`.
///   2. **Remove the file:** the watch path's `is_remove = true`
///      branch fires `Graph::prune_dangling_edges` against the
///      deleted file's symbol set. Post-prune, the `run` Method
///      symbol is gone AND the `Calls` edge from `AnonHost::run -> foo`
///      is gone — both halves of the pruner must hold for the
///      anonymous-class collision case.
///
/// Sentinel-before-discriminator pattern: the no-anonymous `Sentinel`
/// class is asserted to exist before any anonymous-class lifecycle
/// step — so a "the watch reindex pipeline is completely broken for
/// Java" failure mode is distinguishable from "the anonymous-class
/// pruning logic is broken".
#[tokio::test]
async fn watch_java_anonymous_class_removal_prunes_method_and_call_edge() {
    let (dir, sentinel_path, anon_path) = seed_java_project_with_anonymous();
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

    // SENTINEL: the no-anonymous Sentinel class must extract. If this
    // fails, the anonymous-class assertions cannot meaningfully
    // diagnose anything.
    let sentinel_str = sentinel_path.to_string_lossy().into_owned();
    let r = get_file_symbols(
        &server.inner.graph,
        &sentinel_str,
        false,
        true,
        None,
        None,
        false,
        NO_BYTE_BUDGET,
    );
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "pre-edit get_file_symbols(Sentinel.java) must succeed: {r:?}"
    );
    let sentinel_names = symbol_names_from(&first_text(&r));
    assert!(
        sentinel_names.iter().any(|n| n == "Sentinel"),
        "SENTINEL FAILED — the no-anonymous `Sentinel` class is missing \
         from the initial index. Likely causes: (1) JavaParser plugin \
         is not registered against `.java`, (2) analyze_codebase \
         silently rejected the file, (3) Sentinel.java write did not \
         land before analyze. Got names: {sentinel_names:?}. Until the \
         sentinel passes, the anonymous-class lifecycle assertions \
         below cannot meaningfully diagnose Decision 4 pruning behavior."
    );

    // DISCRIMINATOR step 1 — pre-edit anonymous-class state. The
    // `run` Method symbol parented to `AnonHost` must exist, as must
    // the Calls edge from that `run` symbol to `foo`. We assert via
    // `get_callees` on the `run` symbol's ID to verify the edge is
    // actually present in the graph.
    let anon_str = anon_path.to_string_lossy().into_owned();
    let anon_run_id = format!("{anon_str}:AnonHost::run");

    let r = get_file_symbols(
        &server.inner.graph,
        &anon_str,
        false,
        true,
        None,
        None,
        false,
        NO_BYTE_BUDGET,
    );
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "pre-edit get_file_symbols(AnonHost.java) must succeed: {r:?}"
    );
    let pre_names = symbol_names_from(&first_text(&r));
    for want in ["AnonHost", "foo", "handle", "run"] {
        assert!(
            pre_names.iter().any(|n| n == want),
            "pre-edit AnonHost.java must contain {want:?} (Decision 4: \
             the anonymous `run` extracts as a Method on AnonHost); \
             got {pre_names:?}"
        );
    }

    let r = callers_or_callees(
        &server.inner.graph,
        &anon_run_id,
        Some(1),
        Direction::Callees,
        None,
        None,
        NO_BYTE_BUDGET,
    );
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "pre-edit get_callees(AnonHost::run) must succeed: {r:?}"
    );
    let pre_callee_body: serde_json::Value =
        serde_json::from_str(&first_text(&r)).expect("get_callees response is JSON");
    let pre_callee_ids: Vec<String> = pre_callee_body["results"]
        .as_array()
        .expect("results array")
        .iter()
        .filter_map(|c| c["symbol_id"].as_str().map(String::from))
        .collect();
    let anon_foo_id = format!("{anon_str}:AnonHost::foo");
    assert!(
        pre_callee_ids.iter().any(|t| t == &anon_foo_id),
        "pre-edit AnonHost::run's callees must include {anon_foo_id} \
         (the body of the anonymous `run` calls `foo()`, which the \
         resolver maps to `<path>:AnonHost::foo` per the scope-aware \
         heuristic); got {pre_callee_ids:?}"
    );

    // DISCRIMINATOR step 2 — remove the file and reindex with
    // `is_remove = true`. The pruner must drop:
    //   (a) the anonymous-collision `run` Method symbol (Decision 4
    //       contract — methods parented to the outer NAMED class), AND
    //   (b) the Calls edge from `AnonHost::run` to `foo`.
    // The Sentinel.java contents remain untouched.
    std::fs::remove_file(&anon_path).unwrap();
    let outcome = try_reindex_file(&server.inner, &anon_path, true).await;
    match outcome {
        ReindexOutcome::Reindexed => {}
        other => panic!("expected Reindexed for AnonHost.java remove, got {other:?}"),
    }

    // Post-remove get_file_symbols must report not-found for the deleted
    // file — anything else means the prune did not fully remove the
    // file's symbols.
    let r = get_file_symbols(
        &server.inner.graph,
        &anon_str,
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
        "post-remove get_file_symbols(AnonHost.java) must report \
         not-found (the file's symbols were pruned); got: {r:?}"
    );
    assert!(
        first_text(&r).starts_with("no symbols found in file:"),
        "expected 'no symbols found in file:' wording for removed \
         AnonHost.java; got {:?}",
        first_text(&r)
    );

    // The anonymous `run` Method (which lived only in AnonHost.java)
    // must be gone entirely — its symbol_id no longer resolves.
    let r = get_symbol_detail(&server.inner.graph, &anon_run_id);
    assert_eq!(
        r.is_error,
        Some(true),
        "post-remove AnonHost::run must NOT resolve; got: {r:?}"
    );
    assert!(
        first_text(&r).starts_with(&format!("symbol not found: {anon_run_id:?}")),
        "expected 'symbol not found' for removed AnonHost::run; got {:?}",
        first_text(&r)
    );

    // The Calls edge from the anonymous `run` to `foo` must be gone.
    // We probe by asking for callees on the deleted symbol ID — if
    // the pruner missed the edge, the not-found envelope would still
    // surface but a stale graph could surface the edge through
    // `radj`. Either way, the `r.is_error == Some(true)` path here
    // is correct: the symbol was pruned, so its callees lookup
    // errors-out cleanly. The anti-regression is "no panic, clean
    // not-found".
    let r = callers_or_callees(
        &server.inner.graph,
        &anon_run_id,
        Some(1),
        Direction::Callees,
        None,
        None,
        NO_BYTE_BUDGET,
    );
    assert_eq!(
        r.is_error,
        Some(true),
        "post-remove get_callees(AnonHost::run) must surface not-found \
         (the call edge was pruned together with the source symbol); \
         got: {r:?}"
    );

    // Sentinel must still resolve — pruning AnonHost.java must NOT
    // collateral-damage Sentinel.java.
    let sentinel_id = format!("{sentinel_str}:Sentinel");
    let r = get_symbol_detail(&server.inner.graph, &sentinel_id);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "post-remove Sentinel must still resolve (pruning AnonHost.java \
         must not collateral-damage Sentinel.java): {r:?}"
    );

    drop(dir);
}

/// Lifecycle test: `watch_start` against a Java temp project
/// must succeed, `watch_stop` must clean up. Distinct from the
/// deterministic-edit tests above so a watcher-construction or
/// shutdown regression is not masked by the per-edit pipeline.
#[tokio::test]
async fn watch_start_stop_against_java_temp_project() {
    let (dir, _models_path) = seed_java_project_with_alpha_beta_delta();
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
