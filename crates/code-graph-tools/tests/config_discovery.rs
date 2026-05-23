//! End-to-end integration tests for upward-walk `.code-graph.toml`
//! discovery, project-root cache co-location, and the lazy/scoped
//! merge model.
//!
//! Closes the "config flows through the discovery chain into the
//! parser pipeline" gap that pure unit tests miss: a future refactor
//! that accidentally drops the upward walk would make every existing
//! synthetic test pass while silently breaking real-world usage where
//! the user invokes `analyze_codebase` at a subdirectory of their
//! configured project.
//!
//! Tests 4.1–4.5 mirror the cases in the design doc; the trailing
//! `merge_accumulates_across_scoped_invocations` test pins the
//! project-wide merge semantics that turn scoped invocations into a
//! lazy build-up of a project graph.
//!
//! Identifier hygiene: all fixture macros, types, and file paths are
//! generic placeholders (`MYLIB_API`, `MyClass`, `/lib_a/foo.h` style)
//! — the syntactic shapes that exercise the discovery / cache / merge
//! contracts are preserved, but no third-party or proprietary names
//! are present.

use code_graph_core::SymbolKind;
use code_graph_graph::cache_path;
use code_graph_lang::LanguageRegistry;
use code_graph_lang_cpp::CppParser;
use code_graph_tools::handlers::analyze::{analyze_codebase, AnalyzeResult};
use code_graph_tools::CodeGraphServer;
use std::path::Path;
use tempfile::TempDir;

// --- helpers --------------------------------------------------------------

fn fresh_server() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().expect("CppParser::new")))
        .unwrap();
    CodeGraphServer::new(registry)
}

/// Parse the JSON body out of an `analyze_codebase` result. Panics on
/// tool-error responses so each test stays focused on the success path
/// — error-path coverage lives in the existing `analyze::tests`.
fn parse_result(r: &rmcp::model::CallToolResult) -> AnalyzeResult {
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "analyze_codebase returned an error: {r:?}",
    );
    let body = r
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.to_string())
        .expect("result must carry a text body");
    let parsed: serde_json::Value = serde_json::from_str(&body)
        .expect("AnalyzeResult must serialize as valid JSON");
    let files = parsed["files"].as_u64().unwrap() as u32;
    let symbols = parsed["symbols"].as_u64().unwrap() as u32;
    let edges = parsed["edges"].as_u64().unwrap() as u32;
    let root_path = parsed["root_path"].as_str().unwrap().to_string();
    let warnings = parsed["warnings"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    AnalyzeResult {
        files,
        symbols,
        edges,
        root_path,
        warnings,
    }
}

async fn run_analyze(
    server: &CodeGraphServer,
    path: &Path,
    force: bool,
) -> AnalyzeResult {
    let r = analyze_codebase(
        server.inner.clone(),
        path.to_string_lossy().into_owned(),
        force,
        None,
        None,
    )
    .await;
    parse_result(&r)
}

// =========================================================================
// Test 4.1 — config at the parent of indexed root
// =========================================================================

/// The canonical bug from the field report. User invokes
/// `analyze_codebase(<root>/subdir)` while their
/// `.code-graph.toml` lives at `<root>/`. Pre-fix behavior: subdir has
/// no toml, `RootConfig::load` returned defaults silently, and every
/// `class MYLIB_API Foo` failed to extract.
///
/// Post-fix expected behavior:
/// 1. Discovery walks `<root>/subdir` → `<root>` and finds the toml.
/// 2. `MyClass` (with `MYLIB_API` prefix) DOES extract because the
///    parent's `macro_strip` applies.
/// 3. The cache lands at the project root (`<root>/.code-graph-cache.json`),
///    NOT under the subdir.
/// 4. Warnings mention the parent toml so the user knows where
///    their config came from.
#[tokio::test]
async fn discovery_finds_parent_config_and_caches_at_project_root() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let subdir = root.join("subdir");
    std::fs::create_dir(&subdir).unwrap();

    std::fs::write(
        root.join(".code-graph.toml"),
        "[cpp]\nmacro_strip = [\"MYLIB_API\"]\n",
    )
    .unwrap();
    std::fs::write(
        subdir.join("MyClass.h"),
        "class MYLIB_API MyClass {};\n",
    )
    .unwrap();

    let server = fresh_server();
    let result = run_analyze(&server, &subdir, true).await;

    // (1, 2) Class extracted via parent config.
    let g = server.inner.graph.read();
    let symbols: Vec<_> = g.file_symbols(&subdir.join("MyClass.h")).into_iter().collect();
    let myclass = symbols
        .iter()
        .find(|s| s.name == "MyClass")
        .expect("MyClass must extract once parent's MYLIB_API strip applies");
    assert_eq!(myclass.kind, SymbolKind::Class);
    drop(g);

    // (3) Cache landed at project root, not under the subdir.
    assert!(
        cache_path(&root).exists(),
        "cache must land at the project root {}",
        root.display()
    );
    assert!(
        !cache_path(&subdir).exists(),
        "no cache should be written under the invocation subdir {}",
        subdir.display()
    );

    // (4) Warning mentions the parent path.
    let root_str = root.to_string_lossy().to_string();
    let subdir_str = subdir.to_string_lossy().to_string();
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.contains(&root_str) && w.contains(&subdir_str)),
        "warnings must surface that config was found at parent {root_str} \
         while indexing {subdir_str}; got: {:?}",
        result.warnings
    );

    // The root_path in the AnalyzeResult reflects the project root.
    assert_eq!(result.root_path, root_str);
}

