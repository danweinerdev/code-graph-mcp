//! Phase 2.6 watch-mode reindex regression test for the C# parser.
//!
//! Mirrors `watch_python_reindex.rs` (Phase 7.6) and the analogous Go /
//! Rust tests but drives the C# plugin instead. The point is to confirm:
//!
//!   1. The watch path's `try_reindex_file` works end-to-end against
//!      real `.cs` source — same `index_lock` + parse + reconstruct +
//!      merge pipeline that ships in Phase 4.2.
//!   2. `Graph::prune_dangling_edges` (the invariant that closed the
//!      Phase 4.2 dangling-edge bug) is exercised by C# changes for
//!      BOTH edge kinds — `Inherits` AND `Calls`. When `Beta` is
//!      removed from `Models.cs` by a re-parse, no `adj`/`radj` entries
//!      continue to point at the removed `Beta` symbol's ID (the
//!      dangling `Calls` edge from `Delta::UseBeta`), and no `Inherits`
//!      edge from `Beta` survives in `class_hierarchy("Alpha")`.
//!   3. **Partial-class lifecycle** (Decision 3 discriminator): two
//!      `partial class Foo` declarations across two files produce two
//!      Class symbols. Removing one file leaves one Class symbol with
//!      only the surviving file's methods; the removed file's methods
//!      are pruned. This is the load-bearing C# discriminator that the
//!      other plugins do not exercise.
//!
//! Per CLAUDE.md test conventions, each test uses the **diagnostic-
//! sentinel-before-discriminator** pattern: a low-stakes baseline
//! assertion fires first ("a no-partial class extracts at all") whose
//! failure message names the most likely root cause (file write didn't
//! land, debounce window too short, registry dispatch missed `.cs`).
//! Only after the sentinel passes does the discriminator assertion run
//! (the partial-class lifecycle, the Inherits-edge prune, etc.).
//!
//! The tests call `try_reindex_file` directly rather than going through
//! the live debouncer — same rationale as the Python/Go/Rust watch
//! tests: deterministic assertion, no debounce-window flakiness.

use std::path::PathBuf;

use code_graph_lang::LanguageRegistry;
use code_graph_lang_csharp::CSharpParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::handlers::query::{callers_or_callees, Direction};
use code_graph_tools::handlers::structure::get_class_hierarchy;
use code_graph_tools::handlers::symbols::{get_file_symbols, get_symbol_detail};
use code_graph_tools::handlers::watch::{
    try_reindex_file, watch_start, watch_stop, ReindexOutcome,
};
use code_graph_tools::CodeGraphServer;
use tempfile::TempDir;

mod common;
use common::first_text;

/// Fresh server with the C# parser plugin registered. Mirrors
/// `fresh_server` in `watch_python_reindex.rs` but with `CSharpParser`.
fn fresh_server() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CSharpParser::new().expect("CSharpParser::new")))
        .unwrap();
    CodeGraphServer::new(registry)
}

/// Seed a temp C# project: `Models.cs` declaring `Alpha` (base class),
/// `Beta : Alpha` (single-inheritance derivative — anchors the
/// `Inherits` edge from `Beta` to `Alpha`), and `Delta` (whose
/// `UseBeta` method calls `new Beta()` — anchors the `Calls` edge from
/// `Delta::UseBeta` to `Beta`). Returns the dir handle (kept alive by
/// the test) and the canonicalized Models.cs path.
fn seed_csharp_project_with_alpha_beta_delta() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("TempDir");
    std::fs::write(
        dir.path().join("Models.cs"),
        "namespace App\n\
{\n\
    public class Alpha { public void M() { } }\n\
    public class Beta : Alpha { public void M() { } }\n\
    public class Delta\n\
    {\n\
        public void UseBeta() { new Beta(); }\n\
    }\n\
}\n",
    )
    .unwrap();
    let models = std::fs::canonicalize(dir.path().join("Models.cs")).unwrap();
    (dir, models)
}

/// Seed a temp C# project with two partial-class declarations of
/// `Foo` plus a no-partial sentinel class `Sentinel`. Returns the dir
/// handle plus the two canonicalized partial-file paths AND the
/// sentinel path. The sentinel is a regular (non-partial) class in a
/// separate file — it MUST extract from the initial analyze and is
/// the no-stakes baseline asserted before any partial-class behavior.
fn seed_csharp_project_with_partials() -> (TempDir, PathBuf, PathBuf, PathBuf) {
    let dir = TempDir::new().expect("TempDir");
    std::fs::write(
        dir.path().join("Sentinel.cs"),
        "namespace App\n\
{\n\
    public class Sentinel { public void Ping() { } }\n\
}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("Foo_a.cs"),
        "namespace App\n\
{\n\
    public partial class Foo { public void A() { } }\n\
}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("Foo_b.cs"),
        "namespace App\n\
{\n\
    public partial class Foo { public void B() { } }\n\
}\n",
    )
    .unwrap();
    let sentinel = std::fs::canonicalize(dir.path().join("Sentinel.cs")).unwrap();
    let foo_a = std::fs::canonicalize(dir.path().join("Foo_a.cs")).unwrap();
    let foo_b = std::fs::canonicalize(dir.path().join("Foo_b.cs")).unwrap();
    (dir, sentinel, foo_a, foo_b)
}

