//! Cross-cutting integration regression suite for `detect_cycles`'s two
//! independent truncation axes: the `Page<Cycle>` envelope's
//! count-based pagination (honest `truncated` / `next_offset` mid-page,
//! with a load-bearing no-overlap/no-gap paging-resume invariant) and the
//! per-cycle `max_cycle_size` file-list cap (default 50, clamp 500).
//!
//! These are handler/indexer *boundary* tests: each drives the real
//! `analyze_codebase` pipeline against a `TempDir` of mutually-`#include`ing
//! C++ headers and asserts against the serialized client-visible JSON
//! (`first_text` -> `serde_json::Value`), since MCP clients consume
//! `detect_cycles` as JSON, never as Rust types. The focused per-handler
//! unit tests in `handlers/structure.rs` already pin these behaviors
//! against synthetic in-memory graphs built directly via `merge_file_graph`;
//! this suite proves the same guarantees survive the full
//! discover -> parse -> resolve -> merge -> handler pipeline.
//!
//! Why the fixtures must be real source files: the include graph
//! contains ONLY edges between *indexed source files* —
//! `resolve_all_edges` drops an `Includes` edge unless it resolves to a
//! discovered file whose extension a language plugin claims.
//! `detect_cycles` runs Tarjan SCC over `Graph::includes`, so a cycle
//! only forms if every edge in it is a source-to-source `#include`
//! between files that are themselves in the indexed set. Each fixture
//! below therefore writes real `.h` files that `#include` each other by
//! (unique) basename so the basename resolver keeps every cycle edge; a
//! fixture whose cycle edges would be dropped by the include-resolution
//! filter would yield ZERO detected cycles, so every test early-asserts
//! the expected `total` to prove the cycles actually formed (a guard
//! against a vacuous pass).

mod common;
use common::ok_json;

use code_graph_lang::LanguageRegistry;
use code_graph_lang_cpp::CppParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::handlers::structure::detect_cycles;
use code_graph_tools::CodeGraphServer;
use tempfile::TempDir;

/// Fresh server with only the C++ parser registered. Mirrors the helper
/// shape used by `tests/coupling_dependencies.rs` / `integration.rs`.
fn fresh_server() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().expect("CppParser::new")))
        .expect("register CppParser");
    CodeGraphServer::new(registry)
}

/// Run `analyze_codebase` against `dir` with `force=true` (deterministic:
/// never takes the on-disk cache fast path). Panics with the response
/// body on failure so a regression names the offending stage.
async fn analyze(server: &CodeGraphServer, dir: &std::path::Path) {
    let r = analyze_codebase(
        server.inner.clone(),
        dir.to_string_lossy().into_owned(),
        true,
        None,
        None,
    )
    .await;
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "analyze_codebase must succeed for the fixture: {r:?}",
    );
}

/// `(results array, total, offset, limit)` out of a `Page<Cycle>` JSON
/// value — the integration-layer equivalent of `handlers::test_helpers::
/// page_parts` (which is `pub(super)` and unreachable from `tests/`).
fn page_parts(body: &serde_json::Value) -> (Vec<serde_json::Value>, u64, u64, u64) {
    (
        body["results"]
            .as_array()
            .expect("Page<Cycle> must carry a results array")
            .clone(),
        body["total"].as_u64().expect("Page.total"),
        body["offset"].as_u64().expect("Page.offset"),
        body["limit"].as_u64().expect("Page.limit"),
    )
}

/// `(truncated, next_offset)` envelope extras — the integration-layer
/// equivalent of `handlers::test_helpers::page_extras`. `truncated` and
/// `next_offset` are always present in the wire shape (no
/// `skip_serializing_if`); `next_offset` is JSON `null` on the natural
/// tail, which `as_u64()` maps to `None`.
fn page_extras(body: &serde_json::Value) -> (bool, Option<u64>) {
    (
        body["truncated"].as_bool().expect("Page.truncated"),
        body["next_offset"].as_u64(),
    )
}

/// The first (canonical-sorted) file path of a serialized `Cycle`. The
/// handler sorts each cycle's `files` internally and the outer cycle list
/// by each cycle's first path, so this is a stable per-cycle identity used
/// to assert the paging-resume partition.
fn cycle_first_file(c: &serde_json::Value) -> String {
    c["files"].as_array().expect("Cycle.files array")[0]
        .as_str()
        .expect("Cycle.files[0] string")
        .to_string()
}

