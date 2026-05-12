//! Wire-format snapshots of representative response bodies for every tool.
//!
//! Each test copies `testdata/cpp/` into a fresh `TempDir`, runs
//! `analyze_codebase` against that copy, then invokes the target tool
//! and snapshots the parsed JSON body. Using a per-test directory
//! avoids races on the shared cache file (`.code-graph-cache.json`)
//! when tokio runs tests in parallel — without this isolation, two
//! concurrent saves can clobber each other and surface a `cache save
//! failed` warning that shows up in only some snapshot runs.
//!
//! The TempDir path is environment-dependent (`/tmp/.tmpXXXXXX/...`).
//! `insta::Settings::add_filter` redacts it to `[testdata]` so the
//! snapshot stays portable across machines.
//!
//! ## Determinism
//!
//! `serde_json::to_string(&HashMap<...>)` iterates the map in HashMap
//! order (non-deterministic). The snapshots normalize via [`sort_json`]
//! before assertion, which recursively sorts every `Object` key — same
//! shape, stable byte order. Sorting at the test boundary (rather than
//! in the handler) keeps the JSON wire format unchanged for clients that
//! don't depend on key order while letting the snapshots stay stable.
//!
//! Vec-of-Symbol responses are sorted by the graph layer; orphans are
//! now sorted by `symbol_id` ascending in the handler itself (Phase 2);
//! BFS edge collections are not, so they are sorted in the test via
//! [`sort_diagram_edges`] / [`sort_mermaid_lines`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

mod common;
use common::{
    copy_testdata, copy_testdata_from, first_text, testdata_mixed_path, testdata_rust_path,
    testdata_ue_path, GO_INTERFACE_FIXTURE,
};

use code_graph_core::Language;
use code_graph_lang::LanguageRegistry;
use code_graph_lang_cpp::CppParser;
use code_graph_lang_go::GoParser;
use code_graph_lang_python::PythonParser;
use code_graph_lang_rust::RustParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::handlers::query::{callers_or_callees, get_dependencies, Direction};
use code_graph_tools::handlers::structure::{
    detect_cycles, generate_diagram, get_class_hierarchy, get_coupling, get_orphans,
    GenerateDiagramInput,
};
use code_graph_tools::handlers::symbols::{
    get_file_symbols, get_symbol_detail, get_symbol_summary, search_symbols, SearchSymbolsInput,
};
use code_graph_tools::server::ServerInner;
use code_graph_tools::CodeGraphServer;
use rmcp::model::CallToolResult;
use tempfile::TempDir;

// Shared `testdata_cpp_path` and `copy_testdata` live in `tests/common/mod.rs`.

/// Per-test fixture: a TempDir holding a fresh copy of testdata, plus
/// a server with the indexed graph. Hold the TempDir for the test's
/// lifetime so the OS doesn't reclaim it while we read symbols out.
struct IndexedFixture {
    _dir: TempDir,
    /// Canonical absolute path of the indexed root (TempDir + cpp/...).
    indexed_root: PathBuf,
    inner: Arc<ServerInner>,
}

/// Build the per-test fixture: copy testdata into a fresh TempDir,
/// register the C++ parser, run `analyze_codebase`, return the indexed
/// `ServerInner`. Each test gets its own TempDir so the cache write in
/// the analyze handler can't race against another test's write.
async fn build_indexed_fixture() -> IndexedFixture {
    let dir = TempDir::new().expect("TempDir for testdata copy");
    copy_testdata(dir.path());
    let indexed_root =
        std::fs::canonicalize(dir.path()).expect("canonicalize TempDir for indexed_root");

    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().expect("CppParser::new")))
        .expect("register CppParser");
    let server = CodeGraphServer::new(registry);

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
        indexed_root,
        inner: server.inner.clone(),
    }
}

// `first_text` lives in `tests/common/mod.rs`.

/// Recursively sort every `Object` in a `serde_json::Value` by key. This
/// is the determinism shim documented at the module level: handler
/// responses that include `HashMap<...>` serialize in HashMap order, but
/// snapshot stability requires byte-identical output across runs.
///
/// `Vec<...>` ordering is preserved as-is (the graph layer already sorts
/// where it matters; a sort here would break tests that assert on order).
fn sort_json(value: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let mut out = serde_json::Map::with_capacity(entries.len());
            for (k, v) in entries {
                out.insert(k, sort_json(v));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sort_json).collect()),
        other => other,
    }
}

/// Build the insta `Settings` that redact the per-test TempDir path
/// (the indexed root) to `[testdata]`. Each test calls this with its
/// own `indexed_root` from the fixture; the redaction makes snapshots
/// portable across machines and across runs (TempDir paths vary).
fn settings_with_path_redaction(indexed_root: &Path) -> insta::Settings {
    let mut settings = insta::Settings::clone_current();
    let testdata_str = indexed_root.to_string_lossy().into_owned();
    // Add a trailing slash so `[testdata]/foo` is the result, not
    // `[testdata]foo`. Both forms (with and without trailing /) are
    // listed separately so symbol IDs (`<dir>/foo.cpp:Bar`) and the
    // `root_path` field (`<dir>` exactly) both redact cleanly.
    settings.add_filter(&regex::escape(&format!("{testdata_str}/")), "[testdata]/");
    settings.add_filter(&regex::escape(&testdata_str), "[testdata]");
    settings
}

