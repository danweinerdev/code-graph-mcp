//! PathNormalization Phase 3.4 — end-to-end verification that the four
//! file-taking MCP tools (`get_file_symbols`, `get_coupling`,
//! `get_dependencies`, `generate_diagram(file=…)`) resolve short-form path
//! arguments after a real `analyze_codebase` round-trip.
//!
//! On Linux this is effectively a non-regression check: paths are already
//! canonical and `normalize_user_path` is a near-identity transform, so the
//! test's value is "the handler wrap from 3.1/3.2 didn't break the happy
//! path." On Windows this same test becomes the load-bearing fix
//! verification: the indexer stores short-form `D:\…` keys (Phase 1),
//! `normalize_user_path` strips a user-supplied `\\?\D:\…` prefix on
//! lookup, and the handlers find the records.
//!
//! Fixture choice (deviation from the plan's "two Rust files" recommendation):
//! we use a Rust fixture with `mod util;` + `use util::helper;` + a call to
//! `helper()` (NOT the plan's `util::helper()` scoped call). The scoped
//! form would extract a `Calls` edge with `to="util::helper"`, which the
//! default scope-aware resolver looks up under the key
//! `(Rust, "util::helper")` and fails to find — leaving the edge
//! unresolved and `get_coupling` empty. The unqualified form extracts
//! `to="helper"` which resolves correctly to the function symbol in
//! `util.rs`, producing a real cross-file `Calls` edge. The `use
//! util::helper;` statement still gives us an `Includes` edge so
//! `get_dependencies` and `generate_diagram(file=…)` have non-empty
//! results. See the `extract_calls` doc and `default_scope_aware_resolve`
//! in `code-graph-lang` for the resolution rule.
//!
//! Related-test pointer: the cache-migration anti-regression coverage that
//! the PathNormalization design's Testing Strategy §7 originally placed in
//! this file lives instead in `crates/code-graph-graph/src/persist.rs::tests`
//! as `cache_migration_strips_all_path_locations_end_to_end` (per-field
//! assertions) and `cache_migration_preserves_cross_field_consistency`
//! (end-to-end lookup). The split is intentional — those tests construct a
//! synthetic `GraphCache` directly to exercise `Graph::load`'s
//! `simplify_cache` wiring, which requires intra-crate access to private
//! cache types not exposed across the `code-graph-tools` boundary.

use std::sync::Arc;

use code_graph_lang::LanguageRegistry;
use code_graph_lang_rust::RustParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::handlers::query::get_dependencies;
use code_graph_tools::handlers::structure::{generate_diagram, get_coupling, GenerateDiagramInput};
use code_graph_tools::handlers::symbols::get_file_symbols;
use code_graph_tools::handlers::NO_BYTE_BUDGET;
use code_graph_tools::server::ServerInner;
use code_graph_tools::CodeGraphServer;
use tempfile::TempDir;

mod common;
use common::first_text;

/// Fresh server with only the Rust language plugin registered — the
/// fixture is Rust-only, so we keep the registry minimal to avoid pulling
/// every parser's tree-sitter grammar into the test compile.
fn rust_server() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(RustParser::new().expect("RustParser::new")))
        .expect("register RustParser");
    CodeGraphServer::new(registry)
}

/// Build a tempdir containing the cross-file Rust fixture, run
/// `analyze_codebase` against it, and return the indexed server alongside
/// the canonical paths the test will exercise.
struct Indexed {
    _dir: TempDir,
    inner: Arc<ServerInner>,
    /// `root_path` field captured from the `analyze_codebase` response. On
    /// Linux this is the canonical absolute path; on Windows it should be
    /// the short form (no `\\?\` prefix) after Phase 1.
    root_path: String,
    /// Absolute path to `src/main.rs` inside the tempdir, used as the
    /// short-form `file` argument for each of the 4 tools under test.
    main_rs: String,
    /// Absolute path to `src/util.rs` — the sibling we expect the cross-
    /// file edges to point at.
    util_rs: String,
}

