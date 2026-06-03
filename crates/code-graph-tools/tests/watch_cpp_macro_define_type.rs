//! Watch-mode end-to-end test for `[cpp].macro_define_type`.
//!
//! The full `analyze_codebase` path runs each plugin's `preprocess` hook
//! (which is where `macro_define_type` expansion lives) right before
//! `parse_file` (`indexer::index_directory`). The watch reindex path
//! (`handlers::watch::try_reindex_file`) is a SECOND parse pipeline; it must
//! ALSO call `preprocess` so a watched edit of a file whose struct is hidden
//! behind a configured macro re-expands and the type stays recovered.
//!
//! This test pins parity: index a file with one macro-wrapped struct, start
//! the watcher, rewrite the file adding a real symbol (the reindex sentinel)
//! plus a SECOND macro-wrapped struct, then assert BOTH expanded types
//! survive the reindex. Mirrors `watch_cpp_macro_define_function.rs`.

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
async fn cpp_macro_define_type_survives_watch_reindex() {
    let dir = TempDir::new().expect("TempDir");
    // Config carries the macro_define_type entry BEFORE analyze so the cached
    // `inner.config` (what try_reindex_file reads) has it.
    std::fs::write(
        dir.path().join(".code-graph.toml"),
        "[cpp]\nmacro_define_type = [\n  { name = \"EXPORT_STRUCT\", name_arg = 0, keyword = \"struct\" },\n]\n",
    )
    .unwrap();
    // Seed the subject file with a real symbol + one macro-wrapped struct.
    let subject = dir.path().join("subject.cpp");
    std::fs::write(
        &subject,
        "void real_fn() {}\nEXPORT_STRUCT(Alpha, (int a; void am();));\n",
    )
    .unwrap();
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

    // Baseline sentinel: cold analyze expanded Alpha (a no-edit baseline so a
    // pure-config or wiring break is distinguished from a watch-path break).
    {
        let g = server.inner.graph.read();
        let names: Vec<String> = g
            .file_symbols(&subject)
            .iter()
            .map(|s| s.name.clone())
            .collect();
        assert!(
            names.contains(&"Alpha".to_string()),
            "cold analyze must expand EXPORT_STRUCT(Alpha); got: {names:?}"
        );
    }

    // Start the watcher.
    let r = watch_start(&server.inner);
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "watch_start failed: {r:?}"
    );

    // Rewrite the file: add a second real symbol (the reindex sentinel) and a
    // second macro-wrapped struct. A modify event drives try_reindex_file.
    std::fs::write(
        &subject,
        "void real_fn() {}\nvoid real_fn2() {}\nEXPORT_STRUCT(Alpha, (int a; void am();));\nEXPORT_STRUCT(Beta, (int b;));\n",
    )
    .unwrap();

    // Poll for the reindex using the REAL (non-expanded) symbol as the
    // sentinel — once `real_fn2` lands, the file was genuinely reparsed and
    // merged, so the expanded-symbol assertions can run immediately and a
    // dropped-preprocess regression fails fast rather than after a fixed wait.
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

    // Discriminator: the expanded types must survive the watch reindex.
    // Pre-fix (or if try_reindex_file dropped preprocess), Alpha and Beta
    // would be absent because the macro-hidden struct never re-expands.
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
        names.contains(&"Alpha".to_string()),
        "watch reindex must re-run preprocess — Alpha regressed; got: {names:?}"
    );
    assert!(
        names.contains(&"Beta".to_string()),
        "watch reindex must expand the newly-added EXPORT_STRUCT(Beta); got: {names:?}"
    );

    let _ = watch_stop(&server.inner);
    drop(dir);
}
