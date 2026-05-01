//! Shared helpers for `codegraph-tools` integration and snapshot tests.
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

/// Resolve the source `testdata/mixed` directory (Phase 5.6 cross-language
/// fixture: `foo.cpp` + `foo.rs`, both defining `helper`). Canonicalizes
/// for the same reason as `testdata_cpp_path`.
#[allow(dead_code)]
pub fn testdata_mixed_path() -> PathBuf {
    testdata_subdir("mixed")
}

/// Resolve the source `testdata/rust` directory (Phase 5.5 fixture).
/// Canonicalizes for the same reason as `testdata_cpp_path`.
#[allow(dead_code)]
pub fn testdata_rust_path() -> PathBuf {
    testdata_subdir("rust")
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
/// to `copy_testdata` — used by Phase 5.6 mixed-language tests that seed
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