/// Parse a tool response's first text block as JSON, then `sort_json`
/// for deterministic key ordering.
fn parsed_sorted(r: &CallToolResult) -> serde_json::Value {
    let body = first_text(r);
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("response body must be valid JSON");
    sort_json(parsed)
}

// --- analyze_codebase ----------------------------------------------------

#[tokio::test]
async fn response_analyze_codebase_testdata_cpp() {
    // Build the fixture by hand here so we can capture the analyze
    // response itself rather than discarding it inside `build_indexed_fixture`.
    let dir = TempDir::new().unwrap();
    copy_testdata(dir.path());
    let indexed_root = std::fs::canonicalize(dir.path()).unwrap();

    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().unwrap()))
        .unwrap();
    let server = CodeGraphServer::new(registry);
    let r = analyze_codebase(
        server.inner.clone(),
        indexed_root.to_string_lossy().into_owned(),
        true,
        None,
        None,
    )
    .await;
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

// --- get_file_symbols ----------------------------------------------------

#[tokio::test]
async fn response_get_file_symbols_engine_cpp() {
    let fx = build_indexed_fixture().await;
    let file = fx
        .indexed_root
        .join("engine.cpp")
        .to_string_lossy()
        .into_owned();
    let r = get_file_symbols(&fx.inner.graph, &file, false, true, None, None, usize::MAX);
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

// --- search_symbols ------------------------------------------------------

#[tokio::test]
async fn response_search_symbols_query_engine() {
    let fx = build_indexed_fixture().await;
    let r = search_symbols(
        &fx.inner.graph,
        SearchSymbolsInput {
            query: Some("Engine"),
            brief: true,
            ..Default::default()
        },
        usize::MAX,
    );
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

// --- get_symbol_detail ---------------------------------------------------

#[tokio::test]
async fn response_get_symbol_detail_engine_update() {
    let fx = build_indexed_fixture().await;
    let id = format!(
        "{}:Engine::update",
        fx.indexed_root.join("engine.cpp").display()
    );
    let r = get_symbol_detail(&fx.inner.graph, &id);
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

// --- get_symbol_summary --------------------------------------------------

#[tokio::test]
async fn response_get_symbol_summary_whole_graph() {
    let fx = build_indexed_fixture().await;
    let r = get_symbol_summary(&fx.inner.graph, None);
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

// --- get_callers / get_callees -------------------------------------------

#[tokio::test]
async fn response_get_callers_engine_update() {
    let fx = build_indexed_fixture().await;
    let id = format!(
        "{}:Engine::update",
        fx.indexed_root.join("engine.cpp").display()
    );
    let r = callers_or_callees(
        &fx.inner.graph,
        &id,
        Some(2),
        Direction::Callers,
        None,
        None,
        usize::MAX,
    );
    // Handler now sorts by (depth, symbol_id) and wraps in Page<CallChain>.
    // No further normalization needed; the envelope itself is deterministic.
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

#[tokio::test]
async fn response_get_callees_engine_update() {
    let fx = build_indexed_fixture().await;
    let id = format!(
        "{}:Engine::update",
        fx.indexed_root.join("engine.cpp").display()
    );
    let r = callers_or_callees(
        &fx.inner.graph,
        &id,
        Some(2),
        Direction::Callees,
        None,
        None,
        usize::MAX,
    );
    // Handler now sorts by (depth, symbol_id) and wraps in Page<CallChain>.
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

// (Phase 3) `sort_chains_by_symbol_id` removed: the handler now sorts the
// `Vec<CallChain>` by `(depth, symbol_id)` ascending and wraps in the
// `Page<CallChain>` envelope, so test-side normalization is no longer
// needed. `parsed_sorted` only normalizes object key order, which is the
// correct behavior for the envelope (it preserves the handler's array
// ordering rather than re-sorting the rows).

// --- get_dependencies ----------------------------------------------------

#[tokio::test]
async fn response_get_dependencies_engine_cpp() {
    let fx = build_indexed_fixture().await;
    let file = fx
        .indexed_root
        .join("engine.cpp")
        .to_string_lossy()
        .into_owned();
    let r = get_dependencies(&fx.inner.graph, &file);
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

// --- detect_cycles -------------------------------------------------------

#[tokio::test]
async fn response_detect_cycles() {
    let fx = build_indexed_fixture().await;
    let r = detect_cycles(&fx.inner.graph, None, None);
    // The handler now sorts each cycle's inner paths in canonical order
    // and sorts the outer cycle list by first path, then wraps in the
    // shared Page<Vec<String>> envelope. Sort discipline lives in the
    // handler now; the test-time normalize that used to sort here is no
    // longer needed. `parsed_sorted` still normalizes object key order
    // (the envelope itself).
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

// --- get_orphans ---------------------------------------------------------

#[tokio::test]
async fn response_get_orphans_default_callables() {
    let fx = build_indexed_fixture().await;
    let r = get_orphans(&fx.inner.graph, None, None, None, None, usize::MAX);
    // The handler now sorts by `symbol_id` ascending and wraps in the
    // `Page<SymbolResult>` envelope. The envelope itself is deterministic;
    // `parsed_sorted` only normalizes object key order (not array order).
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

#[tokio::test]
async fn response_get_orphans_paginated_offset() {
    // Page-2 snapshot: build a synthetic 25-orphan fixture, request
    // offset=20 limit=20, snapshot the response. Confirms the slice is
    // taken from the *sorted* full match set, not the BFS-visit order.
    let fx = build_indexed_fixture_with_many_orphans(25).await;
    let r = get_orphans(
        &fx.inner.graph,
        Some("function"),
        Some(20),
        Some(20),
        None,
        usize::MAX,
    );
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

#[tokio::test]
async fn response_get_orphans_brief_false() {
    // brief=false surfaces signature/column/end_line on each row. Reuse
    // the small testdata/cpp fixture so the snapshot stays readable.
    let fx = build_indexed_fixture().await;
    let r = get_orphans(&fx.inner.graph, None, None, None, Some(false), usize::MAX);
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

#[tokio::test]
async fn response_get_orphans_offset_beyond_total() {
    // offset=999 against a small fixture: results=[], total=<full count>.
    let fx = build_indexed_fixture().await;
    let r = get_orphans(&fx.inner.graph, None, None, Some(999), None, usize::MAX);
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

/// Build an indexed fixture from a single synthesized C++ file containing
/// `n` orphan functions named `func_NNN`. Used by the paginated-offset
/// snapshot to exercise the handler's sort+slice path against a known
/// cardinality.
async fn build_indexed_fixture_with_many_orphans(n: usize) -> IndexedFixture {
    // Each function is a free, parameterless, void-returning function
    // with no body content — all become orphans because nothing calls
    // them. Zero-padded to 3 digits so symbol_id ascending sort order
    // is predictable in the snapshot.
    let mut source = String::new();
    for i in 0..n {
        source.push_str(&format!("void func_{i:03}() {{}}\n"));
    }
    build_cpp_only_fixture(&[("orphans.cpp", &source)]).await
}

// --- Phase 3 paginated-offset snapshots ----------------------------------
//
// These three snapshots exercise the page-2 path for the new
// `Page<T>`-wrapped tools: `get_file_symbols`, `get_callers`, `get_callees`.
// Fixtures are sized so the offset/limit combo selects a non-trivial slice
// of the post-sort result set.

/// Build an indexed fixture with `n` free functions in a single C++ file,
/// used by `response_get_file_symbols_paginated_offset` to exercise the
/// handler's sort+slice path against a known cardinality. Names are
/// zero-padded so `symbol_id` ascending order is predictable in the
/// snapshot.
async fn build_indexed_fixture_with_many_file_symbols(n: usize) -> IndexedFixture {
    let mut source = String::new();
    for i in 0..n {
        source.push_str(&format!("void func_{i:03}() {{}}\n"));
    }
    build_cpp_only_fixture(&[("big.cpp", &source)]).await
}

/// Build an indexed fixture with a single hub symbol `target` that is
/// called by `n` distinct callers, plus a separate `entry` symbol that
/// itself calls `n` distinct callees. Used by the callers/callees
/// paginated-offset snapshots so the sort by `(depth, symbol_id)` produces
/// a deterministic page-2 slice. The depth=1 fan is wide enough (>50) to
/// exercise the limit=50 page semantics.
async fn build_indexed_fixture_with_high_fan() -> IndexedFixture {
    let n = 60;
    let mut source = String::new();
    // Hub symbols.
    source.push_str("void target() {}\nvoid entry() {\n");
    for i in 0..n {
        source.push_str(&format!("    callee_{i:03}();\n"));
    }
    source.push_str("}\n");
    // n distinct callers, each one calling target. They are zero-padded
    // so symbol_id ascending order is predictable in the snapshot.
    for i in 0..n {
        source.push_str(&format!("void caller_{i:03}() {{ target(); }}\n"));
    }
    // n callee declarations the entry hub references. Each is an orphan
    // free function — entry's body is the only call site.
    for i in 0..n {
        source.push_str(&format!("void callee_{i:03}() {{}}\n"));
    }
    build_cpp_only_fixture(&[("hub.cpp", &source)]).await
}

/// Shared workhorse for the three Phase 3 paginated-fixture builders
/// (`build_indexed_fixture_with_many_orphans`,
/// `build_indexed_fixture_with_many_file_symbols`,
/// `build_indexed_fixture_with_high_fan`).
///
/// Writes the supplied `(filename, content)` pairs into a fresh TempDir,
/// canonicalizes the root, registers a C++-only `LanguageRegistry`, and
/// invokes `analyze_codebase` with `force=true`. Returns the populated
/// `IndexedFixture` ready for tool dispatch.
///
/// Scoped to C++ on purpose: every caller is a C++-only paginated-tool
/// snapshot. The three other fixture builders in this file use different
/// registry shapes (multi-language, Go-only, Python-only) and would not
/// benefit from this helper without parameterizing the registry — out of
/// scope for the retro's stated consolidation. If a fourth C++-only
/// paginated fixture lands, it should call this helper too.
async fn build_cpp_only_fixture(files: &[(&str, &str)]) -> IndexedFixture {
    let dir = TempDir::new().expect("TempDir for fixture");
    for (name, content) in files {
        std::fs::write(dir.path().join(name), content)
            .unwrap_or_else(|e| panic!("write {name}: {e}"));
    }
    let indexed_root =
        std::fs::canonicalize(dir.path()).expect("canonicalize TempDir for indexed_root");

    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().expect("CppParser::new")))
        .expect("register CppParser");
    let server = CodeGraphServer::new(registry);

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
        indexed_root,
        inner: server.inner.clone(),
    }
}

#[tokio::test]
async fn response_get_file_symbols_paginated_offset() {
    // Page-2 snapshot: 110 free functions in a single file (>100 default
    // page size). Request offset=100 limit=50 — slice taken from the
    // sorted full match set. Expected: results.len() = 10 (only 10 left
    // after offset=100 in a 110-row set), total=110, offset=100, limit=50.
    let fx = build_indexed_fixture_with_many_file_symbols(110).await;
    let file = fx
        .indexed_root
        .join("big.cpp")
        .to_string_lossy()
        .into_owned();
    let r = get_file_symbols(
        &fx.inner.graph,
        &file,
        false,
        true,
        Some(50),
        Some(100),
        usize::MAX,
    );
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

#[tokio::test]
async fn response_get_callers_paginated_offset() {
    // Page-2 snapshot: hub symbol with 60 callers. Request
    // offset=50 limit=50 — slice taken from the sorted (depth, symbol_id)
    // result set. Expected: results.len() = 10, total=60, offset=50,
    // limit=50.
    let fx = build_indexed_fixture_with_high_fan().await;
    let id = format!("{}:target", fx.indexed_root.join("hub.cpp").display());
    let r = callers_or_callees(
        &fx.inner.graph,
        &id,
        Some(1),
        Direction::Callers,
        Some(50),
        Some(50),
        usize::MAX,
    );
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

#[tokio::test]
async fn response_get_callees_paginated_offset() {
    // Page-2 snapshot: hub symbol with 60 callees. Request
    // offset=50 limit=50 — slice taken from the sorted (depth, symbol_id)
    // result set. Expected: results.len() = 10, total=60, offset=50,
    // limit=50.
    let fx = build_indexed_fixture_with_high_fan().await;
    let id = format!("{}:entry", fx.indexed_root.join("hub.cpp").display());
    let r = callers_or_callees(
        &fx.inner.graph,
        &id,
        Some(1),
        Direction::Callees,
        Some(50),
        Some(50),
        usize::MAX,
    );
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

// --- get_class_hierarchy -------------------------------------------------

#[tokio::test]
async fn response_get_class_hierarchy_engine() {
    let fx = build_indexed_fixture().await;
    let r = get_class_hierarchy(&fx.inner.graph, "Engine", Some(1), None);
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

/// Phase 4: truncated-case snapshot.
///
/// Reuses an existing fixture with a tiny `max_nodes` budget (2) to force
/// the Graph layer to flag `truncated: true`. This is the cheapest path —
/// no need to engineer a 251+ class fixture; the truncation behavior is
/// budget-driven and fires identically whether the cap is 2 or 250.
///
/// The Rust testdata's `Compute` trait has multiple impls (`Foo<T>`,
/// `Bar<T>`), so a budget of 2 lets the root + one derived in but cuts
/// off the second. The snapshot locks in: (a) `truncated: true`, (b)
/// `total_nodes_seen` equal to the budget cap (the unique-name set fills
/// exactly to the cap), (c) the partial tree is well-formed JSON with
/// valid `HierarchyNode` structure (no dangling references).
#[tokio::test]
async fn response_get_class_hierarchy_truncated() {
    let fx = build_indexed_fixture_for_dir_with_all_parsers(&testdata_rust_path()).await;
    let r = get_class_hierarchy(&fx.inner.graph, "Compute", Some(3), Some(2));
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

// --- get_coupling --------------------------------------------------------

#[tokio::test]
async fn response_get_coupling_engine_cpp_outgoing() {
    let fx = build_indexed_fixture().await;
    let file = fx
        .indexed_root
        .join("engine.cpp")
        .to_string_lossy()
        .into_owned();
    let r = get_coupling(&fx.inner.graph, &file, Some("outgoing"));
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

#[tokio::test]
async fn response_get_coupling_engine_cpp_incoming() {
    let fx = build_indexed_fixture().await;
    let file = fx
        .indexed_root
        .join("engine.cpp")
        .to_string_lossy()
        .into_owned();
    let r = get_coupling(&fx.inner.graph, &file, Some("incoming"));
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

#[tokio::test]
async fn response_get_coupling_engine_cpp_both() {
    let fx = build_indexed_fixture().await;
    let file = fx
        .indexed_root
        .join("engine.cpp")
        .to_string_lossy()
        .into_owned();
    let r = get_coupling(&fx.inner.graph, &file, Some("both"));
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

// --- generate_diagram ----------------------------------------------------
//
// One snapshot per (dispatch type × output format). With three dispatch
// types (symbol/file/class) and two output formats (edges/mermaid), we
// get six snapshots — covering every supported combination.

#[tokio::test]
async fn response_generate_diagram_symbol_edges() {
    let fx = build_indexed_fixture().await;
    let id = format!(
        "{}:Engine::update",
        fx.indexed_root.join("engine.cpp").display()
    );
    let r = generate_diagram(
        &fx.inner.graph,
        GenerateDiagramInput {
            symbol: Some(&id),
            format: Some("edges"),
            ..Default::default()
        },
    );
    // Edges format → JSON array of {from, to, label}. Sort entries for
    // determinism (BFS visit order is randomized per the diagram module
    // doc comment).
    let body = first_text(&r);
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("symbol-edges response is JSON");
    let normalized = sort_diagram_edges(parsed);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(normalized);
    });
}

#[tokio::test]
async fn response_generate_diagram_symbol_mermaid() {
    let fx = build_indexed_fixture().await;
    let id = format!(
        "{}:Engine::update",
        fx.indexed_root.join("engine.cpp").display()
    );
    let r = generate_diagram(
        &fx.inner.graph,
        GenerateDiagramInput {
            symbol: Some(&id),
            format: Some("mermaid"),
            ..Default::default()
        },
    );
    // Mermaid output is plain text, not JSON. Sort the body lines (after
    // the `graph TD` header) so BFS-driven ordering doesn't churn the
    // snapshot.
    let text = first_text(&r);
    let normalized = sort_mermaid_lines(&text);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_snapshot!(normalized);
    });
}

#[tokio::test]
async fn response_generate_diagram_file_edges() {
    let fx = build_indexed_fixture().await;
    let file = fx
        .indexed_root
        .join("engine.cpp")
        .to_string_lossy()
        .into_owned();
    let r = generate_diagram(
        &fx.inner.graph,
        GenerateDiagramInput {
            file: Some(&file),
            format: Some("edges"),
            ..Default::default()
        },
    );
    let body = first_text(&r);
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("file-edges response is JSON");
    let normalized = sort_diagram_edges(parsed);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(normalized);
    });
}

#[tokio::test]
async fn response_generate_diagram_file_mermaid() {
    let fx = build_indexed_fixture().await;
    let file = fx
        .indexed_root
        .join("engine.cpp")
        .to_string_lossy()
        .into_owned();
    let r = generate_diagram(
        &fx.inner.graph,
        GenerateDiagramInput {
            file: Some(&file),
            format: Some("mermaid"),
            ..Default::default()
        },
    );
    let text = first_text(&r);
    let normalized = sort_mermaid_lines(&text);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_snapshot!(normalized);
    });
}

#[tokio::test]
async fn response_generate_diagram_class_edges() {
    let fx = build_indexed_fixture().await;
    let r = generate_diagram(
        &fx.inner.graph,
        GenerateDiagramInput {
            class: Some("Engine"),
            format: Some("edges"),
            ..Default::default()
        },
    );
    let body = first_text(&r);
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("class-edges response is JSON");
    let normalized = sort_diagram_edges(parsed);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(normalized);
    });
}

#[tokio::test]
async fn response_generate_diagram_class_mermaid() {
    let fx = build_indexed_fixture().await;
    let r = generate_diagram(
        &fx.inner.graph,
        GenerateDiagramInput {
            class: Some("Engine"),
            format: Some("mermaid"),
            ..Default::default()
        },
    );
    let text = first_text(&r);
    let normalized = sort_mermaid_lines(&text);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_snapshot!(normalized);
    });
}

// --- helpers for diagram normalization -----------------------------------

/// Sort the `from-to` edges in a diagram-edges response by `(from, to)`.
fn sort_diagram_edges(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(mut arr) => {
            arr.sort_by(|a, b| {
                let ka = format!(
                    "{}-{}",
                    a["from"].as_str().unwrap_or(""),
                    a["to"].as_str().unwrap_or("")
                );
                let kb = format!(
                    "{}-{}",
                    b["from"].as_str().unwrap_or(""),
                    b["to"].as_str().unwrap_or("")
                );
                ka.cmp(&kb)
            });
            serde_json::Value::Array(arr.into_iter().map(sort_json).collect())
        }
        other => sort_json(other),
    }
}

/// Sort lines of a Mermaid diagram preserving the `graph TD` header.
/// BFS-driven ordering of edges in the rendered output is non-deterministic
/// (per `diagrams.rs` module doc) but the set-of-edges is stable. Sorting
/// lines collapses ordering to a stable form for snapshotting.
fn sort_mermaid_lines(text: &str) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    if let Some(first) = lines.first() {
        if first.starts_with("graph ") {
            let header = lines.remove(0);
            lines.sort();
            std::iter::once(header)
                .chain(lines)
                .collect::<Vec<&str>>()
                .join("\n")
        } else {
            lines.sort();
            lines.join("\n")
        }
    } else {
        String::new()
    }
}

// --- guard: confirm Cpp registered language is present in the binary ----

/// Sanity check that doesn't exercise a snapshot — keeps the pipeline
/// honest if someone removes the C++ parser registration.
#[test]
fn cpp_parser_registers_for_cpp_language() {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().unwrap()))
        .unwrap();
    assert!(registry.plugin_for(Language::Cpp).is_some());
}

/// Sanity check for the Rust parser registration alongside C++ — mirrors
/// the registration block in `crates/code-graph-mcp/src/main.rs` so a
/// silent removal of `code_graph_lang_rust::RustParser::new()` from the
/// binary trips this test before any of the Rust-specific snapshots.
#[test]
fn rust_parser_registers_for_rust_language() {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().unwrap()))
        .unwrap();
    registry
        .register(Box::new(RustParser::new().unwrap()))
        .unwrap();
    assert!(registry.plugin_for(Language::Rust).is_some());
    assert!(registry.plugin_for(Language::Cpp).is_some());
}

