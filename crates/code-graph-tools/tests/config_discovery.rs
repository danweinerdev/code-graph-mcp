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

// =========================================================================
// mtime-driven incremental re-index (force=false common path)
// =========================================================================

/// The most common production path: user edits one file in a scope
/// they previously indexed, then re-runs `analyze_codebase` on the
/// same scope without `force=true`. The cache load + mtime check must
/// pick up the edited file, re-parse it, and replace its old graph
/// entries — old symbols gone, new symbols present, no orphan symbol
/// leaks.
///
/// The unit-level `re_merge_replaces_edges_not_just_nodes` in
/// `graph.rs` covers this at the storage layer; here we drive it
/// through the full handler so the mtime detection + scoped re-parse +
/// merge wiring is exercised end-to-end.
#[tokio::test]
async fn mtime_driven_incremental_replaces_stale_symbols_in_scope() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(root.join(".code-graph.toml"), "[cpp]\nmacro_strip = []\n").unwrap();

    let header = root.join("Subject.h");
    std::fs::write(&header, "class Original {};\n").unwrap();

    let server = fresh_server();

    // First invocation: index Original.
    run_analyze(&server, &root, false).await;
    {
        let g = server.inner.graph.read();
        let syms = g.file_symbols(&header);
        assert!(
            syms.iter().any(|s| s.name == "Original"),
            "Original must be indexed after first invocation"
        );
        assert!(
            !syms.iter().any(|s| s.name == "Replaced"),
            "Replaced must NOT exist yet"
        );
    }

    // Edit the file. Sleep briefly so the mtime is observably newer —
    // some filesystems have second-resolution mtimes and a write
    // immediately after another can land on the same value.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&header, "class Replaced {};\n").unwrap();

    // Second invocation, NO force. The mtime check must detect
    // Subject.h as stale, re-parse it, and replace the cached entries.
    let result = run_analyze(&server, &root, false).await;
    assert_eq!(
        result.files, 1,
        "still one file after re-index — re-merge, not duplicate"
    );

    let g = server.inner.graph.read();
    let syms = g.file_symbols(&header);
    assert!(
        syms.iter().any(|s| s.name == "Replaced"),
        "Replaced must be indexed after mtime-driven re-parse; got: {:?}",
        syms.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
    assert!(
        !syms.iter().any(|s| s.name == "Original"),
        "Original must NOT survive — the re-merge must replace per-file entries; got: {:?}",
        syms.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
    // Total node count must reflect the replacement: 1 file × 1 class
    // = 1 node. A regression that double-merged would show 2 nodes.
    assert_eq!(
        g.stats().nodes,
        1,
        "exactly one node after re-merge; double-merge would show 2"
    );
}

// =========================================================================
// Out-of-scope sweep cadence
// =========================================================================

/// The opportunistic sweep should run when at least 24h has elapsed
/// since `last_sweep_at`. The simplest way to drive both branches
/// without a clock-mocking facility: manipulate `Graph::last_sweep_at`
/// directly between invocations via the public setter.
///
/// Setup:
/// 1. Index subtree A (a.cpp), then subtree B (b.cpp). Both in cache.
/// 2. Delete b.cpp from disk. Cache still has its entry.
/// 3. Set `last_sweep_at` to "right now" — the next invocation must
///    SKIP the sweep (cadence not elapsed).
/// 4. Invoke analyze on A. b.cpp entry must SURVIVE.
/// 5. Reset `last_sweep_at` to 0 (never-swept). Next invocation must
///    RUN the sweep.
/// 6. Invoke analyze on A again. b.cpp entry must be GONE.
#[tokio::test]
async fn sweep_cadence_skips_when_recent_runs_when_elapsed() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let subtree_a = root.join("a");
    let subtree_b = root.join("b");
    std::fs::create_dir_all(&subtree_a).unwrap();
    std::fs::create_dir_all(&subtree_b).unwrap();

    std::fs::write(root.join(".code-graph.toml"), "[cpp]\nmacro_strip = []\n").unwrap();
    std::fs::write(subtree_a.join("a.cpp"), "void a() {}\n").unwrap();
    let b_path = subtree_b.join("b.cpp");
    std::fs::write(&b_path, "void b() {}\n").unwrap();

    let server = fresh_server();

    // Seed both subtrees into the cache.
    run_analyze(&server, &subtree_a, true).await;
    run_analyze(&server, &subtree_b, true).await;
    {
        let g = server.inner.graph.read();
        assert_eq!(g.stats().files, 2, "both subtrees seeded");
        assert!(
            !g.file_symbols(&b_path).is_empty(),
            "b.cpp's symbols must be present after seeding"
        );
    }

    // Delete b.cpp from disk. Cache entry still references it.
    std::fs::remove_file(&b_path).unwrap();

    // Skip-sweep branch: stamp last_sweep_at to a value such that the
    // handler computes a delta SMALLER than SWEEP_INTERVAL_NANOS. We
    // use "now minus a tiny amount" (1 second's worth of nanos) so the
    // delta is essentially zero — well below the 24h threshold.
    //
    // The fast-path inside `analyze_codebase` reloads the graph from
    // disk via `probe.load(&project_root)`, so the in-memory mutation
    // must be persisted before the next invocation observes it.
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let recent = now_nanos.saturating_sub(1_000_000_000); // 1s ago
    {
        let mut g = server.inner.graph.write();
        g.set_last_sweep_at(recent);
    }
    server.inner.graph.read().save(&root).unwrap();

    // Invoke analyze on subtree A. Sweep should NOT run; b.cpp's
    // cache entry survives.
    run_analyze(&server, &subtree_a, false).await;
    {
        let g = server.inner.graph.read();
        assert_eq!(
            g.files_in_scope_count(&subtree_b),
            1,
            "b.cpp's cache entry must SURVIVE — sweep was within cadence"
        );
    }

    // Run-sweep branch: reset last_sweep_at to 0 (never-swept). The
    // handler computes delta = now - 0 = ages ago, far above
    // SWEEP_INTERVAL_NANOS → sweep runs. Same disk-persist dance as
    // above: the fast-path's `probe.load` reads from disk, not from
    // the in-memory state.
    {
        let mut g = server.inner.graph.write();
        g.set_last_sweep_at(0);
    }
    server.inner.graph.read().save(&root).unwrap();

    run_analyze(&server, &subtree_a, false).await;
    {
        let g = server.inner.graph.read();
        assert_eq!(
            g.files_in_scope_count(&subtree_b),
            0,
            "b.cpp's cache entry must be DROPPED — sweep ran and stat'd missing"
        );
        // a.cpp (in scope this invocation) is untouched by the sweep
        // (sweep only checks OUT-of-scope files). Survives.
        assert_eq!(
            g.files_in_scope_count(&subtree_a),
            1,
            "a.cpp (in scope) must survive — sweep only walks out-of-scope"
        );
    }

    // After running the sweep, last_sweep_at must have been advanced
    // away from 0. We don't pin a specific value (it's a wall-clock
    // timestamp), only that it changed.
    let after_sweep = server.inner.graph.read().last_sweep_at();
    assert!(
        after_sweep > 0,
        "last_sweep_at must advance past 0 once a sweep runs"
    );
}

