//! Baseline integration test for `analyze_codebase` against `testdata/cpp`.
//!
//! Locks in the empirical Phase 1.6 byte-counts (8 files, 18 symbols, 21
//! edges, 0 warnings) so any future change to discovery, parsing, or edge
//! resolution that drifts the totals trips this test before a snapshot
//! review catches it. Calls the analyze handler function directly to keep
//! the test focused on the indexing pipeline rather than the rmcp wire
//! plumbing — `binary_advertises_fifteen_tools` already covers the wire
//! path.
//!
//! The edge count baseline comes from the Phase 1.6 debrief, which is
//! authoritative; `testdata/cpp/MANIFEST.md` only enumerates a subset of
//! edges and does not match the indexed total.

use std::path::PathBuf;

use code_graph_lang::LanguageRegistry;
use code_graph_lang_cpp::CppParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::server::CodeGraphServer;

/// Resolve the absolute path of `testdata/cpp` from this crate's manifest
/// directory. Two `..` segments back up out of `crates/code-graph-tools/`
/// to the workspace root, matching the layout the smoke test relies on.
fn testdata_cpp_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("testdata")
        .join("cpp")
}

fn server_with_cpp_parser() -> CodeGraphServer {
    let mut reg = LanguageRegistry::new();
    reg.register(Box::new(CppParser::new().expect("CppParser::new")))
        .expect("register CppParser");
    CodeGraphServer::new(reg)
}

#[tokio::test]
async fn analyze_testdata_cpp_locks_in_baseline_counts() {
    let path = testdata_cpp_path();
    assert!(
        path.is_dir(),
        "testdata/cpp must exist at {} for this test",
        path.display()
    );

    let server = server_with_cpp_parser();
    // `force = true` so any stale `.code-graph-cache.json` left over from a
    // manual run never masks a real regression.
    let r = analyze_codebase(
        server.inner.clone(),
        path.to_string_lossy().into_owned(),
        true,
        None,
        None,
    )
    .await;

    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "analyze_codebase must succeed, got: {r:?}",
    );

    let body = r
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.to_string())
        .unwrap_or_default();
    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("analyze response must be valid JSON");

    assert_eq!(
        parsed["files"],
        serde_json::json!(8),
        "files count drifted from baseline; full body: {body}",
    );
    assert_eq!(
        parsed["symbols"],
        serde_json::json!(18),
        "symbols count drifted from baseline; full body: {body}",
    );
    assert_eq!(
        parsed["edges"],
        serde_json::json!(21),
        "edges count drifted from baseline; full body: {body}",
    );
    // `warnings` is `omitempty`-flavored on the Rust side: the field is
    // skipped when the Vec is empty. Either absent or an empty array is
    // acceptable; a non-empty array would be a regression.
    match parsed.get("warnings") {
        None => {}
        Some(serde_json::Value::Array(a)) => assert!(
            a.is_empty(),
            "warnings must be empty for testdata/cpp, got: {a:?}",
        ),
        Some(other) => panic!("warnings must be array or absent, got {other:?}"),
    }
}
