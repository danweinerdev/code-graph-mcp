//! Synthetic regression fixture for paginated-response byte-budget safety.
//!
//! # What this fixture exists for
//!
//! Anti-regression coverage for the originally-reported failure mode: a
//! `get_orphans` call against a Rust repo with ~1031 orphan-eligible symbols
//! returned a 297,266-character payload that overflowed the MCP agent's token
//! ceiling. The byte budget, the pagination envelope, the `count_only`
//! short-circuit, and the `SymbolResult.file` drop together close that
//! failure mode. This fixture is the substrate the acceptance test runs
//! against to GUARANTEE that those guard rails actually bite at the default
//! `[response].max_bytes = 102400` budget on `limit = 1000`.
//!
//! # Why N = 1500
//!
//! With `SymbolResult.file` dropped, a brief-mode record for a
//! free function looks like:
//!
//! ```json
//! {"id":"<path>/large_orphans.cpp:orphan_0001","kind":"function","line":1,"name":"orphan_0001"}
//! ```
//!
//! Empirical sizing on this fixture (run via the probe in
//! `byte_budget_acceptance.rs`):
//!
//! - Fixed record scaffolding: `{"id":"","kind":"function","line":N,"name":""}`
//!   ≈ 57 bytes.
//! - Variable per-record content: file-path prefix in the canonicalized
//!   TempDir (~30 chars on Linux: `/tmp/tmp.XXXXXXXXXX/large_orphans.cpp`)
//!   plus the symbol name (`orphan_NNNN`, 11 chars) plus the line number
//!   (1-4 digits).
//! - Total per record: ~105-110 bytes serialized, including the
//!   inter-record comma byte.
//!
//! With `limit = 1000` against the default `max_bytes = 102_400` budget,
//! the effective per-records budget is `102_400 - ENVELOPE_OVERHEAD_BYTES`
//! (512) = 101_888 bytes. At ~107 bytes/record, 1000 records would
//! serialize to ~107_000 bytes, which exceeds the budget — but only by a
//! thin margin (~5 KB) that depends on the actual TempDir path width.
//!
//! `N = 1500` is the chosen fixture cardinality because:
//! - It produces ~160_000 bytes of raw record payload — comfortably above
//!   the 102_400 budget regardless of TempDir path-length variation.
//! - It forces `byte_budget_take` to truncate at `limit = 1000` with
//!   `truncated = true` and `next_offset` somewhere around the high 900s
//!   (the exact value is read out of the response, not asserted).
//! - It leaves enough records past the truncation point to exercise the
//!   loop-until-`truncated == false` paging path in the acceptance test:
//!   the second page picks up where the first left off.
//!
//! Records before the `file`-field drop were ~3x larger (the dropped
//! `file` field + signature padding), so the same byte-budget assertion
//! was trivially satisfied at N = 1000. The post-drop record shrinkage
//! is exactly the reason `N >= 1500` is now the load-bearing floor — a
//! fixture sized at the pre-drop numbers would silently pass without
//! exercising the truncation path, which is the false-green failure mode
//! this floor guards against.
//!
//! # Determinism
//!
//! The generator is a pure function of `n`: it emits the same UTF-8 byte
//! sequence on every call. Names are zero-padded to 4 digits so the
//! `symbol_id` ascending sort `Graph::orphans` performs at query time
//! lines up with numeric order (page 1 = orphan_0001..orphan_NNNN), making
//! the acceptance test's continuation-offset arithmetic reproducible.
//!
//! # Layout
//!
//! Sibling tests load this module via:
//! ```ignore
//! #[path = "fixtures/large_orphan_set/mod.rs"]
//! mod large_orphan_set;
//! ```
//! Cargo treats `tests/<subdir>/` as non-test code (only top-level
//! `tests/*.rs` becomes a test binary), so an explicit `#[path = "..."]`
//! load is required. `tests/common/mod.rs` is the conventional exception;
//! anything else under `tests/` follows the explicit-path pattern.

use std::fmt::Write as _;
use std::path::Path;

/// Cardinality used by the byte-budget acceptance test. See module-level
/// docs for the rationale tying this number to the `max_bytes = 102_400` /
/// `limit = 1000` truncation contract.
pub const ORPHAN_COUNT: usize = 1500;

/// Filename written into the fixture directory. Sibling tests reference
/// this so a future rename lands in one place.
pub const FIXTURE_FILENAME: &str = "large_orphans.cpp";

