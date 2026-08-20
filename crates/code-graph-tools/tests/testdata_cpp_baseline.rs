//! Baseline integration test for `analyze_codebase` against `testdata/cpp`.
//!
//! Locks in the empirical indexed totals (8 files, 18 symbols, 17 edges,
//! 0 warnings) so any future change to discovery, parsing, or edge
//! resolution that drifts the totals trips this test before a snapshot
//! review catches it. Calls the analyze handler function directly to keep
//! the test focused on the indexing pipeline rather than the rmcp wire
//! plumbing — `binary_advertises_fifteen_tools` already covers the wire
//! path.
//!
//! Edge-count provenance: an Includes edge is retained only when it
//! resolves to an indexed source file. The `testdata/cpp` fixture's four
//! angle-bracket system-header includes (`<iostream>` in `main.cpp`,
//! `<string>` in `engine.h`, `orphan.cpp`, and `utils.h`) never resolve to
//! an indexed file, so they are dropped rather than leaked into the
//! dependency graph as unresolvable noise — they do not count toward the
//! edge total. The 17 retained edges are the resolvable source-to-source
//! `#include`s plus the Calls/Inherits edges among the indexed symbols;
//! `files` (8) and `symbols` (18) are unaffected by the include filter.
//! `testdata/cpp/MANIFEST.md` only enumerates a subset of edges and does
//! not match the indexed total.

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

/// Copy the fixture's source files into `dest` so the analyze runs against
/// an isolated tree. Analyzing the fixture IN the repo checkout had two
/// defects (found by dogfooding): the project-root upward walk discovers the
/// repo's own `.code-graph.toml`, so the test WROTE its cache into the
/// developer's repo root (poisoning later real analyzes of the repo), and a
/// pre-existing real cache there made merge-not-clobber report the whole
/// project graph — failing the 8-file baseline with whatever the developer
/// last indexed.
fn copy_fixture_to(dest: &std::path::Path) {
    let src = testdata_cpp_path();
    assert!(
        src.is_dir(),
        "testdata/cpp must exist at {} for this test",
        src.display()
    );
    for entry in std::fs::read_dir(&src).expect("read fixture dir") {
        let entry = entry.expect("read fixture entry");
        let path = entry.path();
        let is_source = path
            .extension()
            .is_some_and(|ext| ext == "cpp" || ext == "h");
        if entry.file_type().expect("fixture entry type").is_file() && is_source {
            std::fs::copy(&path, dest.join(entry.file_name())).expect("copy fixture file");
        }
    }
}

fn server_with_cpp_parser() -> CodeGraphServer {
    let mut reg = LanguageRegistry::new();
    reg.register(Box::new(CppParser::new().expect("CppParser::new")))
        .expect("register CppParser");
    CodeGraphServer::new(reg)
}

#[tokio::test]
async fn analyze_testdata_cpp_locks_in_baseline_counts() {
    let dir = tempfile::TempDir::new().expect("create isolated fixture dir");
    let path = code_graph_core::paths::canonicalize(dir.path()).expect("canonicalize fixture dir");
    copy_fixture_to(&path);

    let server = server_with_cpp_parser();
    // `force = true` so any stale `.code-graph-cache.db` left over from a
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
        serde_json::json!(17),
        "edges count drifted from baseline; full body: {body}",
    );
    // `warnings` is `omitempty`-flavored on the Rust side: the field is
    // skipped when the Vec is empty. The counts above are this test's real
    // contract; the warning check is a tripwire for anything unexpected
    // alongside them.
    //
    // The fixture now runs from an isolated temp copy (see
    // `copy_fixture_to`), so exactly one by-design notice can appear:
    // "no .code-graph.toml found" — nothing exists above a temp dir. The
    // previously tolerated repo-config and cached-entries notices are gone
    // with the isolation; their reappearance would mean the test leaked
    // back into ambient state and should fail loudly.
    //
    // Deliberately NOT tolerated: the orphan-cache notice. That one flags
    // a real stale artifact and the remedy is to delete it, not to silence
    // the warning.
    const EXPECTED_NOTICES: [&str; 1] = ["no .code-graph.toml found"];
    if let Some(serde_json::Value::Array(a)) = parsed.get("warnings") {
        for w in a {
            let s = w.as_str().unwrap_or("");
            assert!(
                EXPECTED_NOTICES.iter().any(|notice| s.contains(notice)),
                "unexpected warning beyond the by-design config/cache notices: {s:?}"
            );
        }
    }
}