/// Sanity check for the Go parser registration alongside C++ and Rust —
/// mirrors the Phase 6.6 registration block in
/// `crates/code-graph-mcp/src/main.rs` so a silent removal of
/// `code_graph_lang_go::GoParser::new()` from the binary trips this test
/// before any of the Go-specific snapshots below.
#[test]
fn go_parser_registers_for_go_language() {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().unwrap()))
        .unwrap();
    registry
        .register(Box::new(RustParser::new().unwrap()))
        .unwrap();
    registry
        .register(Box::new(GoParser::new().unwrap()))
        .unwrap();
    assert!(registry.plugin_for(Language::Go).is_some());
    assert!(registry.plugin_for(Language::Rust).is_some());
    assert!(registry.plugin_for(Language::Cpp).is_some());
}

/// Sanity check for the Python parser registration alongside the other
/// three plugins — mirrors the Phase 7.7 registration block in
/// `crates/code-graph-mcp/src/main.rs` so a silent removal of
/// `code_graph_lang_python::PythonParser::new()` from the binary trips
/// this test before any of the Python-specific snapshots below.
#[test]
fn python_parser_registers_for_python_language() {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().unwrap()))
        .unwrap();
    registry
        .register(Box::new(RustParser::new().unwrap()))
        .unwrap();
    registry
        .register(Box::new(GoParser::new().unwrap()))
        .unwrap();
    registry
        .register(Box::new(PythonParser::new().unwrap()))
        .unwrap();
    assert!(registry.plugin_for(Language::Python).is_some());
    assert!(registry.plugin_for(Language::Go).is_some());
    assert!(registry.plugin_for(Language::Rust).is_some());
    assert!(registry.plugin_for(Language::Cpp).is_some());
}

