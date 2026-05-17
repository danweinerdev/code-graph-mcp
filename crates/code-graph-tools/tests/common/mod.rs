//! Shared helpers for `code-graph-tools` integration and snapshot tests.
//!
//! Cargo treats each `tests/*.rs` file as an independent crate, so a shared
//! module needs the special-cased `tests/common/mod.rs` path. Test files
//! pull these in with `mod common;`.
//!
//! `#[allow(dead_code)]` on each helper because cargo recompiles this
//! module once per test crate and not every crate uses every helper —
//! e.g. the watch-race tests don't seed from `testdata/cpp` and so don't
//! call `testdata_cpp_path`/`copy_testdata`.

use std::path::{Path, PathBuf};

use rmcp::model::CallToolResult;

/// Resolve the source `testdata/cpp` directory used to seed each
/// per-test TempDir copy. Canonicalizes to defeat symlink-based path
/// surprises in CI environments.
#[allow(dead_code)]
pub fn testdata_cpp_path() -> PathBuf {
    testdata_subdir("cpp")
}

/// Resolve the source `testdata/mixed` directory (cross-language
/// fixture: `foo.cpp` + `foo.rs`, both defining `helper`). Canonicalizes
/// for the same reason as `testdata_cpp_path`.
#[allow(dead_code)]
pub fn testdata_mixed_path() -> PathBuf {
    testdata_subdir("mixed")
}

/// Resolve the source `testdata/rust` directory (Rust corpus fixture).
/// Canonicalizes for the same reason as `testdata_cpp_path`.
#[allow(dead_code)]
pub fn testdata_rust_path() -> PathBuf {
    testdata_subdir("rust")
}

/// Resolve the source `testdata/ue` directory (a hand-crafted UE-style
/// header with API-export macros plus a `.code-graph.toml` declaring the
/// matching `[cpp].macro_strip`).
/// Canonicalizes for the same reason as `testdata_cpp_path`.
#[allow(dead_code)]
pub fn testdata_ue_path() -> PathBuf {
    testdata_subdir("ue")
}

/// Shared workhorse for the per-language `testdata_*_path` helpers.
fn testdata_subdir(name: &str) -> PathBuf {
    let raw = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("testdata")
        .join(name);
    std::fs::canonicalize(&raw)
        .unwrap_or_else(|e| panic!("canonicalize {raw:?} failed: {e}; testdata must exist"))
}

/// Recursively copy every file in `testdata_cpp_path()` into `dest`. Each
/// test using this gets its own TempDir so concurrent `analyze_codebase`
/// calls don't race on the shared `.code-graph-cache.json` write.
#[allow(dead_code)]
pub fn copy_testdata(dest: &Path) {
    copy_testdata_from(&testdata_cpp_path(), dest);
}

/// Recursively copy every file under `src` into `dest`. Generic counterpart
/// to `copy_testdata` — used by mixed-language tests that seed
/// from `testdata/mixed/` or `testdata/rust/` instead of the C++ corpus.
#[allow(dead_code)]
pub fn copy_testdata_from(src: &Path, dest: &Path) {
    for entry in walkdir::WalkDir::new(src) {
        let entry = entry.expect("walk testdata");
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(src)
            .expect("path within testdata");
        let target = dest.join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::copy(entry.path(), &target).unwrap_or_else(|e| {
            panic!("copy {:?} → {:?}: {e}", entry.path(), target);
        });
    }
}

/// Inline Go fixture used by both `mixed_language.rs` and
/// `snapshot_responses.rs` to exercise `get_class_hierarchy` /
/// `get_file_symbols` against a Go interface plus a struct that
/// structurally implements it. The struct must NOT show up as `derived`
/// for the interface — Go interfaces are structural, so the parser emits
/// zero `Inherits` edges. Centralized here so both
/// call sites stay byte-identical and any future shape tweak lands in
/// one place.
#[allow(dead_code)]
pub const GO_INTERFACE_FIXTURE: &str = "package main\n\n\
     type Reader interface {\n\
     \tRead() error\n\
     }\n\n\
     type MyReader struct{}\n\n\
     func (m *MyReader) Read() error { return nil }\n";

/// Pull the first text block out of a `CallToolResult`. All callers route
/// through here so a future change to rmcp's `Content` shape surfaces in
/// one place rather than across every test.
#[allow(dead_code)]
pub fn first_text(r: &CallToolResult) -> String {
    r.content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.to_string())
        .unwrap_or_default()
}

/// Parse a tool response body into a JSON value, asserting it is not an
/// error result first (so a failed handler surfaces its message instead
/// of an opaque JSON-parse panic). Centralized so every integration
/// suite shares one definition.
#[allow(dead_code)]
pub fn ok_json(r: &CallToolResult) -> serde_json::Value {
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "expected a non-error result; body: {}",
        first_text(r),
    );
    serde_json::from_str(&first_text(r)).expect("response body must be valid JSON")
}

/// Poll `cond` every 25ms until it returns `true` or `timeout` elapses;
/// returns whether the condition was observed.
///
/// Watch-mode tests wait on an asynchronous pipeline with no fixed upper
/// bound: filesystem event → 250ms debounce window → in-process channel
/// → parse → graph merge under the write lock. A one-shot `sleep(800ms)`
/// is a flaky cliff — it fails whenever `cargo test --workspace`
/// parallelism pushes that pipeline past the slack. Polling returns as
/// soon as the merge lands (≈300ms common case) and tolerates a slow or
/// contended CI machine up to `timeout`. `cond` returns a bool, so any
/// read-lock guard it takes is released at the end of each call and is
/// never held across the poll-interval await.
#[allow(dead_code)]
pub async fn wait_until<F>(timeout: std::time::Duration, mut cond: F) -> bool
where
    F: FnMut() -> bool,
{
    const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
