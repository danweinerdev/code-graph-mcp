//! Watch-mode end-to-end test for `[cpp].macro_define_function` (Finding 1).
//!
//! The full `analyze_codebase` path runs each plugin's per-file
//! `synthesize_symbols` hook right after `parse_file`
//! (`indexer::index_directory`). The watch reindex path
//! (`handlers::watch::try_reindex_file`) is a SECOND parse pipeline; before
//! the fix it parsed and merged without ever calling `synthesize_symbols`,
//! so a watched edit of a file containing a `macro_define_function`
//! invocation silently replaced its FileGraph with one missing every
//! synthesized symbol — split-brain indexing (correct after cold analyze,
//! wrong after the next save).
//!
//! This test pins parity: index a file with one macro invocation, start the
//! watcher, rewrite the file with a real symbol (the reindex sentinel) plus
//! a SECOND macro invocation, then assert BOTH synthesized symbols survive
//! the reindex. None of the other watch tests (`watch_cpp_macro_strip` only
//! covers `preprocess`; the Rust/Go/Python ones inherit the no-op synthesis
//! default) would fail if `synthesize_symbols` were dropped on this path.

use std::time::Duration;

use code_graph_lang::LanguageRegistry;
use code_graph_lang_cpp::CppParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::handlers::watch::{watch_start, watch_stop};
use code_graph_tools::CodeGraphServer;
use tempfile::TempDir;

mod common;

fn fresh_server() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().expect("CppParser::new")))
        .unwrap();
    CodeGraphServer::new(registry)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cpp_macro_define_function_survives_watch_reindex() {
    let dir = TempDir::new().expect("TempDir");
    // Config carries the macro_define_function entry BEFORE analyze so the
    // cached `inner.config` (what try_reindex_file reads) has it.
    std::fs::write(
        dir.path().join(".code-graph.toml"),
        "[cpp]\nmacro_define_function = [\n  { name = \"MAKE_FN\", arg = 0, suffix = \"_impl\" },\n]\n",
    )
    .unwrap();
    // Seed the subject file with a real symbol + one macro invocation.
    let subject = dir.path().join("subject.cpp");
    std::fs::write(&subject, "void real_fn() {}\nMAKE_FN(Alpha)\n").unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let subject = std::fs::canonicalize(&subject).unwrap();

    let server = fresh_server();

    // Cold analyze. force=true so no stale cache masks the assertion.
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

    // Baseline: cold analyze synthesized Alpha_impl.
    {
        let g = server.inner.graph.read();
        let names: Vec<String> = g
            .file_symbols(&subject)
            .iter()
            .map(|s| s.name.clone())
            .collect();
        assert!(
            names.contains(&"Alpha_impl".to_string()),
            "cold analyze must synthesize Alpha_impl; got: {names:?}"
        );
    }

    // Start the watcher.
    let r = watch_start(&server.inner);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "watch_start failed: {r:?}"
    );

    // Rewrite the file: add a second real symbol (the reindex sentinel) and
    // a second macro invocation. A modify event drives try_reindex_file.
    std::fs::write(
        &subject,
        "void real_fn() {}\nvoid real_fn2() {}\nMAKE_FN(Alpha)\nMAKE_FN(Beta)\n",
    )
    .unwrap();

    // Poll for the reindex using the REAL (non-synthesized) symbol as the
    // sentinel — once `real_fn2` lands, the file was genuinely reparsed and
    // merged, so the synthesized-symbol assertions can run immediately and a
    // dropped-synthesis regression fails fast rather than after a fixed wait.
    let merged = common::wait_until(Duration::from_secs(10), || {
        server
            .inner
            .graph
            .read()
            .file_symbols(&subject)
            .iter()
            .any(|s| s.name == "real_fn2")
    })
    .await;
    assert!(
        merged,
        "watch reindex never merged the edit (real_fn2 sentinel never appeared)"
    );

    // Discriminator: the synthesized symbols must survive the watch reindex.
    // Pre-fix, both Alpha_impl and Beta_impl would be absent here because the
    // reindex replaced the FileGraph without running synthesize_symbols.
    let g = server.inner.graph.read();
    let names: Vec<String> = g
        .file_symbols(&subject)
        .iter()
        .map(|s| s.name.clone())
        .collect();
    assert!(
        names.contains(&"real_fn2".to_string()),
        "sentinel real_fn2 must be present post-reindex; got: {names:?}"
    );
    assert!(
        names.contains(&"Alpha_impl".to_string()),
        "watch reindex must re-run synthesize_symbols — Alpha_impl regressed; got: {names:?}"
    );
    assert!(
        names.contains(&"Beta_impl".to_string()),
        "watch reindex must synthesize the newly-added MAKE_FN(Beta); got: {names:?}"
    );

    let _ = watch_stop(&server.inner);
    drop(dir);
}