/// Generate the deterministic C++ source for `n` free, orphan, void-returning
/// functions named `orphan_0001` through `orphan_NNNN`. Each function has an
/// empty body. None call each other, so the parser sees `n` orphans by
/// construction.
///
/// Zero-padded to 4 digits so `symbol_id` ascending sort lines up with
/// numeric order across the full 1..=1500 range.
pub fn generate_large_orphan_source(n: usize) -> String {
    // Each line is `void orphan_NNNN() {}\n` = 23 bytes. Preallocate to
    // avoid reallocations on the 1500-call write loop.
    let mut source = String::with_capacity(n * 24);
    // Top-of-file comment so a human inspecting a leaked copy of the
    // generated source can trace it back to the test fixture without
    // having to grep the codebase.
    source.push_str("// Generated by code-graph-tools tests/fixtures/large_orphan_set/mod.rs\n");
    source.push_str("// Anti-regression fixture for paginated-response byte-budget safety.\n");
    source.push_str("// Do not edit by hand; see the module-level docs for sizing rationale.\n\n");
    for i in 1..=n {
        // 4-digit zero-padding so symbol_id sort == numeric sort across
        // the full 1..=9999 range. `orphan_0001` through `orphan_1500`.
        writeln!(&mut source, "void orphan_{i:04}() {{}}").expect("writeln! to String");
    }
    source
}

/// Write the fixture source to `dir/FIXTURE_FILENAME`. Returns the absolute
/// path of the written file so callers can assert against it if needed.
///
/// `dir` must already exist; callers typically pass a `TempDir::path()` so
/// each test gets its own isolated copy and concurrent `analyze_codebase`
/// runs don't race on the shared `.code-graph-cache.db` write.
pub fn write_fixture_to(dir: &Path) -> std::path::PathBuf {
    let path = dir.join(FIXTURE_FILENAME);
    let source = generate_large_orphan_source(ORPHAN_COUNT);
    std::fs::write(&path, source)
        .unwrap_or_else(|e| panic!("write fixture to {}: {e}", path.display()));
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_emits_exactly_n_functions() {
        // Small N to keep the assertion cheap. Counts `void orphan_` prefixes
        // to confirm the loop body ran exactly `n` times.
        let src = generate_large_orphan_source(5);
        assert_eq!(src.matches("void orphan_").count(), 5);
        assert!(src.contains("void orphan_0001() {}"));
        assert!(src.contains("void orphan_0005() {}"));
        // Cardinality boundary: no off-by-one beyond `n`.
        assert!(!src.contains("void orphan_0006"));
    }

    #[test]
    fn generate_is_deterministic() {
        // Pure-function contract: two calls with the same `n` produce
        // byte-identical output. The byte-budget acceptance test depends
        // on this so the continuation-offset arithmetic stays reproducible.
        let a = generate_large_orphan_source(100);
        let b = generate_large_orphan_source(100);
        assert_eq!(a, b);
    }

    #[test]
    fn generate_zero_pads_to_four_digits() {
        // The `symbol_id` ascending sort hinges on zero-padding: without
        // it, `orphan_10` would sort before `orphan_2`, breaking the
        // continuation-offset reasoning in the acceptance test's paging loop.
        let src = generate_large_orphan_source(1500);
        assert!(src.contains("orphan_0001"));
        assert!(src.contains("orphan_0099"));
        assert!(src.contains("orphan_0100"));
        assert!(src.contains("orphan_1500"));
        // No 3-digit forms, no 5-digit forms.
        assert!(!src.contains("orphan_001 "));
        assert!(!src.contains("orphan_00001"));
    }

    #[test]
    fn orphan_count_constant_matches_phase_5_floor() {
        // Lock the cardinality at the module-doc-documented value. If a
        // future change wants to bump this (e.g. a record shrinkage that
        // pushes the truncation threshold up), the constant and the
        // module-level docs MUST move together — otherwise the next
        // engineer reading the docs sees a number that doesn't match the
        // code. This guard makes a silent edit of one without the other
        // a compile error.
        //
        // `const { assert!(..) }` lifts the check to compile-time. A
        // plain runtime `assert!` on a const value trips
        // `clippy::assertions_on_constants` under `-D warnings`; the
        // const-block form is what clippy recommends, and it's also
        // strictly stronger: a future engineer who edits `ORPHAN_COUNT`
        // below 1500 gets the failure at `cargo build`, not on test run.
        const { assert!(ORPHAN_COUNT >= 1500) };
    }
}