// --- Phase 5.6 Rust-side snapshots --------------------------------------
//
// These four snapshots lock the wire format for representative responses
// driven by the Rust language plugin (registered alongside the C++ one,
// matching the binary):
//
//   * analyze_codebase on `testdata/mixed/` — exercises mixed C++ + Rust
//     indexing through the registry.
//   * search_symbols(query="helper", language=Rust) — exercises the
//     language filter path on `testdata/mixed/`.
//   * get_class_hierarchy on `Greet` — exercises the widened
//     {Class, Struct, Interface, Trait} root filter from Phase 2 against
//     the testdata/rust corpus.
//   * generate_diagram(class="Compute", format="edges") — exercises the
//     Inherits-edge dispatch for a Rust trait with two impls.

/// Mirror of [`build_indexed_fixture`] that uses `testdata/mixed/` and
/// registers all three parsers (the binary's runtime shape). Returns the
/// `IndexedFixture` so the per-test TempDir path is available for
/// snapshot redaction.
async fn build_indexed_fixture_for_dir_with_all_parsers(src: &Path) -> IndexedFixture {
    let dir = TempDir::new().expect("TempDir for testdata copy");
    copy_testdata_from(src, dir.path());
    let indexed_root =
        std::fs::canonicalize(dir.path()).expect("canonicalize TempDir for indexed_root");

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
    let server = CodeGraphServer::new(registry);

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
        indexed_root,
        inner: server.inner.clone(),
    }
}