async fn build_indexed() -> Indexed {
    let dir = TempDir::new().expect("TempDir for indexed fixture");

    // Cross-file fixture:
    //   src/main.rs — `mod util; use util::helper; fn main() { helper(); }`
    //   src/util.rs — `pub fn helper() {}`
    //
    // Edges produced after `resolve_all_edges`:
    //   - (no edge from `mod util;` — module declarations are namespace
    //     anchors only, not Includes sources; the file `util.rs` becomes
    //     reachable via the `mod` machinery but no graph edge represents it)
    //   - Includes (main.rs → "util::helper"): from `use util::helper;`,
    //     unresolved-by-design (Rust dotted module paths don't basename-
    //     resolve), but still populates `Graph.includes[main.rs]` so
    //     `get_dependencies` and `generate_diagram(file=…)` return
    //     non-empty results.
    //   - Calls (main.rs:main → "helper"): resolves to
    //     `<util_rs>:helper` via the default scope-aware resolver
    //     (single candidate with name `helper`, language Rust). This is
    //     the cross-file edge `get_coupling` surfaces.
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).expect("create src/");

    let main_rs_path = src.join("main.rs");
    let util_rs_path = src.join("util.rs");

    std::fs::write(
        &main_rs_path,
        "mod util;\n\
         use util::helper;\n\
         fn main() {\n\
         \x20   helper();\n\
         }\n",
    )
    .expect("write main.rs");
    std::fs::write(&util_rs_path, "pub fn helper() {}\n").expect("write util.rs");

    // Canonicalize so the indexed_root matches what the indexer stores
    // (the tempdir path can include `/tmp/.tmpXXXXXX` symlinks on some
    // platforms; canonicalize resolves them so per-file paths in the
    // graph share the same prefix the analyze response reports).
    let indexed_root =
        std::fs::canonicalize(dir.path()).expect("canonicalize tempdir for indexed_root");

    let server = rust_server();
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

    let body = first_text(&r);
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("analyze_codebase returns JSON");
    let root_path = parsed["root_path"]
        .as_str()
        .expect("AnalyzeResult.root_path is a string")
        .to_owned();

    // Use the canonicalized paths the indexer stores — these ARE the
    // short-form paths on every platform after PathNormalization Phase 1
    // (no `\\?\` prefix on Windows). Passing them through to the 4 tools
    // exercises the handler wrap from 3.1/3.2: on Linux a no-op identity,
    // on Windows a real prefix-strip path.
    let main_rs = indexed_root
        .join("src")
        .join("main.rs")
        .to_string_lossy()
        .into_owned();
    let util_rs = indexed_root
        .join("src")
        .join("util.rs")
        .to_string_lossy()
        .into_owned();

    Indexed {
        _dir: dir,
        inner: server.inner.clone(),
        root_path,
        main_rs,
        util_rs,
    }
}

