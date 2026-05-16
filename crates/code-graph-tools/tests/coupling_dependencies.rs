//! Cross-cutting integration regression suite for the include-graph
//! reshape: `get_coupling` split/both response shapes, `get_dependencies`
//! line-preserving `Page<DependencyEntry>`, and the non-source-extension
//! (`.ini`) include filter at both the indexer and the watch reindex
//! path.
//!
//! These are handler/indexer/watch *boundary* tests: each one drives the
//! real `analyze_codebase` (or `try_reindex_file`) pipeline against a
//! `TempDir` fixture and asserts against the serialized client-visible
//! JSON (`first_text` → `serde_json::Value`), since MCP clients consume
//! these tools as JSON, never as Rust types. The per-handler unit tests
//! already pin the focused behaviors against synthetic graphs; this suite
//! proves the four sub-changes compose correctly end to end.
//!
//! Discovery interaction worth recording for future readers: the include
//! resolver (`default_basename_resolve`) only resolves a raw `#include`
//! target via `FileIndex.by_basename`, and that index is built from
//! *discovered* files — discovery emits only files whose extension a
//! plugin claims. A `.ini` is therefore never in the FileIndex on the
//! real `analyze_codebase` path, so a `#include "x.ini"` is dropped by
//! the pre-existing resolve-miss (resolver returns `None`) rather than by
//! the newer `language_for_path` extension filter. The client-visible
//! contract — "non-source include targets never appear in dependencies"
//! — holds identically either way, and that contract is what the
//! integration layer pins. The newer filter *mechanism* (resolver
//! returns `Some(<.ini>)` because the `.ini` was force-injected into the
//! FileIndex) is exercised specifically by the indexer unit test
//! `resolve_all_edges_drops_include_to_non_source_target` and, at the
//! watch boundary, by `watch_reindex_applies_ini_filter` below (which
//! seeds a `.ini` FileGraph into the graph so the watch-path FileIndex
//! genuinely carries it).

mod common;
use common::ok_json;

use code_graph_core::{FileGraph, Language};
use code_graph_lang::LanguageRegistry;
use code_graph_lang_cpp::CppParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::handlers::query::get_dependencies;
use code_graph_tools::handlers::structure::get_coupling;
use code_graph_tools::handlers::watch::{try_reindex_file, ReindexOutcome};
use code_graph_tools::handlers::NO_BYTE_BUDGET;
use code_graph_tools::CodeGraphServer;
use tempfile::TempDir;

/// Fresh server with only the C++ parser registered. Mirrors the helper
/// shape used by `integration.rs` / `watch_dangling_edges.rs`.
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


