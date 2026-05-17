//! Acceptance regression test for the three response-shape fixes that
//! came out of running the server against a generic UE project.
//!
//! # Failure modes this file pins
//!
//! Three distinct shapes that an AI agent driving the MCP server hit on a
//! large real codebase, each of which produced an unusable or misleading
//! response. Every test below reconstructs the *shape* of the original
//! failure with a small synthetic Rust fixture, then asserts the failure
//! mode is GONE — not "smaller", gone.
//!
//! 1. **`get_symbol_summary` blew the response byte budget.** On a
//!    codebase with hundreds of `(namespace, kind)` pairs the un-capped
//!    summary serialized well past the 100 KB harness ceiling, so the
//!    agent got a hard rejection with no continuation hint. The fix is
//!    routing the summary through the same `byte_budget_take` /
//!    `{truncated, next_offset}` envelope the other paginated tools use.
//!    This test proves the cap is *load-bearing*: the same fixture
//!    serialized with no budget exceeds 100 KB, and with the production
//!    default budget it stays under and the envelope honestly reports
//!    `truncated: true` + a non-null `next_offset`.
//!
//! 2. **`get_class_hierarchy` silently dropped a diamond arm.** A class
//!    reachable through two inheritance arms used to come back as an
//!    empty leaf on the second arm — the agent could not tell the second
//!    arm even existed. The fix emits an explicit `{name, ref: true}`
//!    stub for the deduplicated re-occurrence so a client can rejoin the
//!    canonical subtree. This test builds a real trait/impl diamond and
//!    asserts a `"ref": true` stub is present in the serialized tree.
//!
//! 3. **`generate_diagram` leaked unresolved file-basename pseudo-nodes
//!    and lost an arm.** A high-fan-in symbol used to render edges whose
//!    endpoint was a bare file basename (an unresolved call target with
//!    no symbol behind it), and a `both`-direction request did not
//!    reliably surface both arms. The fix drops edges whose endpoint
//!    doesn't resolve to a real symbol and tags every edge with the arm
//!    that produced it. This test asserts a `both` request yields BOTH a
//!    `"calls"` and a `"called_by"` edge AND that no edge endpoint is a
//!    path/basename pseudo-node.
//!
//! # Scenario intentionally omitted
//!
//! The original scenario set also included a 100-file include-cycle scenario
//! whose response over-ran the budget. It is intentionally omitted here:
//! Rust has no real cyclic imports, and bolting a C++ include-ring
//! fixture onto this file would defeat its Rust-only harness simplicity.
//! That scenario's regression is already pinned by
//! `per_cycle_cap_truncates_large_scc` — a 200-file synthetic
//! strongly-connected include ring that asserts the per-cycle file-list
//! cap clips the cycle and self-reports `Cycle.truncated` /
//! `Cycle.original_len`.
//!
//! # Harness
//!
//! Each test writes a small generated Rust fixture into a fresh
//! `TempDir` (so concurrent runs cannot race on `.code-graph-cache.json`),
//! runs `analyze_codebase(force=true)` through the real indexing
//! pipeline, then drives the target handler exactly as the MCP server
//! would. Same shape as `byte_budget_acceptance.rs`; the shared
//! `common::first_text` helper extracts the response body.

mod common;
use common::first_text;

use std::collections::HashSet;
use std::sync::Arc;

use code_graph_core::DEFAULT_RESPONSE_MAX_BYTES;
use code_graph_lang::LanguageRegistry;
use code_graph_lang_rust::RustParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::handlers::structure::{
    generate_diagram, get_class_hierarchy, GenerateDiagramInput,
};
use code_graph_tools::handlers::symbols::get_symbol_summary;
use code_graph_tools::handlers::{ENVELOPE_OVERHEAD_BYTES, NO_BYTE_BUDGET};
use code_graph_tools::server::ServerInner;
use code_graph_tools::CodeGraphServer;
use tempfile::TempDir;

