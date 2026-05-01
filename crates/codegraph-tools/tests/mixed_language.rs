//! Phase 5.6 integration tests — exercise the full MCP-server path with
//! both the C++ and Rust language plugins registered.
//!
//! The C++ and Rust plugins share the [`LanguageRegistry`]; this file
//! confirms they coexist correctly and that the symbol/edge surface
//! continues to work for Rust-specific shapes (traits, trait impls, the
//! widened `{Class, Struct, Interface, Trait}` root filter on
//! `get_class_hierarchy`, and inheritance diagrams driven by
//! `impl Trait for Type` edges).
//!
//! The mixed-language pieces use `testdata/mixed/` (`foo.cpp` + `foo.rs`,
//! both defining `helper`) so the search-by-language tests assert
//! per-language isolation deterministically with a single shared anchor.
//! The trait/diagram pieces use `testdata/rust/` (the Phase 5.5 corpus)
//! so the assertions ride on the existing manifest-locked symbol set
//! rather than a parallel inline fixture.

use std::sync::Arc;

use codegraph_core::Language;
use codegraph_lang::LanguageRegistry;
use codegraph_lang_cpp::CppParser;
use codegraph_lang_rust::RustParser;
use codegraph_tools::handlers::analyze::analyze_codebase;
use codegraph_tools::handlers::structure::{
    generate_diagram, get_class_hierarchy, GenerateDiagramInput,
};
use codegraph_tools::handlers::symbols::{search_symbols, SearchSymbolsInput};
use codegraph_tools::server::ServerInner;
use codegraph_tools::CodeGraphServer;
use tempfile::TempDir;

mod common;
use common::{copy_testdata_from, first_text, testdata_mixed_path, testdata_rust_path};

/// Fresh server with both the C++ and Rust language plugins registered —
/// mirrors the registration block in `crates/code-graph-mcp/src/main.rs`.
/// Used by every test in this file so each test exercises the same
/// registry shape the binary ships.
fn server_with_cpp_and_rust() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().expect("CppParser::new")))
        .expect("register CppParser");
    registry
        .register(Box::new(RustParser::new().expect("RustParser::new")))
        .expect("register RustParser");
    CodeGraphServer::new(registry)
}

/// Per-test fixture: copy `src` into a fresh TempDir, register both
/// parsers, run `analyze_codebase`, return the indexed `ServerInner`.
/// Each test gets its own TempDir so the `analyze` cache write can't
/// race with another test running in parallel.
struct IndexedFixture {
    _dir: TempDir,
    inner: Arc<ServerInner>,
}

async fn build_indexed(src: &std::path::Path) -> IndexedFixture {
    let dir = TempDir::new().expect("TempDir for indexed fixture");
    copy_testdata_from(src, dir.path());
    let indexed_root =
        std::fs::canonicalize(dir.path()).expect("canonicalize TempDir for indexed_root");

    let server = server_with_cpp_and_rust();
    let r = analyze_codebase(
        server.inner.clone(),
        indexed_root.to_string_lossy().into_owned(),
        true,
        None,
        None,
    )
    .await;
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "analyze_codebase failed: {r:?}",
    );
    IndexedFixture {
        _dir: dir,
        inner: server.inner.clone(),
    }
}

// ---------------------------------------------------------------------
// Mixed C++ + Rust indexing — both `helper` symbols must coexist.
// ---------------------------------------------------------------------

#[tokio::test]
async fn mixed_cpp_rust_indexes_both() {
    let fx = build_indexed(&testdata_mixed_path()).await;
    let g = fx.inner.graph.read();

    let mut cpp_helpers = 0u32;
    let mut rust_helpers = 0u32;
    for s in g.search_symbols("helper", None) {
        if s.name != "helper" {
            continue;
        }
        match s.language {
            Language::Cpp => cpp_helpers += 1,
            Language::Rust => rust_helpers += 1,
            _ => {}
        }
    }

    assert_eq!(
        cpp_helpers, 1,
        "expected exactly 1 C++ helper symbol, got {cpp_helpers}",
    );
    assert_eq!(
        rust_helpers, 1,
        "expected exactly 1 Rust helper symbol, got {rust_helpers}",
    );
}

// ---------------------------------------------------------------------
// search_symbols by language filter — cross-language isolation.
// ---------------------------------------------------------------------

/// Infer the language of a `search_symbols` result from its `file`
/// extension. The wire format (`SymbolResult`) deliberately omits the
/// `language` field — Phase 1 keeps the JSON shape byte-identical with
/// the Go reference, which never serialized it. The file extension is
/// the next-cheapest discriminant and is unambiguous for the mixed
/// fixture (`.cpp` ↔ Cpp, `.rs` ↔ Rust). Returns `"?"` for any other
/// extension to surface unexpected results loudly rather than silently.
fn language_from_file(file: &str) -> &'static str {
    if file.ends_with(".cpp") || file.ends_with(".cc") || file.ends_with(".cxx") {
        "cpp"
    } else if file.ends_with(".rs") {
        "rust"
    } else {
        "?"
    }
}

