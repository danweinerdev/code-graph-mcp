//! Phase 5.6 + 6.6 integration tests — exercise the full MCP-server path
//! with the C++, Rust, and Go language plugins registered.
//!
//! All three plugins share the [`LanguageRegistry`]; this file confirms
//! they coexist correctly and that the symbol/edge surface continues to
//! work for language-specific shapes (Rust traits and trait impls drive
//! the widened `{Class, Struct, Interface, Trait}` root filter on
//! `get_class_hierarchy`; Go interfaces exercise the same widened filter
//! while producing zero `Inherits` edges; mixed indexing and the
//! `(Language, name)`-keyed SymbolIndex isolate cross-language collisions).
//!
//! The mixed-language pieces use `testdata/mixed/` (with `foo.cpp`,
//! `foo.rs`, and `foo.go` — all defining `helper`) so the
//! search-by-language tests assert per-language isolation deterministically
//! with a single shared anchor. The trait/diagram pieces use
//! `testdata/rust/` (the Phase 5.5 corpus) so the assertions ride on the
//! existing manifest-locked symbol set rather than a parallel inline
//! fixture. The Go interface and cross-language `init`-collision pieces
//! use inline fixtures synthesized per-test inside a TempDir so they
//! don't perturb the shared corpora.

use std::sync::Arc;

use codegraph_core::Language;
use codegraph_lang::LanguageRegistry;
use codegraph_lang_cpp::CppParser;
use codegraph_lang_go::GoParser;
use codegraph_lang_rust::RustParser;
use codegraph_tools::handlers::analyze::analyze_codebase;
use codegraph_tools::handlers::query::{callers_or_callees, Direction};
use codegraph_tools::handlers::structure::{
    generate_diagram, get_class_hierarchy, GenerateDiagramInput,
};
use codegraph_tools::handlers::symbols::{search_symbols, SearchSymbolsInput};
use codegraph_tools::server::ServerInner;
use codegraph_tools::CodeGraphServer;
use tempfile::TempDir;

mod common;
use common::{
    copy_testdata_from, first_text, testdata_mixed_path, testdata_rust_path, GO_INTERFACE_FIXTURE,
};

/// Fresh server with the C++, Rust, and Go language plugins registered —
/// mirrors the registration block in `crates/code-graph-mcp/src/main.rs`.
/// Used by every test in this file so each test exercises the same
/// registry shape the binary ships.
fn server_with_all_parsers() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().expect("CppParser::new")))
        .expect("register CppParser");
    registry
        .register(Box::new(RustParser::new().expect("RustParser::new")))
        .expect("register RustParser");
    registry
        .register(Box::new(GoParser::new().expect("GoParser::new")))
        .expect("register GoParser");
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
    build_indexed_from_dir(dir).await
}

