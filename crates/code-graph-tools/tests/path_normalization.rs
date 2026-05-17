//! End-to-end verification that the four file-taking MCP tools
//! (`get_file_symbols`, `get_coupling`, `get_dependencies`,
//! `generate_diagram(file=…)`) resolve short-form path arguments after a
//! real `analyze_codebase` round-trip.
//!
//! On Linux this is effectively a non-regression check: paths are already
//! canonical and `normalize_user_path` is a near-identity transform, so the
//! test's value is "the handler `normalize_user_path` wrap didn't break the
//! happy path." On Windows this same test becomes the load-bearing fix
//! verification: the indexer stores short-form `D:\…` keys,
//! `normalize_user_path` strips a user-supplied `\\?\D:\…` prefix on
//! lookup, and the handlers find the records.
//!
//! Fixture choice: we use a C++ fixture — `src/main.cpp` with
//! `#include "util.h"` plus a
//! call to `helper()`, and `src/util.h` which *defines* `helper()` inline.
//! C++ is required here (not Rust) because the include-graph contract is
//! "an Includes edge survives only if it resolves to an indexed source
//! file." Rust `use util::helper;` produces an Includes edge whose `to`
//! is the dotted module path `"util::helper"`, which the default basename
//! resolver cannot map to a file (Rust module paths are not filesystem
//! paths — see the RustParser `resolve_include` doc); that edge is
//! correctly dropped, leaving `get_dependencies` /
//! `generate_diagram(file=…)` empty. C++ `#include "util.h"`
//! basename-resolves to the indexed sibling `src/util.h`, producing a
//! *resolved* Includes edge that survives into `Graph.includes[main.cpp]`.
//! The same `src/util.h` is also where `helper()` is defined, so the
//! `helper()` call site in `main.cpp` resolves cross-file to
//! `<util_h>:helper`, producing the real `Calls` edge `get_coupling`
//! surfaces. One sibling file is therefore the resolved target of *both*
//! the Includes edge (deps / diagram) and the Calls edge (coupling),
//! keeping every arm's proof at full strength.
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
use code_graph_lang_cpp::CppParser;
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

/// Fresh server with only the C++ language plugin registered — the
/// fixture is C++-only, so we keep the registry minimal to avoid pulling
/// every parser's tree-sitter grammar into the test compile.
fn cpp_server() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().expect("CppParser::new")))
        .expect("register CppParser");
    CodeGraphServer::new(registry)
}

/// Build a tempdir containing the cross-file C++ fixture, run
/// `analyze_codebase` against it, and return the indexed server alongside
/// the canonical paths the test will exercise.
struct Indexed {
    _dir: TempDir,
    inner: Arc<ServerInner>,
    /// `root_path` field captured from the `analyze_codebase` response. On
    /// Linux this is the canonical absolute path; on Windows it should be
    /// the short form (no `\\?\` prefix) once the indexer canonicalizes.
    root_path: String,
    /// Absolute path to `src/main.cpp` inside the tempdir, used as the
    /// short-form `file` argument for each of the 4 tools under test.
    main_cpp: String,
    /// Absolute path to `src/util.h` — the indexed sibling both the
    /// resolved Includes edge and the resolved cross-file Calls edge point
    /// at.
    util_h: String,
}