/// Generous overhead margin layered on top of `max_bytes` for the
/// fits-under-budget assertion, mirroring `byte_budget_acceptance.rs`.
/// The handler reserves `ENVELOPE_OVERHEAD_BYTES` from the per-records
/// budget; checking against `max_bytes + ENVELOPE_OVERHEAD_BYTES` is a
/// conservative ceiling that catches "budget bypassed entirely"
/// regressions without flaking on envelope-size jitter.
const ENVELOPE_HEADROOM_BYTES: usize = ENVELOPE_OVERHEAD_BYTES;

/// Per-test fixture: holds the `TempDir` for the test's lifetime and the
/// indexed `ServerInner`. The `TempDir` must outlive every query because
/// the in-memory graph references file paths that were canonicalized
/// against it at index time.
struct IndexedFixture {
    _dir: TempDir,
    inner: Arc<ServerInner>,
}

/// Write `files` (relative name -> source) into a fresh `TempDir`, index
/// it with the Rust parser through the real `analyze_codebase` pipeline,
/// and return the indexed server. Each test gets its own `TempDir` so
/// concurrent runs cannot race on the shared `.code-graph-cache.json`.
async fn build_indexed_fixture(files: &[(&str, String)]) -> IndexedFixture {
    let dir = TempDir::new().expect("TempDir for response-shape fixture");
    for (name, src) in files {
        std::fs::write(dir.path().join(name), src)
            .unwrap_or_else(|e| panic!("write fixture file {name}: {e}"));
    }
    let indexed_root =
        std::fs::canonicalize(dir.path()).expect("canonicalize fixture dir for analyze");

    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(RustParser::new().expect("RustParser::new")))
        .expect("register RustParser");
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

// ---------------------------------------------------------------------------
// Scenario 1: get_symbol_summary stays under the byte budget, and the cap
// is load-bearing (uncapped would exceed 100 KB; capped stays under and
// the envelope honestly reports truncation).
// ---------------------------------------------------------------------------

/// Number of fixture files. Each contributes one unique 6-segment module
/// path; every leaf module holds the full set of Rust symbol kinds, so
/// each file produces several distinct `(namespace, kind)` summary rows.
/// 60 files × the per-file kind spread lands at a few hundred rows whose
/// long namespace strings make the un-capped serialization clear 100 KB.
const SUMMARY_FILE_COUNT: usize = 60;

/// Build one fixture file whose symbols all sit under a unique, long,
/// deeply-nested module path. The long namespace strings are deliberate:
/// `SummaryRow` is `{namespace, kind, count}`, so a long `namespace`
/// field is what drives the un-capped serialization past 100 KB at a
/// realistic row count (this mirrors the deep `module::sub::sub` paths
/// the real UE-adjacent codebase carried).
fn summary_fixture_file(file_idx: usize) -> String {
    let mut src = String::new();
    for depth in 0..6 {
        src.push_str(&format!(
            "pub mod really_long_namespace_segment_number_{file_idx}_{depth}_padding_xxxxxxxxxxxx {{\n"
        ));
    }
    // A spread of kinds so each namespace yields multiple summary rows:
    // function, struct, enum, trait, typedef, plus methods via an impl.
    src.push_str("pub fn func_a() {}\npub fn func_b() {}\n");
    src.push_str("pub struct StructA; pub struct StructB;\n");
    src.push_str("pub enum EnumA { V } pub enum EnumB { V }\n");
    src.push_str("pub trait TraitA {} pub trait TraitB {}\n");
    src.push_str("pub type AliasA = u32; pub type AliasB = u64;\n");
    src.push_str("pub struct Holder;\nimpl Holder { pub fn m1(&self) {} pub fn m2(&self) {} }\n");
    for _ in 0..6 {
        src.push_str("}\n");
    }
    src
}

