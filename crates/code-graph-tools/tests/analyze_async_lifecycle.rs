//! End-to-end lifecycle test for `analyze_codebase_async` through the
//! production `LanguageRegistry` (every shipped plugin registered, same
//! as the binary's `main.rs`).
//!
//! Exercises the agent's expected flow: kickoff -> poll `get_status`
//! until terminal -> query `get_file_symbols`. The 1-second poll bound
//! is a hang catcher per the plan; on a real machine the small mixed
//! C++ fixture indexes in tens of milliseconds.
//!
//! The cross-server symbol-count cross-check (plan 2.4(a)) runs a second
//! `analyze_codebase` (sync) on a separate `CodeGraphServer` over the
//! same fixture and asserts the async terminal `result.files` /
//! `result.symbols` / `result.edges` match the sync wire response
//! byte-for-byte — pinning that the two code paths produce the same
//! `AnalyzeResult` for identical input.

use std::sync::Arc;
use std::time::{Duration, Instant};

use code_graph_lang::LanguageRegistry;
use code_graph_lang_cpp::CppParser;
use code_graph_lang_csharp::CSharpParser;
use code_graph_lang_go::GoParser;
use code_graph_lang_java::JavaParser;
use code_graph_lang_python::PythonParser;
use code_graph_lang_rust::RustParser;
use code_graph_tools::handlers::analyze::{analyze_codebase, analyze_codebase_async};
use code_graph_tools::handlers::status::get_status;
use code_graph_tools::handlers::symbols::get_file_symbols;
use code_graph_tools::handlers::NO_BYTE_BUDGET;
use code_graph_tools::CodeGraphServer;
use rmcp::model::CallToolResult;
use tempfile::TempDir;

mod common;
use common::{copy_testdata, first_text, ok_json};

/// Build a `CodeGraphServer` with every shipped language plugin
/// registered — mirrors `crates/code-graph-mcp/src/main.rs`.
fn production_server() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().expect("CppParser::new")))
        .expect("register C++");
    registry
        .register(Box::new(RustParser::new().expect("RustParser::new")))
        .expect("register Rust");
    registry
        .register(Box::new(GoParser::new().expect("GoParser::new")))
        .expect("register Go");
    registry
        .register(Box::new(PythonParser::new().expect("PythonParser::new")))
        .expect("register Python");
    registry
        .register(Box::new(CSharpParser::new().expect("CSharpParser::new")))
        .expect("register C#");
    registry
        .register(Box::new(JavaParser::new().expect("JavaParser::new")))
        .expect("register Java");
    CodeGraphServer::new(registry)
}

/// Snapshot the `analyze_job` sub-object of a `get_status` response.
/// Returns `None` while the field is JSON `null` (no analyze ever).
fn analyze_job_view(r: &CallToolResult) -> Option<serde_json::Value> {
    let parsed: serde_json::Value =
        serde_json::from_str(&first_text(r)).expect("get_status body must be valid JSON");
    match &parsed["analyze_job"] {
        serde_json::Value::Null => None,
        v => Some(v.clone()),
    }
}