/// End-to-end short-form path resolution across the 4 file-taking tools.
///
/// Each tool gets its own assertion block; failure messages name the
/// offending tool so a per-tool regression is easy to localize without
/// re-running with `--nocapture` or bisecting which assertion fired.
#[tokio::test]
async fn four_file_taking_tools_resolve_short_form_paths() {
    let fx = build_indexed().await;

    // ---------- root_path shape ---------------------------------------
    //
    // On Linux this is trivially true (no `\\?\` ever appears in
    // canonical POSIX paths). On Windows it's the load-bearing assertion
    // that PathNormalization Phase 1 stripped the extended-path prefix
    // before storing the root.
    assert!(
        !fx.root_path.contains(r"\\?\"),
        "analyze_codebase root_path must not contain the Windows extended-path prefix; got {:?}",
        fx.root_path
    );

    // ---------- get_file_symbols --------------------------------------
    let r = get_file_symbols(
        &fx.inner.graph,
        &fx.main_rs,
        false,
        true,
        None,
        None,
        false,
        NO_BYTE_BUDGET,
    );
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "get_file_symbols: expected non-error result for short-form path {:?}, got: {:?}",
        fx.main_rs,
        r,
    );
    let parsed: serde_json::Value = serde_json::from_str(&first_text(&r))
        .expect("get_file_symbols returns Page<SymbolResult> JSON");
    let results = parsed["results"]
        .as_array()
        .expect("get_file_symbols: response has a `results` array");
    assert!(
        !results.is_empty(),
        "get_file_symbols: expected non-empty results for {:?} (main.rs contains `fn main`), got envelope: {parsed:?}",
        fx.main_rs,
    );

    // Testing Strategy §3 of the PathNormalization design specifies "every
    // file in the temp dir" — also exercise util.rs to prove the wiring
    // works for non-entry-point files. util.rs contains `pub fn helper`,
    // so a successful normalize + lookup yields a non-empty results page.
    let r = get_file_symbols(
        &fx.inner.graph,
        &fx.util_rs,
        false,
        true,
        None,
        None,
        false,
        NO_BYTE_BUDGET,
    );
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "get_file_symbols(util.rs): expected non-error result for short-form path {:?}, got: {:?}",
        fx.util_rs,
        r,
    );
    let parsed: serde_json::Value = serde_json::from_str(&first_text(&r))
        .expect("get_file_symbols(util.rs) returns Page<SymbolResult> JSON");
    let results = parsed["results"]
        .as_array()
        .expect("get_file_symbols(util.rs): response has a `results` array");
    assert!(
        !results.is_empty(),
        "get_file_symbols(util.rs): expected non-empty results for {:?} (util.rs contains `pub fn helper`), got envelope: {parsed:?}",
        fx.util_rs,
    );

    // ---------- get_coupling ------------------------------------------
    //
    // Default direction is "outgoing": cross-file Calls from main.rs plus
    // includes from main.rs. The Calls edge (main → helper) resolves to
    // `<util_rs>:helper`, so the coupling response carries `util.rs` as a
    // key. The unresolved Includes edge (to="util::helper") also lands in
    // the response as a key, but we only assert on the util.rs key — the
    // resolved cross-file *edge*, which is the load-bearing one.
    let r = get_coupling(&fx.inner.graph, &fx.main_rs, None);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "get_coupling: expected non-error result for short-form path {:?}, got: {:?}",
        fx.main_rs,
        r,
    );
    let parsed: serde_json::Value = serde_json::from_str(&first_text(&r))
        .expect("get_coupling returns a JSON object keyed by file path");
    let obj = parsed
        .as_object()
        .expect("get_coupling: response is a JSON object");
    assert!(
        obj.contains_key(&fx.util_rs),
        "get_coupling: expected coupling entry for sibling {:?} (resolved cross-file Calls edge), got: {parsed:?}",
        fx.util_rs,
    );

    // ---------- get_dependencies --------------------------------------
    //
    // `get_dependencies` returns the raw `Graph.includes[main.rs]` list as
    // strings. For Rust the `use util::helper;` statement produces an
    // Includes edge with `to="util::helper"` that does NOT basename-
    // resolve (Rust dotted module paths aren't filesystem paths, per the
    // RustParser doc), so the entry stays as the literal use-path. The
    // "expected cross-file edge to the sibling file" assertion is met
    // structurally: the response carries a non-empty entry that names
    // `util` (the module name shared with the sibling `util.rs` file).
    let r = get_dependencies(&fx.inner.graph, &fx.main_rs);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "get_dependencies: expected non-error result for short-form path {:?}, got: {:?}",
        fx.main_rs,
        r,
    );
    let parsed: serde_json::Value = serde_json::from_str(&first_text(&r))
        .expect("get_dependencies returns a JSON array of strings");
    let arr = parsed
        .as_array()
        .expect("get_dependencies: response is a JSON array");
    assert!(
        arr.iter()
            .filter_map(|v| v.as_str())
            .any(|s| s.contains("util")),
        "get_dependencies: expected at least one entry naming the `util` sibling (got: {parsed:?})",
    );

    // ---------- generate_diagram(file=…) ------------------------------
    //
    // `diagram_file_graph` BFS-walks `Graph.includes` from the start
    // path. The unresolved include edge (main.rs → "util::helper")
    // produces a single DiagramEdge — non-empty `edges` proves the tool
    // accepted the short-form path and produced a real file-graph view.
    let r = generate_diagram(
        &fx.inner.graph,
        GenerateDiagramInput {
            file: Some(&fx.main_rs),
            ..Default::default()
        },
    );
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "generate_diagram(file=…): expected non-error result for short-form path {:?}, got: {:?}",
        fx.main_rs,
        r,
    );
    let parsed: serde_json::Value = serde_json::from_str(&first_text(&r))
        .expect("generate_diagram returns DiagramEdge[] JSON (default format=edges)");
    let edges = parsed
        .as_array()
        .expect("generate_diagram(file=…) edges format returns a JSON array");
    assert!(
        !edges.is_empty(),
        "generate_diagram(file=…): expected non-empty edges for {:?} (main.rs has 1 include edge), got: {parsed:?}",
        fx.main_rs,
    );
}

