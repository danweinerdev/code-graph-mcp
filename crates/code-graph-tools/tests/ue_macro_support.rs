//! UE-style macro extraction end-to-end tests — `UeMacroSupport` Phase 4.2 + 4.3.
//!
//! These tests close the "config flows from `.code-graph.toml` through
//! `analyze_codebase` into the C++ plugin's parameterized-macro substitution"
//! gap for the UE-targeted `[cpp].macro_strip_with_args` field. The closest
//! existing pattern lives at `tests/cpp_macro_strip.rs`, which exercises the
//! bare-word `[cpp].macro_strip` field; the harness here mirrors it but
//! drives the `macro_strip_with_args` path against a more realistic UE-style
//! fixture (`Object.h` / `Actor.h` / `ActorComponent.h`).
//!
//! Test 1 (`ue_fixture_extracts_uclass_with_preset`): asserts the preset
//! enables extraction — `AActor`, `UObject`, `UActorComponent` all show up,
//! the diamond `UObject -> {AActor, UActorComponent}` materializes, and the
//! `Tick` method's source-line is preserved via the byte-offset-preserving
//! substitution invariant.
//!
//! Test 2 (`ue_fixture_no_config_extracts_zero_aactor_symbols`): pins
//! today's broken behavior — with `macro_strip_with_args = []` the
//! UE-parameterized macros leave the class declarations unparseable and
//! `AActor`/`UObject` extract zero symbols. The test's PASSING documents
//! the unfixed-state baseline; its eventual FAILURE would be the marker
//! that a better fix (e.g. a parser upgrade) made the preset unnecessary.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use code_graph_lang::LanguageRegistry;
use code_graph_lang_cpp::CppParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::handlers::structure::get_class_hierarchy;
use code_graph_tools::handlers::symbols::{get_file_symbols, search_symbols, SearchSymbolsInput};
use code_graph_tools::handlers::NO_BYTE_BUDGET;
use code_graph_tools::server::ServerInner;
use code_graph_tools::CodeGraphServer;
use tempfile::TempDir;

mod common;
use common::first_text;

/// Fresh server with only the C++ parser plugin registered. Mirrors
/// `tests/cpp_macro_strip.rs::fresh_server`.
fn fresh_server() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().expect("CppParser::new")))
        .expect("register CppParser");
    CodeGraphServer::new(registry)
}

/// In-tree fixture source: `crates/code-graph-tools/tests/fixtures/ue_minimal/`.
/// Canonicalized so symlinked tempdirs on some CI hosts don't surprise
/// the copy step.
fn fixture_src() -> PathBuf {
    let raw = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("ue_minimal");
    std::fs::canonicalize(&raw)
        .unwrap_or_else(|e| panic!("canonicalize {raw:?} failed: {e}; fixture must exist"))
}

/// Copy the four fixture files (`Object.h`, `Actor.h`, `ActorComponent.h`,
/// `.code-graph.toml`) into `dest`. Mirrors `common::copy_testdata_from`
/// but inlined here so the test crate doesn't grow a new shared helper for
/// a single caller.
fn copy_fixture_files(dest: &Path) {
    let src = fixture_src();
    for name in [
        "Object.h",
        "Actor.h",
        "ActorComponent.h",
        ".code-graph.toml",
    ] {
        let from = src.join(name);
        let to = dest.join(name);
        std::fs::copy(&from, &to).unwrap_or_else(|e| panic!("copy {:?} -> {:?}: {e}", from, to));
    }
}

/// Build a tempdir holding the UE fixture (optionally overwriting the
/// `.code-graph.toml` with `override_toml`), run `analyze_codebase` against
/// it, and return the indexed server plus the canonical paths the test will
/// query. The tempdir is kept alive via the `_dir` field so the caller's
/// borrow stays valid for the duration of the test.
struct Indexed {
    _dir: TempDir,
    inner: Arc<ServerInner>,
    root: PathBuf,
}