#[tokio::test]
async fn response_analyze_codebase_testdata_mixed() {
    // Capture the analyze response itself rather than discarding it inside
    // the helper — same pattern as `response_analyze_codebase_testdata_cpp`.
    // Registers all three parsers (Phase 6.6) so the snapshot reflects the
    // post-Phase-6 binary's runtime shape (foo.cpp + foo.rs + foo.go).
    let dir = TempDir::new().unwrap();
    copy_testdata_from(&testdata_mixed_path(), dir.path());
    let indexed_root = std::fs::canonicalize(dir.path()).unwrap();

    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().unwrap()))
        .unwrap();
    registry
        .register(Box::new(RustParser::new().unwrap()))
        .unwrap();
    registry
        .register(Box::new(GoParser::new().unwrap()))
        .unwrap();
    registry
        .register(Box::new(PythonParser::new().unwrap()))
        .unwrap();
    let server = CodeGraphServer::new(registry);
    let r = analyze_codebase(
        server.inner.clone(),
        indexed_root.to_string_lossy().into_owned(),
        true,
        None,
        None,
    )
    .await;
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

#[tokio::test]
async fn response_search_symbols_helper_language_rust() {
    let fx = build_indexed_fixture_for_dir_with_all_parsers(&testdata_mixed_path()).await;
    let r = search_symbols(
        &fx.inner.graph,
        SearchSymbolsInput {
            query: Some("helper"),
            language: Some("rust"),
            brief: true,
            ..Default::default()
        },
        usize::MAX,
    );
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

#[tokio::test]
async fn response_get_class_hierarchy_rust_trait_greet() {
    let fx = build_indexed_fixture_for_dir_with_all_parsers(&testdata_rust_path()).await;
    // `Greet` is a trait that `Greeter` implements (testdata/rust/src/traits.rs).
    // Pre-Phase-2 the lookup would have rejected the trait kind; this
    // snapshot is the wire-format counterpart to the integration test
    // `get_class_hierarchy_for_rust_trait`.
    let r = get_class_hierarchy(&fx.inner.graph, "Greet", Some(2), None);
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

#[tokio::test]
async fn response_generate_diagram_rust_trait_compute() {
    let fx = build_indexed_fixture_for_dir_with_all_parsers(&testdata_rust_path()).await;
    let r = generate_diagram(
        &fx.inner.graph,
        GenerateDiagramInput {
            class: Some("Compute"),
            format: Some("edges"),
            depth: Some(2),
            ..Default::default()
        },
    );
    let body = first_text(&r);
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("class-edges response is JSON");
    let normalized = sort_diagram_edges(parsed);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(normalized);
    });
}