// ---------------------------------------------------------------------------
// Synthetic-cycle fixture generators (rebuilt for the integration layer)
// ---------------------------------------------------------------------------
//
// REUSED vs REBUILT: the unit-test helpers `graph_with_n_cycles` /
// `graph_with_one_cycle_of_n_files` (handlers/structure.rs, `#[cfg(test)]`)
// are NOT visible to `tests/`, and they construct a `Graph` directly via
// `merge_file_graph` — which bypasses the `resolve_all_edges` include
// filter entirely. An integration suite must drive the real pipeline, so
// these generators are REBUILT to write real `.h` files whose mutual
// `#include`s survive the source-to-source include filter. They mirror the
// unit helpers' topology exactly: `write_n_disjoint_2cycles` is the on-disk
// analogue of `graph_with_n_cycles` (n disjoint A<->B pairs);
// `write_one_ring_of_n` is the analogue of `graph_with_one_cycle_of_n_files`
// (a single n-file ring f000 -> f001 -> ... -> f(n-1) -> f000).

/// Write `n` disjoint 2-node include cycles into `root`. For each `i`,
/// `cyc{i:04}a.h` `#include`s `cyc{i:04}b.h` and vice-versa, so the
/// basename resolver keeps both edges (unique basenames, `.h` extension =
/// source) and Tarjan collapses each pair into its own size-2 SCC. The
/// trailing `void` declaration guarantees the C++ parser emits a parsed
/// `FileGraph` for the header even though `detect_cycles` only needs the
/// include topology.
fn write_n_disjoint_2cycles(root: &std::path::Path, n: usize) {
    for i in 0..n {
        let a = format!("cyc{i:04}a.h");
        let b = format!("cyc{i:04}b.h");
        std::fs::write(
            root.join(&a),
            format!("#include \"{b}\"\nvoid cyc{i:04}a();\n").as_bytes(),
        )
        .unwrap();
        std::fs::write(
            root.join(&b),
            format!("#include \"{a}\"\nvoid cyc{i:04}b();\n").as_bytes(),
        )
        .unwrap();
    }
}

/// Write a single `n`-file include ring into `root`:
/// `ring{0:04}.h` -> `ring{1:04}.h` -> ... -> `ring{n-1:04}.h` ->
/// `ring{0:04}.h`. Every header `#include`s exactly the next one by unique
/// basename so all `n` edges survive the include-resolution filter and
/// Tarjan collapses the whole ring into ONE SCC of size `n`.
fn write_one_ring_of_n(root: &std::path::Path, n: usize) {
    for i in 0..n {
        let here = format!("ring{i:04}.h");
        let next = format!("ring{:04}.h", (i + 1) % n);
        std::fs::write(
            root.join(&here),
            format!("#include \"{next}\"\nvoid ring{i:04}();\n").as_bytes(),
        )
        .unwrap();
    }
}

// ---------------------------------------------------------------------------
// (a) envelope honesty mid-page + load-bearing paging-resume partition
// ---------------------------------------------------------------------------