async fn build_indexed(override_toml: Option<&str>) -> Indexed {
    let dir = TempDir::new().expect("TempDir for UE fixture");

    // Stage the fixture into a fresh tempdir so concurrent tests don't
    // race on the shared `.code-graph-cache.json` write — same isolation
    // pattern as `tests/path_normalization.rs`.
    copy_fixture_files(dir.path());

    if let Some(toml) = override_toml {
        std::fs::write(dir.path().join(".code-graph.toml"), toml)
            .expect("override .code-graph.toml");
    }

    // Canonicalize so the indexed root matches what the indexer stores:
    // tempdir paths on some platforms contain `/tmp/.tmpXXXXXX` symlinks;
    // resolving them here keeps per-file paths in the graph aligned with
    // the path strings we hand to the query handlers.
    let root = std::fs::canonicalize(dir.path()).expect("canonicalize tempdir");

    let server = fresh_server();

    // force=true so the assertion can't be masked by a stale cache.
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
        "analyze_codebase failed: {r:?}",
    );

    Indexed {
        _dir: dir,
        inner: server.inner.clone(),
        root,
    }
}

/// `search_symbols` thin wrapper: anchored regex on `query`, no other
/// filters. Returns the parsed JSON envelope so each test can drive its
/// assertions against `total` plus the `results` array.
fn search_for(inner: &Arc<ServerInner>, pattern: &str) -> serde_json::Value {
    let r = search_symbols(
        &inner.graph,
        SearchSymbolsInput {
            query: Some(pattern),
            // Compact records keep `line` populated for the byte-offset
            // preservation assertion below.
            brief: true,
            ..Default::default()
        },
        NO_BYTE_BUDGET,
    );
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "search_symbols({pattern:?}) returned error: {r:?}",
    );
    serde_json::from_str(&first_text(&r))
        .unwrap_or_else(|e| panic!("search_symbols({pattern:?}) body must be JSON: {e}"))
}