#[tokio::test]
async fn get_symbol_summary_byte_budget_is_load_bearing() {
    let files: Vec<(String, String)> = (0..SUMMARY_FILE_COUNT)
        .map(|i| (format!("f{i}.rs"), summary_fixture_file(i)))
        .collect();
    let files_ref: Vec<(&str, String)> =
        files.iter().map(|(n, s)| (n.as_str(), s.clone())).collect();
    let fx = build_indexed_fixture(&files_ref).await;

    // (a) Un-capped: prove the fixture is genuinely large enough that the
    // budget has real work to do. If this body is already under 100 KB
    // the rest of the test would be vacuous (a too-small fixture, not a
    // working cap), so assert the un-capped serialization EXCEEDS the
    // production ceiling. A generous limit makes the helper try to emit
    // every row.
    let uncapped = get_symbol_summary(
        &fx.inner.graph,
        None,
        Some(1_000_000),
        Some(0),
        false,
        NO_BYTE_BUDGET,
    );
    let uncapped_body = first_text(&uncapped);
    let uncapped_len = uncapped_body.len();
    let uncapped_json: serde_json::Value =
        serde_json::from_str(&uncapped_body).expect("uncapped summary body must be valid JSON");
    let total = uncapped_json["total"]
        .as_u64()
        .expect("summary envelope must carry `total`");

    assert!(
        uncapped_len > DEFAULT_RESPONSE_MAX_BYTES,
        "fixture not large enough to exercise the cap: un-capped \
         get_symbol_summary serialized to {uncapped_len} bytes (total={total} \
         rows), which does NOT exceed the {DEFAULT_RESPONSE_MAX_BYTES}-byte \
         production ceiling. Scale SUMMARY_FILE_COUNT up until the un-capped \
         body clears 100 KB, otherwise the capped assertion below proves \
         nothing."
    );

    // (b) Capped at the production default: the body MUST fit, and the
    // envelope MUST honestly report it was cut short by the byte budget
    // (truncated=true + non-null next_offset), not silently drop rows.
    let capped = get_symbol_summary(
        &fx.inner.graph,
        None,
        Some(1_000_000),
        Some(0),
        false,
        DEFAULT_RESPONSE_MAX_BYTES,
    );
    let capped_body = first_text(&capped);
    let capped_len = capped_body.len();
    let capped_json: serde_json::Value =
        serde_json::from_str(&capped_body).expect("capped summary body must be valid JSON");

    assert!(
        capped_len <= DEFAULT_RESPONSE_MAX_BYTES + ENVELOPE_HEADROOM_BYTES,
        "get_symbol_summary exceeded the response byte budget — the cap \
         is not being applied on this path. capped body {capped_len} bytes \
         > budget {DEFAULT_RESPONSE_MAX_BYTES} + headroom \
         {ENVELOPE_HEADROOM_BYTES}. This is the original UE failure mode \
         (un-paginated summary overran the harness ceiling)."
    );

    let capped_total = capped_json["total"]
        .as_u64()
        .expect("capped summary envelope must carry `total`");
    assert_eq!(
        capped_total, total,
        "`total` must be the pre-pagination row count and stable whether \
         or not the budget bites: uncapped={total}, capped={capped_total}"
    );

    let truncated = capped_json["truncated"]
        .as_bool()
        .expect("summary envelope must carry `truncated`");
    assert!(
        truncated,
        "the capped summary was cut short by the byte budget but the \
         envelope reports truncated=false — a dishonest envelope is the \
         exact misleading-response failure mode the fix removed. \
         capped_len={capped_len} total={total}"
    );

    let next_offset = &capped_json["next_offset"];
    assert!(
        next_offset.is_u64(),
        "truncated=true MUST come with a non-null `next_offset` so the \
         agent can resume paging; got next_offset={next_offset}. Without \
         it the agent is stuck exactly as in the original failure."
    );
    assert!(
        next_offset.as_u64().unwrap() > 0,
        "next_offset must point strictly past the first emitted page; \
         got {next_offset}"
    );
}

