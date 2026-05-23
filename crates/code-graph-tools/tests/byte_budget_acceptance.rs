//! Acceptance regression test for paginated-response byte-budget safety.
//!
//! # Failure mode this test pins
//!
//! The originally-reported failure mode that motivated the entire plan: a
//! single `get_orphans(limit=1000)` call against the `rust-main` checkout
//! of this very repo (71 files / 1759 symbols) returned 1031 records
//! serialized as a single-line JSON payload **297,266 characters** long —
//! roughly **74,000 tokens**. The Claude Code MCP harness rejected the
//! response outright because it exceeded the per-tool-response token
//! ceiling. The user could not page past the error: there was no
//! truncation, no continuation hint, no recoverable state.
//!
//! The byte budget (`[response].max_bytes`, default 102,400), the
//! `{truncated, next_offset}` pagination envelope, the `count_only`
//! short-circuit, and the `SymbolResult.file` drop together prevent
//! that payload. This test file's contract is that **no future refactor
//! can re-introduce the 74K-token payload**. If the assertions below
//! ever start failing, those guard rails have been bypassed in some way
//! and the original bug is back. The fix is to restore the byte-budget
//! enforcement, not to loosen the assertions.
//!
//! # Mechanism
//!
//! Each test:
//! 1. Writes the [`large_orphan_set`] fixture (1500 orphan-eligible C++
//!    functions) into a fresh `TempDir` so concurrent test runs cannot
//!    race on `.code-graph-cache.db`.
//! 2. Runs `analyze_codebase` to populate the in-memory graph.
//! 3. Calls the target tool with `limit=1000` and `max_bytes =
//!    DEFAULT_RESPONSE_MAX_BYTES` (the production default, 102_400).
//! 4. Asserts the first page is `truncated=true` AND the serialized body
//!    is under `max_bytes + ENVELOPE_OVERHEAD_BYTES` — the +overhead is
//!    the generous accounting slack documented next to the constant in
//!    `code-graph-tools/src/handlers/mod.rs`.
//! 5. Loops on `next_offset` until `truncated=false`, asserting every
//!    intermediate page is under budget.
//! 6. Asserts the cross-page sum of `results.len()` equals the initial
//!    `total` — no overlap, no gap, every fixture record accounted for
//!    in exactly one page.
//!
//! # Why `DEFAULT_RESPONSE_MAX_BYTES` and not a magic number
//!
//! Using the constant means a future bump of the production default (or
//! a typo'd literal) flows through to the test automatically. The
//! contract being tested is "responses fit the *configured* budget", not
//! "responses fit 102,400 bytes" — same shape, the constant just keeps
//! the two halves in sync.
//!
//! # count_only smoke tests in this same file
//!
//! Three tiny tests asserting `serde_json::to_string(&response).len() <
//! 1024` for `get_orphans` / `search_symbols` / `get_file_symbols` with
//! `count_only=true`. They sit beneath the two acceptance tests and rely
//! on the same fixture-building helper.

mod common;
use common::first_text;

use std::sync::Arc;

use code_graph_core::DEFAULT_RESPONSE_MAX_BYTES;
use code_graph_lang::LanguageRegistry;
use code_graph_lang_cpp::CppParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::handlers::structure::get_orphans;
use code_graph_tools::handlers::symbols::{get_file_symbols, search_symbols, SearchSymbolsInput};
use code_graph_tools::handlers::ENVELOPE_OVERHEAD_BYTES;
use code_graph_tools::server::ServerInner;
use code_graph_tools::CodeGraphServer;
use rmcp::model::CallToolResult;
use tempfile::TempDir;

// The fixture module lives under `tests/fixtures/`. Cargo only treats
// top-level `tests/*.rs` as test crates and `tests/common/mod.rs` as the
// conventional shared-helper exception. Anything else under `tests/`
// requires an explicit `#[path = "..."]` load.
#[path = "fixtures/large_orphan_set/mod.rs"]
mod large_orphan_set;