/// Closes the Linux-observability gap of the canonical-path test above.
///
/// The canonical paths returned by `analyze_codebase`'s `root_path` field
/// are already in their final form on every platform after Phase 1; on
/// Linux they are byte-equal to what `Path::new(file).to_path_buf()` would
/// produce inside each handler, so the canonical-path test cannot
/// distinguish "the `normalize_user_path` wrap runs" from "the wrap was
/// silently removed." This test supplies a path with embedded `./` and
/// `..` segments and asserts each handler still resolves to the indexed
/// entry. If `normalize_user_path` were removed from any of the four
/// handlers, that handler's lookup key would be the literal dotty path
/// (which is NOT a key in the graph) and the assertion would fail with a
/// not-found result — making the wiring detectable on Linux.
#[tokio::test]
async fn four_file_taking_tools_resolve_dot_segment_paths() {
    let fx = build_indexed().await;

    // Build a path that resolves to the same file as `fx.main_rs` but
    // contains redundant `./` and `..` segments. `Path::new(&dotty)` would
    // produce a `PathBuf` whose lexical form is `<...>/src/./sub/../main.rs`
    // — NOT equal to the canonical key in the graph. `normalize_user_path`
    // calls `dunce::canonicalize` which resolves the segments back to
    // `<root>/src/main.rs`, hitting the graph.
    //
    // We need an existing intermediate directory for `dunce::canonicalize`
    // to walk through. The fixture already has `src/`; create `src/sub/`
    // alongside it so the `./sub/..` round-trip is well-defined.
    let indexed_root_for_sub = std::path::Path::new(&fx.root_path);
    let sub = indexed_root_for_sub.join("src").join("sub");
    std::fs::create_dir_all(&sub).expect("create src/sub/ for dotty round-trip");

    let dotty = format!(
        "{}/src/./sub/../main.rs",
        indexed_root_for_sub.to_string_lossy()
    );
    assert_ne!(
        dotty, fx.main_rs,
        "fixture sanity: dotty form must differ byte-wise from canonical so a passing test \
         proves the normalize step did real work rather than passing trivially",
    );

    // get_file_symbols
    let r = get_file_symbols(
        &fx.inner.graph,
        &dotty,
        false,
        true,
        None,
        None,
        false,
        NO_BYTE_BUDGET,
    );
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "get_file_symbols: dotty path {dotty:?} must resolve via normalize_user_path; got: {r:?}",
    );
    let results = serde_json::from_str::<serde_json::Value>(&first_text(&r))
        .expect("get_file_symbols returns JSON")["results"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !results.is_empty(),
        "get_file_symbols: dotty path {dotty:?} returned empty results — \
         likely the normalize_user_path wrap was removed from the handler",
    );

    // get_coupling
    let r = get_coupling(&fx.inner.graph, &dotty, None);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "get_coupling: dotty path {dotty:?} must resolve; got: {r:?}",
    );
    let obj = serde_json::from_str::<serde_json::Value>(&first_text(&r))
        .expect("get_coupling returns JSON object")
        .as_object()
        .cloned()
        .unwrap_or_default();
    assert!(
        obj.contains_key(&fx.util_rs),
        "get_coupling: dotty path {dotty:?} did not surface util.rs coupling — \
         normalize_user_path wrap likely missing in get_coupling",
    );

    // get_dependencies
    let r = get_dependencies(&fx.inner.graph, &dotty);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "get_dependencies: dotty path {dotty:?} must resolve; got: {r:?}",
    );
    let arr = serde_json::from_str::<serde_json::Value>(&first_text(&r))
        .expect("get_dependencies returns JSON array")
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        arr.iter()
            .filter_map(|v| v.as_str())
            .any(|s| s.contains("util")),
        "get_dependencies: dotty path {dotty:?} did not surface `util` dependency — \
         normalize_user_path wrap likely missing in get_dependencies",
    );

    // generate_diagram(file=…)
    let r = generate_diagram(
        &fx.inner.graph,
        GenerateDiagramInput {
            file: Some(&dotty),
            ..Default::default()
        },
    );
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "generate_diagram(file=…): dotty path {dotty:?} must resolve; got: {r:?}",
    );
    let edges = serde_json::from_str::<serde_json::Value>(&first_text(&r))
        .expect("generate_diagram returns JSON array")
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !edges.is_empty(),
        "generate_diagram(file=…): dotty path {dotty:?} returned empty edges — \
         normalize_user_path wrap likely missing in generate_diagram(file=…)",
    );
}