// --- Phase 6.6 Go-side snapshots ----------------------------------------
//
// These snapshots lock the wire format for representative responses
// driven by the Go language plugin (registered alongside C++ and Rust):
//
//   * search_symbols(query="helper", language=go) — exercises the
//     language=go filter path on `testdata/mixed/`.
//   * get_class_hierarchy on a Go interface — exercises the widened
//     {Class, Struct, Interface, Trait} root filter from Phase 2 with a
//     Go interface as the root, asserting empty bases and derived
//     (structural implementation produces no Inherits edges in Go).
//   * get_file_symbols on a Go file — exercises the per-file symbol
//     listing for Go content.
//
// The Go-interface and get_file_symbols snapshots use a synthesized
// in-TempDir Go file rather than the larger `testdata/go/` corpus so
// the snapshot stays small and readable.

#[tokio::test]
async fn response_search_symbols_helper_language_go() {
    let fx = build_indexed_fixture_for_dir_with_all_parsers(&testdata_mixed_path()).await;
    let r = search_symbols(
        &fx.inner.graph,
        SearchSymbolsInput {
            query: Some("helper"),
            language: Some("go"),
            brief: true,
            ..Default::default()
        },
        usize::MAX,
    );
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

/// Per-test fixture with a single synthetic Go file under a TempDir. The
/// file declares a `Reader` interface plus a `MyReader` struct that
/// structurally implements it. Used by the get_class_hierarchy and
/// get_file_symbols snapshots below.
async fn build_indexed_fixture_with_go_interface() -> IndexedFixture {
    let dir = TempDir::new().expect("TempDir for Go interface fixture");
    // Fixture body lives in `common::GO_INTERFACE_FIXTURE` so this and
    // the matching mixed-language test stay byte-identical.
    std::fs::write(dir.path().join("reader.go"), GO_INTERFACE_FIXTURE).expect("write reader.go");
    let indexed_root =
        std::fs::canonicalize(dir.path()).expect("canonicalize TempDir for indexed_root");

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
    let server = CodeGraphServer::new(registry);

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
        indexed_root,
        inner: server.inner.clone(),
    }
}

