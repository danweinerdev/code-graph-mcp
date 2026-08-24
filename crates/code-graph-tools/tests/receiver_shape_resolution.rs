//! F2 (KNOWN_ISSUES) end-to-end regression: receiver-typed sole-candidate
//! calls must not resolve as `Resolved/1`.
//!
//! Mirrors the build-mcp smoke-test shape that surfaced the issue: exactly
//! ONE indexed method named `is_empty` (`AdapterRegistry::is_empty`), many
//! receiver-typed `.is_empty()` call sites on receivers the index cannot
//! verify (std `Vec`, a struct field), and one verified caller
//! (`self.is_empty()` inside the same impl — the SelfReceiver shape whose
//! receiver type is the enclosing type, so the parent match keeps it
//! `Resolved`). Under `min_confidence="resolved"` only the verified caller
//! survives; under the default `"any"` every receiver-typed caller still
//! appears, now honestly tagged (observable via the resolved filter, since
//! `CallChain` rows carry `candidates` but not `confidence`).

mod common;

use std::path::Path;

use code_graph_lang::LanguageRegistry;
use code_graph_lang_rust::RustParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::handlers::query::{callers_or_callees, Direction};
use code_graph_tools::handlers::NO_BYTE_BUDGET;
use code_graph_tools::CodeGraphServer;
use common::ok_json;
use tempfile::TempDir;

fn rust_only_server() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(RustParser::new().expect("RustParser::new")))
        .expect("register RustParser");
    CodeGraphServer::new(registry)
}

async fn analyze(server: &CodeGraphServer, dir: &Path) {
    let result = analyze_codebase(
        server.inner.clone(),
        dir.to_string_lossy().into_owned(),
        true,
        None,
        None,
    )
    .await;
    assert!(
        result.is_error.is_none() || result.is_error == Some(false),
        "analyze_codebase must succeed: {result:?}"
    );
}

fn write_rs(dir: &Path, relative: &str, source: &str) -> std::path::PathBuf {
    let path = dir.join(relative);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    std::fs::write(&path, source).expect("write source");
    code_graph_core::paths::canonicalize(&path).expect("canonicalize source")
}

#[tokio::test]
async fn receiver_typed_sole_candidate_callers_are_filterable() {
    let dir = TempDir::new().expect("TempDir");

    // The ONE indexed `is_empty` in the project. Its own body calls
    // `self.adapters.is_empty()` — a receiver-typed call whose true target
    // is std's `Vec::is_empty` (unindexed), historically a false
    // `Resolved/1` self-edge. `validate` calls `self.is_empty()` — the
    // SelfReceiver shape whose parent match keeps it verified.
    let registry_rs = write_rs(
        dir.path(),
        "src/registry.rs",
        "pub struct AdapterRegistry {\n\
         \x20   adapters: Vec<u32>,\n\
         }\n\
         impl AdapterRegistry {\n\
         \x20   pub fn is_empty(&self) -> bool { self.adapters.is_empty() }\n\
         \x20   pub fn validate(&self) -> bool { self.is_empty() }\n\
         }\n",
    );
    // True caller through a receiver variable: correct edge, but the
    // generic resolver cannot verify `registry`'s type — honest Heuristic.
    let server_rs = write_rs(
        dir.path(),
        "src/server.rs",
        "use crate::registry::AdapterRegistry;\n\
         pub fn serve(registry: &AdapterRegistry) -> bool { registry.is_empty() }\n",
    );
    // False caller: `v` is a std Vec; its `is_empty` lives in std, not the
    // index. Pre-F2 this resolved `Resolved/1` to AdapterRegistry::is_empty.
    let noise_rs = write_rs(
        dir.path(),
        "src/noise.rs",
        "pub fn noise() -> bool {\n\
         \x20   let v: Vec<u32> = Vec::new();\n\
         \x20   v.is_empty()\n\
         }\n",
    );

    let root = code_graph_core::paths::canonicalize(dir.path()).expect("canonicalize root");
    let server = rust_only_server();
    analyze(&server, &root).await;

    let target = format!(
        "{}:AdapterRegistry::is_empty",
        registry_rs.to_string_lossy()
    );

    // Default `any`: every receiver-typed caller still surfaces — the fix
    // does not hide edges, it re-tags them.
    let any = ok_json(&callers_or_callees(
        &server.inner.graph,
        true,
        &target,
        Some(1),
        Direction::Callers,
        Some(50),
        Some(0),
        NO_BYTE_BUDGET,
        None,
    ));
    let any_ids: Vec<String> = any["results"]
        .as_array()
        .expect("caller rows")
        .iter()
        .filter_map(|row| row["symbol_id"].as_str().map(String::from))
        .collect();
    for expected in [
        format!(
            "{}:AdapterRegistry::validate",
            registry_rs.to_string_lossy()
        ),
        format!("{}:serve", server_rs.to_string_lossy()),
        format!("{}:noise", noise_rs.to_string_lossy()),
    ] {
        assert!(
            any_ids.contains(&expected),
            "min_confidence=any must keep every receiver-typed caller \
             ({expected} missing): {any}"
        );
    }

    // `resolved`: exactly the verified SelfReceiver caller survives. The
    // false std-receiver caller (`noise`), the unverifiable-but-true
    // receiver caller (`serve`), and the false std self-edge from
    // `is_empty`'s own body are all Heuristic now — the confidence
    // protocol's promise ("resolved = confident") is restored.
    let resolved = ok_json(&callers_or_callees(
        &server.inner.graph,
        true,
        &target,
        Some(1),
        Direction::Callers,
        Some(50),
        Some(0),
        NO_BYTE_BUDGET,
        Some("resolved"),
    ));
    let resolved_ids: Vec<String> = resolved["results"]
        .as_array()
        .expect("resolved caller rows")
        .iter()
        .filter_map(|row| row["symbol_id"].as_str().map(String::from))
        .collect();
    assert_eq!(
        resolved_ids,
        vec![format!(
            "{}:AdapterRegistry::validate",
            registry_rs.to_string_lossy()
        )],
        "min_confidence=resolved must return exactly the parent-verified \
         self-receiver caller: {resolved}"
    );
}