// ---------------------------------------------------------------------------
// Scenario 2: get_class_hierarchy emits an explicit `ref: true` stub for a
// diamond's deduplicated re-occurrence (instead of silently dropping the
// second arm to an empty leaf).
// ---------------------------------------------------------------------------

/// A genuine inheritance diamond expressed the only way the Rust parser
/// emits `Inherits` edges: `impl Trait for Type` blocks (the parser's
/// inheritance query matches `impl_item` with a `trait:` field; trait
/// supertrait bounds like `trait Leaf: D1 + D2 {}` do NOT produce
/// `Inherits` edges, so the diamond must be modeled through impls).
///
/// Edges (child -> parent, the `derived -> base` direction):
///   Arm1 -> Root, Arm1 -> D1, Arm2 -> Root, Arm2 -> D2,
///   Apex -> D1,  Apex -> D2,  Apex -> Leaf.
///
/// Walking the hierarchy from `Root` reaches `Arm2` twice — once directly
/// as a `Root` descendant, once through the `D1 -> Apex -> D2 -> Arm2`
/// arm. The second reach is the deduplicated occurrence and MUST come
/// back as a `{name: "Arm2", ref: true}` stub.
const DIAMOND_FIXTURE: &str = "\
pub trait Root {}
pub trait D1 {}
pub trait D2 {}
pub trait Leaf {}

pub struct Apex;
pub struct Arm1;
pub struct Arm2;

impl D1 for Apex {}
impl D2 for Apex {}
impl Leaf for Apex {}

impl Root for Arm1 {}
impl D1 for Arm1 {}
impl Root for Arm2 {}
impl D2 for Arm2 {}
";

/// Recursively walk a `HierarchyNode` JSON value and return true if any
/// node carries `"ref": true`. The `ref` JSON key is the serialized form
/// of the Rust `r#ref: Option<bool>` field (serde strips the `r#`
/// raw-identifier prefix), present only when the value is `true`
/// (`skip_serializing_if = "Option::is_none"`).
fn any_ref_stub(node: &serde_json::Value) -> bool {
    if node["ref"].as_bool() == Some(true) {
        return true;
    }
    for arm in ["bases", "derived"] {
        if let Some(children) = node[arm].as_array() {
            if children.iter().any(any_ref_stub) {
                return true;
            }
        }
    }
    false
}

#[tokio::test]
async fn get_class_hierarchy_emits_diamond_ref_stub() {
    let fx = build_indexed_fixture(&[("diamond.rs", DIAMOND_FIXTURE.to_string())]).await;

    // `Root` is the diamond's shared ancestor; the walk reaches `Arm2`
    // through two arms, so the second reach must be a ref-stub. depth is
    // generous and max_nodes is well above the 7-name fixture so neither
    // depth nor the node budget can mask the dedupe behavior.
    let r = get_class_hierarchy(&fx.inner.graph, "Root", Some(8), Some(1000));
    let body = first_text(&r);
    let json: serde_json::Value =
        serde_json::from_str(&body).expect("class hierarchy body must be valid JSON");

    let hierarchy = &json["hierarchy"];
    assert_eq!(
        hierarchy["name"].as_str(),
        Some("Root"),
        "hierarchy apex must be the queried class `Root`; body: {body}"
    );

    // Sanity: the diamond actually materialized. If the Inherits edges
    // didn't fire, the apex would be a bare leaf and the ref-stub
    // assertion below would be vacuous rather than meaningful.
    assert!(
        hierarchy["derived"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false),
        "diamond did not materialize: `Root` came back with no `derived` \
         children, so the impl-block Inherits edges were not produced. \
         body: {body}"
    );

    assert!(
        any_ref_stub(hierarchy),
        "get_class_hierarchy did NOT emit a `\"ref\": true` stub for the \
         diamond's deduplicated re-occurrence. Without the stub the second \
         arm collapses to an empty leaf and the agent cannot tell the \
         shared node is reachable both ways — the original diamond-drop \
         failure mode. body: {body}"
    );
}