/// Extract `(file, count)` rows out of a `Page<CouplingEntry>` JSON value
/// in serialized order (the handler's desc-count / asc-file sort must be
/// observable here, so this preserves array order — no re-sorting).
fn coupling_pairs(page: &serde_json::Value) -> Vec<(String, u64)> {
    page["results"]
        .as_array()
        .expect("Page<CouplingEntry> must carry a results array")
        .iter()
        .map(|row| {
            (
                row["file"]
                    .as_str()
                    .expect("CouplingEntry.file")
                    .to_string(),
                row["count"].as_u64().expect("CouplingEntry.count"),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// (a) get_coupling(direction=both) split shape + sort order
// ---------------------------------------------------------------------------

/// `get_coupling(direction=both)` returns the `CouplingBoth`
/// `{incoming, outgoing}` envelope with two non-empty `Page<CouplingEntry>`
/// sides, each sorted desc-by-count then asc-by-file.
///
/// Fixture: `hub.h` is `#include`d by three translation units (`in1.cpp`
/// includes it twice → count 2; `in2.cpp`, `in3.cpp` once each → count 1)
/// = 3 incoming files with a non-uniform count so the count-desc primary
/// key and the file-asc tiebreak are both observable. `hub.h` itself
/// `#include`s `dep_a.h` and `dep_b.h` = 2 outgoing.
#[tokio::test]
async fn get_coupling_both_split_shape() {
    let dir = TempDir::new().expect("TempDir");
    let root = dir.path();
    std::fs::write(
        root.join("hub.h"),
        b"#include \"dep_a.h\"\n#include \"dep_b.h\"\nvoid hub();\n",
    )
    .unwrap();
    std::fs::write(root.join("dep_a.h"), b"void dep_a();\n").unwrap();
    std::fs::write(root.join("dep_b.h"), b"void dep_b();\n").unwrap();
    // in1.cpp includes hub.h TWICE → two Includes edges → incoming count 2.
    std::fs::write(
        root.join("in1.cpp"),
        b"#include \"hub.h\"\n#include \"hub.h\"\nvoid in1() {}\n",
    )
    .unwrap();
    std::fs::write(root.join("in2.cpp"), b"#include \"hub.h\"\nvoid in2() {}\n").unwrap();
    std::fs::write(root.join("in3.cpp"), b"#include \"hub.h\"\nvoid in3() {}\n").unwrap();

    let canonical = std::fs::canonicalize(root).unwrap();
    let server = fresh_server();
    analyze(&server, &canonical).await;

    let hub_h = canonical.join("hub.h").to_string_lossy().into_owned();
    let r = get_coupling(
        &server.inner.graph,
        &hub_h,
        Some("both"),
        None,
        None,
        NO_BYTE_BUDGET,
    );
    let body = ok_json(&r);

    // Split shape: a `CouplingBoth` carries `incoming` and `outgoing`
    // objects, NOT a flat `results` array (that would be the directional
    // `Page<CouplingEntry>` shape — a regression that collapsed the two
    // shapes).
    assert!(
        body.get("incoming").is_some() && body.get("outgoing").is_some(),
        "direction=both must return the CouplingBoth {{incoming, outgoing}} \
         envelope, got: {body}",
    );
    assert!(
        body.get("results").is_none(),
        "CouplingBoth must NOT carry a top-level `results` array (that is \
         the directional shape); got: {body}",
    );

    let incoming = coupling_pairs(&body["incoming"]);
    let outgoing = coupling_pairs(&body["outgoing"]);

    // Incoming: in1.cpp(2) > in2.cpp(1) = in3.cpp(1). Desc-count primary,
    // asc-file tiebreak → in1, then in2, then in3.
    assert_eq!(
        incoming.len(),
        3,
        "hub.h must have 3 incoming files (in1/in2/in3); got {incoming:?}",
    );
    assert!(
        incoming[0].0.ends_with("in1.cpp") && incoming[0].1 == 2,
        "highest-count incoming file (in1.cpp, count 2) must sort first; \
         got {incoming:?}",
    );
    assert!(
        incoming[1].0.ends_with("in2.cpp") && incoming[1].1 == 1,
        "count-1 ties must break asc-by-file: in2.cpp before in3.cpp; \
         got {incoming:?}",
    );
    assert!(
        incoming[2].0.ends_with("in3.cpp") && incoming[2].1 == 1,
        "count-1 ties must break asc-by-file: in3.cpp last; got {incoming:?}",
    );

    // Outgoing: hub.h #includes dep_a.h and dep_b.h, count 1 each, sorted
    // asc-by-file → dep_a.h before dep_b.h.
    assert_eq!(
        outgoing.len(),
        2,
        "hub.h must have 2 outgoing includes (dep_a.h/dep_b.h); got {outgoing:?}",
    );
    assert!(
        outgoing[0].0.ends_with("dep_a.h") && outgoing[1].0.ends_with("dep_b.h"),
        "equal-count outgoing rows must sort asc-by-file (dep_a.h before \
         dep_b.h); got {outgoing:?}",
    );

    drop(dir);
}

// ---------------------------------------------------------------------------
// (b) sequential byte-budget allocation: incoming first, outgoing starved
// ---------------------------------------------------------------------------

/// With `direction=both` and a `max_bytes` budget too small for both
/// sides, the budget is allocated sequentially: incoming is sized against
/// the full budget first, and when it consumes (essentially) all of it
/// the outgoing side is emitted as an *empty* page flagged
/// `truncated: true` with `next_offset: 0` — the start-fresh marker
/// telling a client to re-request the outgoing side via
/// `direction=outgoing offset=0`.
///
/// Byte math (why the magic number works):
/// `byte_budget_take` reserves `ENVELOPE_OVERHEAD_BYTES` (512) off the
/// top, so the per-records budget for the incoming side is
/// `max_bytes - 512`. We pick `max_bytes = 512 + 80 = 592`, leaving an
/// 80-byte records budget — enough for one small `CouplingEntry`
/// (`{"file":"…in1.cpp","count":1}` ≈ 30–55 bytes depending on the temp
/// path width) but not many. Critically, the *serialized incoming `Page`*
/// the handler then measures is the full envelope
/// (`{"results":[…],"total":N,"offset":0,"limit":50,"truncated":…,"next_offset":…}`)
/// which is ~90 bytes of fixed envelope **alone**, well above
/// `592 - 48` (`COUPLING_BOTH_WRAPPER_OVERHEAD = 48`). So
/// `remaining = max_bytes - incoming_bytes - 48` saturates to 0 and the
/// handler takes the "incoming ate the whole budget" branch, emitting the
/// empty/truncated/`next_offset:0` outgoing page. The fixture supplies
/// many incoming files and a couple of outgoing ones so the starvation is
/// caused by the budget, not by an empty graph.
#[tokio::test]
async fn get_coupling_byte_budget_sequential() {
    let dir = TempDir::new().expect("TempDir");
    let root = dir.path();
    // hub.h has outgoing includes (so a non-budget run would emit them).
    std::fs::write(
        root.join("hub.h"),
        b"#include \"dep_a.h\"\n#include \"dep_b.h\"\nvoid hub();\n",
    )
    .unwrap();
    std::fs::write(root.join("dep_a.h"), b"void dep_a();\n").unwrap();
    std::fs::write(root.join("dep_b.h"), b"void dep_b();\n").unwrap();
    // Many incoming translation units so the incoming side is large.
    for i in 0..12 {
        std::fs::write(
            root.join(format!("inc{i:02}.cpp")),
            format!("#include \"hub.h\"\nvoid inc{i:02}() {{}}\n").as_bytes(),
        )
        .unwrap();
    }

    let canonical = std::fs::canonicalize(root).unwrap();
    let server = fresh_server();
    analyze(&server, &canonical).await;

    let hub_h = canonical.join("hub.h").to_string_lossy().into_owned();
    // 512 (ENVELOPE_OVERHEAD_BYTES, reserved inside byte_budget_take) + 80
    // records budget for the incoming side. See the byte-math doc above.
    let max_bytes = 512 + 80;
    let r = get_coupling(
        &server.inner.graph,
        &hub_h,
        Some("both"),
        None,
        None,
        max_bytes,
    );
    let body = ok_json(&r);

    // Sanity: the graph genuinely has a large incoming side (12 TUs all
    // include hub.h). `total` is the pre-pagination count, independent
    // of the tempdir path width — so this rules out the "everything is
    // empty because the graph was empty" false pass without depending
    // on exactly how many rows physically fit the byte budget.
    let incoming = &body["incoming"];
    assert_eq!(
        incoming["total"].as_u64(),
        Some(12),
        "incoming side must have 12 real entries (else starvation is \
         trivial, not sequential); body: {body}",
    );
    // The incoming byte budget genuinely bit: it is itself truncated.
    // This is the path-width-independent proof that incoming consumed
    // the budget (a wide /tmp prefix changes how many rows fit but not
    // that the cap fired), which is what starves outgoing below.
    assert_eq!(
        incoming["truncated"].as_bool(),
        Some(true),
        "incoming side must be byte-capped (truncated=true) so the \
         sequential starvation of outgoing is real; body: {body}",
    );

    // Outgoing was starved: empty results, truncated=true, next_offset=0.
    let outgoing = &body["outgoing"];
    assert_eq!(
        outgoing["results"].as_array().map(|a| a.len()),
        Some(0),
        "outgoing must be an EMPTY page when incoming ate the budget; \
         got: {outgoing}",
    );
    assert_eq!(
        outgoing["truncated"].as_bool(),
        Some(true),
        "starved outgoing page must be flagged truncated=true; got: {outgoing}",
    );
    assert_eq!(
        outgoing["next_offset"].as_u64(),
        Some(0),
        "starved outgoing page must carry next_offset=0 (start-fresh marker \
         for `direction=outgoing offset=0`); got: {outgoing}",
    );
    // `total` still reflects the true pre-pagination outgoing count (2
    // includes: dep_a.h, dep_b.h) even though zero rows were emitted.
    assert_eq!(
        outgoing["total"].as_u64(),
        Some(2),
        "starved outgoing page must still report the true total (2); \
         got: {outgoing}",
    );

    drop(dir);
}

// ---------------------------------------------------------------------------
// (c) directional pagination resume: both → incoming continuation
// ---------------------------------------------------------------------------

/// Load-bearing paging-continuation contract. Call `direction=both` with
/// a `max_bytes` small enough that the incoming side is byte-truncated
/// (`truncated=true`, `next_offset=Some(n)`); then re-call
/// `direction=incoming` with `offset = n` and assert the union of
/// page-1's incoming rows and page-2's rows equals the full sorted
/// incoming set exactly — no overlap, no gap.
#[tokio::test]
async fn get_coupling_directional_pagination_resume() {
    let dir = TempDir::new().expect("TempDir");
    let root = dir.path();
    std::fs::write(root.join("hub.h"), b"void hub();\n").unwrap();
    // 10 translation units each including hub.h once → 10 incoming files,
    // each count 1; deterministic asc-by-file order incNN.cpp.
    const N: usize = 10;
    for i in 0..N {
        std::fs::write(
            root.join(format!("inc{i:02}.cpp")),
            format!("#include \"hub.h\"\nvoid inc{i:02}() {{}}\n").as_bytes(),
        )
        .unwrap();
    }

    let canonical = std::fs::canonicalize(root).unwrap();
    let server = fresh_server();
    analyze(&server, &canonical).await;
    let hub_h = canonical.join("hub.h").to_string_lossy().into_owned();

    // Full sorted incoming set (the ground truth the two pages must
    // partition). NO_BYTE_BUDGET + a generous limit so this is the
    // complete, untruncated reference list.
    let full = get_coupling(
        &server.inner.graph,
        &hub_h,
        Some("incoming"),
        None,
        Some(1000),
        NO_BYTE_BUDGET,
    );
    let full_pairs = coupling_pairs(&ok_json(&full));
    assert_eq!(
        full_pairs.len(),
        N,
        "ground-truth incoming set must hold all {N} translation units; \
         got {full_pairs:?}",
    );

    // Page 1 via direction=both with a byte budget tight enough to cut the
    // incoming side mid-list. 512 reserved + ~120 records budget admits a
    // few small CouplingEntry rows but not all 10.
    let max_bytes = 512 + 120;
    let p1 = get_coupling(
        &server.inner.graph,
        &hub_h,
        Some("both"),
        None,
        None,
        max_bytes,
    );
    let p1_body = ok_json(&p1);
    let p1_incoming = &p1_body["incoming"];
    assert_eq!(
        p1_incoming["truncated"].as_bool(),
        Some(true),
        "page-1 incoming side must be byte-truncated for this test to \
         exercise the resume contract; body: {p1_body}",
    );
    let next = p1_incoming["next_offset"]
        .as_u64()
        .expect("a truncated incoming page must carry a numeric next_offset") as u32;
    let p1_pairs = coupling_pairs(p1_incoming);
    assert!(
        !p1_pairs.is_empty() && p1_pairs.len() < N,
        "page-1 incoming must be a non-empty strict prefix (truncation \
         must have actually cut the list); got {p1_pairs:?}",
    );

    // Page 2: re-call direction=incoming with offset = next_offset.
    let p2 = get_coupling(
        &server.inner.graph,
        &hub_h,
        Some("incoming"),
        Some(next),
        Some(1000),
        NO_BYTE_BUDGET,
    );
    let p2_pairs = coupling_pairs(&ok_json(&p2));

    // THE load-bearing assertion: page1.incoming ++ page2 == full sorted
    // incoming set, exactly. Concatenation (not set-union) so any overlap
    // (a duplicated row) or gap (a skipped row) fails — the two pages must
    // partition the sorted reference list contiguously at `next_offset`.
    let mut joined = p1_pairs.clone();
    joined.extend(p2_pairs.clone());
    assert_eq!(
        joined, full_pairs,
        "paging-resume contract violated: page1.incoming ({p1_pairs:?}) \
         concatenated with page2 (offset={next}, {p2_pairs:?}) must equal \
         the full sorted incoming set ({full_pairs:?}) with no overlap and \
         no gap",
    );

    drop(dir);
}

// ---------------------------------------------------------------------------
// (d) get_dependencies preserves #include source line numbers
// ---------------------------------------------------------------------------

/// `get_dependencies` returns `Page<DependencyEntry>` where every row
/// carries the source line of its `#include` directive and `kind`
/// `"includes"`, sorted `(file, line)` ascending. Fixture places three
/// `#include`s at lines 5, 10, 15 of `main.cpp`, each resolving to a real
/// discovered header.
#[tokio::test]
async fn get_dependencies_line_numbers_preserved() {
    let dir = TempDir::new().expect("TempDir");
    let root = dir.path();
    std::fs::write(root.join("alpha.h"), b"void alpha();\n").unwrap();
    std::fs::write(root.join("mid.h"), b"void mid();\n").unwrap();
    std::fs::write(root.join("zeta.h"), b"void zeta();\n").unwrap();
    // #include "alpha.h" on line 5, "mid.h" on line 10, "zeta.h" on line
    // 15. The leading filler lines are comments so the C++ parser records
    // the directive's exact 1-based line.
    let mut src = String::new();
    for line in 1..=15 {
        match line {
            5 => src.push_str("#include \"alpha.h\"\n"),
            10 => src.push_str("#include \"mid.h\"\n"),
            15 => src.push_str("#include \"zeta.h\"\n"),
            _ => src.push_str("// filler\n"),
        }
    }
    src.push_str("void m() {}\n");
    std::fs::write(root.join("main.cpp"), src.as_bytes()).unwrap();

    let canonical = std::fs::canonicalize(root).unwrap();
    let server = fresh_server();
    analyze(&server, &canonical).await;

    let main_cpp = canonical.join("main.cpp").to_string_lossy().into_owned();
    let r = get_dependencies(&server.inner.graph, &main_cpp, None, None, NO_BYTE_BUDGET);
    let body = ok_json(&r);
    let rows = body["results"]
        .as_array()
        .expect("Page<DependencyEntry> results array");

    assert_eq!(
        rows.len(),
        3,
        "main.cpp has exactly 3 #include dependencies; got {rows:?}",
    );
    // Sorted (file, line) ascending: alpha.h(5) < mid.h(10) < zeta.h(15).
    let observed: Vec<(String, u64, String)> = rows
        .iter()
        .map(|r| {
            (
                r["file"].as_str().unwrap().to_string(),
                r["line"].as_u64().expect("DependencyEntry.line"),
                r["kind"]
                    .as_str()
                    .expect("DependencyEntry.kind")
                    .to_string(),
            )
        })
        .collect();
    assert!(
        observed[0].0.ends_with("alpha.h") && observed[0].1 == 5,
        "first dep row must be alpha.h at line 5; got {observed:?}",
    );
    assert!(
        observed[1].0.ends_with("mid.h") && observed[1].1 == 10,
        "second dep row must be mid.h at line 10; got {observed:?}",
    );
    assert!(
        observed[2].0.ends_with("zeta.h") && observed[2].1 == 15,
        "third dep row must be zeta.h at line 15; got {observed:?}",
    );
    assert!(
        observed.iter().all(|(_, _, kind)| kind == "includes"),
        "every DependencyEntry.kind must be \"includes\"; got {observed:?}",
    );

    drop(dir);
}

// ---------------------------------------------------------------------------
// (e) .ini filter — integration confirmation via the real analyze path
// ---------------------------------------------------------------------------

/// Integration-level confirmation that a non-source (`.ini`) include
/// target never reaches `Graph::includes` after a full `analyze_codebase`
/// run, observed through the public `get_dependencies` handler. The
/// Includes-edge resolution arm drops an include unless it resolves to an
/// indexed source file: both the resolve-miss case (target never in the
/// FileIndex — the dominant real-world path for system/external/`.ini`
/// headers) and the resolved-to-non-source case are dropped. The
/// `language_for_path` extension-filter half of that arm is pinned by the
/// indexer unit test `resolve_all_edges_drops_include_to_non_source_target`
/// (which force-injects the `.ini` into the FileIndex via a synthetic
/// FileGraph so the resolver returns `Some(<.ini>)`). On the real
/// `analyze_codebase` path a `.ini` is never discovered, so it is never in
/// the FileIndex, so the resolver returns `None` and the edge is dropped
/// by the resolve-miss branch of the same arm — the *client-visible
/// contract* ("the `.ini` does not appear in deps") is identical and is
/// what this integration wrapper independently pins through the full
/// discover→parse→resolve→merge→handler pipeline. A regression that
/// re-introduced unresolved-target leakage into `Graph::includes` would
/// fail here even though the unit test (which takes the `Some(<.ini>)`
/// path) would not see it.
#[tokio::test]
async fn indexer_ini_filter_drops_non_source_edges() {
    let dir = TempDir::new().expect("TempDir");
    let root = dir.path();
    std::fs::write(root.join("config.ini"), b"[section]\nkey=value\n").unwrap();
    std::fs::write(root.join("sibling.h"), b"void sibling();\n").unwrap();
    std::fs::write(
        root.join("main.cpp"),
        b"#include \"config.ini\"\n#include \"sibling.h\"\nvoid m() {}\n",
    )
    .unwrap();

    let canonical = std::fs::canonicalize(root).unwrap();
    let server = fresh_server();
    analyze(&server, &canonical).await;

    let main_cpp = canonical.join("main.cpp").to_string_lossy().into_owned();
    let r = get_dependencies(&server.inner.graph, &main_cpp, None, None, NO_BYTE_BUDGET);
    let body = ok_json(&r);
    let files: Vec<String> = body["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|row| row["file"].as_str().unwrap().to_string())
        .collect();

    assert!(
        files.iter().any(|f| f.ends_with("sibling.h")),
        "the real .h include must survive into Graph::includes / the deps \
         response; got {files:?}",
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".ini")),
        "no .ini target may reach Graph::includes / the deps response; \
         got {files:?}",
    );

    drop(dir);
}

// ---------------------------------------------------------------------------
// (f) .ini filter shows through to the get_dependencies handler response
// ---------------------------------------------------------------------------

/// Handler-layer round-trip: after a real index, a file that `#include`s
/// both a `.ini` and a real header must produce a `get_dependencies`
/// response containing exactly the header — the `.ini` filter is visible
/// at the client boundary, not just inside the graph. Distinct from (e):
/// (e) frames the assertion as "never reaches `Graph::includes`"; (f)
/// frames it as "the serialized `Page<DependencyEntry>` the agent sees
/// has only the source entry", and additionally pins `total` so a
/// regression that emitted the `.ini` but hid it via pagination would
/// still fail.
#[tokio::test]
async fn get_dependencies_ini_excluded_from_response() {
    let dir = TempDir::new().expect("TempDir");
    let root = dir.path();
    std::fs::write(root.join("settings.ini"), b"[a]\nb=c\n").unwrap();
    std::fs::write(root.join("only.h"), b"void only();\n").unwrap();
    std::fs::write(
        root.join("app.cpp"),
        b"#include \"settings.ini\"\n#include \"only.h\"\nvoid app() {}\n",
    )
    .unwrap();

    let canonical = std::fs::canonicalize(root).unwrap();
    let server = fresh_server();
    analyze(&server, &canonical).await;

    let app_cpp = canonical.join("app.cpp").to_string_lossy().into_owned();
    let r = get_dependencies(&server.inner.graph, &app_cpp, None, None, NO_BYTE_BUDGET);
    let body = ok_json(&r);
    let rows = body["results"].as_array().expect("results array");

    assert_eq!(
        rows.len(),
        1,
        "exactly one dependency row (only.h) must show through; the \
         settings.ini must be filtered out; got {rows:?}",
    );
    assert_eq!(
        body["total"].as_u64(),
        Some(1),
        "Page.total must be 1 — a regression that emitted the .ini but \
         paginated it out of view would still bump total; got: {body}",
    );
    assert!(
        rows[0]["file"].as_str().unwrap().ends_with("only.h"),
        "the single surviving dependency must be only.h; got {rows:?}",
    );
    assert_eq!(
        rows[0]["kind"].as_str(),
        Some("includes"),
        "surviving row kind must be \"includes\"; got {rows:?}",
    );

    drop(dir);
}

// ---------------------------------------------------------------------------
// (g) the .ini filter must also fire on the WATCH reindex path
// ---------------------------------------------------------------------------

/// The `.ini` filter is duplicated in `try_reindex_file`
/// (`handlers/watch.rs`) — a separate copy-paste of the indexer's
/// resolve loop. CLAUDE.md documents `watch.rs` historically drifting
/// from the indexer; this test independently pins the *watch-path*
/// filter so a future divergence is caught.
///
/// To genuinely exercise the watch-path `language_for_path` filter (not
/// just the pre-existing resolve-miss), the watch FileIndex must actually
/// contain the `.ini`. The watch path builds its FileIndex from
/// `Graph::file_graphs_snapshot()`, so we seed a `config.ini` FileGraph
/// directly into the graph (a `files` entry with zero symbols survives
/// the snapshot). The reindexed `app.cpp` then `#include`s both
/// `config.ini` and `helper.h`; `resolve_include` resolves the `.ini`
/// from the seeded FileIndex entry, and the watch-path filter must drop
/// it while keeping `helper.h`.
///
/// `try_reindex_file` is called directly (no debouncer) for determinism,
/// the established idiom from `tests/watch_dangling_edges.rs`.
#[tokio::test]
async fn watch_reindex_applies_ini_filter() {
    let dir = TempDir::new().expect("TempDir");
    let root = dir.path();
    std::fs::write(root.join("helper.h"), b"void helper();\n").unwrap();
    // app.cpp initially includes only the real header so the first index
    // is a clean baseline.
    std::fs::write(
        root.join("app.cpp"),
        b"#include \"helper.h\"\nvoid app() {}\n",
    )
    .unwrap();

    let canonical = std::fs::canonicalize(root).unwrap();
    let server = fresh_server();
    analyze(&server, &canonical).await;

    let app_cpp = canonical.join("app.cpp");
    let ini_path = canonical.join("config.ini");

    // Sentinel (diagnostic-before-discriminator, per CLAUDE.md test
    // conventions): the baseline real-header dep resolved before we
    // exercise the .ini discriminator, so a sentinel failure names the
    // likely root cause (resolution/indexing) rather than the filter.
    {
        let r = get_dependencies(
            &server.inner.graph,
            &app_cpp.to_string_lossy(),
            None,
            None,
            NO_BYTE_BUDGET,
        );
        let body = ok_json(&r);
        let files: Vec<String> = body["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["file"].as_str().unwrap().to_string())
            .collect();
        assert!(
            files.iter().any(|f| f.ends_with("helper.h")),
            "sentinel: baseline app.cpp must depend on helper.h before the \
             watch reindex (a failure here is a resolution/indexing problem, \
             not a .ini-filter problem); got {files:?}",
        );
    }

    // Seed a `config.ini` FileGraph (zero symbols, zero edges) so the
    // watch-path FileIndex — built from file_graphs_snapshot() — actually
    // carries the .ini basename. Without this the resolver would return
    // None and the edge would be dropped by the pre-existing resolve-miss,
    // not the watch-path language_for_path filter under test.
    std::fs::write(&ini_path, b"[section]\nk=v\n").unwrap();
    // The `language` tag here is irrelevant — only the path needs to
    // appear in file_graphs_snapshot() so the watch FileIndex carries
    // the .ini basename. The watch filter under test keys off the
    // resolved path's extension (`.ini`) via `language_for_path`, not
    // off this stored tag, so `Cpp` is an arbitrary filler.
    server.inner.graph.write().merge_file_graph(FileGraph {
        path: ini_path.to_string_lossy().into_owned(),
        language: Language::Cpp,
        symbols: Vec::new(),
        edges: Vec::new(),
    });
    // Sanity: the seeded .ini is now a known file (so the FileIndex the
    // watch path builds will index its basename).
    assert!(
        server
            .inner
            .graph
            .read()
            .file_graphs_snapshot()
            .iter()
            .any(|fg| fg.path.ends_with("config.ini")),
        "seeded config.ini FileGraph must survive file_graphs_snapshot so \
         the watch FileIndex carries it",
    );

    // Rewrite app.cpp to ALSO #include the .ini, then drive the watch
    // reindex directly (idiom from tests/watch_dangling_edges.rs).
    std::fs::write(
        &app_cpp,
        b"#include \"helper.h\"\n#include \"config.ini\"\nvoid app() {}\n",
    )
    .unwrap();
    let outcome = try_reindex_file(&server.inner, &app_cpp, false).await;
    match outcome {
        ReindexOutcome::Reindexed => {}
        other => panic!("expected Reindexed from try_reindex_file, got {other:?}"),
    }

    // After the watch reindex, app.cpp's deps must contain helper.h but
    // NOT config.ini — proving the watch-path filter fired.
    let r = get_dependencies(
        &server.inner.graph,
        &app_cpp.to_string_lossy(),
        None,
        None,
        NO_BYTE_BUDGET,
    );
    let body = ok_json(&r);
    let files: Vec<String> = body["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|v| v["file"].as_str().unwrap().to_string())
        .collect();
    assert!(
        files.iter().any(|f| f.ends_with("helper.h")),
        "post-watch-reindex deps must still contain helper.h; got {files:?}",
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".ini")),
        "the watch reindex path must apply the .ini filter — config.ini \
         must NOT appear in app.cpp's dependencies after reindex; got \
         {files:?}",
    );

    drop(dir);
}