// =========================================================================
// Test 4.2 — config three levels up from indexed root
// =========================================================================

/// Confirms the upward walk is not depth-limited and works through
/// arbitrary nested subdirs. Same pattern as 4.1 but the source file
/// lives three levels below the toml.
#[tokio::test]
async fn discovery_walks_three_levels_to_find_config() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let deep = root.join("a").join("b").join("c");
    std::fs::create_dir_all(&deep).unwrap();

    std::fs::write(
        root.join(".code-graph.toml"),
        "[cpp]\nmacro_strip = [\"DEEP_API\"]\n",
    )
    .unwrap();
    std::fs::write(deep.join("Buried.h"), "class DEEP_API Buried {};\n").unwrap();

    let server = fresh_server();
    run_analyze(&server, &deep, true).await;

    let g = server.inner.graph.read();
    let symbols: Vec<_> = g.file_symbols(&deep.join("Buried.h")).into_iter().collect();
    let buried = symbols
        .iter()
        .find(|s| s.name == "Buried")
        .expect("Buried must extract — walk must traverse three levels");
    assert_eq!(buried.kind, SymbolKind::Class);
    drop(g);

    assert!(
        cache_path(&root).exists(),
        "cache must land at the project root, three levels up"
    );
}

// =========================================================================
// Test 4.3 — no config found anywhere up to filesystem root
// =========================================================================

/// When the user has no `.code-graph.toml` and never created one, the
/// walk runs all the way to filesystem root and falls back to defaults.
/// The warning must explicitly call out the consequence (engine-style
/// classes with API macros will not extract) so the user can fix it.
#[tokio::test]
async fn discovery_no_config_anywhere_falls_back_with_warning() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    // Deliberately do NOT write a .code-graph.toml.
    std::fs::write(root.join("NoMacro.h"), "class Plain {};\n").unwrap();

    let server = fresh_server();
    let result = run_analyze(&server, &root, true).await;

    // The class without an API macro still extracts (no config needed).
    let g = server.inner.graph.read();
    let plain = g
        .file_symbols(&root.join("NoMacro.h"))
        .into_iter()
        .find(|s| s.name == "Plain")
        .expect("plain class must still extract — no macro_strip needed");
    assert_eq!(plain.kind, SymbolKind::Class);
    drop(g);

    // Warning must call out the absent-config consequence.
    assert!(
        result.warnings.iter().any(|w| w.contains("no .code-graph.toml found")),
        "warnings must include the no-config notice; got: {:?}",
        result.warnings
    );
}

// =========================================================================
// Test 4.4 — nested toml: first-match-wins, no merging across files
// =========================================================================

