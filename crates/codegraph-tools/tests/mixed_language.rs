//! Phase 5.6 + 6.6 + 7.7 integration tests — exercise the full MCP-server
//! path with the C++, Rust, Go, and Python language plugins registered.
//!
//! All four plugins share the [`LanguageRegistry`]; this file confirms
//! they coexist correctly and that the symbol/edge surface continues to
//! work for language-specific shapes (Rust traits and trait impls drive
//! the widened `{Class, Struct, Interface, Trait}` root filter on
//! `get_class_hierarchy`; Go interfaces exercise the same widened filter
//! while producing zero `Inherits` edges; Python's dynamic typing means
//! call resolution is best-effort but cross-language isolation still
//! holds; mixed indexing and the `(Language, name)`-keyed SymbolIndex
//! isolate cross-language collisions across all four languages).
//!
//! The mixed-language pieces use `testdata/mixed/` (with `foo.cpp`,
//! `foo.rs`, `foo.go`, and `foo.py` — all defining `helper`) so the
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
use codegraph_lang_python::PythonParser;
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

/// Fresh server with the C++, Rust, Go, and Python language plugins
/// registered — mirrors the registration block in
/// `crates/code-graph-mcp/src/main.rs`. Used by every test in this file
/// so each test exercises the same registry shape the binary ships.
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
    registry
        .register(Box::new(PythonParser::new().expect("PythonParser::new")))
        .expect("register PythonParser");
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
// Mixed C++ + Rust + Go + Python indexing — all four `helper` symbols
// must coexist. Phase 7.7 widened the original 3-language assertion to
// cover the fourth (and final) plugin.
// ---------------------------------------------------------------------