/// Generous overhead margin layered on top of `max_bytes` for the
/// envelope-fits-under-budget assertion. The handler already reserves
/// `ENVELOPE_OVERHEAD_BYTES` (512) from the per-records budget; in
/// practice envelopes serialize to under that, so checking against
/// `max_bytes + ENVELOPE_OVERHEAD_BYTES` is a conservative ceiling that
/// catches "budget bypassed entirely" regressions without flaking on
/// envelope-size jitter from optional fields (`next_offset` flips
/// between `null` and a small integer, etc.).
const ENVELOPE_HEADROOM_BYTES: usize = ENVELOPE_OVERHEAD_BYTES;

/// Per-test fixture: holds the `TempDir` for the test's lifetime and the
/// indexed `ServerInner`. Hold the `TempDir` until the test ends so the
/// OS doesn't reclaim the indexed file while we read symbols out of the
/// in-memory graph.
struct IndexedFixture {
    _dir: TempDir,
    inner: Arc<ServerInner>,
}

/// Build the fixture: a fresh `TempDir`, the 1500-orphan C++ file
/// written into it, the C++ parser registered, `analyze_codebase` run,
/// and the resulting `ServerInner` returned. Each test gets its own
/// `TempDir` so concurrent runs cannot race on `.code-graph-cache.db`.
async fn build_indexed_fixture() -> IndexedFixture {
    let dir = TempDir::new().expect("TempDir for large_orphan_set fixture");
    large_orphan_set::write_fixture_to(dir.path());
    let indexed_root =
        std::fs::canonicalize(dir.path()).expect("canonicalize fixture dir for analyze");

    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().expect("CppParser::new")))
        .expect("register CppParser");
    let server = CodeGraphServer::new(registry);

    let r = analyze_codebase(
        server.inner.clone(),
        indexed_root.to_string_lossy().into_owned(),
        true,
        None,
        None,
    )
    .await;
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "analyze_codebase failed: {r:?}",
    );
    IndexedFixture {
        _dir: dir,
        inner: server.inner.clone(),
    }
}

/// Parse `(results.len(), total, truncated, next_offset)` out of a tool
/// response. Tightly coupled to the `Page<T>` envelope shape; mirrors the
/// internal `page_parts` / `page_extras` helpers (which live behind
/// `pub(super)` and aren't reachable from integration tests). One parse
/// step per call so the test reads top-to-bottom without a separate
/// "destructure" line for each field.
fn page_summary(r: &CallToolResult) -> (usize, u32, bool, Option<u32>) {
    let parsed: serde_json::Value =
        serde_json::from_str(&first_text(r)).expect("response body must be valid JSON");
    let results_len = parsed["results"]
        .as_array()
        .map(|a| a.len())
        .expect("Page<T> envelope must have `results` array");
    let total = parsed["total"]
        .as_u64()
        .expect("Page<T> envelope must have `total`") as u32;
    let truncated = parsed["truncated"]
        .as_bool()
        .expect("Page<T> envelope must have `truncated`");
    let next_offset = match &parsed["next_offset"] {
        serde_json::Value::Null => None,
        v => Some(v.as_u64().expect("`next_offset` must be null or integer") as u32),
    };
    (results_len, total, truncated, next_offset)
}