/// 100 disjoint 2-node cycles. A mid-stream page request
/// `(limit=10, offset=30)` must report an honest envelope
/// (`truncated=true`, `next_offset=Some(40)`); resuming at `offset=40`
/// must continue contiguously; and the union of the two pages' cycles must
/// be exactly disjoint and gap-free — the load-bearing paging-resume
/// invariant. A final walk to the natural tail must flip
/// `truncated=false` / `next_offset=None`.
#[tokio::test]
async fn envelope_honesty_mid_page() {
    let dir = TempDir::new().expect("TempDir");
    let root = dir.path();
    write_n_disjoint_2cycles(root, 100);

    let canonical = std::fs::canonicalize(root).unwrap();
    let server = fresh_server();
    analyze(&server, &canonical).await;

    // Mid-stream page: offset 30, limit 10.
    let r1 = detect_cycles(&server.inner.graph, None, Some(10), Some(30), None);
    let b1 = ok_json(&r1);
    let (arr1, total1, offset1, limit1) = page_parts(&b1);

    // Early total guard: if the mutual #includes had been dropped by the
    // source-to-source include filter, total would be 0 and every
    // assertion below would pass vacuously. 100 disjoint 2-cycles MUST
    // yield exactly 100 SCCs.
    assert_eq!(
        total1, 100,
        "fixture must form exactly 100 cycles through the real index \
         pipeline (total=0 would mean the mutual #includes were dropped \
         as non-source); body: {b1}",
    );
    assert_eq!(offset1, 30, "envelope echoes the resolved offset");
    assert_eq!(limit1, 10, "envelope echoes the resolved limit");
    assert_eq!(
        arr1.len(),
        10,
        "limit caps the mid-stream page at 10 cycles"
    );
    let (t1, n1) = page_extras(&b1);
    assert!(
        t1,
        "offset(30)+emitted(10)=40 < total(100): envelope MUST be \
         truncated mid-page; body: {b1}",
    );
    assert_eq!(
        n1,
        Some(40),
        "next_offset must point one past the last emitted cycle (30+10); \
         body: {b1}",
    );

    // Resume exactly at next_offset.
    let r2 = detect_cycles(&server.inner.graph, None, Some(10), Some(40), None);
    let b2 = ok_json(&r2);
    let (arr2, total2, offset2, _) = page_parts(&b2);
    assert_eq!(total2, 100, "total is invariant across pages; body: {b2}");
    assert_eq!(offset2, 40, "resumed page echoes offset=next_offset");
    assert_eq!(arr2.len(), 10, "second page also full at limit 10");
    let (t2, n2) = page_extras(&b2);
    assert!(
        t2,
        "offset(40)+emitted(10)=50 < total(100): still truncated"
    );
    assert_eq!(n2, Some(50), "next_offset advances contiguously to 50");

    // THE load-bearing paging-resume assertion: page-1 and page-2 cycle
    // sets must be disjoint (no overlap) AND contiguous (no gap) — i.e.
    // concatenating their per-cycle identities equals the corresponding
    // 20-cycle window of the full sorted reference list, exactly. We take
    // the ground-truth ordering from a single unpaginated call and slice
    // [30..50), then compare against page1 ++ page2 by identity.
    let full = ok_json(&detect_cycles(
        &server.inner.graph,
        None,
        Some(1000),
        Some(0),
        None,
    ));
    let (full_arr, full_total, _, _) = page_parts(&full);
    assert_eq!(full_total, 100, "ground-truth call also sees 100 cycles");
    let reference_window: Vec<String> = full_arr
        .iter()
        .skip(30)
        .take(20)
        .map(cycle_first_file)
        .collect();
    let mut joined: Vec<String> = arr1.iter().map(cycle_first_file).collect();
    joined.extend(arr2.iter().map(cycle_first_file));
    assert_eq!(
        joined, reference_window,
        "paging-resume contract violated: page1(offset=30) ++ \
         page2(offset=40) must equal the full sorted cycle set's [30..50) \
         window EXACTLY — any duplicate is an overlap, any missing cycle is \
         a gap; joined={joined:?} reference={reference_window:?}",
    );

    // Walk to the natural tail: offset 95, only 5 cycles remain (< limit),
    // so 95+5 == 100 == total -> truncated=false / next_offset=None.
    let r_tail = detect_cycles(&server.inner.graph, None, Some(10), Some(95), None);
    let b_tail = ok_json(&r_tail);
    let (arr_tail, total_tail, _, _) = page_parts(&b_tail);
    assert_eq!(total_tail, 100);
    assert_eq!(arr_tail.len(), 5, "only the trailing 5 cycles remain");
    let (t_tail, n_tail) = page_extras(&b_tail);
    assert!(
        !t_tail,
        "natural tail (offset 95 + 5 == total 100) must NOT be truncated; \
         body: {b_tail}",
    );
    assert_eq!(
        n_tail, None,
        "tail page must carry next_offset=null (no resume); body: {b_tail}",
    );

    drop(dir);
}

// ---------------------------------------------------------------------------
// (b) per-cycle cap truncates a 200-file SCC at an explicit max_cycle_size
// ---------------------------------------------------------------------------