async fn build_indexed() -> Indexed {
    let dir = TempDir::new().expect("TempDir for indexed fixture");

    // Cross-file fixture:
    //   src/main.cpp — `#include "util.h"` + `void main_fn() { helper(); }`
    //   src/util.h   — `void helper() {}` (inline definition, has a body)
    //
    // Edges produced after `resolve_all_edges`:
    //   - Includes (main.cpp → "util.h"): from `#include "util.h"`. The
    //     default basename resolver maps the raw target `"util.h"` to the
    //     indexed sibling `src/util.h` (its file_name matches a FileIndex
    //     `by_basename` entry), so the edge resolves and `edge.to` is
    //     rewritten to the absolute `<util_h>` path. A *resolved* Includes
    //     edge to an indexed source file survives the
    //     drop-unless-resolved-to-indexed-source filter, so
    //     `Graph.includes[main.cpp]` carries `<util_h>` and
    //     `get_dependencies` / `generate_diagram(file=…)` return non-empty
    //     results naming `util.h`.
    //   - Calls (main.cpp:main_fn → "helper"): resolves to
    //     `<util_h>:helper` via the default scope-aware resolver — `util.h`
    //     defines `helper()` *with a body* (forward declarations are
    //     excluded, so the only `helper` symbol is the definition). This is
    //     the cross-file edge `get_coupling` surfaces. The same `util.h`
    //     file is thus the resolved target of both edges.
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).expect("create src/");

    let main_cpp_path = src.join("main.cpp");
    let util_h_path = src.join("util.h");

    std::fs::write(
        &main_cpp_path,
        "#include \"util.h\"\n\
         void main_fn() {\n\
         \x20   helper();\n\
         }\n",
    )
    .expect("write main.cpp");
    std::fs::write(&util_h_path, "void helper() {}\n").expect("write util.h");

    // Canonicalize so the indexed_root matches what the indexer stores
    // (the tempdir path can include `/tmp/.tmpXXXXXX` symlinks on some
    // platforms; canonicalize resolves them so per-file paths in the
    // graph share the same prefix the analyze response reports).
    let indexed_root =
        std::fs::canonicalize(dir.path()).expect("canonicalize tempdir for indexed_root");

    let server = cpp_server();
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
    // short-form paths on every platform (no `\\?\` prefix on Windows).
    // Passing them through to the 4 tools exercises the handler
    // `normalize_user_path` wrap: on Linux a no-op identity,
    // on Windows a real prefix-strip path.
    let main_cpp = indexed_root
        .join("src")
        .join("main.cpp")
        .to_string_lossy()
        .into_owned();
    let util_h = indexed_root
        .join("src")
        .join("util.h")
        .to_string_lossy()
        .into_owned();

    Indexed {
        _dir: dir,
        inner: server.inner.clone(),
        root_path,
        main_cpp,
        util_h,
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
    // that the indexer stripped the extended-path prefix
    // before storing the root.
    assert!(
        !fx.root_path.contains(r"\\?\"),
        "analyze_codebase root_path must not contain the Windows extended-path prefix; got {:?}",
        fx.root_path
    );

    // ---------- get_file_symbols --------------------------------------
    let r = get_file_symbols(
        &fx.inner.graph,
        &fx.main_cpp,
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
        fx.main_cpp,
        r,
    );
    let parsed: serde_json::Value = serde_json::from_str(&first_text(&r))
        .expect("get_file_symbols returns Page<SymbolResult> JSON");
    let results = parsed["results"]
        .as_array()
        .expect("get_file_symbols: response has a `results` array");
    assert!(
        !results.is_empty(),
        "get_file_symbols: expected non-empty results for {:?} (main.cpp contains `main_fn`), got envelope: {parsed:?}",
        fx.main_cpp,
    );

    // Testing Strategy §3 of the PathNormalization design specifies "every
    // file in the temp dir" — also exercise util.h to prove the wiring
    // works for non-entry-point files. util.h contains `void helper`, so a
    // successful normalize + lookup yields a non-empty results page.
    let r = get_file_symbols(
        &fx.inner.graph,
        &fx.util_h,
        false,
        true,
        None,
        None,
        false,
        NO_BYTE_BUDGET,
    );
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "get_file_symbols(util.h): expected non-error result for short-form path {:?}, got: {:?}",
        fx.util_h,
        r,
    );
    let parsed: serde_json::Value = serde_json::from_str(&first_text(&r))
        .expect("get_file_symbols(util.h) returns Page<SymbolResult> JSON");
    let results = parsed["results"]
        .as_array()
        .expect("get_file_symbols(util.h): response has a `results` array");
    assert!(
        !results.is_empty(),
        "get_file_symbols(util.h): expected non-empty results for {:?} (util.h contains `void helper`), got envelope: {parsed:?}",
        fx.util_h,
    );

    // ---------- get_coupling ------------------------------------------
    //
    // Default direction is "outgoing": cross-file Calls from main.cpp plus
    // includes from main.cpp. The Calls edge (main_fn → helper) resolves to
    // `<util_h>:helper`, and the Includes edge (main.cpp → "util.h")
    // resolves to `<util_h>` — both land on the same sibling, so the
    // coupling response carries `util.h` as a key.
    let r = get_coupling(
        &fx.inner.graph,
        &fx.main_cpp,
        None,
        None,
        None,
        NO_BYTE_BUDGET,
    );
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "get_coupling: expected non-error result for short-form path {:?}, got: {:?}",
        fx.main_cpp,
        r,
    );
    let parsed: serde_json::Value = serde_json::from_str(&first_text(&r))
        .expect("get_coupling returns a Page<CouplingEntry> envelope");
    let has_util = parsed["results"]
        .as_array()
        .expect("get_coupling: response carries a results array")
        .iter()
        .any(|row| row["file"] == serde_json::json!(fx.util_h));
    assert!(
        has_util,
        "get_coupling: expected a coupling row for sibling {:?} (resolved cross-file Calls edge), got: {parsed:?}",
        fx.util_h,
    );

    // ---------- get_dependencies --------------------------------------
    //
    // `get_dependencies` returns a Page<DependencyEntry> envelope: one
    // {file, kind, line} row per `Graph.includes[main.cpp]` entry. The
    // `#include "util.h"` directive produces an Includes edge whose raw
    // `to="util.h"` basename-resolves to the indexed sibling
    // `src/util.h`, so the resolved edge survives the
    // drop-unless-resolved-to-indexed-source filter and `edge.to` is the
    // absolute `<util_h>` path. We assert the SPECIFIC resolved
    // dependency row — `file == <util_h>` — is present, the same strength
    // as the resolved-edge assertion the other arms use.
    let r = get_dependencies(&fx.inner.graph, &fx.main_cpp, None, None, NO_BYTE_BUDGET);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "get_dependencies: expected non-error result for short-form path {:?}, got: {:?}",
        fx.main_cpp,
        r,
    );
    let parsed: serde_json::Value = serde_json::from_str(&first_text(&r))
        .expect("get_dependencies returns a Page<DependencyEntry> envelope");
    let arr = parsed["results"]
        .as_array()
        .expect("get_dependencies: response carries a `results` array");
    assert!(
        arr.iter().any(|row| row["file"] == serde_json::json!(fx.util_h)),
        "get_dependencies: expected the resolved dependency row for sibling {:?} (resolved Includes edge), got: {parsed:?}",
        fx.util_h,
    );

    // ---------- generate_diagram(file=…) ------------------------------
    //
    // `diagram_file_graph` BFS-walks `Graph.includes` from the start
    // path. The resolved include edge (main.cpp → `<util_h>`) produces a
    // single DiagramEdge. The file-graph diagram deliberately renders
    // endpoint paths as their `Path::file_name` basename (see
    // `diagrams.rs` `file_display_name`), so the edge surfaces as
    // `from="main.cpp"`, `to="util.h"`. We assert that SPECIFIC resolved
    // edge appears — same strength as the deps arm: it proves the include
    // resolved to the indexed sibling AND the short-form path normalized
    // before the graph lookup.
    let r = generate_diagram(
        &fx.inner.graph,
        GenerateDiagramInput {
            file: Some(&fx.main_cpp),
            ..Default::default()
        },
    );
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "generate_diagram(file=…): expected non-error result for short-form path {:?}, got: {:?}",
        fx.main_cpp,
        r,
    );
    let parsed: serde_json::Value = serde_json::from_str(&first_text(&r))
        .expect("generate_diagram returns DiagramEdge[] JSON (default format=edges)");
    let edges = parsed
        .as_array()
        .expect("generate_diagram(file=…) edges format returns a JSON array");
    assert!(
        edges
            .iter()
            .any(|e| e["from"] == serde_json::json!("main.cpp")
                && e["to"] == serde_json::json!("util.h")),
        "generate_diagram(file=…): expected the resolved include edge main.cpp → util.h, got: {parsed:?}",
    );
}