/// Pull a per-result language tag out of a `search_symbols` response,
/// using the file extension as the discriminant (see `language_from_file`).
fn result_languages(body: &str) -> Vec<&'static str> {
    let parsed: serde_json::Value =
        serde_json::from_str(body).expect("search_symbols returns JSON");
    parsed["results"]
        .as_array()
        .expect("results is an array")
        .iter()
        .map(|r| language_from_file(r["file"].as_str().expect("each result has a file field")))
        .collect()
}

#[tokio::test]
async fn search_helper_no_filter_returns_both_languages() {
    let fx = build_indexed(&testdata_mixed_path()).await;
    let r = search_symbols(
        &fx.inner.graph,
        SearchSymbolsInput {
            query: Some("helper"),
            brief: true,
            ..Default::default()
        },
    );
    let body = first_text(&r);
    let languages = result_languages(&body);
    assert_eq!(
        languages.len(),
        2,
        "no-filter search must return both helpers; got: {languages:?}",
    );
    assert!(
        languages.contains(&"cpp"),
        "expected cpp in results, got: {languages:?}",
    );
    assert!(
        languages.contains(&"rust"),
        "expected rust in results, got: {languages:?}",
    );
}

#[tokio::test]
async fn search_helper_language_cpp_returns_only_cpp() {
    let fx = build_indexed(&testdata_mixed_path()).await;
    let r = search_symbols(
        &fx.inner.graph,
        SearchSymbolsInput {
            query: Some("helper"),
            language: Some("cpp"),
            brief: true,
            ..Default::default()
        },
    );
    let body = first_text(&r);
    let languages = result_languages(&body);
    assert_eq!(
        languages,
        vec!["cpp"],
        "language=cpp filter must return exactly the C++ helper"
    );
}

#[tokio::test]
async fn search_helper_language_rust_returns_only_rust() {
    let fx = build_indexed(&testdata_mixed_path()).await;
    let r = search_symbols(
        &fx.inner.graph,
        SearchSymbolsInput {
            query: Some("helper"),
            language: Some("rust"),
            brief: true,
            ..Default::default()
        },
    );
    let body = first_text(&r);
    let languages = result_languages(&body);
    assert_eq!(
        languages,
        vec!["rust"],
        "language=rust filter must return exactly the Rust helper"
    );
}

// ---------------------------------------------------------------------
// get_class_hierarchy on a Rust trait — regression for Phase 2's
// widened {Class, Struct, Interface, Trait} root filter.
// ---------------------------------------------------------------------

#[tokio::test]
async fn get_class_hierarchy_for_rust_trait() {
    let fx = build_indexed(&testdata_rust_path()).await;
    // `Greet` is the trait that `Greeter` implements (see
    // `testdata/rust/src/traits.rs`). Pre-Phase 2 the lookup would have
    // narrowed to {Class, Struct, Interface} and skipped the trait — so
    // the success of this lookup is the regression assertion.
    let r = get_class_hierarchy(&fx.inner.graph, "Greet", Some(2));
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "get_class_hierarchy must succeed for a Rust trait: {r:?}",
    );

    let body = first_text(&r);
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("hierarchy is JSON");
    assert_eq!(
        parsed["name"].as_str(),
        Some("Greet"),
        "hierarchy root must be the queried trait, got: {parsed}",
    );
    // `Greeter` impls `Greet`, so the trait's `derived` list (incoming
    // Inherits edges) must include it.
    let derived: Vec<&str> = parsed["derived"]
        .as_array()
        .expect("derived is an array")
        .iter()
        .filter_map(|n| n["name"].as_str())
        .collect();
    assert!(
        derived.contains(&"Greeter"),
        "trait `Greet`'s derived list must include `Greeter`, got: {derived:?}",
    );
}

// ---------------------------------------------------------------------
// generate_diagram for a Rust trait inheritance — `Compute` is impl'd
// by both `Foo<T>` and `Bar<T>` in testdata/rust, so the inheritance
// diagram has at least two Inherits edges from those types to `Compute`.
// ---------------------------------------------------------------------

#[tokio::test]
async fn generate_diagram_for_rust_trait_inheritance() {
    let fx = build_indexed(&testdata_rust_path()).await;
    let r = generate_diagram(
        &fx.inner.graph,
        GenerateDiagramInput {
            class: Some("Compute"),
            format: Some("edges"),
            depth: Some(2),
            ..Default::default()
        },
    );
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "generate_diagram must succeed for a Rust trait: {r:?}",
    );

    let body = first_text(&r);
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("diagram is JSON");
    let edges = parsed.as_array().expect("edges format returns an array");
    let pairs: Vec<(&str, &str)> = edges
        .iter()
        .map(|e| {
            (
                e["from"].as_str().unwrap_or(""),
                e["to"].as_str().unwrap_or(""),
            )
        })
        .collect();

    // The Phase 5.5 manifest documents three Inherits edges in
    // traits.rs: `Greeter -> Greet`, `Foo<T> -> Compute`, `Bar<T> ->
    // Compute`. The Compute-rooted diagram must surface the latter two.
    assert!(
        pairs.iter().any(|(f, t)| *f == "Foo<T>" && *t == "Compute"),
        "expected Foo<T> -> Compute Inherits edge, got: {pairs:?}",
    );
    assert!(
        pairs.iter().any(|(f, t)| *f == "Bar<T>" && *t == "Compute"),
        "expected Bar<T> -> Compute Inherits edge, got: {pairs:?}",
    );
}