/// Pull symbol names out of a `get_file_symbols` JSON response body.
/// Same shape as the Python watch test.
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
/// response body. Same shape as the Python watch test.
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

/// CRITICAL — Phase 2.6 verification: a watch-driven reindex of a
/// `.cs` file that removes a class (and removes the only call to its
/// constructor) must:
///   1. Drop the removed class symbol AND its method from the graph.
///   2. Surface the new class symbol on subsequent queries.
///   3. NOT leave any dangling `Inherits` edge with `from = "Beta"`
///      (this is the inheritance half of `Graph::prune_dangling_edges`
///      from Phase 4.2 — pruning must hold for C# the same way it
///      does for C++/Rust/Go/Python).
///   4. NOT leave any dangling `Calls` edge from `Delta::UseBeta` to
///      `Beta` (the calls half — both edge kinds flow through the
///      same pruner; this asserts both halves in the same regression).
///
/// Diagnostic-sentinel pattern (CLAUDE.md test conventions): a
/// no-stakes "Alpha extracts at all" assertion fires before the
/// load-bearing "Beta-derived-class assertion" so a debounce-window or
/// IO-race failure has a distinguishable error message.
#[tokio::test]
async fn watch_csharp_reindex_drops_removed_class_and_no_dangling_edges() {
    let (dir, models_path) = seed_csharp_project_with_alpha_beta_delta();
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
    let delta_use_beta_id = format!("{models_str}:Delta::UseBeta");

    // SENTINEL (low-stakes baseline): the no-partial, non-inherited
    // class `Alpha` must extract at all. If this fails, the rest of
    // the test cannot meaningfully diagnose anything — the most
    // likely root causes are listed in the assertion message.
    let r = get_file_symbols(&server.inner.graph, &models_str, false, true, None, None);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "pre-edit get_file_symbols must succeed: {r:?}"
    );
    let pre_names = symbol_names_from(&first_text(&r));
    assert!(
        pre_names.iter().any(|n| n == "Alpha"),
        "SENTINEL FAILED — Alpha (a no-frills C# class) is missing \
         from the initial index. Likely causes (in order): (1) the \
         CSharpParser plugin is not registered against `.cs`, (2) the \
         file write did not land before analyze_codebase ran, (3) the \
         analyze pipeline silently rejected the file. Got names: \
         {pre_names:?}"
    );

    // DISCRIMINATOR (load-bearing): pre-edit file symbols list must
    // contain all three classes plus their methods; class_hierarchy
    // on Alpha includes Beta as derived.
    for want in ["Alpha", "Beta", "Delta", "UseBeta"] {
        assert!(
            pre_names.iter().any(|n| n == want),
            "pre-edit Models.cs must contain {want:?}; got {pre_names:?}"
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

    // Pre-edit Calls-edge sanity: `Delta::UseBeta` must have a
    // `Calls` edge to `Beta` BEFORE we remove Beta. Without this
    // pre-check, a regression where the call was never captured at
    // all would silently pass the post-edit "no dangling Beta callee"
    // assertion — both halves would trivially hold for the wrong
    // reason. Mirrors the Python watch test pattern.
    let r = callers_or_callees(
        &server.inner.graph,
        &delta_use_beta_id,
        Some(1),
        Direction::Callees,
        None,
        None,
    );
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "pre-edit get_callees(Delta::UseBeta) must succeed: {r:?}"
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
        "pre-edit Delta::UseBeta's callees must include {beta_id} \
         (anchors the dangling-Calls-edge assertion below); got \
         {pre_callee_ids:?}"
    );

    // Edit: remove Beta and Delta entirely; add Gamma : Alpha. Alpha
    // is left untouched. Post-edit shape:
    //   - keep Alpha (untouched)
    //   - drop Beta (and its M method)
    //   - drop Delta (and its UseBeta method)
    //   - add Gamma (which inherits from Alpha)
    std::fs::write(
        &models_path,
        "namespace App\n\
{\n\
    public class Alpha { public void M() { } }\n\
    public class Gamma : Alpha { public void M() { } }\n\
}\n",
    )
    .unwrap();

    let outcome = try_reindex_file(&server.inner, &models_path, false).await;
    match outcome {
        ReindexOutcome::Reindexed => {}
        other => panic!("expected Reindexed, got {other:?}"),
    }

    // Post-edit: file symbols must contain Alpha + Gamma (and their
    // M methods), and must NOT contain Beta, Delta, or UseBeta.
    let r = get_file_symbols(&server.inner.graph, &models_str, false, true, None, None);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "post-edit get_file_symbols must succeed: {r:?}"
    );
    let post_names = symbol_names_from(&first_text(&r));
    for want in ["Alpha", "Gamma"] {
        assert!(
            post_names.iter().any(|n| n == want),
            "post-edit Models.cs must contain {want:?}; got {post_names:?}"
        );
    }
    for forbidden in ["Beta", "Delta", "UseBeta"] {
        assert!(
            !post_names.iter().any(|n| n == forbidden),
            "post-edit Models.cs must NOT contain {forbidden:?}; got \
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
    // get_callees(Delta::UseBeta) must NOT report a callee with
    // symbol_id = Beta. Canonical post-fix shape: Delta::UseBeta
    // itself was deleted, so the symbol-id lookup at the start of
    // callers_or_callees fails with the standard not-found message.
    let r = callers_or_callees(
        &server.inner.graph,
        &delta_use_beta_id,
        Some(1),
        Direction::Callees,
        None,
        None,
    );
    if r.is_error == Some(true) {
        let body = first_text(&r);
        assert!(
            body.starts_with(&format!("symbol not found: {delta_use_beta_id:?}")),
            "expected 'symbol not found' for deleted Delta::UseBeta; \
             got {body}"
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
            "post-edit callees of Delta::UseBeta must NOT include the \
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

    // Same for the deleted Delta::UseBeta method ID.
    let r = get_symbol_detail(&server.inner.graph, &delta_use_beta_id);
    assert_eq!(r.is_error, Some(true));
    let body = first_text(&r);
    assert!(
        body.starts_with(&format!("symbol not found: {delta_use_beta_id:?}")),
        "expected 'symbol not found: …' wording for removed \
         Delta::UseBeta; got {body}"
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

/// CRITICAL — Phase 2.6 partial-class lifecycle (Decision 3
/// discriminator):
///
///   1. Initial state: two `partial class Foo` declarations in two
///      files. The graph contains TWO Class symbols both named `Foo`,
///      one per file. Each partial's method (`A` in Foo_a.cs, `B` in
///      Foo_b.cs) lives under its own file's Class symbol.
///   2. **Mid-run add**: write a third partial declaration to
///      Foo_c.cs. Reindex picks it up — graph now has THREE `Foo`
///      Class symbols plus a new method `C`.
///   3. **Mid-run remove**: delete Foo_b.cs. The watch path's
///      `is_remove = true` branch fires `Graph::prune_dangling_edges`
///      against the deleted file's symbol set. Post-prune, the graph
///      has TWO `Foo` Class symbols (from a and c) and the method
///      `B` is gone — but methods `A` and `C` survive intact.
///
/// Sentinel-before-discriminator pattern: the no-partial `Sentinel`
/// class is asserted to exist before any partial-class lifecycle step
/// — so a "the watch reindex pipeline is completely broken for C#"
/// failure mode is distinguishable from "the partial-class logic is
/// broken".
#[tokio::test]
async fn watch_csharp_partial_class_lifecycle_add_and_remove() {
    let (dir, sentinel_path, foo_a_path, foo_b_path) = seed_csharp_project_with_partials();
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

    // SENTINEL: the no-partial Sentinel class must extract. If this
    // fails, the partial-class assertions cannot meaningfully
    // diagnose anything.
    let sentinel_str = sentinel_path.to_string_lossy().into_owned();
    let r = get_file_symbols(&server.inner.graph, &sentinel_str, false, true, None, None);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "pre-edit get_file_symbols(Sentinel.cs) must succeed: {r:?}"
    );
    let sentinel_names = symbol_names_from(&first_text(&r));
    assert!(
        sentinel_names.iter().any(|n| n == "Sentinel"),
        "SENTINEL FAILED — the no-partial `Sentinel` class is missing \
         from the initial index. Likely causes: (1) CSharpParser plugin \
         is not registered against `.cs`, (2) analyze_codebase silently \
         rejected the file, (3) Sentinel.cs write did not land before \
         analyze. Got names: {sentinel_names:?}. Until the sentinel \
         passes, the partial-class lifecycle assertions below cannot \
         meaningfully diagnose Decision 3 behavior."
    );

    // DISCRIMINATOR step 1 — initial partial-class state. Each file
    // contributes its own Class symbol named `Foo`.
    let foo_a_str = foo_a_path.to_string_lossy().into_owned();
    let foo_b_str = foo_b_path.to_string_lossy().into_owned();

    let r = get_file_symbols(&server.inner.graph, &foo_a_str, false, true, None, None);
    let names_a = symbol_names_from(&first_text(&r));
    assert!(
        names_a.iter().any(|n| n == "Foo") && names_a.iter().any(|n| n == "A"),
        "Foo_a.cs must contain Foo and method A; got {names_a:?}"
    );

    let r = get_file_symbols(&server.inner.graph, &foo_b_str, false, true, None, None);
    let names_b = symbol_names_from(&first_text(&r));
    assert!(
        names_b.iter().any(|n| n == "Foo") && names_b.iter().any(|n| n == "B"),
        "Foo_b.cs must contain Foo and method B; got {names_b:?}"
    );

    // DISCRIMINATOR step 2 — mid-run add: write a third partial
    // declaration to Foo_c.cs and reindex.
    let foo_c_path = dir.path().join("Foo_c.cs");
    std::fs::write(
        &foo_c_path,
        "namespace App\n\
{\n\
    public partial class Foo { public void C() { } }\n\
}\n",
    )
    .unwrap();
    let foo_c_path = std::fs::canonicalize(&foo_c_path).unwrap();
    let outcome = try_reindex_file(&server.inner, &foo_c_path, false).await;
    match outcome {
        ReindexOutcome::Reindexed => {}
        other => panic!("expected Reindexed for Foo_c.cs add, got {other:?}"),
    }
    let foo_c_str = foo_c_path.to_string_lossy().into_owned();

    let r = get_file_symbols(&server.inner.graph, &foo_c_str, false, true, None, None);
    let names_c = symbol_names_from(&first_text(&r));
    assert!(
        names_c.iter().any(|n| n == "Foo") && names_c.iter().any(|n| n == "C"),
        "post-add Foo_c.cs must contain Foo and method C; got {names_c:?}"
    );

    // The original two partials must still be visible — adding the
    // third does not perturb them.
    let foo_a_method_id = format!("{foo_a_str}:Foo::A");
    let r = get_symbol_detail(&server.inner.graph, &foo_a_method_id);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "post-add Foo::A from Foo_a.cs must still resolve: {r:?}"
    );

    // DISCRIMINATOR step 3 — mid-run remove: delete Foo_b.cs and
    // reindex with `is_remove = true`. The b file's Class+Method
    // symbols are pruned; `A` and `C` survive.
    std::fs::remove_file(&foo_b_path).unwrap();
    let outcome = try_reindex_file(&server.inner, &foo_b_path, true).await;
    match outcome {
        ReindexOutcome::Reindexed => {}
        other => panic!("expected Reindexed for Foo_b.cs remove, got {other:?}"),
    }

    // Foo_b.cs is gone — get_file_symbols must surface the canonical
    // "no symbols found in file" not-found envelope. Any other shape
    // means the prune did not fully remove the file's symbols.
    let r = get_file_symbols(&server.inner.graph, &foo_b_str, false, true, None, None);
    assert_eq!(
        r.is_error,
        Some(true),
        "post-remove get_file_symbols(Foo_b.cs) must report not-found \
         (the file's symbols were pruned); got: {r:?}"
    );
    assert!(
        first_text(&r).starts_with("no symbols found in file:"),
        "expected 'no symbols found in file:' wording for removed \
         Foo_b.cs; got {:?}",
        first_text(&r)
    );

    // Method `B` (which lived only in Foo_b.cs) must be gone from the
    // graph entirely — its symbol_id no longer resolves.
    let foo_b_method_id = format!("{foo_b_str}:Foo::B");
    let r = get_symbol_detail(&server.inner.graph, &foo_b_method_id);
    assert_eq!(
        r.is_error,
        Some(true),
        "post-remove Foo::B must NOT resolve; got: {r:?}"
    );
    assert!(
        first_text(&r).starts_with(&format!("symbol not found: {foo_b_method_id:?}")),
        "expected 'symbol not found' for removed Foo::B; got {:?}",
        first_text(&r)
    );

    // Surviving partials (`A` from Foo_a.cs and `C` from Foo_c.cs)
    // must still resolve — pruning the b file's symbols must NOT
    // collateral-damage the other partials' methods.
    for id in [format!("{foo_a_str}:Foo::A"), format!("{foo_c_str}:Foo::C")] {
        let r = get_symbol_detail(&server.inner.graph, &id);
        assert!(
            r.is_error.is_none() || r.is_error == Some(false),
            "post-remove surviving partial method {id} must still \
             resolve (pruning Foo_b.cs must not collateral-damage \
             other partials): {r:?}"
        );
    }

    drop(dir);
}

/// Phase 2.6 lifecycle test: `watch_start` against a C# temp project
/// must succeed, `watch_stop` must clean up. Distinct from the
/// deterministic-edit tests above so a watcher-construction or
/// shutdown regression is not masked by the per-edit pipeline.
#[tokio::test]
async fn watch_start_stop_against_csharp_temp_project() {
    let (dir, _models_path) = seed_csharp_project_with_alpha_beta_delta();
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