// ---------------------------------------------------------------------------
// Test 1: get_orphans pagination loop stays under budget
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_orphans_under_budget_at_limit_1000() {
    let fx = build_indexed_fixture().await;

    let r = get_orphans(
        &fx.inner.graph,
        None,
        None,
        Some(1000),
        Some(0),
        None,
        false,
        None,
        DEFAULT_RESPONSE_MAX_BYTES,
    );
    let body = first_text(&r);
    let body_len = body.len();
    let (first_page_len, total, truncated, next_offset) = page_summary(&r);

    // Fixture-shape sanity. `large_orphan_set` is sized so the parser
    // sees >= ORPHAN_COUNT (1500) orphan-eligible callables. If `total`
    // ever drops below the fixture floor, either the parser stopped
    // recognizing free functions or the orphan detector regressed —
    // either way the rest of this test would assert on a degenerate
    // input. Catch that here, with a message that names the likely root
    // cause.
    assert!(
        total >= large_orphan_set::ORPHAN_COUNT as u32,
        "fixture should yield >= {} orphans, got total={total}",
        large_orphan_set::ORPHAN_COUNT
    );

    // The headline assertion. If this fails, the byte budget is no
    // longer biting at limit=1000 against a fixture *engineered* to
    // trigger it — i.e. the originally-reported 74K-token payload
    // failure mode can recur.
    assert!(
        truncated,
        "get_orphans(limit=1000) on the 1500-orphan fixture MUST be \
         truncated. total={total} first_page_len={first_page_len} \
         body_len={body_len}. If you see this fail, the byte budget \
         either isn't running or the fixture isn't large enough."
    );

    // Byte-budget assertion. Mirrors the docstring promise of
    // `byte_budget_take`: the serialized response fits within
    // `max_bytes + ENVELOPE_OVERHEAD_BYTES` of headroom.
    assert!(
        body_len <= DEFAULT_RESPONSE_MAX_BYTES + ENVELOPE_HEADROOM_BYTES,
        "get_orphans first-page body {} bytes exceeds budget {} + headroom {}",
        body_len,
        DEFAULT_RESPONSE_MAX_BYTES,
        ENVELOPE_HEADROOM_BYTES,
    );

    assert!(
        next_offset.is_some(),
        "truncated=true MUST come with a Some(next_offset); got None"
    );

    // Drive the continuation loop. Sum kept records across pages,
    // assert every intermediate page is under budget, and at the end
    // assert the sum equals `total` (no overlap, no gap).
    let mut total_kept: usize = first_page_len;
    let mut cur_truncated = truncated;
    let mut cur_next_offset = next_offset;
    // Bound on the loop so a stuck `next_offset` (degenerate handler
    // would return the same offset forever) surfaces as a test failure
    // rather than an infinite loop. Generous: the worst case for a
    // 1500-record fixture with ~970 records/page is 2 pages.
    let max_iters = 50usize;
    let mut iters = 0usize;

    while cur_truncated {
        iters += 1;
        assert!(
            iters <= max_iters,
            "get_orphans paging loop exceeded {max_iters} iterations; \
             next_offset appears not to advance. total_kept so far={total_kept}"
        );

        let offset =
            cur_next_offset.expect("truncated=true must always come with Some(next_offset)");
        let r = get_orphans(
            &fx.inner.graph,
            None,
            None,
            Some(1000),
            Some(offset),
            None,
            false,
            None,
            DEFAULT_RESPONSE_MAX_BYTES,
        );
        let body = first_text(&r);
        let body_len = body.len();
        let (page_len, page_total, page_truncated, page_next_offset) = page_summary(&r);

        // Every continuation page, including the final one, stays under
        // the same budget ceiling as the first page — pinning the
        // contract across every iteration of the paging loop.
        assert!(
            body_len <= DEFAULT_RESPONSE_MAX_BYTES + ENVELOPE_HEADROOM_BYTES,
            "get_orphans page (offset={offset}) body {body_len} bytes \
             exceeds budget {} + headroom {}",
            DEFAULT_RESPONSE_MAX_BYTES,
            ENVELOPE_HEADROOM_BYTES,
        );

        // `total` is invariant across pages (pre-pagination match count).
        // If a page disagrees the handler is mis-reporting; catch it
        // here so the no-overlap/no-gap assertion below isn't masked.
        assert_eq!(
            page_total, total,
            "total must be stable across pages: page reported {page_total}, \
             initial reported {total}"
        );

        total_kept += page_len;

        cur_truncated = page_truncated;
        cur_next_offset = page_next_offset;
    }

    // Loop exit guarantees the last response had `truncated=false`.
    // Final page must report `next_offset=None` (no further page).
    assert!(
        cur_next_offset.is_none(),
        "final page (truncated=false) must have next_offset=None, got {cur_next_offset:?}"
    );

    // The no-overlap, no-gap contract: every fixture record appears in
    // exactly one page.
    assert_eq!(
        total_kept, total as usize,
        "sum of results.len() across all pages ({total_kept}) must equal \
         the pre-pagination total ({total}); a mismatch means the paging \
         loop dropped or double-counted records"
    );
}

