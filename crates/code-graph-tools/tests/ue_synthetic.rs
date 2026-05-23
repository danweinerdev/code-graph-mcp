//! UE-pattern exhaustive synthetic fixture.
//!
//! Uses an in-tree fixture rather than a third-party UE plugin submodule
//! to exercise every macro shape we claim to handle.
//! Compared to a submodule:
//!
//! - Deterministic: assertions are exact symbol-presence checks, not the
//!   ±10% drift tolerance third-party repos need.
//! - Always-on in CI: no submodule init, no auto-skip.
//! - Independent: no third-party trust, no license review.
//! - Self-documenting: each fixture file's content names the macro shape
//!   it pins; the test names the failure mode if that shape regresses.
//!
//! Coverage matrix (one test per concern so failures localize):
//!   Test 1 — class/method/property shapes across `UEBasics.h`, `UEMethods.h`,
//!            `UEProperties.h`, `UEDelegates.h`. Asserts headline classes and
//!            their methods extract; asserts inheritance edges materialize.
//!   Test 2 — `USTRUCT` + `UENUM` shapes from `UEStructsAndEnums.h`.
//!   Test 3 — `UEEdgeCases.h` positive: classes following comment/string
//!            macro lookalikes still extract (scanner correctly resumed
//!            past the lexical region).
//!   Test 4 — `UEEdgeCases.h` negative + `UEUserFunctionCollision.h`: macro
//!            lookalikes inside strings/comments did NOT spawn fake symbols,
//!            and the `UCLASS` function in the collision file was stripped
//!            (the documented "user error → understandable failure" case).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
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

const FIXTURE_FILES: &[&str] = &[
    "UEBasics.h",
    "UEMethods.h",
    "UEProperties.h",
    "UEDelegates.h",
    "UEStructsAndEnums.h",
    "UEEdgeCases.h",
    "UEUserFunctionCollision.h",
    ".code-graph.toml",
];

fn fresh_server() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().expect("CppParser::new")))
        .expect("register CppParser");
    CodeGraphServer::new(registry)
}

fn fixture_src() -> PathBuf {
    let raw = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("ue_synthetic");
    std::fs::canonicalize(&raw)
        .unwrap_or_else(|e| panic!("canonicalize {raw:?} failed: {e}; fixture must exist"))
}

fn copy_fixture_files(dest: &Path) {
    let src = fixture_src();
    for name in FIXTURE_FILES {
        let from = src.join(name);
        let to = dest.join(name);
        std::fs::copy(&from, &to).unwrap_or_else(|e| panic!("copy {:?} -> {:?}: {e}", from, to));
    }
}

struct Indexed {
    _dir: TempDir,
    inner: Arc<ServerInner>,
}

async fn build_indexed() -> Indexed {
    let dir = TempDir::new().expect("TempDir for UE synthetic fixture");
    copy_fixture_files(dir.path());
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
    Indexed {
        _dir: dir,
        inner: server.inner.clone(),
    }
}

/// Collect every C++ symbol name in the indexed graph. `search_symbols`
/// requires at least one filter, so we use `language=cpp` to get the full
/// set — this fixture only registers `CppParser`, so every indexed symbol
/// is C++.
fn all_symbol_names(inner: &Arc<ServerInner>) -> HashSet<String> {
    let r = search_symbols(
        &inner.graph,
        SearchSymbolsInput {
            subtree: None,
            language: Some("cpp"),
            brief: true,
            limit: Some(10_000),
            ..Default::default()
        },
        NO_BYTE_BUDGET,
    );
    let body = first_text(&r);
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("search_symbols body is JSON");
    parsed["results"]
        .as_array()
        .expect("`results` is an array")
        .iter()
        .filter_map(|s| s["name"].as_str().map(String::from))
        .collect()
}

/// Anchored-exact `search_symbols` count.
fn count_exact(inner: &Arc<ServerInner>, name: &str) -> u64 {
    let r = search_symbols(
        &inner.graph,
        SearchSymbolsInput {
            subtree: None,
            query: Some(&format!("^{name}$")),
            brief: true,
            ..Default::default()
        },
        NO_BYTE_BUDGET,
    );
    let body = first_text(&r);
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("search_symbols body is JSON");
    parsed["total"].as_u64().unwrap_or(0)
}

