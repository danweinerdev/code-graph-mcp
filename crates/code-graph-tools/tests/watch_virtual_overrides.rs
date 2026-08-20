//! Watch-path Overrides resolution regression (gate artifact 21
//! follow-up): `watch.rs`'s edge match had no `EdgeKind::Overrides` arm,
//! so a watch-reindexed file's override edges kept their bare
//! `Parent::name` token, `is_resolved_node` filtered them, and
//! `find_overrides` MISSED overrides from any file edited under watch
//! until the next full analyze.
//!
//! The test drives `try_reindex_file` directly (the
//! `watch_dangling_edges` determinism convention — the OS-watcher path is
//! pinned by `watch_cpp_macro_strip`/`watch_race`); the property under
//! test is the resolve arm, not event delivery.
//!
//! Identifier hygiene: generic placeholders only (`Base` / `Derived` /
//! `Foo`), matching `virtual_overrides.rs`.

use code_graph_lang::LanguageRegistry;
use code_graph_lang_cpp::CppParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::handlers::watch::{try_reindex_file, ReindexOutcome};
use code_graph_tools::CodeGraphServer;
use tempfile::TempDir;

fn fresh_server() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().expect("CppParser::new")))
        .unwrap();
    CodeGraphServer::new(registry)
}

const BASE_SRC: &str =
    "class Base {\npublic:\n    virtual void Foo();\n};\n\nvoid Base::Foo() {}\n";
const DERIVED_SRC: &str = "#include \"base.cpp\"\n\nclass Derived : public Base {\npublic:\n    void Foo() override;\n};\n\nvoid Derived::Foo() {}\n";
/// Same shape, different body — a realistic watched edit that must NOT
/// cost the override its resolution.
const DERIVED_EDITED_SRC: &str = "#include \"base.cpp\"\n\nclass Derived : public Base {\npublic:\n    void Foo() override;\n};\n\nvoid Derived::Foo() {\n    // edited under watch\n}\n";

#[tokio::test]
async fn watch_reindex_keeps_override_edges_resolved() {
    let server = fresh_server();
    let dir = TempDir::new().unwrap();
    let root = code_graph_core::paths::canonicalize(dir.path()).unwrap();
    std::fs::write(root.join(".code-graph.toml"), "[cpp]\n").unwrap();
    std::fs::write(root.join("base.cpp"), BASE_SRC).unwrap();
    let derived_path = root.join("derived.cpp");
    std::fs::write(&derived_path, DERIVED_SRC).unwrap();

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
        "analyze_codebase failed: {r:?}"
    );

    let base_foo_id = format!("{}:Base::Foo", root.join("base.cpp").to_string_lossy());
    let derived_foo_id = format!("{}:Derived::Foo", derived_path.to_string_lossy());

    // Sentinel (analyze path): the override resolves through the indexer's
    // Overrides arm. If THIS fails, the fixture is broken — not the watch
    // arm under test.
    {
        let g = server.inner.graph.read();
        let overrides = g.find_overrides(&base_foo_id);
        assert!(
            overrides.iter().any(|c| c.symbol_id == derived_foo_id),
            "sentinel: analyze must resolve Derived::Foo over Base::Foo; got {:?}",
            overrides.iter().map(|c| &c.symbol_id).collect::<Vec<_>>()
        );
    }

    // The watched edit: same override, different body.
    std::fs::write(&derived_path, DERIVED_EDITED_SRC).unwrap();
    let outcome = try_reindex_file(&server.inner, &derived_path, false).await;
    assert!(
        matches!(outcome, ReindexOutcome::Reindexed),
        "expected Reindexed, got {outcome:?}"
    );

    // Discriminator: pre-fix, the reindexed file's Overrides edge kept its
    // bare `Base::Foo` token (never resolved), is_resolved_node dropped
    // it, and this list came back WITHOUT Derived::Foo until the next
    // analyze. Post-fix the watch arm resolves it like the indexer does.
    let g = server.inner.graph.read();
    let overrides = g.find_overrides(&base_foo_id);
    assert!(
        overrides.iter().any(|c| c.symbol_id == derived_foo_id),
        "watch reindex must keep Derived::Foo resolved over Base::Foo \
         (the Overrides arm added by the artifact-21 follow-up); got {:?}",
        overrides.iter().map(|c| &c.symbol_id).collect::<Vec<_>>()
    );
    let hop = overrides
        .iter()
        .find(|c| c.symbol_id == derived_foo_id)
        .expect("checked above");
    assert_eq!(
        hop.candidates, 1,
        "sole-candidate override resolves with count 1 through the watch arm too"
    );
}