// ---------------------------------------------------------------------------
// Test 2: search_symbols pagination loop stays under budget
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_symbols_under_budget_at_limit_1000() {
    let fx = build_indexed_fixture().await;

    // Broad-match query: every fixture function's name contains the
    // substring "orphan", so this hits all ORPHAN_COUNT (1500) records.
    // Same byte-budget trigger condition as test 1.
    let query = "orphan";

    let r = search_symbols(
        &fx.inner.graph,
        SearchSymbolsInput {
            query: Some(query),
            limit: Some(1000),
            offset: Some(0),
            brief: true,
            ..SearchSymbolsInput::default()
        },
        DEFAULT_RESPONSE_MAX_BYTES,
    );
    let body = first_text(&r);
    let body_len = body.len();
    let (first_page_len, total, truncated, next_offset) = page_summary(&r);

    // Fixture-shape sanity (mirrors test 1). The broad-match query must
    // hit every fixture function or the rest of this test runs against
    // a degenerate input.
    assert!(
        total >= large_orphan_set::ORPHAN_COUNT as u32,
        "search_symbols(query={query:?}) should yield >= {} matches, got total={total}",
        large_orphan_set::ORPHAN_COUNT
    );

    // Headline assertion: pinning the 74K-token failure mode for search_symbols.
    assert!(
        truncated,
        "search_symbols(query={query:?}, limit=1000) on the 1500-orphan \
         fixture MUST be truncated. total={total} first_page_len={first_page_len} \
         body_len={body_len}"
    );

    assert!(
        body_len <= DEFAULT_RESPONSE_MAX_BYTES + ENVELOPE_HEADROOM_BYTES,
        "search_symbols first-page body {body_len} bytes exceeds budget {} + headroom {}",
        DEFAULT_RESPONSE_MAX_BYTES,
        ENVELOPE_HEADROOM_BYTES,
    );

    assert!(
        next_offset.is_some(),
        "truncated=true MUST come with a Some(next_offset); got None"
    );

    let mut total_kept: usize = first_page_len;
    let mut cur_truncated = truncated;
    let mut cur_next_offset = next_offset;
    let max_iters = 50usize;
    let mut iters = 0usize;

    while cur_truncated {
        iters += 1;
        assert!(
            iters <= max_iters,
            "search_symbols paging loop exceeded {max_iters} iterations; \
             next_offset appears not to advance. total_kept so far={total_kept}"
        );

        let offset =
            cur_next_offset.expect("truncated=true must always come with Some(next_offset)");
        let r = search_symbols(
            &fx.inner.graph,
            SearchSymbolsInput {
                query: Some(query),
                limit: Some(1000),
                offset: Some(offset),
                brief: true,
                ..SearchSymbolsInput::default()
            },
            DEFAULT_RESPONSE_MAX_BYTES,
        );
        let body = first_text(&r);
        let body_len = body.len();
        let (page_len, page_total, page_truncated, page_next_offset) = page_summary(&r);

        assert!(
            body_len <= DEFAULT_RESPONSE_MAX_BYTES + ENVELOPE_HEADROOM_BYTES,
            "search_symbols page (offset={offset}) body {body_len} bytes \
             exceeds budget {} + headroom {}",
            DEFAULT_RESPONSE_MAX_BYTES,
            ENVELOPE_HEADROOM_BYTES,
        );

        assert_eq!(
            page_total, total,
            "total must be stable across pages: page reported {page_total}, \
             initial reported {total}"
        );

        total_kept += page_len;

        cur_truncated = page_truncated;
        cur_next_offset = page_next_offset;
    }

    assert!(
        cur_next_offset.is_none(),
        "final page (truncated=false) must have next_offset=None, got {cur_next_offset:?}"
    );

    assert_eq!(
        total_kept, total as usize,
        "sum of results.len() across all pages ({total_kept}) must equal \
         the pre-pagination total ({total}); a mismatch means the paging \
         loop dropped or double-counted records"
    );
}

// ---------------------------------------------------------------------------
// count_only smoke tests
// ---------------------------------------------------------------------------
//
// Each test calls one of `get_orphans` / `search_symbols` /
// `get_file_symbols` with `count_only=true` against the same
// 1500-orphan fixture and pins the documented count-only contract:
//
//   - serialized body < 1024 bytes
//   - total > 0 (sanity: fixture has matches)
//   - results.is_empty()
//   - truncated == false
//   - next_offset.is_none()
//
// Trivial line count, but they pin the 1KB contract against an
// accidental future bloat of the count-only envelope (e.g. a new
// always-present metadata field). The handler unit tests in
// `structure.rs` / `symbols.rs` cover the per-tool sentinel-shape
// invariants on tiny in-memory graphs; these integration smoke tests
// confirm the same contract holds end-to-end on the real-scale fixture
// that drove the originally-reported failure mode.