/// Closes the Linux-observability gap of the canonical-path test above.
///
/// The canonical paths returned by `analyze_codebase`'s `root_path` field
/// are already in their final form on every platform once the indexer
/// canonicalizes. On Linux they are byte-equal to what
/// `Path::new(file).to_path_buf()` would
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

    // Build a path that resolves to the same file as `fx.main_cpp` but
    // contains redundant `./` and `..` segments. `Path::new(&dotty)` would
    // produce a `PathBuf` whose lexical form is `<...>/src/./sub/../main.cpp`
    // — NOT equal to the canonical key in the graph. `normalize_user_path`
    // calls `dunce::canonicalize` which resolves the segments back to
    // `<root>/src/main.cpp`, hitting the graph.
    //
    // We need an existing intermediate directory for `dunce::canonicalize`
    // to walk through. The fixture already has `src/`; create `src/sub/`
    // alongside it so the `./sub/..` round-trip is well-defined.
    let indexed_root_for_sub = std::path::Path::new(&fx.root_path);
    let sub = indexed_root_for_sub.join("src").join("sub");
    std::fs::create_dir_all(&sub).expect("create src/sub/ for dotty round-trip");

    let dotty = format!(
        "{}/src/./sub/../main.cpp",
        indexed_root_for_sub.to_string_lossy()
    );
    assert_ne!(
        dotty, fx.main_cpp,
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
    let r = get_coupling(&fx.inner.graph, &dotty, None, None, None, NO_BYTE_BUDGET);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "get_coupling: dotty path {dotty:?} must resolve; got: {r:?}",
    );
    let parsed = serde_json::from_str::<serde_json::Value>(&first_text(&r))
        .expect("get_coupling returns a Page<CouplingEntry> envelope");
    let has_util = parsed["results"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .any(|row| row["file"] == serde_json::json!(fx.util_h));
    assert!(
        has_util,
        "get_coupling: dotty path {dotty:?} did not surface util.h coupling — \
         normalize_user_path wrap likely missing in get_coupling",
    );

    // get_dependencies
    let r = get_dependencies(&fx.inner.graph, &dotty, None, None, NO_BYTE_BUDGET);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "get_dependencies: dotty path {dotty:?} must resolve; got: {r:?}",
    );
    let parsed = serde_json::from_str::<serde_json::Value>(&first_text(&r))
        .expect("get_dependencies returns a Page<DependencyEntry> envelope");
    let arr = parsed["results"].as_array().cloned().unwrap_or_default();
    assert!(
        arr.iter().any(|row| row["file"] == serde_json::json!(fx.util_h)),
        "get_dependencies: dotty path {dotty:?} did not surface the resolved `util.h` dependency — \
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
        edges
            .iter()
            .any(|e| e["from"] == serde_json::json!("main.cpp")
                && e["to"] == serde_json::json!("util.h")),
        "generate_diagram(file=…): dotty path {dotty:?} did not surface the resolved \
         main.cpp → util.h include edge — normalize_user_path wrap likely missing in \
         generate_diagram(file=…)",
    );
}