// =========================================================================
// Cross-scope edge resolution asymmetry
// =========================================================================

/// Documented contract from `resolve_edges_with_indexes`:
/// - **Fresh → cached resolves.** A fresh edge whose target is a
///   symbol added by an earlier invocation can resolve, because the
///   combined symbol_index used during this invocation's resolve
///   includes cached symbols.
/// - **Cached → fresh does NOT spontaneously resolve.** A cached edge
///   from an earlier invocation pointing at a symbol now in the cache
///   (added by this invocation) stays unresolved, because cached
///   edges are not re-resolved on subsequent invocations. The user
///   must `force=true` at the originating subtree to re-parse and
///   re-resolve.
///
/// This test pins both halves so a future change that "improves" or
/// breaks the asymmetry gets caught immediately.
#[tokio::test]
async fn cross_scope_edge_resolution_is_asymmetric() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let subtree_a = root.join("a");
    let subtree_b = root.join("b");
    std::fs::create_dir_all(&subtree_a).unwrap();
    std::fs::create_dir_all(&subtree_b).unwrap();

    std::fs::write(root.join(".code-graph.toml"), "[cpp]\nmacro_strip = []\n").unwrap();

    // A defines a_func() and a_caller() which calls b_target().
    // b_target lives in B and will not exist when we first index A.
    let a_cpp = subtree_a.join("a.cpp");
    std::fs::write(
        &a_cpp,
        "\
void a_func() {}
void b_target();
void a_caller() { b_target(); }
",
    )
    .unwrap();

    // B defines b_target() and b_caller() which calls a_func().
    let b_cpp = subtree_b.join("b.cpp");
    std::fs::write(
        &b_cpp,
        "\
void a_func();
void b_target() {}
void b_caller() { a_func(); }
",
    )
    .unwrap();

    let server = fresh_server();

    // Phase 1: index A only. A's call to b_target stays unresolved
    // (B not yet indexed → no candidate in the symbol_index).
    run_analyze(&server, &subtree_a, false).await;
    let a_cpp_str = a_cpp.to_string_lossy().to_string();
    let b_cpp_str = b_cpp.to_string_lossy().to_string();
    {
        let g = server.inner.graph.read();
        let a_caller_id = format!("{a_cpp_str}:a_caller");
        let unresolved_after_phase1 = g
            .callees(&a_caller_id, 1, None)
            .into_iter()
            .any(|c| c.symbol_id.contains(&b_cpp_str));
        assert!(
            !unresolved_after_phase1,
            "a_caller's call to b_target must NOT resolve before B is indexed"
        );
    }

    // Phase 2: index B. Fresh→cached path activates: b_caller's call
    // to a_func resolves against A's cached symbol_index entry.
    run_analyze(&server, &subtree_b, false).await;
    {
        let g = server.inner.graph.read();
        let b_caller_id = format!("{b_cpp_str}:b_caller");
        let b_caller_callees = g.callees(&b_caller_id, 1, None);
        assert!(
            b_caller_callees
                .iter()
                .any(|c| c.symbol_id == format!("{a_cpp_str}:a_func")),
            "b_caller's call to a_func MUST resolve via the combined symbol_index \
             (fresh→cached path); got callees: {:?}",
            b_caller_callees.iter().map(|c| &c.symbol_id).collect::<Vec<_>>()
        );

        // Asymmetric half: A's cached call to b_target STILL does not
        // resolve. The cached edge was emitted when B wasn't indexed
        // and it is not re-resolved on subsequent invocations.
        let a_caller_id = format!("{a_cpp_str}:a_caller");
        let a_caller_callees = g.callees(&a_caller_id, 1, None);
        let resolved_to_b = a_caller_callees
            .iter()
            .any(|c| c.symbol_id == format!("{b_cpp_str}:b_target"));
        assert!(
            !resolved_to_b,
            "a_caller's cached call to b_target must NOT spontaneously resolve \
             after B is indexed — documented asymmetric-resolve contract. \
             Got: {:?}",
            a_caller_callees.iter().map(|c| &c.symbol_id).collect::<Vec<_>>()
        );
    }

    // Phase 3: force=true at A re-parses A and re-resolves its edges
    // against the now-larger symbol_index (which includes B's
    // symbols). The cached-edge asymmetry is resolved by the force.
    run_analyze(&server, &subtree_a, true).await;
    {
        let g = server.inner.graph.read();
        let a_caller_id = format!("{a_cpp_str}:a_caller");
        let a_caller_callees = g.callees(&a_caller_id, 1, None);
        assert!(
            a_caller_callees
                .iter()
                .any(|c| c.symbol_id == format!("{b_cpp_str}:b_target")),
            "after force=true at A, a_caller's call to b_target MUST now resolve \
             — force-reindex closes the asymmetric-resolve gap. Got: {:?}",
            a_caller_callees.iter().map(|c| &c.symbol_id).collect::<Vec<_>>()
        );
    }
}