/// Phase 4.2 — preset-on integration test.
///
/// Indexes the in-tree UE fixture with its bundled `.code-graph.toml`
/// (which enables `[cpp].macro_strip` for the bare `_API` macros AND
/// `[cpp].macro_strip_with_args` for the parameterized UE macros
/// `UCLASS(...)`, `UFUNCTION(...)`, `UPROPERTY(...)`, `GENERATED_BODY()`,
/// etc.) and asserts six properties:
///   (a) `^AActor$` returns total >= 1 with a `Class` result;
///   (b) `^UObject$` returns total >= 1;
///   (c) `^UActorComponent$` returns total >= 1;
///   (d) `get_class_hierarchy("UObject")` derives BOTH `AActor` and
///       `UActorComponent` (the diamond);
///   (e) `^Tick$` finds `AActor::Tick` whose `line` matches the source
///       position in `Actor.h` (byte-offset preservation invariant —
///       parameterized-macro substitution must overwrite with same-length
///       spaces);
///   (f) `get_file_symbols(<root>/Actor.h)` returns both `AActor` and
///       `Tick`.
#[tokio::test]
async fn ue_fixture_extracts_uclass_with_preset() {
    let fx = build_indexed(None).await;

    // ---------- (a) AActor extracts as Class -----------------------------
    let envelope = search_for(&fx.inner, "^AActor$");
    let aactor_total = envelope["total"].as_u64().unwrap_or(0);
    assert!(
        aactor_total >= 1,
        "expected search_symbols(\"^AActor$\") total >= 1 with the UE preset; got envelope: {envelope}",
    );
    let aactor_kind = envelope["results"]
        .as_array()
        .and_then(|arr| arr.iter().find(|s| s["name"].as_str() == Some("AActor")))
        .and_then(|s| s["kind"].as_str().map(String::from));
    assert_eq!(
        aactor_kind.as_deref(),
        Some("class"),
        "expected AActor to be a Class kind result; got envelope: {envelope}",
    );

    // ---------- (b) UObject extracts -------------------------------------
    let envelope = search_for(&fx.inner, "^UObject$");
    let uobject_total = envelope["total"].as_u64().unwrap_or(0);
    assert!(
        uobject_total >= 1,
        "expected search_symbols(\"^UObject$\") total >= 1 with the UE preset; got envelope: {envelope}",
    );

    // ---------- (c) UActorComponent extracts -----------------------------
    let envelope = search_for(&fx.inner, "^UActorComponent$");
    let uac_total = envelope["total"].as_u64().unwrap_or(0);
    assert!(
        uac_total >= 1,
        "expected search_symbols(\"^UActorComponent$\") total >= 1 with the UE preset; got envelope: {envelope}",
    );

    // ---------- (d) Diamond: UObject -> {AActor, UActorComponent} --------
    //
    // `get_class_hierarchy("UObject")` walks the reverse `Inherits` edges
    // into UObject. Both AActor and UActorComponent declare `: public
    // UObject` in the fixture, so both must appear in `hierarchy.derived`.
    // Order is implementation-defined (diamond walk uses BFS but the
    // sibling order at a given level isn't part of the wire contract); we
    // assert membership via a HashSet rather than positional equality.
    let r = get_class_hierarchy(&fx.inner.graph, "UObject", Some(1), None);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "get_class_hierarchy(\"UObject\") returned error: {r:?}",
    );
    let body: serde_json::Value =
        serde_json::from_str(&first_text(&r)).expect("get_class_hierarchy body must be JSON");
    let derived: HashSet<String> = body["hierarchy"]["derived"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|n| n["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        derived.contains("AActor"),
        "expected AActor in UObject's derived set (diamond walk); got derived: {derived:?}, body: {body}",
    );
    assert!(
        derived.contains("UActorComponent"),
        "expected UActorComponent in UObject's derived set (diamond walk); got derived: {derived:?}, body: {body}",
    );

    // ---------- (e) Tick line preserved (byte-offset invariant) ----------
    //
    // The parameterized-macro substitution (Phase 2 of UeMacroSupport)
    // overwrites the matched span with same-length spaces, preserving
    // every subsequent byte's offset. The fixture's `Actor.h` declares
    // `virtual void Tick(float DeltaSeconds) override {}` on line 8
    // (line 1 is `#pragma once`, line 2 is blank, line 3 is `UCLASS(...)`,
    // line 4 is `class ENGINE_API AActor : public UObject {`, line 5 is
    // `GENERATED_BODY()`, line 6 is `public:`, line 7 is the `UFUNCTION`
    // attribute, line 8 is the Tick declaration with an inline empty
    // body). The body `{}` is required for symbol extraction —
    // forward-declared methods don't emit Symbol records (CLAUDE.md C++
    // Limitation 5). If a future refactor changes `strip_macros_with_args`
    // to a non-byte-preserving form, the recorded line number drifts off 8
    // and this assertion fires.
    //
    // Search pattern: `^AActor::Tick$` (NOT `^Tick$`). `Graph::search`
    // builds the regex match target as `{parent}::{name}` when `parent`
    // is non-empty (queries.rs:235-240), so an anchored pattern on the
    // bare name `Tick` never matches a method whose effective full-name
    // is `AActor::Tick`. The plan task description specified `^Tick$`,
    // but that pattern returns 0 results for parented methods — implementer
    // note surfaced in the phase report.
    let envelope = search_for(&fx.inner, "^AActor::Tick$");
    let aactor_tick_line = envelope["results"]
        .as_array()
        .and_then(|arr| {
            arr.iter().find(|s| {
                s["name"].as_str() == Some("Tick") && s["parent"].as_str() == Some("AActor")
            })
        })
        .and_then(|s| s["line"].as_u64());
    assert_eq!(
        aactor_tick_line,
        Some(8),
        "expected AActor::Tick at line 8 of Actor.h (byte-offset preservation invariant); got envelope: {envelope}",
    );

    // ---------- (f) get_file_symbols(Actor.h) contains AActor + Tick -----
    let actor_h = fx.root.join("Actor.h").to_string_lossy().into_owned();
    let r = get_file_symbols(
        &fx.inner.graph,
        &actor_h,
        false,
        true,
        None,
        None,
        false,
        NO_BYTE_BUDGET,
    );
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "get_file_symbols({actor_h:?}) returned error: {r:?}",
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&first_text(&r)).expect("get_file_symbols body must be JSON");
    let names: Vec<String> = parsed["results"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        names.iter().any(|n| n == "AActor"),
        "expected AActor in get_file_symbols(Actor.h); got names: {names:?}",
    );
    assert!(
        names.iter().any(|n| n == "Tick"),
        "expected Tick in get_file_symbols(Actor.h); got names: {names:?}",
    );
}