/// Two `.code-graph.toml` files: outer at the project boundary, inner
/// at a subdir. Invoking inside the inner subdir must load the INNER
/// toml only — the outer is not consulted at all (first match wins,
/// no merging).
///
/// This locks in the documented semantics: nested tomls scope to
/// their own subtree as a separate "project". A future refactor that
/// silently layered configs across the walk would break this test.
#[tokio::test]
async fn nested_toml_first_match_wins_no_merging() {
    let dir = TempDir::new().unwrap();
    let outer = std::fs::canonicalize(dir.path()).unwrap();
    let inner = outer.join("inner");
    std::fs::create_dir(&inner).unwrap();

    // Outer config strips OUTER_API; inner config strips INNER_API.
    std::fs::write(
        outer.join(".code-graph.toml"),
        "[cpp]\nmacro_strip = [\"OUTER_API\"]\n",
    )
    .unwrap();
    std::fs::write(
        inner.join(".code-graph.toml"),
        "[cpp]\nmacro_strip = [\"INNER_API\"]\n",
    )
    .unwrap();

    // Fixture has BOTH macros — if the walk merged configs, both
    // classes would extract; if first-match-wins, only the
    // INNER_API-prefixed one extracts.
    std::fs::write(
        inner.join("Mixed.h"),
        "class INNER_API InnerClass {};\nclass OUTER_API OuterClass {};\n",
    )
    .unwrap();

    let server = fresh_server();
    run_analyze(&server, &inner, true).await;

    let g = server.inner.graph.read();
    let symbols: Vec<_> = g.file_symbols(&inner.join("Mixed.h")).into_iter().collect();

    assert!(
        symbols.iter().any(|s| s.name == "InnerClass"),
        "InnerClass must extract via inner toml's INNER_API strip; got: {:?}",
        symbols.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
    assert!(
        !symbols.iter().any(|s| s.name == "OuterClass"),
        "OuterClass must NOT extract — outer toml's OUTER_API is not consulted \
         when an inner toml is found first. Configs do not merge. Got: {:?}",
        symbols.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
    drop(g);

    // Cache lands at the inner toml's directory — that's the "project root"
    // for this invocation.
    assert!(
        cache_path(&inner).exists(),
        "cache must land at the inner toml's directory ({})",
        inner.display()
    );
    assert!(
        !cache_path(&outer).exists(),
        "no cache should be written at the outer toml's directory ({})",
        outer.display()
    );
}

// =========================================================================
// Test 4.5 — orphan-cache detection
// =========================================================================

/// A pre-existing `.code-graph-cache.json` at the invocation subdir
/// (where the old behavior wrote it) is now orphaned because the new
/// model caches at the project root. The handler must detect this and
/// surface a warning so the user can reclaim disk. The new cache must
/// land at the project root regardless of the orphan.
#[tokio::test]
async fn orphan_cache_at_invocation_dir_is_detected_and_warned() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let subdir = root.join("subdir");
    std::fs::create_dir(&subdir).unwrap();

    std::fs::write(root.join(".code-graph.toml"), "[cpp]\nmacro_strip = []\n").unwrap();
    std::fs::write(subdir.join("Trivial.h"), "class Trivial {};\n").unwrap();

    // Plant a stale cache at the subdir to simulate the pre-fix state.
    std::fs::write(
        cache_path(&subdir),
        b"this is not valid cache content; serves only to exist on disk",
    )
    .unwrap();

    let server = fresh_server();
    let result = run_analyze(&server, &subdir, true).await;

    // New cache lands at project root.
    assert!(
        cache_path(&root).exists(),
        "fresh cache must land at project root despite the orphan"
    );
    // Orphan warning fires.
    let subdir_cache_str = cache_path(&subdir).to_string_lossy().to_string();
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.contains("orphan cache") && w.contains(&subdir_cache_str)),
        "orphan-cache warning must reference {subdir_cache_str}; got: {:?}",
        result.warnings
    );
}

// =========================================================================
// Project-wide merge: scoped invocations accumulate into one graph
// =========================================================================