#[tokio::test]
async fn count_only_under_1kb_orphans() {
    let fx = build_indexed_fixture().await;

    let r = get_orphans(
        &fx.inner.graph,
        None,
        None,
        None,
        None,
        None,
        true,
        None,
        DEFAULT_RESPONSE_MAX_BYTES,
    );
    let body = first_text(&r);
    let body_len = body.len();
    let (results_len, total, truncated, next_offset) = page_summary(&r);

    // The 1KB contract. The count-only sentinel envelope is
    // engineered to fit well under this ceiling regardless of fixture
    // size — `total` is the only variable-width field and it's a single
    // u32. If this fails, someone has added a per-call metadata field
    // to the envelope.
    assert!(
        body_len < 1024,
        "get_orphans(count_only=true) body {body_len} bytes must be < 1024",
    );
    assert!(
        total > 0,
        "fixture must yield > 0 orphans (sanity); got total={total}",
    );
    assert_eq!(
        results_len, 0,
        "count_only must emit empty results; got results.len()={results_len}",
    );
    assert!(!truncated, "count_only must never set truncated=true");
    assert!(
        next_offset.is_none(),
        "count_only must emit next_offset=null; got {next_offset:?}",
    );
}

#[tokio::test]
async fn count_only_under_1kb_search_symbols() {
    let fx = build_indexed_fixture().await;

    let r = search_symbols(
        &fx.inner.graph,
        SearchSymbolsInput {
            query: Some("orphan"),
            count_only: true,
            ..SearchSymbolsInput::default()
        },
        DEFAULT_RESPONSE_MAX_BYTES,
    );
    let body = first_text(&r);
    let body_len = body.len();
    let (results_len, total, truncated, next_offset) = page_summary(&r);

    assert!(
        body_len < 1024,
        "search_symbols(count_only=true) body {body_len} bytes must be < 1024",
    );
    assert!(
        total > 0,
        "fixture must yield > 0 matches for query=\"orphan\" (sanity); got total={total}",
    );
    assert_eq!(
        results_len, 0,
        "count_only must emit empty results; got results.len()={results_len}",
    );
    assert!(!truncated, "count_only must never set truncated=true");
    assert!(
        next_offset.is_none(),
        "count_only must emit next_offset=null; got {next_offset:?}",
    );
}

#[tokio::test]
async fn count_only_under_1kb_file_symbols() {
    let fx = build_indexed_fixture().await;

    // The fixture writes `large_orphans.cpp` into the canonicalized
    // TempDir. `get_file_symbols` keys off the absolute path that was
    // recorded at index time, so reconstruct it the same way
    // `build_indexed_fixture` did.
    let file_path = std::fs::canonicalize(fx._dir.path())
        .expect("canonicalize fixture dir")
        .join(large_orphan_set::FIXTURE_FILENAME)
        .to_string_lossy()
        .into_owned();

    let r = get_file_symbols(
        &fx.inner.graph,
        &file_path,
        false,
        true,
        None,
        None,
        true,
        DEFAULT_RESPONSE_MAX_BYTES,
    );
    let body = first_text(&r);
    let body_len = body.len();
    let (results_len, total, truncated, next_offset) = page_summary(&r);

    assert!(
        body_len < 1024,
        "get_file_symbols(count_only=true) body {body_len} bytes must be < 1024",
    );
    assert!(
        total > 0,
        "fixture must yield > 0 symbols for large_orphans.cpp (sanity); got total={total}",
    );
    assert_eq!(
        results_len, 0,
        "count_only must emit empty results; got results.len()={results_len}",
    );
    assert!(!truncated, "count_only must never set truncated=true");
    assert!(
        next_offset.is_none(),
        "count_only must emit next_offset=null; got {next_offset:?}",
    );
}