#[tokio::test]
async fn mixed_cpp_rust_go_python_indexes_all_four() {
    let fx = build_indexed(&testdata_mixed_path()).await;
    let g = fx.inner.graph.read();

    let mut cpp_helpers = 0u32;
    let mut rust_helpers = 0u32;
    let mut go_helpers = 0u32;
    let mut python_helpers = 0u32;
    for s in g.search_symbols("helper", None) {
        if s.name != "helper" {
            continue;
        }
        match s.language {
            Language::Cpp => cpp_helpers += 1,
            Language::Rust => rust_helpers += 1,
            Language::Go => go_helpers += 1,
            Language::Python => python_helpers += 1,
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
    assert_eq!(
        python_helpers, 1,
        "expected exactly 1 Python helper symbol, got {python_helpers}",
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
/// fixture (`.cpp` ↔ Cpp, `.rs` ↔ Rust, `.go` ↔ Go, `.py`/`.pyi` ↔
/// Python). Returns `"?"` for any other extension to surface unexpected
/// results loudly rather than silently.
fn language_from_file(file: &str) -> &'static str {
    if file.ends_with(".cpp") || file.ends_with(".cc") || file.ends_with(".cxx") {
        "cpp"
    } else if file.ends_with(".rs") {
        "rust"
    } else if file.ends_with(".go") {
        "go"
    } else if file.ends_with(".py") || file.ends_with(".pyi") {
        "python"
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
async fn search_helper_no_filter_returns_all_four_languages() {
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
        4,
        "no-filter search must return all four helpers; got: {languages:?}",
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
    assert!(
        languages.contains(&"python"),
        "expected python in results, got: {languages:?}",
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

#[tokio::test]
async fn search_helper_language_python_returns_only_python() {
    let fx = build_indexed(&testdata_mixed_path()).await;
    let r = search_symbols(
        &fx.inner.graph,
        SearchSymbolsInput {
            query: Some("helper"),
            language: Some("python"),
            brief: true,
            ..Default::default()
        },
    );
    let body = first_text(&r);
    let languages = result_languages(&body);
    assert_eq!(
        languages,
        vec!["python"],
        "language=python filter must return exactly the Python helper"
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
// Phase 6.6 / 7.7 — 3-way collision fixture across the 4-language
// registry — Rust excluded by design.
//
// A function named `init` exists in C++, Go, AND Python; the
// `(Language, name)`-keyed `SymbolIndex` from Phase 3 must keep them
// isolated during call resolution. Each language's caller resolves to
// its own-language `init`; nothing leaks across language boundaries.
// Inline fixture so it stays close to the assertions and doesn't
// pollute the shared `testdata/mixed/` corpus.
//
// Phase 6.6 shipped this as a 2-way (C++ + Go) regression; Phase 7.7
// widens it to 3-way (C++ + Go + Python) to confirm the SymbolIndex
// keying scales with the fourth plugin. Rust is excluded by design:
// Rust's `init` would parse as an ordinary function and would add no
// new structural pressure (the load-bearing assertion is asymmetric
// isolation across distinct *languages*, not the pair count).
// ---------------------------------------------------------------------

/// Synthesize a TempDir with a C++ file, a Go file, AND a Python file,
/// each declaring a function named `init` plus an in-language caller of
/// that `init`. Returns the indexed fixture so callers can issue tool
/// requests.
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
    // Python side: module-level `def init(): pass` plus a free function
    // `caller_py` that calls `init()`. Edge must resolve to the Python
    // `init` via (Language=Python, name="init"). Python's `init` is
    // distinct from `__init__` (the constructor) — this is just an
    // ordinary function whose name happens to collide with C++/Go's.
    std::fs::write(
        dir.path().join("init_py.py"),
        "def init():\n    pass\n\ndef caller_py():\n    init()\n",
    )
    .expect("write init_py.py");
    build_indexed_from_dir(dir).await
}

#[tokio::test]
async fn search_init_returns_all_three_languages() {
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
    // Exactly three `init` entries — one per language. Each language's
    // symbol_id sits in a distinct (Language, name) bucket of the
    // SymbolIndex. Without per-language keying, the resolver would
    // collapse them into one entry on call lookup.
    let cpp_count = languages.iter().filter(|l| **l == "cpp").count();
    let go_count = languages.iter().filter(|l| **l == "go").count();
    let python_count = languages.iter().filter(|l| **l == "python").count();
    assert_eq!(
        cpp_count, 1,
        "expected exactly 1 C++ init, got languages: {languages:?}"
    );
    assert_eq!(
        go_count, 1,
        "expected exactly 1 Go init, got languages: {languages:?}"
    );
    assert_eq!(
        python_count, 1,
        "expected exactly 1 Python init, got languages: {languages:?}"
    );
}

/// CRITICAL regression: with `init` defined in C++, Go, AND Python, the
/// in-language caller's `Calls` edge must resolve to the same-language
/// `init`. The (Language, name) keying of `SymbolIndex` (Phase 3
/// invariant at `crates/codegraph-lang/src/lib.rs:116`) is what prevents
/// any caller from showing up in another language's init's caller list.
/// If that keying ever degrades to bare `name`, callers from one
/// language would be candidates for another language's init's resolution
/// and these asymmetric assertions would fail.
///
/// The shape is: for each pair (A, B) of languages, A's caller IS in
/// A-init's callers AND IS NOT in B-init's callers. For three languages
/// that's 3 positive assertions plus 6 negative assertions (each pair
/// in both directions), all of which must hold simultaneously.
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
    let python_init_id = g
        .search_symbols("init", None)
        .into_iter()
        .find(|s| s.language == Language::Python && s.name == "init")
        .map(|s| codegraph_core::symbol_id(&s))
        .expect("Python init symbol must exist");
    drop(g);

    /// Pull the `symbol_id` field from each entry in a `get_callers`
    /// response. Local helper — not pulled out to module scope because
    /// only this test consumes the shape. Phase 3: callers response is now
    /// a `Page<CallChain>` envelope with the rows under `results`.
    fn caller_ids(envelope: &serde_json::Value) -> Vec<String> {
        envelope["results"]
            .as_array()
            .expect("results is an array")
            .iter()
            .filter_map(|c| c["symbol_id"].as_str().map(str::to_owned))
            .collect()
    }

    // C++ init's callers — must include caller_cpp and must NOT include
    // either caller_go or caller_py.
    let cpp_callers = callers_or_callees(
        &fx.inner.graph,
        &cpp_init_id,
        Some(1),
        Direction::Callers,
        None,
        None,
    );
    let cpp_arr: serde_json::Value =
        serde_json::from_str(&first_text(&cpp_callers)).expect("get_callers response is JSON");
    let cpp_caller_names = caller_ids(&cpp_arr);
    assert!(
        cpp_caller_names.iter().any(|s| s.ends_with(":caller_cpp")),
        "C++ init must have caller_cpp in its callers, got: {cpp_caller_names:?}"
    );
    assert!(
        !cpp_caller_names.iter().any(|s| s.ends_with(":caller_go")),
        "C++ init must NOT have the Go caller in its callers — \
         (Language, name) SymbolIndex keying broke; got: {cpp_caller_names:?}"
    );
    assert!(
        !cpp_caller_names.iter().any(|s| s.ends_with(":caller_py")),
        "C++ init must NOT have the Python caller in its callers — \
         (Language, name) SymbolIndex keying broke; got: {cpp_caller_names:?}"
    );

    // Go init's callers — must include caller_go and must NOT include
    // caller_cpp or caller_py.
    let go_callers = callers_or_callees(
        &fx.inner.graph,
        &go_init_id,
        Some(1),
        Direction::Callers,
        None,
        None,
    );
    let go_arr: serde_json::Value =
        serde_json::from_str(&first_text(&go_callers)).expect("get_callers response is JSON");
    let go_caller_names = caller_ids(&go_arr);
    assert!(
        go_caller_names.iter().any(|s| s.ends_with(":caller_go")),
        "Go init must have caller_go in its callers, got: {go_caller_names:?}"
    );
    assert!(
        !go_caller_names.iter().any(|s| s.ends_with(":caller_cpp")),
        "Go init must NOT have the C++ caller in its callers — \
         (Language, name) SymbolIndex keying broke; got: {go_caller_names:?}"
    );
    assert!(
        !go_caller_names.iter().any(|s| s.ends_with(":caller_py")),
        "Go init must NOT have the Python caller in its callers — \
         (Language, name) SymbolIndex keying broke; got: {go_caller_names:?}"
    );

    // Python init's callers — must include caller_py and must NOT
    // include caller_cpp or caller_go. Closes the third leg of the
    // 3-way isolation assertion.
    let python_callers = callers_or_callees(
        &fx.inner.graph,
        &python_init_id,
        Some(1),
        Direction::Callers,
        None,
        None,
    );
    let python_arr: serde_json::Value =
        serde_json::from_str(&first_text(&python_callers)).expect("get_callers response is JSON");
    let python_caller_names = caller_ids(&python_arr);
    assert!(
        python_caller_names
            .iter()
            .any(|s| s.ends_with(":caller_py")),
        "Python init must have caller_py in its callers, got: {python_caller_names:?}"
    );
    assert!(
        !python_caller_names
            .iter()
            .any(|s| s.ends_with(":caller_cpp")),
        "Python init must NOT have the C++ caller in its callers — \
         (Language, name) SymbolIndex keying broke; got: {python_caller_names:?}"
    );
    assert!(
        !python_caller_names
            .iter()
            .any(|s| s.ends_with(":caller_go")),
        "Python init must NOT have the Go caller in its callers — \
         (Language, name) SymbolIndex keying broke; got: {python_caller_names:?}"
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