/// Anti-regression: pins today's broken state where UE-style parameterized
/// macros prevent extraction of `AActor`/`UObject`. This test PASSES while
/// `UeMacroSupport` is the only mechanism for fixing the bug. Deletion of
/// this test is the marker that we have a better fix (e.g., a future parser
/// upgrade that handles `UCLASS(...)` natively). Until then, this test
/// documents the diff `UeMacroSupport` provides.
#[tokio::test]
async fn ue_fixture_no_config_extracts_zero_aactor_symbols() {
    // Same fixture content (`Object.h`/`Actor.h`/`ActorComponent.h`), but
    // overwrite the `.code-graph.toml` with one that has BOTH strip lists
    // empty (the unconfigured-user baseline). The plan task description
    // permits "omit `macro_strip` entirely" as an option; empirically on
    // this fixture, leaving the bare-word `_API` macros in place defeats
    // tree-sitter's class extraction on the very first declaration —
    // `class ENGINE_API AActor : public UObject {` — independent of the
    // parameterized `UCLASS(...)` macro line above. Stripping just
    // `ENGINE_API` is enough to make AActor extract even with
    // `macro_strip_with_args = []`, so we must clear BOTH lists to
    // reproduce the originally-reported zero-symbols UE bug.
    let override_toml = "\
[cpp]
macro_strip = []
macro_strip_with_args = []
";
    let fx = build_indexed(Some(override_toml)).await;

    // ---------- (a) AActor must NOT extract -------------------------------
    //
    // With BOTH lists empty, `class ENGINE_API AActor : public UObject {`
    // leaves the bare `ENGINE_API` macro in the source, which tree-sitter
    // parses as the class name (so AActor never appears as a Symbol).
    // The parameterized `UCLASS(...)` on the line above is independently
    // broken too, but for AActor specifically the bare-word block is the
    // proximate cause. `UeMacroSupport` fixes the parameterized side;
    // `CppMacroStrip` (already shipped) fixes the bare side.
    let envelope = search_for(&fx.inner, "^AActor$");
    let aactor_total = envelope["total"].as_u64().unwrap_or(u64::MAX);
    assert_eq!(
        aactor_total, 0,
        "with both macro_strip and macro_strip_with_args empty, AActor must NOT \
         extract — pins today's broken behavior across the full UE-macro surface; \
         got envelope: {envelope}",
    );

    // ---------- (b) UObject must NOT extract ------------------------------
    //
    // `Object.h` opens with `class COREUOBJECT_API UObject {`. With
    // `macro_strip = []`, `COREUOBJECT_API` is left in the source and
    // tree-sitter parses IT as the class name (UObject never appears).
    // The next line's `GENERATED_UCLASS_BODY()` is also unstripped and
    // would independently defeat parsing of any inner methods. Both
    // contribute to the originally-reported UE bug.
    let envelope = search_for(&fx.inner, "^UObject$");
    let uobject_total = envelope["total"].as_u64().unwrap_or(u64::MAX);
    assert_eq!(
        uobject_total, 0,
        "with both macro_strip and macro_strip_with_args empty, UObject must NOT \
         extract — COREUOBJECT_API blocks the class name and GENERATED_UCLASS_BODY() \
         blocks the body; got envelope: {envelope}",
    );

    // ---------- (c) Class hierarchy lookup must fail ---------------------
    //
    // With UObject not in the graph at all, `get_class_hierarchy("UObject")`
    // must return a tool-level error (either "class not found" or the
    // fuzzy-suggestion variant). We don't pin the exact wording — only that
    // the call surfaces an error result rather than a successful hierarchy.
    let r = get_class_hierarchy(&fx.inner.graph, "UObject", Some(1), None);
    assert_eq!(
        r.is_error,
        Some(true),
        "with both macro_strip and macro_strip_with_args empty, \
         get_class_hierarchy(\"UObject\") must return an error result \
         (UObject is not in the graph); got: {r:?}",
    );
}