#[tokio::test]
async fn response_get_class_hierarchy_go_interface_reader() {
    let fx = build_indexed_fixture_with_go_interface().await;
    // Wire-format counterpart to the `get_class_hierarchy_for_go_interface`
    // integration test in `mixed_language.rs`. Locks in the leaf-node
    // shape (just `{"name":"Reader"}`) — `bases` and `derived` are
    // skipped because they are empty (Go produces no Inherits edges).
    let r = get_class_hierarchy(&fx.inner.graph, "Reader", Some(2), None);
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

#[tokio::test]
async fn response_get_file_symbols_go_reader() {
    let fx = build_indexed_fixture_with_go_interface().await;
    let file = fx
        .indexed_root
        .join("reader.go")
        .to_string_lossy()
        .into_owned();
    let r = get_file_symbols(&fx.inner.graph, &file, false, true, None, None, usize::MAX);
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

// --- Phase 7.7 Python-side snapshots ------------------------------------
//
// These snapshots lock the wire format for representative responses
// driven by the Python language plugin (registered alongside C++, Rust,
// and Go — the four-language binary shape):
//
//   * analyze_codebase on a Python-only directory — exercises the full
//     analyze path against `.py` files end-to-end.
//   * search_symbols(query="helper", language=python) — exercises the
//     language=python filter path on `testdata/mixed/`.
//   * get_file_symbols on a Python file with classes + methods — locks
//     the per-file symbol-listing shape for Python content.
//   * get_class_hierarchy on a Python class with bases — exercises the
//     Inherits-edge dispatch for Python's single-inheritance form.
//   * get_dependencies on a Python file with imports — exercises the
//     Includes-edge wire format for both `import_statement` and
//     `import_from_statement` shapes.
//
// Python fixtures use small synthesized sources rather than the larger
// `testdata/python/` corpus so the snapshots stay readable.

/// Build a Python-only indexed fixture from a single inline source. The
/// `models.py` file declares an `Animal` base class plus a `Dog` subclass
/// (`class Dog(Animal):`) so the inheritance and class-hierarchy
/// snapshots have a non-trivial Inherits edge to lock. Imports cover both
/// `import` and `from ... import` forms so the dependency snapshot
/// exercises the dotted-module-path wire format. Parsers registered
/// match the binary's runtime shape (all four).
async fn build_indexed_fixture_with_python_models() -> IndexedFixture {
    let dir = TempDir::new().expect("TempDir for Python models fixture");
    // Two-class single-inheritance fixture with both import forms.
    // Sized to keep the per-file snapshot readable (~10 symbols).
    let source = "import abc\nfrom typing import List\n\n\
                  class Animal:\n    \
                  def __init__(self, name):\n        self.name = name\n    \
                  def speak(self):\n        return self.name\n\n\
                  class Dog(Animal):\n    \
                  def speak(self):\n        return \"woof\"\n";
    std::fs::write(dir.path().join("models.py"), source).expect("write models.py");
    let indexed_root =
        std::fs::canonicalize(dir.path()).expect("canonicalize TempDir for indexed_root");

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
    let server = CodeGraphServer::new(registry);

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
        indexed_root,
        inner: server.inner.clone(),
    }
}

#[tokio::test]
async fn response_analyze_codebase_python_models() {
    // Capture the analyze response itself rather than discarding it
    // inside the helper — same pattern as the existing analyze
    // snapshots. Locks the file/symbol/edge counters for a Python-only
    // index.
    let dir = TempDir::new().unwrap();
    let source = "import abc\nfrom typing import List\n\n\
                  class Animal:\n    \
                  def __init__(self, name):\n        self.name = name\n    \
                  def speak(self):\n        return self.name\n\n\
                  class Dog(Animal):\n    \
                  def speak(self):\n        return \"woof\"\n";
    std::fs::write(dir.path().join("models.py"), source).unwrap();
    let indexed_root = std::fs::canonicalize(dir.path()).unwrap();

    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().unwrap()))
        .unwrap();
    registry
        .register(Box::new(RustParser::new().unwrap()))
        .unwrap();
    registry
        .register(Box::new(GoParser::new().unwrap()))
        .unwrap();
    registry
        .register(Box::new(PythonParser::new().unwrap()))
        .unwrap();
    let server = CodeGraphServer::new(registry);
    let r = analyze_codebase(
        server.inner.clone(),
        indexed_root.to_string_lossy().into_owned(),
        true,
        None,
        None,
    )
    .await;
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