/// Lower-level fixture builder used by tests that synthesize their own
/// in-TempDir source files (e.g. the cross-language `init` collision and
/// the Go interface tests) instead of seeding from a `testdata/` corpus.
async fn build_indexed_from_dir(dir: TempDir) -> IndexedFixture {
    let indexed_root =
        std::fs::canonicalize(dir.path()).expect("canonicalize TempDir for indexed_root");

    let server = server_with_all_parsers();
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
// Mixed C++ + Rust + Go indexing — all three `helper` symbols must coexist.
// ---------------------------------------------------------------------

#[tokio::test]
async fn mixed_cpp_rust_go_indexes_all_three() {
    let fx = build_indexed(&testdata_mixed_path()).await;
    let g = fx.inner.graph.read();

    let mut cpp_helpers = 0u32;
    let mut rust_helpers = 0u32;
    let mut go_helpers = 0u32;
    for s in g.search_symbols("helper", None) {
        if s.name != "helper" {
            continue;
        }
        match s.language {
            Language::Cpp => cpp_helpers += 1,
            Language::Rust => rust_helpers += 1,
            Language::Go => go_helpers += 1,
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
    assert_eq!(
        go_helpers, 1,
        "expected exactly 1 Go helper symbol, got {go_helpers}",
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
/// fixture (`.cpp` ↔ Cpp, `.rs` ↔ Rust, `.go` ↔ Go). Returns `"?"` for
/// any other extension to surface unexpected results loudly rather than
/// silently.
fn language_from_file(file: &str) -> &'static str {
    if file.ends_with(".cpp") || file.ends_with(".cc") || file.ends_with(".cxx") {
        "cpp"
    } else if file.ends_with(".rs") {
        "rust"
    } else if file.ends_with(".go") {
        "go"
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
async fn search_helper_no_filter_returns_all_three_languages() {
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
        3,
        "no-filter search must return all three helpers; got: {languages:?}",
    );
    assert!(
        languages.contains(&"cpp"),
        "expected cpp in results, got: {languages:?}",
    );
    assert!(
        languages.contains(&"rust"),
        "expected rust in results, got: {languages:?}",
    );
    assert!(
        languages.contains(&"go"),
        "expected go in results, got: {languages:?}",
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

#[tokio::test]
async fn search_helper_language_go_returns_only_go() {
    let fx = build_indexed(&testdata_mixed_path()).await;
    let r = search_symbols(
        &fx.inner.graph,
        SearchSymbolsInput {
            query: Some("helper"),
            language: Some("go"),
            brief: true,
            ..Default::default()
        },
    );
    let body = first_text(&r);
    let languages = result_languages(&body);
    assert_eq!(
        languages,
        vec!["go"],
        "language=go filter must return exactly the Go helper"
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

// ---------------------------------------------------------------------
// Phase 6.6 — cross-language `init` collision regression.
//
// A function named `init` exists in both C++ and Go; the
// `(Language, name)`-keyed `SymbolIndex` from Phase 3 must keep them
// isolated during call resolution. Each language's caller resolves to
// its own-language `init`; neither leaks across the language boundary.
// Inline fixture so it stays close to the assertions and doesn't
// pollute the shared `testdata/mixed/` corpus.
// ---------------------------------------------------------------------

/// Synthesize a TempDir with a C++ file and a Go file, each declaring a
/// function named `init` plus an in-language caller of that `init`.
/// Returns the indexed fixture so callers can issue tool requests.
async fn build_init_collision_fixture() -> IndexedFixture {
    let dir = TempDir::new().expect("TempDir for init-collision fixture");
    // C++ side: `init()` plus an in-file caller `caller_cpp` that calls
    // `init()`. The caller's call edge must resolve to the C++ `init` via
    // the (Language=Cpp, name="init") bucket of the SymbolIndex.
    std::fs::write(
        dir.path().join("init_cpp.cpp"),
        "void init() {}\nvoid caller_cpp() { init(); }\n",
    )
    .expect("write init_cpp.cpp");
    // Go side: `init()` plus `caller_go()` that calls `init()`. Edge must
    // resolve to the Go `init` via (Language=Go, name="init"). Note: Go's
    // own runtime never calls `init` directly via a call expression, so
    // the synthetic `caller_go` is what exercises the resolver.
    std::fs::write(
        dir.path().join("init_go.go"),
        "package main\n\nfunc init() {}\nfunc caller_go() { init() }\n",
    )
    .expect("write init_go.go");
    build_indexed_from_dir(dir).await
}

#[tokio::test]
async fn search_init_returns_both_languages() {
    let fx = build_init_collision_fixture().await;
    let r = search_symbols(
        &fx.inner.graph,
        SearchSymbolsInput {
            query: Some("init"),
            brief: true,
            ..Default::default()
        },
    );
    let body = first_text(&r);
    let languages = result_languages(&body);
    // Exactly two `init` entries — one per language. The C++ symbol_id
    // is `<root>/init_cpp.cpp:init` and the Go symbol_id is
    // `<root>/init_go.go:init`. The (Language, name)-keyed SymbolIndex
    // is what keeps them in separate buckets; without it, the resolver
    // would collapse them into one entry on call lookup.
    let cpp_count = languages.iter().filter(|l| **l == "cpp").count();
    let go_count = languages.iter().filter(|l| **l == "go").count();
    assert_eq!(
        cpp_count, 1,
        "expected exactly 1 C++ init, got languages: {languages:?}"
    );
    assert_eq!(
        go_count, 1,
        "expected exactly 1 Go init, got languages: {languages:?}"
    );
}

/// CRITICAL regression: with `init` defined in both C++ and Go, the
/// in-language caller's `Calls` edge must resolve to the same-language
/// `init`. The (Language, name) keying of `SymbolIndex` (Phase 3
/// invariant at `crates/codegraph-lang/src/lib.rs:116`) is what prevents
/// the C++ caller from showing up in the Go init's caller list and
/// vice versa. If that keying ever degrades to bare `name`, the C++
/// caller would be a candidate for the Go init's resolution and this
/// assertion would fail.
#[tokio::test]
async fn cross_language_init_callers_stay_isolated() {
    let fx = build_init_collision_fixture().await;
    let g = fx.inner.graph.read();

    // Locate the per-language symbol IDs from the indexed graph rather
    // than reconstructing them by string formatting — TempDir paths are
    // opaque and canonicalized.
    let cpp_init_id = g
        .search_symbols("init", None)
        .into_iter()
        .find(|s| s.language == Language::Cpp && s.name == "init")
        .map(|s| codegraph_core::symbol_id(&s))
        .expect("C++ init symbol must exist");
    let go_init_id = g
        .search_symbols("init", None)
        .into_iter()
        .find(|s| s.language == Language::Go && s.name == "init")
        .map(|s| codegraph_core::symbol_id(&s))
        .expect("Go init symbol must exist");
    drop(g);

    // C++ init's callers — must include caller_cpp and must NOT include
    // caller_go.
    let cpp_callers =
        callers_or_callees(&fx.inner.graph, &cpp_init_id, Some(1), Direction::Callers);
    let cpp_body = first_text(&cpp_callers);
    let cpp_arr: serde_json::Value =
        serde_json::from_str(&cpp_body).expect("get_callers response is JSON");
    let cpp_caller_names: Vec<String> = cpp_arr
        .as_array()
        .expect("callers is an array")
        .iter()
        .filter_map(|c| c["symbol_id"].as_str().map(str::to_owned))
        .collect();
    assert!(
        cpp_caller_names.iter().any(|s| s.ends_with(":caller_cpp")),
        "C++ init must have caller_cpp in its callers, got: {cpp_caller_names:?}"
    );
    assert!(
        !cpp_caller_names.iter().any(|s| s.ends_with(":caller_go")),
        "C++ init must NOT have the Go caller in its callers — \
         that would mean the (Language, name) SymbolIndex keying broke; \
         got: {cpp_caller_names:?}"
    );

    // Go init's callers — must include caller_go and must NOT include
    // caller_cpp. This is the symmetric assertion.
    let go_callers = callers_or_callees(&fx.inner.graph, &go_init_id, Some(1), Direction::Callers);
    let go_body = first_text(&go_callers);
    let go_arr: serde_json::Value =
        serde_json::from_str(&go_body).expect("get_callers response is JSON");
    let go_caller_names: Vec<String> = go_arr
        .as_array()
        .expect("callers is an array")
        .iter()
        .filter_map(|c| c["symbol_id"].as_str().map(str::to_owned))
        .collect();
    assert!(
        go_caller_names.iter().any(|s| s.ends_with(":caller_go")),
        "Go init must have caller_go in its callers, got: {go_caller_names:?}"
    );
    assert!(
        !go_caller_names.iter().any(|s| s.ends_with(":caller_cpp")),
        "Go init must NOT have the C++ caller in its callers — \
         that would mean the (Language, name) SymbolIndex keying broke; \
         got: {go_caller_names:?}"
    );
}

// ---------------------------------------------------------------------
// Phase 6.6 — get_class_hierarchy on a Go interface.
//
// Go interfaces are structural — a concrete type satisfies an interface
// by having the right method set, with no syntactic declaration. The
// Go parser emits zero `Inherits` edges (Phase 6.2 design). The
// hierarchy lookup must still succeed (Phase 2 widened the root filter
// to `{Class, Struct, Interface, Trait}`), and the result must show
// empty `bases` and `derived` arrays.
// ---------------------------------------------------------------------

/// Synthesize a TempDir with a Go interface plus a struct that
/// structurally satisfies it. The struct must NOT appear as `derived`
/// because there is no syntactic inheritance edge in Go.
async fn build_go_interface_fixture() -> IndexedFixture {
    let dir = TempDir::new().expect("TempDir for Go interface fixture");
    // `Reader` is the interface; `MyReader` structurally implements it
    // by having a `Read()` method. The parser must NOT emit an Inherits
    // edge for that relationship. Source string is shared with the
    // matching snapshot fixture in `snapshot_responses.rs` via
    // `common::GO_INTERFACE_FIXTURE`.
    std::fs::write(dir.path().join("reader.go"), GO_INTERFACE_FIXTURE).expect("write reader.go");
    build_indexed_from_dir(dir).await
}

#[tokio::test]
async fn get_class_hierarchy_for_go_interface() {
    let fx = build_go_interface_fixture().await;
    let r = get_class_hierarchy(&fx.inner.graph, "Reader", Some(2));
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "get_class_hierarchy must succeed for a Go interface (Phase 2 \
         widened root filter accepts Interface): {r:?}",
    );

    let body = first_text(&r);
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("hierarchy is JSON");
    assert_eq!(
        parsed["name"].as_str(),
        Some("Reader"),
        "hierarchy root must be the queried interface, got: {parsed}",
    );
    // No structural inheritance edges in Go — `bases` and `derived` are
    // both absent (the `HierarchyNode` serializer uses
    // `skip_serializing_if = "Vec::is_empty"`, so a leaf node with zero
    // bases/derived emits just `{"name": "Reader"}`). Treat both an
    // absent key and an explicit empty array as success; if either field
    // shows up populated, the structural-implementation-not-edges
    // invariant has been violated.
    let bases_empty = parsed
        .get("bases")
        .map(|v| v.as_array().is_some_and(|a| a.is_empty()))
        .unwrap_or(true);
    let derived_empty = parsed
        .get("derived")
        .map(|v| v.as_array().is_some_and(|a| a.is_empty()))
        .unwrap_or(true);
    assert!(
        bases_empty,
        "Go interface has no bases (no inheritance edges in Go), got: {parsed}",
    );
    assert!(
        derived_empty,
        "Go interface has no derived types — structural implementation \
         is NOT represented as an edge (Phase 6.2 design intent); got: {parsed}",
    );
}