/// ONE 200-file include ring -> exactly one SCC of size 200. With an
/// explicit `max_cycle_size=50` the single returned cycle's `files` list
/// must be clipped to 50, with `Cycle.truncated=true` and
/// `Cycle.original_len=Some(200)`. The envelope (only one cycle exists) is
/// complete and untouched — the per-cycle cap is an axis orthogonal to
/// envelope pagination.
#[tokio::test]
async fn per_cycle_cap_truncates_large_scc() {
    let dir = TempDir::new().expect("TempDir");
    let root = dir.path();
    write_one_ring_of_n(root, 200);

    let canonical = std::fs::canonicalize(root).unwrap();
    let server = fresh_server();
    analyze(&server, &canonical).await;

    let r = detect_cycles(&server.inner.graph, None, None, None, Some(50));
    let body = ok_json(&r);
    let (arr, total, _, _) = page_parts(&body);

    // Early total guard: a 200-file ring must collapse into exactly ONE
    // SCC. total=0 would mean the ring's #include edges were dropped as
    // non-source by the include-resolution filter; total>1 would mean
    // the ring didn't actually close.
    assert_eq!(
        total, 1,
        "the 200-file ring must form exactly one SCC through the real \
         index pipeline (total != 1 means the ring's #includes did not \
         all survive / close); body: {body}",
    );
    assert_eq!(arr.len(), 1, "the single cycle is on the page");

    let files = arr[0]["files"]
        .as_array()
        .expect("Cycle.files must be an array");
    assert_eq!(
        files.len(),
        50,
        "explicit max_cycle_size=50 must clip the 200-file cycle to 50 \
         paths; got {} paths; body: {body}",
        files.len(),
    );
    assert_eq!(
        arr[0]["truncated"],
        serde_json::json!(true),
        "the clipped cycle must self-report Cycle.truncated=true; body: {body}",
    );
    assert_eq!(
        arr[0]["original_len"],
        serde_json::json!(200),
        "Cycle.original_len must carry the pre-truncation file count (200); \
         body: {body}",
    );

    // The per-cycle cap must not perturb the envelope: the lone cycle fits
    // the page, so the envelope is the natural, untruncated end.
    let (env_truncated, env_next) = page_extras(&body);
    assert!(
        !env_truncated,
        "the only cycle fits the page; the per-cycle cap must NOT make the \
         ENVELOPE truncated (the two axes are independent); body: {body}",
    );
    assert_eq!(
        env_next, None,
        "no further page after the only cycle; body: {body}",
    );

    drop(dir);
}

// ---------------------------------------------------------------------------
// (c) per-cycle cap default of 50 applies when max_cycle_size is omitted
// ---------------------------------------------------------------------------

/// Same 200-file-SCC fixture, NO `max_cycle_size` argument. The default of
/// 50 must apply, producing the identical clip / `truncated` / `original_len`
/// as the explicit-50 case above — this pins that the absent argument
/// resolves to the documented default, not to "unbounded".
#[tokio::test]
async fn per_cycle_cap_default_50() {
    let dir = TempDir::new().expect("TempDir");
    let root = dir.path();
    write_one_ring_of_n(root, 200);

    let canonical = std::fs::canonicalize(root).unwrap();
    let server = fresh_server();
    analyze(&server, &canonical).await;

    let r = detect_cycles(&server.inner.graph, None, None, None, None);
    let body = ok_json(&r);
    let (arr, total, _, _) = page_parts(&body);

    // Early total guard: same as the explicit-cap test — the 200-file
    // ring must collapse into exactly ONE SCC through the real pipeline.
    assert_eq!(
        total, 1,
        "the 200-file ring must form exactly one SCC through the real \
         index pipeline; body: {body}",
    );
    assert_eq!(arr.len(), 1, "the single cycle is on the page");

    let files = arr[0]["files"]
        .as_array()
        .expect("Cycle.files must be an array");
    assert_eq!(
        files.len(),
        50,
        "absent max_cycle_size must resolve to the default cap of 50 \
         (NOT unbounded); got {} paths; body: {body}",
        files.len(),
    );
    assert_eq!(
        arr[0]["truncated"],
        serde_json::json!(true),
        "the default-clipped cycle must self-report Cycle.truncated=true; \
         body: {body}",
    );
    assert_eq!(
        arr[0]["original_len"],
        serde_json::json!(200),
        "Cycle.original_len must carry the pre-truncation file count (200) \
         identically to the explicit-50 case; body: {body}",
    );

    drop(dir);
}