/// Test 1 — class/method/property shapes across the headline
/// fixture files. Every expected class and method MUST extract; absence
/// is a real regression of the parameterized-macro scanner.
#[tokio::test]
async fn ue_synthetic_class_method_property_shapes_extract() {
    let fx = build_indexed().await;
    let names = all_symbol_names(&fx.inner);

    // ---------- UEBasics.h — 7 classes + 2 methods ----------------------
    for expected in [
        "USimpleObject",
        "UBlueprintableObject",
        "USpawnableObject",
        "UAnnotatedObject",
        "UDerivedFromBlueprintable",
        "UDeepDerived",
        "UClassWithMethods",
        "PlainMethod",
        "GetValue",
    ] {
        assert!(
            names.contains(expected),
            "UEBasics.h: expected symbol {expected:?} missing from indexed graph; \
             all names: {names:?}",
        );
    }

    // ---------- UEMethods.h — 1 class + 6 methods -----------------------
    for expected in [
        "UFunctionExamples",
        "EmptyArgs",
        "BasicCallable",
        "TickWithCategory",
        "StringArgWithCommaAndParen",
        "MultilineDeclaration",
        "NormalMethodAfterMultiline",
    ] {
        assert!(
            names.contains(expected),
            "UEMethods.h: expected symbol {expected:?} missing; all names: {names:?}",
        );
    }

    // ---------- UEProperties.h — 2 classes + 2 methods ------------------
    for expected in [
        "UPropertyExamples",
        "UAnotherPropertyHolder",
        "RegularMethod",
        "AccessorMethod",
    ] {
        assert!(
            names.contains(expected),
            "UEProperties.h: expected symbol {expected:?} missing; all names: {names:?}",
        );
    }

    // ---------- UEDelegates.h — 1 class + 1 method ----------------------
    //
    // The DECLARE_*_DELEGATE macros expand to type definitions outside the
    // scope of our reflection coverage; they produce no Symbol records.
    // Only the trailing UCLASS-decorated UDelegateHolder + its method are
    // asserted on, which is the load-bearing check: the multi-line
    // DECLARE_DYNAMIC_MULTICAST_DELEGATE_ThreeParams above it must not
    // shift line offsets or break tree-sitter's ability to parse the
    // class declaration below.
    for expected in ["UDelegateHolder", "TriggerDelegate"] {
        assert!(
            names.contains(expected),
            "UEDelegates.h: expected symbol {expected:?} missing; all names: {names:?}",
        );
    }
}

/// Test 2 — `USTRUCT` and `UENUM` shapes.
#[tokio::test]
async fn ue_synthetic_struct_and_enum_shapes_extract() {
    let fx = build_indexed().await;
    let names = all_symbol_names(&fx.inner);

    for expected in [
        "FBasicStruct",
        "FAnotherStruct",
        "HelperMethod",
        "ESimpleEnum",
        "EAnotherEnum",
    ] {
        assert!(
            names.contains(expected),
            "UEStructsAndEnums.h: expected symbol {expected:?} missing; all names: {names:?}",
        );
    }
}

/// Test 3 — edge cases (positive). After every adversarial macro
/// lookalike (in comment, in string, with nested parens, with parens
/// inside a string inside a meta block), the scanner must correctly
/// resume normal scanning and let the real class declarations below
/// extract.
#[tokio::test]
async fn ue_synthetic_edge_cases_real_classes_extract() {
    let fx = build_indexed().await;
    let names = all_symbol_names(&fx.inner);

    for expected in [
        "URealClassAfterComments",
        "URealClassAfterString",
        "UDeeplyNestedMeta",
        "UParenInToolTip",
    ] {
        assert!(
            names.contains(expected),
            "UEEdgeCases.h: expected real class {expected:?} missing — \
             scanner likely failed to resume past an adversarial lexical \
             region; all names: {names:?}",
        );
    }
}

/// Test 4 — negative assertions. Macro lookalikes inside strings
/// and comments must NOT have spawned fake symbols. And the documented
/// "user function named like a macro disappears" case must hold for the
/// `UCLASS` function in the collision file.
#[tokio::test]
async fn ue_synthetic_negatives_no_spurious_symbols() {
    let fx = build_indexed().await;
    let names = all_symbol_names(&fx.inner);

    // ---------- UEEdgeCases.h — no fake classes from lookalikes --------
    for forbidden in [
        "FakeFromLineComment",
        "FakeFromBlockComment",
        "FakeFromString",
    ] {
        assert!(
            !names.contains(forbidden),
            "UEEdgeCases.h: forbidden symbol {forbidden:?} appeared — \
             scanner stripped a macro lookalike inside a comment or string; \
             all names: {names:?}",
        );
    }

    // ---------- UEUserFunctionCollision.h — function `UCLASS` stripped -
    //
    // The function `int UCLASS(int x, int y) { return x + y; }` shares
    // the IDENT(args)body shape with a parameterized macro use; the
    // scanner strips its signature, leaving an unparseable orphan body.
    // The function MUST NOT appear in the indexed graph.
    //
    // This is the pinned "user error → understandable failure" case
    // (UeMacroSupport design Decision 4). The test exists to surface
    // any future regression that lets such functions through silently
    // — at which point we'd either celebrate (a scanner improvement)
    // or flag (a coverage hole).
    //
    // The class declared AFTER the collision function (`URealClassAfterCollision`)
    // MUST extract — confirms the scanner correctly resumed past the
    // collision-damage zone.
    assert_eq!(
        count_exact(&fx.inner, "UCLASS"),
        0,
        "UEUserFunctionCollision.h: a function named `UCLASS` survived the \
         parameterized strip — either the scanner regressed, or this test \
         needs updating to celebrate a real improvement. all names: {names:?}",
    );
    assert!(
        names.contains("URealClassAfterCollision"),
        "UEUserFunctionCollision.h: real class after the collision must still \
         extract; got names: {names:?}",
    );
}
