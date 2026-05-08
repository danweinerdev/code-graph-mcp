//! Watch-mode end-to-end test for `[cpp].macro_strip` — Phase 2.4 (Test 3).
//!
//! This is the ONLY test that catches a broken `preprocess` wiring in
//! `code-graph-tools::handlers::watch::try_reindex_file`. The other watch
//! tests (`watch_dangling_edges`, `watch_rust_reindex`, `watch_python_reindex`,
//! `watch_go_reindex`) all use Rust/Go/Python fixtures and inherit the
//! default no-op `LanguagePlugin::preprocess` impl, so an implementer
//! could pass `RootConfig::default()` (or skip the call entirely) and
//! every existing watch test would still pass.
//!
//! Coverage: index a tempdir with `[cpp].macro_strip = ["CORE_API"]`,
//! start the watcher, write a fresh `MyActor.h` with a macro-prefixed
//! class to the watched dir, wait for the debounce + reindex, and assert
//! that the macro-stripped class appears in the graph.

use std::time::Duration;

use code_graph_lang::LanguageRegistry;
use code_graph_lang_cpp::CppParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::handlers::watch::{watch_start, watch_stop};
use code_graph_tools::CodeGraphServer;
use tempfile::TempDir;

/// Fresh server with the C++ parser plugin registered. Mirrors
/// `tests/watch_race.rs::fresh_server`.
fn fresh_server() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().expect("CppParser::new")))
        .unwrap();
    CodeGraphServer::new(registry)
}

/// Phase 2.3 verification: `try_reindex_file` must call
/// `LanguagePlugin::preprocess` with the cached `RootConfig` (the same
/// config the most-recent `analyze_codebase` saw), so a file written
/// under a watched root after `analyze_codebase` extracts its
/// macro-prefixed classes correctly.
///
/// If the watch path passes `RootConfig::default()` (or skips
/// `preprocess` altogether), this test fails — the file would parse
/// without substitution and `AActor` would be invisible exactly as it
/// is for the unconfigured user.
///
/// We use the live debouncer here (not a direct `try_reindex_file`
/// call) because the gap this test closes is in the watch loop's
/// reindex path. The 250ms debounce + 800ms slack mirrors
/// `watch_race.rs::editor_atomic_save_rename_coalesces_to_single_reindex`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cpp_macro_strip_watch_mode_picks_up_cached_config() {
    let dir = TempDir::new().expect("TempDir");
    // Seed with `.code-graph.toml` BEFORE analyze_codebase so the
    // resulting `inner.config` carries `macro_strip = ["CORE_API"]`.
    std::fs::write(
        dir.path().join(".code-graph.toml"),
        "[cpp]\nmacro_strip = [\"CORE_API\"]\n",
    )
    .unwrap();
    // Empty seed file ensures `analyze_codebase` finds at least one
    // `.h` file and doesn't short-circuit on an empty root.
    std::fs::write(dir.path().join("seed.h"), "// seed\n").unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();

    let server = fresh_server();

    // Initial index. force=true so the assertion can't be masked by a
    // stale cache. After this returns, `inner.config` carries the
    // `[cpp].macro_strip = ["CORE_API"]` list — that cached config is
    // what `try_reindex_file` must read on the next file event.
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
        "initial analyze must succeed: {r:?}"
    );

    // Sanity: the cached config really does carry macro_strip.
    {
        let cfg = server.inner.config.read();
        assert_eq!(
            cfg.cpp.macro_strip,
            vec!["CORE_API".to_string()],
            "post-analyze, inner.config must carry CORE_API in macro_strip",
        );
    }

    // Start the watcher.
    let r = watch_start(&server.inner);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "watch_start failed: {r:?}"
    );

    // Write the macro-prefixed-class header to the watched dir AFTER
    // the watcher is up. The debouncer (250ms window) collapses the
    // notify event(s) into a single batch; the loop's per-batch
    // reindex calls `try_reindex_file`, which is where Phase 2.3's
    // `preprocess` call site lives.
    let actor_path = root.join("MyActor.h");
    std::fs::write(
        &actor_path,
        "class UObject {};\nclass CORE_API AActor : public UObject {};\n",
    )
    .unwrap();

    // 250ms debounce + generous slack so the merge has landed before
    // we query. Same pattern as `watch_race.rs`.
    tokio::time::sleep(Duration::from_millis(800)).await;

    // The macro-prefixed class must be in the graph. If
    // `try_reindex_file` skipped `preprocess` or used a default
    // `RootConfig`, `AActor` would be missing exactly as it is for an
    // unconfigured user.
    {
        let g = server.inner.graph.read();
        let symbols = g.file_symbols(&actor_path);
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        // UObject is the no-macro baseline — it extracts regardless of
        // macro_strip. If it's missing, the file didn't parse at all
        // (debounce window too short, file write failed, etc.); fail with
        // that diagnosis BEFORE the AActor check so the failure mode is
        // distinguishable.
        assert!(
            names.contains(&"UObject"),
            "UObject is the file-parsed sentinel — its absence means the \
             debounce window is too short or the file write didn't land. \
             Got symbols: {names:?}",
        );
        assert!(
            names.contains(&"AActor"),
            "watch-mode reindex must apply [cpp].macro_strip from cached \
             RootConfig — AActor is the canary symbol that proves \
             try_reindex_file -> preprocess wiring works. Got symbols: \
             {names:?}",
        );
    }

    let _ = watch_stop(&server.inner);
    drop(dir);
}