/// Phase 4.6 follow-up — pins the `macro_strip = [], macro_strip_with_args =
/// [...]` code path through `CppParser::preprocess`. The two preset-on/preset-
/// off tests above always populate both lists; this one isolates the case
/// where pass 1 (`strip_macros`) is a no-op (returns `Cow::Borrowed`) and
/// pass 2 must allocate the buffer via `into_owned()` to do the
/// parameterized-macro work. A regression that swaps the `into_owned()` call
/// for something subtly wrong (e.g. keeps a `Cow::Borrowed` and then
/// mutates the source bytes) would silently corrupt input or panic; this
/// test exercises the branch end-to-end.
///
/// The fixture is written inline rather than copied from `ue_minimal/`
/// because the existing fixture's classes (`AActor`, `UObject`,
/// `UActorComponent`) all rely on `_API` macros being stripped via
/// `macro_strip` — they would not extract under `macro_strip = []`
/// regardless of `macro_strip_with_args`. This test needs a class WITHOUT
/// an `_API` macro on its declaration line so that `macro_strip_with_args`
/// alone is enough to produce a valid post-preprocess buffer.
#[tokio::test]
async fn ue_only_macro_strip_with_args_path_extracts_no_api_class() {
    let dir = TempDir::new().expect("TempDir for no-_API fixture");

    // Minimal fixture: a UE-style class with parameterized macros but no
    // `_API` bare-token macro. `UCLASS()` and `GENERATED_BODY()` are
    // stripped via `macro_strip_with_args`; nothing in this file requires
    // `macro_strip`.
    let source = "\
#pragma once

UCLASS()
class CleanClass : public UObject {
    GENERATED_BODY()
public:
    void DoSomething() {}
};
";
    std::fs::write(dir.path().join("CleanClass.h"), source).expect("write CleanClass.h");

    // Config: `macro_strip = []` is the load-bearing constraint — pass 1
    // returns `Cow::Borrowed`, forcing the `into_owned()` allocation in
    // pass 2 to be the buffer pass 2 mutates.
    let cfg = "\
[cpp]
macro_strip = []
macro_strip_with_args = [\"UCLASS\", \"GENERATED_BODY\"]
";
    std::fs::write(dir.path().join(".code-graph.toml"), cfg).expect("write .code-graph.toml");

    let root = std::fs::canonicalize(dir.path()).expect("canonicalize tempdir");
    let server = fresh_server();
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
        "analyze_codebase failed: {r:?}",
    );

    let inner = server.inner.clone();
    let envelope = serde_json::from_str::<serde_json::Value>(&first_text(&search_symbols(
        &inner.graph,
        SearchSymbolsInput {
            query: Some("^CleanClass$"),
            brief: true,
            ..Default::default()
        },
        NO_BYTE_BUDGET,
    )))
    .expect("search_symbols body is JSON");
    let total = envelope["total"].as_u64().unwrap_or(0);
    assert!(
        total >= 1,
        "with macro_strip=[] and macro_strip_with_args=[UCLASS, GENERATED_BODY], \
         CleanClass MUST extract — this pins the Cow::Borrowed → into_owned() \
         allocation branch in CppParser::preprocess. envelope: {envelope}",
    );

    // Bonus: the method `DoSomething` survives the GENERATED_BODY() strip.
    let envelope = serde_json::from_str::<serde_json::Value>(&first_text(&search_symbols(
        &inner.graph,
        SearchSymbolsInput {
            query: Some("^CleanClass::DoSomething$"),
            brief: true,
            ..Default::default()
        },
        NO_BYTE_BUDGET,
    )))
    .expect("search_symbols body is JSON");
    assert!(
        envelope["total"].as_u64().unwrap_or(0) >= 1,
        "method survives the parameterized strip; got: {envelope}",
    );
}
