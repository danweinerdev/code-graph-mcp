//! End-to-end integration test for `[cpp].macro_strip`.
//!
//! These tests close the "config flows from `.code-graph.toml` through
//! `analyze_codebase` into the C++ plugin's substitution" gap that pure
//! unit tests in `code-graph-lang-cpp` don't cover. A future refactor that
//! accidentally passes `RootConfig::default()` somewhere in the indexer
//! pipeline would make every existing snapshot/corpus test pass while
//! silently breaking macro stripping; the discriminator below is Test 1
//! (positive) + Test 2 (control) on identical fixture content but
//! differing config.

use code_graph_core::SymbolKind;
use code_graph_lang::LanguageRegistry;
use code_graph_lang_cpp::CppParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::CodeGraphServer;
use tempfile::TempDir;

/// Identical fixture used by both the positive (Test 1) and control
/// (Test 2) tests. Holding this constant guarantees any difference in
/// observed graph state is attributable solely to `[cpp].macro_strip`,
/// not to fixture wording drift.
const MY_ACTOR_HEADER: &str = "\
class UObject {};
class CORE_API AActor : public UObject {};
class ENGINE_API APawn : public AActor {};
";

/// Fresh server with the C++ parser plugin registered. Mirrors
/// `tests/integration.rs::fresh_server`.
fn fresh_server() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().expect("CppParser::new")))
        .unwrap();
    CodeGraphServer::new(registry)
}

/// Seed a tempdir with `MyActor.h` and the supplied `.code-graph.toml`
/// content (or no config file when `cfg = None`). Returns the dir handle
/// (kept alive by the caller) and the canonicalized root path used by
/// `analyze_codebase`.
fn seed_root(cfg: Option<&str>) -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().expect("TempDir");
    std::fs::write(dir.path().join("MyActor.h"), MY_ACTOR_HEADER).unwrap();
    if let Some(toml) = cfg {
        std::fs::write(dir.path().join(".code-graph.toml"), toml).unwrap();
    }
    let root = std::fs::canonicalize(dir.path()).unwrap();
    (dir, root)
}

/// Test 1 (positive case): with `[cpp].macro_strip = ["CORE_API",
/// "ENGINE_API"]` configured, the macro-prefixed classes extract correctly
/// and the chained `Inherits` edges materialize end-to-end through the
/// indexer pipeline.
#[tokio::test]
async fn cpp_macro_strip_extracts_class_with_api_macro() {
    let (dir, root) = seed_root(Some(
        "[cpp]\nmacro_strip = [\"CORE_API\", \"ENGINE_API\"]\n",
    ));
    let server = fresh_server();

    // force=true keeps the assertion deterministic — the cache cannot
    // mask a regression in the parse pipeline.
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

    let g = server.inner.graph.read();
    let symbols: Vec<_> = g
        .file_symbols(&root.join("MyActor.h"))
        .into_iter()
        .collect();

    // AActor and APawn must extract as Class symbols.
    let aactor = symbols
        .iter()
        .find(|s| s.name == "AActor")
        .expect("AActor must extract once CORE_API is stripped");
    assert_eq!(aactor.kind, SymbolKind::Class);

    let apawn = symbols
        .iter()
        .find(|s| s.name == "APawn")
        .expect("APawn must extract once ENGINE_API is stripped");
    assert_eq!(apawn.kind, SymbolKind::Class);

    // Inheritance edges materialize through the graph's `class_hierarchy`
    // walker. AActor's bases must include UObject; APawn's bases must
    // include AActor. The walker reads the forward `adj` keyed by bare
    // derived name, so a successful query is the load-bearing proof that
    // both `Inherits` edges landed.
    let (aactor_hier, _, _) = g
        .class_hierarchy("AActor", 1, u32::MAX)
        .expect("AActor must be discoverable as a class");
    let aactor_base_names: Vec<&str> = aactor_hier.bases.iter().map(|n| n.name.as_str()).collect();
    assert!(
        aactor_base_names.contains(&"UObject"),
        "expected AActor -> UObject inherits edge; got bases {aactor_base_names:?}",
    );

    let (apawn_hier, _, _) = g
        .class_hierarchy("APawn", 1, u32::MAX)
        .expect("APawn must be discoverable as a class");
    let apawn_base_names: Vec<&str> = apawn_hier.bases.iter().map(|n| n.name.as_str()).collect();
    assert!(
        apawn_base_names.contains(&"AActor"),
        "expected APawn -> AActor inherits edge; got bases {apawn_base_names:?}",
    );

    drop(g);
    drop(dir);
}

/// Test 2 (control case): identical fixture, no `[cpp]` section — the
/// macro-prefixed classes must NOT extract. This preserves the buggy
/// opt-in semantics for users who haven't configured `macro_strip` and is
/// the discriminator that proves Test 1's symbols come from the
/// substitution layer, not from fixture syntax that tree-sitter happens
/// to accept either way.
///
/// Without this control, an implementer could pass `RootConfig::default()`
/// into `preprocess` somewhere in the indexer pipeline and Test 1 would
/// still observe `AActor`/`APawn` if they extracted for some unrelated
/// reason. Asserting both the positive AND the negative case on the same
/// fixture forecloses that failure mode.
#[tokio::test]
async fn cpp_macro_strip_control_empty_list_does_not_extract() {
    // No `.code-graph.toml` at all — equivalent to `[cpp]\nmacro_strip = []`
    // (a `code-graph-core` anti-regression test covers the explicit
    // empty-array form; here we verify the implicit default).
    let (dir, root) = seed_root(None);
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

    let g = server.inner.graph.read();
    let symbols = g.file_symbols(&root.join("MyActor.h"));
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();

    // The unprefixed `UObject` still extracts — this confirms the file
    // parsed at all and the absence below is meaningful.
    assert!(
        names.contains(&"UObject"),
        "UObject must still extract (unprefixed class); got {names:?}",
    );
    assert!(
        !names.contains(&"AActor"),
        "without macro_strip, AActor must NOT extract — preserves opt-in \
         semantics; got {names:?}",
    );
    assert!(
        !names.contains(&"APawn"),
        "without macro_strip, APawn must NOT extract — preserves opt-in \
         semantics; got {names:?}",
    );

    drop(g);
    drop(dir);
}