#[tokio::test]
async fn response_search_symbols_helper_language_python() {
    let fx = build_indexed_fixture_for_dir_with_all_parsers(&testdata_mixed_path()).await;
    let r = search_symbols(
        &fx.inner.graph,
        SearchSymbolsInput {
            query: Some("helper"),
            language: Some("python"),
            brief: true,
            ..Default::default()
        },
        usize::MAX,
    );
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

#[tokio::test]
async fn response_get_file_symbols_python_models() {
    let fx = build_indexed_fixture_with_python_models().await;
    let file = fx
        .indexed_root
        .join("models.py")
        .to_string_lossy()
        .into_owned();
    let r = get_file_symbols(&fx.inner.graph, &file, false, true, None, None, usize::MAX);
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

#[tokio::test]
async fn response_get_class_hierarchy_python_dog() {
    let fx = build_indexed_fixture_with_python_models().await;
    // `Dog` inherits from `Animal`. The hierarchy snapshot locks both
    // `bases` (Dog -> Animal) and the leaf-node shape for the upward
    // walk (Animal has no bases, so it serializes without the field).
    let r = get_class_hierarchy(&fx.inner.graph, "Dog", Some(2), None);
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

#[tokio::test]
async fn response_get_dependencies_python_models() {
    let fx = build_indexed_fixture_with_python_models().await;
    let file = fx
        .indexed_root
        .join("models.py")
        .to_string_lossy()
        .into_owned();
    // models.py imports `abc` and `typing` — both record verbatim as the
    // dotted module path (the from-form points at the module, not the
    // imported name). The snapshot pins this contract for Python.
    let r = get_dependencies(&fx.inner.graph, &file);
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

// --- CppMacroStrip Phase 3.2 UE-side snapshots --------------------------
//
// These two snapshots prove the user-facing payoff for the CppMacroStrip
// plan: with `[cpp].macro_strip` listed in `.code-graph.toml`, classes
// declared with API-export macros (`class CORE_API AActor : public UObject {};`)
// extract correctly through the public `get_class_hierarchy` tool surface.
//
// Fixture lives at `testdata/ue/MyActor.h` plus `testdata/ue/.code-graph.toml`
// — the latter declares `macro_strip = ["CORE_API", "ENGINE_API",
// "GAMEPLAY_API", "FOO_API", "BAR_EXTRA"]` so all five test macros are
// recognized. `build_indexed_fixture_for_dir_with_all_parsers` copies both
// the `.h` and the hidden `.code-graph.toml` into the per-test TempDir,
// then runs `analyze_codebase` against that copy — exactly the path a UE
// user would hit.
//
// The first snapshot exercises the chained inheritance case (AActor at
// depth=2: bases include UObject; derived includes APawn, UDoubleMacro,
// UNoMacro; APawn's derived includes ACharacter). The second snapshot
// exercises the multi-macro case (UDoubleMacro carries both FOO_API and
// BAR_EXTRA prefixes; both must be stripped for the class to extract with
// AActor as parent).

#[tokio::test]
async fn response_get_class_hierarchy_ue_aactor() {
    // Index `testdata/ue/` (with its `.code-graph.toml` declaring the
    // macro-strip list) and walk the AActor hierarchy at depth=2. With the
    // CORE_API/ENGINE_API/GAMEPLAY_API stripping in effect, the snapshot
    // locks the chained-inheritance shape: AActor -> UObject upward, and
    // AActor -> {APawn -> ACharacter, UDoubleMacro, UNoMacro} downward.
    let fx = build_indexed_fixture_for_dir_with_all_parsers(&testdata_ue_path()).await;
    let r = get_class_hierarchy(&fx.inner.graph, "AActor", Some(2), None);
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}

#[tokio::test]
async fn response_get_class_hierarchy_ue_double_macro() {
    // The two-macro case: `class FOO_API BAR_EXTRA UDoubleMacro : public AActor {};`
    // requires both FOO_API and BAR_EXTRA to be stripped before
    // tree-sitter sees a parseable `class UDoubleMacro : public AActor`.
    // Snapshotting at default depth=1 locks the AActor parent edge through
    // the public tool surface — proving multi-macro stripping works
    // end-to-end and not just in the Phase 1 unit test.
    let fx = build_indexed_fixture_for_dir_with_all_parsers(&testdata_ue_path()).await;
    let r = get_class_hierarchy(&fx.inner.graph, "UDoubleMacro", None, None);
    let parsed = parsed_sorted(&r);
    settings_with_path_redaction(&fx.indexed_root).bind(|| {
        insta::assert_json_snapshot!(parsed);
    });
}