/// The lazy/scoped indexing contract: invoking analyze on subtree A,
/// then subtree B (both under the same project root), produces a
/// project graph that contains BOTH subtrees. Subtree A's symbols
/// survive subtree B's invocation; the cache at the project root
/// grows monotonically until something forces invalidation.
///
/// This pins the merge-not-clobber behavior introduced alongside
/// upward walk: without it, the second invocation would wipe the
/// first invocation's contributions and the user would have to
/// re-index the whole project to see them together.
#[tokio::test]
async fn merge_accumulates_across_scoped_invocations() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let subtree_a = root.join("a");
    let subtree_b = root.join("b");
    std::fs::create_dir_all(&subtree_a).unwrap();
    std::fs::create_dir_all(&subtree_b).unwrap();

    std::fs::write(root.join(".code-graph.toml"), "[cpp]\nmacro_strip = []\n").unwrap();
    std::fs::write(subtree_a.join("AOnly.h"), "class AOnly {};\n").unwrap();
    std::fs::write(subtree_b.join("BOnly.h"), "class BOnly {};\n").unwrap();

    let server = fresh_server();

    // First invocation: only subtree A. Graph has AOnly only.
    let first = run_analyze(&server, &subtree_a, true).await;
    assert_eq!(first.files, 1, "first invocation indexes only subtree A");
    {
        let g = server.inner.graph.read();
        assert!(
            g.file_symbols(&subtree_a.join("AOnly.h"))
                .into_iter()
                .any(|s| s.name == "AOnly"),
            "AOnly must be present after first invocation"
        );
        assert!(
            g.file_symbols(&subtree_b.join("BOnly.h")).is_empty(),
            "BOnly must NOT be present yet — subtree B not indexed"
        );
    }

    // Second invocation: subtree B. The accumulator design means
    // BOnly is added AND AOnly survives. Without merge semantics,
    // AOnly would be wiped here.
    let second = run_analyze(&server, &subtree_b, true).await;
    assert_eq!(
        second.files, 2,
        "after second invocation the project graph carries both subtree files"
    );
    {
        let g = server.inner.graph.read();
        assert!(
            g.file_symbols(&subtree_a.join("AOnly.h"))
                .into_iter()
                .any(|s| s.name == "AOnly"),
            "AOnly must SURVIVE subtree B's invocation — this is the merge contract"
        );
        assert!(
            g.file_symbols(&subtree_b.join("BOnly.h"))
                .into_iter()
                .any(|s| s.name == "BOnly"),
            "BOnly must be added by the second invocation"
        );
    }

    // The project cache at root contains both subtrees too.
    assert!(cache_path(&root).exists(), "cache lives at project root");
}

// =========================================================================
// Scoped force=true invalidates only the scope
// =========================================================================

/// `force=true` at a subdir of a configured project drops in-scope
/// cache entries and re-indexes them, but does NOT touch out-of-scope
/// entries from prior scoped invocations. This is the scope-limited
/// invalidation that makes lazy indexing usable on large projects:
/// you can refresh one subtree without paying the cost of refreshing
/// the whole project.
#[tokio::test]
async fn scoped_force_invalidates_only_in_scope_entries() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let subtree_a = root.join("a");
    let subtree_b = root.join("b");
    std::fs::create_dir_all(&subtree_a).unwrap();
    std::fs::create_dir_all(&subtree_b).unwrap();

    std::fs::write(root.join(".code-graph.toml"), "[cpp]\nmacro_strip = []\n").unwrap();
    std::fs::write(subtree_a.join("AOne.h"), "class AOne {};\n").unwrap();
    std::fs::write(subtree_b.join("BOne.h"), "class BOne {};\n").unwrap();

    let server = fresh_server();

    // Seed both subtrees.
    run_analyze(&server, &subtree_a, true).await;
    run_analyze(&server, &subtree_b, true).await;
    assert_eq!(server.inner.graph.read().stats().files, 2);

    // Add a new file under A and force=true at A. B's entry must survive.
    std::fs::write(subtree_a.join("ATwo.h"), "class ATwo {};\n").unwrap();
    let third = run_analyze(&server, &subtree_a, true).await;

    assert_eq!(
        third.files, 3,
        "after force at subtree A: A (2 files) + B (1 file from prior invocation)"
    );

    let g = server.inner.graph.read();
    assert!(
        g.file_symbols(&subtree_a.join("AOne.h"))
            .into_iter()
            .any(|s| s.name == "AOne"),
        "AOne must still be present after force at A"
    );
    assert!(
        g.file_symbols(&subtree_a.join("ATwo.h"))
            .into_iter()
            .any(|s| s.name == "ATwo"),
        "ATwo must be added by the force re-index"
    );
    assert!(
        g.file_symbols(&subtree_b.join("BOne.h"))
            .into_iter()
            .any(|s| s.name == "BOne"),
        "BOne must SURVIVE force=true at the unrelated subtree A — \
         scope-limited invalidation contract"
    );
}