// ---------------------------------------------------------------------------
// Scenario 3: generate_diagram (symbol mode, direction="both") surfaces
// BOTH arms and never leaks an unresolved file-basename pseudo-node.
// ---------------------------------------------------------------------------

/// `target_fn` is both a high-fan-in callee (100 distinct callers) AND a
/// caller of `sink_fn`. A `direction="both"` symbol diagram centered on
/// it must therefore yield BOTH a forward `calls` edge
/// (`target_fn -> sink_fn`) and a reverse `called_by` edge
/// (`caller_N -> target_fn`). The 100 callers reproduce the original
/// high-fan-out shape that surfaced the unresolved-target leak.
fn fanout_fixture() -> String {
    let mut src = String::from("pub fn sink_fn() {}\npub fn target_fn() { sink_fn(); }\n");
    for i in 0..100 {
        src.push_str(&format!("pub fn caller_{i}() {{ target_fn(); }}\n"));
    }
    src
}

#[tokio::test]
async fn generate_diagram_both_directions_and_no_file_node_leak() {
    let fx = build_indexed_fixture(&[("fanout.rs", fanout_fixture())]).await;

    // Reconstruct the `file:name` symbol id the same way the indexer did:
    // the canonicalized fixture-dir path joined with the file name, then
    // `:target_fn`.
    let root = std::fs::canonicalize(fx._dir.path()).expect("canonicalize fixture dir");
    let file_path = root.join("fanout.rs");
    let symbol_id = format!("{}:target_fn", file_path.to_string_lossy());

    let r = generate_diagram(
        &fx.inner.graph,
        GenerateDiagramInput {
            symbol: Some(&symbol_id),
            direction: Some("both"),
            depth: Some(3),
            max_nodes: Some(500),
            format: Some("edges"),
            ..Default::default()
        },
    );
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "generate_diagram returned an error result: {r:?}"
    );
    let body = first_text(&r);
    let edges: serde_json::Value =
        serde_json::from_str(&body).expect("diagram edges body must be valid JSON");
    let arr = edges
        .as_array()
        .expect("symbol-mode diagram (format=edges) must serialize as a JSON array");

    assert!(
        !arr.is_empty(),
        "diagram produced zero edges for a symbol with 100 callers and one \
         callee — the call graph did not materialize. body: {body}"
    );

    // Both arms must be present. `direction` serializes as the per-edge
    // orientation tag: `"calls"` (forward / callee arm) or `"called_by"`
    // (reverse / caller arm).
    let directions: HashSet<&str> = arr.iter().filter_map(|e| e["direction"].as_str()).collect();
    assert!(
        directions.contains("calls"),
        "direction=both did not yield any `\"direction\": \"calls\"` edge \
         (expected target_fn -> sink_fn). The forward arm was dropped. \
         distinct directions seen: {directions:?}; body: {body}"
    );
    assert!(
        directions.contains("called_by"),
        "direction=both did not yield any `\"direction\": \"called_by\"` \
         edge (expected caller_N -> target_fn). The reverse arm was \
         dropped. distinct directions seen: {directions:?}; body: {body}"
    );

    // No edge endpoint may be an unresolved file-basename / path
    // pseudo-node. A real resolved endpoint is a symbol display name
    // (`name` or `Parent::name`) and never contains a path separator or
    // a source-file extension; a leaked unresolved target would render as
    // the bare basename, e.g. `fanout.rs`.
    for e in arr {
        for end in ["from", "to"] {
            let label = e[end]
                .as_str()
                .unwrap_or_else(|| panic!("diagram edge {end} must be a string; edge: {e}"));
            assert!(
                !label.contains('/') && !label.contains('\\') && !label.ends_with(".rs"),
                "diagram edge `{end}` is a file/path pseudo-node {label:?} \
                 — an unresolved call target leaked into the rendered graph \
                 instead of being dropped. This is the original \
                 file-basename-leak failure mode. edge: {e}"
            );
        }
    }
}