#[tokio::test]
async fn async_kickoff_poll_then_query_symbols_end_to_end() {
    // Seed a TempDir from `testdata/cpp` (8 source files: utils.h,
    // utils.cpp, engine.h, engine.cpp, main.cpp, circular_a.h,
    // circular_b.h, orphan.cpp) — well inside the 5-10 file band and
    // a mix of free functions, classes, methods, enums, typedefs.
    let dir = TempDir::new().expect("tempdir");
    copy_testdata(dir.path());
    let path = std::fs::canonicalize(dir.path()).expect("canonicalize tempdir");
    let path_str: String = path.to_string_lossy().into_owned();

    // Count the source files we wrote so we can pin `result.files`
    // exactly (the testdata fixture is stable but the assertion stays
    // robust against `MANIFEST.md` reshuffles).
    let expected_files = count_source_files(&path);
    assert!(
        expected_files >= 5,
        "fixture must have >= 5 source files; copied {expected_files}"
    );

    // ----- Async server: kickoff + poll to terminal. ----------------------
    let server_async = production_server();
    let inner_async = server_async.inner.clone();

    let kickoff = analyze_codebase_async(inner_async.clone(), path_str.clone(), false).await;
    let kickoff_body: serde_json::Value = ok_json(&kickoff);
    assert_eq!(kickoff_body["status"], serde_json::json!("running"));
    assert_eq!(kickoff_body["existing"], serde_json::json!(false));
    let job_id = kickoff_body["job_id"]
        .as_str()
        .expect("kickoff response must carry a job_id")
        .to_string();
    assert!(!job_id.is_empty(), "job_id must be non-empty");

    // Poll loop: 50ms cadence, 1s total bound. A small fixture indexes
    // in tens of ms on any reasonable machine; hitting the 1s bound
    // means the worker hung or terminal state never flushed.
    let deadline = Instant::now() + Duration::from_secs(1);
    let terminal: serde_json::Value = loop {
        if Instant::now() >= deadline {
            let final_status = get_status(inner_async.clone());
            panic!(
                "async job {job_id} did not reach terminal within 1s; \
                 last get_status body: {}",
                first_text(&final_status)
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        let status = get_status(inner_async.clone());
        if let Some(job) = analyze_job_view(&status) {
            let s = job["status"].as_str().unwrap_or("");
            if s == "completed" || s == "failed" {
                break job;
            }
        }
    };

    assert_eq!(
        terminal["status"],
        serde_json::json!("completed"),
        "expected Completed terminal; got: {terminal}"
    );
    assert_eq!(
        terminal["job_id"].as_str(),
        Some(job_id.as_str()),
        "terminal job_id must match the kickoff job_id"
    );
    let async_result = &terminal["result"];
    assert!(
        async_result.is_object(),
        "Completed terminal must carry a result object; got: {terminal}"
    );
    assert_eq!(
        async_result["files"],
        serde_json::json!(expected_files),
        "result.files must match the on-disk source-file count"
    );

    // ----- Query path: get_file_symbols on an indexed file. ---------------
    // engine.cpp is the densest fixture file (5 method definitions per
    // the testdata MANIFEST); pick it to assert a non-empty symbol list.
    let engine_cpp = path.join("engine.cpp").to_string_lossy().into_owned();
    let symbols = get_file_symbols(
        &inner_async.graph,
        &engine_cpp,
        false,
        true,
        None,
        None,
        false,
        NO_BYTE_BUDGET,
    );
    let symbols_body = ok_json(&symbols);
    let results = symbols_body["results"]
        .as_array()
        .expect("get_file_symbols returns a Page<SymbolResult> envelope");
    assert!(
        !results.is_empty(),
        "engine.cpp must have at least one symbol after async indexing; body: {symbols_body}"
    );

    // ----- Cross-server cross-check: sync analyze on the SAME fixture. ----
    // A second server with its own TempDir-rooted graph confirms the
    // async path produced the same `AnalyzeResult` shape as sync.
    // Using a separate TempDir keeps the two `.code-graph-cache.db`
    // writes from racing or sharing data.
    let dir_sync = TempDir::new().expect("sync tempdir");
    copy_testdata(dir_sync.path());
    let path_sync = std::fs::canonicalize(dir_sync.path()).expect("canonicalize sync tempdir");
    let server_sync = production_server();
    let sync_r = analyze_codebase(
        server_sync.inner.clone(),
        path_sync.to_string_lossy().into_owned(),
        false,
        None,
        None,
    )
    .await;
    let sync_body = ok_json(&sync_r);

    assert_eq!(
        async_result["files"], sync_body["files"],
        "files count must match sync/async; async: {async_result}, sync: {sync_body}"
    );
    assert_eq!(
        async_result["symbols"], sync_body["symbols"],
        "symbol count must match sync/async; async: {async_result}, sync: {sync_body}"
    );
    assert_eq!(
        async_result["edges"], sync_body["edges"],
        "edge count must match sync/async; async: {async_result}, sync: {sync_body}"
    );

    // Sanity floor — the engine.cpp fixture has methods, so symbols and
    // edges must be non-zero. Catches the failure mode where both paths
    // silently produce an empty graph.
    assert!(
        async_result["symbols"].as_u64().unwrap_or(0) >= 5,
        "symbol count below sanity floor; got: {async_result}"
    );
    assert!(
        async_result["edges"].as_u64().unwrap_or(0) >= 1,
        "edge count below sanity floor; got: {async_result}"
    );

    drop(server_async);
    drop(server_sync);
    let _ = Arc::strong_count(&inner_async);
}

/// Count source files in `dir` for the file extensions the production
/// `LanguageRegistry` claims. Mirrors the discovery walker's filter
/// closely enough to predict `result.files`; centralized so the
/// assertion stays robust if the fixture gains/loses files.
fn count_source_files(dir: &std::path::Path) -> u32 {
    let mut count = 0u32;
    for entry in walkdir::WalkDir::new(dir) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        if let Some(ext) = entry.path().extension().and_then(|s| s.to_str()) {
            let lower = ext.to_ascii_lowercase();
            if matches!(
                lower.as_str(),
                "cpp"
                    | "cc"
                    | "cxx"
                    | "c"
                    | "h"
                    | "hpp"
                    | "hxx"
                    | "rs"
                    | "go"
                    | "py"
                    | "pyi"
                    | "cs"
                    | "java"
            ) {
                count += 1;
            }
        }
    }
    count
}
