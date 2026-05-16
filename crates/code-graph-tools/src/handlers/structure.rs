//! Structural-analysis handlers: `detect_cycles`, `get_orphans`,
//! `get_class_hierarchy`, `get_coupling`, `generate_diagram`.
//!
//! Mirrors the Go reference at `internal/tools/structure.go` for shape
//! and JSON output. Error wording follows the Phase 3.4 carry-forward
//! principle: Rust idioms (e.g. listing valid values inline) over rote
//! Go parity. Specific divergences are documented inline so future
//! readers understand which strings are deliberate.

use std::collections::HashMap;
use std::path::PathBuf;

use code_graph_core::{paths, symbol_id, SymbolKind};
use code_graph_graph::{DiagramDirection, DiagramEdge, Graph, HierarchyNode};
use parking_lot::RwLock;
use rmcp::model::{CallToolResult, Content};
use serde::Serialize;

use super::{
    byte_budget_take, parse_kind, suggest_symbols, symbol_to_result, tool_error, tool_success_json,
    CouplingBoth, CouplingEntry, Cycle, Page, SymbolResult,
};

// ----- detect_cycles -----

/// `detect_cycles` body. Returns the SCCs (size > 1) of the include
/// graph wrapped in the shared [`Page`] envelope so a UE-scale codebase
/// with many circular includes doesn't blow the MCP token ceiling.
///
/// Each cycle is a [`Cycle`] whose `files` is a list of file path
/// strings (PathBuf → String via `to_string_lossy` for cross-platform
/// stability). For deterministic pagination the inner cycle paths are
/// sorted, then the outer cycle list is sorted by each cycle's first
/// path — Tarjan's SCC output order is stable per build but not
/// lexicographic, so we canonicalize both axes to keep page boundaries
/// reproducible.
///
/// Defaults: `limit = 20`, `offset = 0`. `limit = 0` resolves to 20
/// (mirrors `search_symbols` / `get_orphans`); `limit` clamps at 1000.
/// `offset >= total` returns an empty `results` page with the correct
/// `total`.
///
/// The envelope's `truncated`/`next_offset` are honest: when the slice
/// stops short of `total`, `truncated` is `true` and `next_offset`
/// points one past the last emitted cycle so a client can resume
/// paging. Pagination is purely by COUNT — the byte-budget cap that
/// governs the symbol-list tools is intentionally NOT consulted here.
pub fn detect_cycles(
    graph: &RwLock<Graph>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> CallToolResult {
    let resolved_limit = limit.filter(|&n| n != 0).unwrap_or(20).min(1000);
    let resolved_offset = offset.unwrap_or(0);

    let cycles: Vec<Vec<PathBuf>> = graph.read().detect_cycles();

    // Convert PathBuf -> String for stable JSON output. PathBuf serializes
    // through serde as `String` on Unix, but going through to_string_lossy
    // makes the conversion explicit and is robust on platforms whose
    // OsStr is not UTF-8 (Windows). Sort within each cycle for canonical
    // representation.
    let mut stringified: Vec<Vec<String>> = cycles
        .into_iter()
        .map(|cycle| {
            let mut paths: Vec<String> = cycle
                .into_iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            paths.sort();
            paths
        })
        .collect();

    // Sort outer Vec by the first path in each cycle. The cycles are
    // already canonical-sorted internally, so first-path is stable. This
    // makes page 1 + page 2 partition deterministically across calls.
    stringified.sort_by(|a, b| a.first().cmp(&b.first()));

    let total = stringified.len() as u32;
    let results: Vec<Cycle> = stringified
        .into_iter()
        .skip(resolved_offset as usize)
        .take(resolved_limit as usize)
        .map(|files| Cycle {
            files,
            // Per-cycle truncation (a single oversized cycle's `files`
            // list being clipped) is a separate axis from envelope
            // pagination; it stays unset here and is governed elsewhere.
            truncated: false,
            original_len: None,
        })
        .collect();

    // Cycle pagination is by COUNT, not by serialized byte size: the
    // envelope's `truncated`/`next_offset` are derived purely from
    // offset/emitted/total, NOT from `[response].max_bytes`. A future
    // reader must NOT "fix" this by routing through `byte_budget_take`
    // — detect_cycles deliberately has no byte budget threaded in.
    // `resolved_offset` and `emitted` are both `u32` bounded by the
    // 1000-clamped limit and a graph that fits in memory, so the sum
    // cannot overflow; the `as u32` cast matches the handler's existing
    // count-cast idiom.
    let emitted = results.len() as u32;
    let truncated = (resolved_offset + emitted) < total;
    let next_offset = if truncated {
        Some(resolved_offset + emitted)
    } else {
        None
    };

    let response = Page::<Cycle> {
        results,
        total,
        offset: resolved_offset,
        limit: resolved_limit,
        truncated,
        next_offset,
    };
    tool_success_json(&response)
}

// ----- get_orphans -----

/// `get_orphans` body. `kind = None` defaults to callables (Function and
/// Method). `kind = Some("class")` etc. parses through [`parse_kind`].
/// Unknown kind strings return `"invalid kind: <kind>"` in line with
/// `search_symbols`.
///
/// Output is the shared [`Page`]`<`[`SymbolResult`]`>` envelope — the full
/// match set is collected from `Graph::orphans`, sorted by `symbol_id`
/// ascending for stable pagination across calls, then sliced by the
/// resolved offset/limit. `total` reports the pre-pagination match count
/// so clients can render "page X of Y" UIs.
///
/// Defaults: `limit = 20`, `offset = 0`, `brief = true`. `limit = 0`
/// means "use the default" (mirrors `search_symbols`); `limit` is
/// silently clamped at 1000. `offset >= total` returns an empty `results`
/// page with the correct `total`.
///
/// When `count_only = true` (Phase 3 of `PaginatedResponseSizeSafety`),
/// the handler returns the sentinel response shape `Page { results: [],
/// total, offset: 0, limit: 0, truncated: false, next_offset: None }`
/// without ever materializing `SymbolResult`s or invoking the byte-budget
/// helper. `total` reflects the true pre-pagination match count after the
/// kind filter. See plan Decision 9 for why `limit: 0` is a deliberate
/// exception to the "envelope echoes resolved limit" contract.
pub fn get_orphans(
    graph: &RwLock<Graph>,
    kind: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
    brief: Option<bool>,
    count_only: bool,
    max_bytes: usize,
) -> CallToolResult {
    let parsed_kind: Option<SymbolKind> =
        match kind.and_then(|s| if s.is_empty() { None } else { Some(s) }) {
            None => None,
            Some(s) => match parse_kind(s) {
                Some(k) => Some(k),
                None => return tool_error(format!("invalid kind: {s}")),
            },
        };

    // Count-only short-circuit (Phase 3.2 of PaginatedResponseSizeSafety):
    // compute `total` via the cheap path (filter + count) and emit the
    // sentinel envelope WITHOUT materializing SymbolResults or invoking
    // `byte_budget_take`. Order is load-bearing — must precede the
    // materialization step below so the byte-budget cost is never paid.
    if count_only {
        let total = graph.read().orphans(parsed_kind).len() as u32;
        // `limit: 0` is a deliberate exception to the
        // "envelope echoes resolved limit" contract — see plan Decision 9.
        // count_only callers opted out of paging; echoing a would-have-been
        // limit would mislead them into thinking there's a record page to
        // fetch. The exception is documented in CLAUDE.md alongside the
        // count_only sub-block (Phase 4.2).
        let response = Page::<SymbolResult> {
            results: vec![],
            total,
            offset: 0,
            limit: 0,
            truncated: false,
            next_offset: None,
        };
        return tool_success_json(&response);
    }

    // Resolve defaults: zero-or-missing limit -> 20; clamp at 1000.
    let resolved_limit = limit.filter(|&n| n != 0).unwrap_or(20).min(1000);
    let resolved_offset = offset.unwrap_or(0);
    let resolved_brief = brief.unwrap_or(true);

    let mut matches = graph.read().orphans(parsed_kind);
    let total = matches.len() as u32;

    // Sort by symbol_id ascending so page 1 + page 2 partition the result
    // deterministically across calls. Graph::orphans walks a HashMap and
    // returns symbols in non-deterministic order; symbol_id is unique by
    // construction, so this canonicalizes the sequence without needing
    // tie-break rules.
    matches.sort_by_key(symbol_id);

    // Materialize to SymbolResult first, then route through byte_budget_take
    // (Phase 2 of PaginatedResponseSizeSafety): the helper internally applies
    // offset+limit skip/take and stops early if the running serialized byte
    // count would exceed `max_bytes - ENVELOPE_OVERHEAD_BYTES`. `total_kept`
    // from the helper is `results.len() as u32`, NOT the pre-pagination match
    // count — that's `total` captured above and held unchanged.
    let (results, _total_kept, truncated, next_offset) = byte_budget_take(
        matches
            .into_iter()
            .map(|s| symbol_to_result(&s, resolved_brief)),
        resolved_offset,
        resolved_limit,
        max_bytes,
    );

    let response = Page::<SymbolResult> {
        results,
        total,
        offset: resolved_offset,
        limit: resolved_limit,
        truncated,
        next_offset,
    };
    tool_success_json(&response)
}

// ----- get_class_hierarchy -----

/// Wire-format envelope for `get_class_hierarchy`. Tree-shaped tool, so
/// the wrapper carries `max_nodes` budget metadata instead of the
/// list-shaped `Page<T>`'s `total/offset/limit`. Field-declaration order
/// — `hierarchy`, `truncated`, `max_nodes`, `total_nodes_seen` — is the
/// JSON wire-format contract; reordering is a breaking change. Insta
/// alphabetizes keys before snapshotting, so the snapshot files do not
/// preserve declaration order — the struct is the source of truth.
///
/// `total_nodes_seen` is the count of *unique* class names actually
/// walked; equal to `max_nodes` when truncation occurred, less when the
/// hierarchy fit. Combined with `truncated`, agents can decide whether
/// to retry with a larger budget.
#[derive(Debug, Serialize)]
struct ClassHierarchyResponse {
    hierarchy: HierarchyNode,
    truncated: bool,
    max_nodes: u32,
    total_nodes_seen: u32,
}

/// `get_class_hierarchy` body. Required `class` string; optional `depth`
/// (default 1) and `max_nodes` (default 250, clamped at 1000; `0` is
/// treated as "use default"). Unknown class produces a did-you-mean
/// message filtered to class-like kinds (`Class`, `Struct`, `Interface`,
/// `Trait`).
///
/// The did-you-mean wording mirrors the symbol_detail / callers
/// patterns in 3.4: `class not found: "<name>". Did you mean: a, b, c?`
/// when suggestions exist; otherwise just `class not found: "<name>"`.
///
/// On success, returns the [`ClassHierarchyResponse`] envelope:
/// `{hierarchy, truncated, max_nodes, total_nodes_seen}`. The Graph
/// layer's unique-name budget guarantees diamond inheritance doesn't
/// burn the budget twice for shared ancestors — see
/// `Graph::class_hierarchy`.
pub fn get_class_hierarchy(
    graph: &RwLock<Graph>,
    class: &str,
    depth: Option<u32>,
    max_nodes: Option<u32>,
) -> CallToolResult {
    if class.is_empty() {
        return tool_error("'class' is required");
    }

    let depth = depth.filter(|&d| d > 0).unwrap_or(1);
    // Resolve max_nodes: zero-or-missing -> default 250; clamp at 1000.
    // Matches the Phase 2/3 pagination convention for limit resolution.
    let resolved_max_nodes = max_nodes.filter(|&n| n != 0).unwrap_or(250).min(1000);

    let g = graph.read();
    if let Some((hierarchy, total_nodes_seen, truncated)) =
        g.class_hierarchy(class, depth, resolved_max_nodes)
    {
        let response = ClassHierarchyResponse {
            hierarchy,
            truncated,
            max_nodes: resolved_max_nodes,
            total_nodes_seen,
        };
        return tool_success_json(&response);
    }
    let class_like = suggest_class_symbols(&g, class, 5);
    drop(g);

    if class_like.is_empty() {
        tool_error(format!("class not found: {class:?}"))
    } else {
        let suggestions = class_like.join(", ");
        tool_error(format!(
            "class not found: {class:?}. Did you mean: {suggestions}?"
        ))
    }
}

/// Did-you-mean helper for class-like lookups. Filters the candidate pool
/// to `{Class, Struct, Interface, Trait}` so a Function named "FooBar"
/// never appears as a suggestion for `class_hierarchy("Foo")`. Deliberately
/// does NOT reuse `suggest_symbols` from `mod.rs` because that helper is
/// kind-agnostic.
fn suggest_class_symbols(graph: &Graph, name: &str, limit: usize) -> Vec<String> {
    graph
        .search_symbols(name, None)
        .into_iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Class | SymbolKind::Struct | SymbolKind::Interface | SymbolKind::Trait
            )
        })
        .take(limit)
        .map(|s| s.name)
        .collect()
}

// ----- get_coupling -----

/// Fixed reserve, in bytes, for the [`CouplingBoth`] outer wrapper
/// (`{"incoming":<page>,"outgoing":<page>}`) when sizing the `both`
/// response. The literal wrapper text outside the two nested pages is
/// `{"incoming":,"outgoing":}` = 24 bytes; this rounds up to a
/// conservative 48 so the envelope can never exceed `max_bytes` even
/// after the incoming page is serialized at its full byte cost.
/// Under-estimating here risks an over-budget envelope, so the slack is
/// deliberate.
const COUPLING_BOTH_WRAPPER_OVERHEAD: usize = 48;

/// Sort coupling rows by `count` descending, then `file` ascending. The
/// secondary file-ascending key makes pagination deterministic across
/// calls when several files share the same edge count (the underlying
/// `Graph::coupling` walks a `HashMap`, so insertion order is not
/// stable). Page 1 + page 2 partition the result deterministically.
fn sort_coupling_rows(rows: &mut [CouplingEntry]) {
    rows.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.file.cmp(&b.file)));
}

/// Map a raw `HashMap<PathBuf, u32>` of coupling counts into sorted
/// [`CouplingEntry`] rows. Path keys are stringified via
/// `to_string_lossy` for stable cross-platform JSON (`PathBuf`
/// serializes through `OsStr`, which on Windows can hold a non-UTF-8
/// surrogate) — same pattern as `detect_cycles`.
fn coupling_rows(counts: HashMap<PathBuf, u32>) -> Vec<CouplingEntry> {
    let mut rows: Vec<CouplingEntry> = counts
        .into_iter()
        .map(|(path, count)| CouplingEntry {
            file: path.to_string_lossy().into_owned(),
            count,
        })
        .collect();
    sort_coupling_rows(&mut rows);
    rows
}

/// `get_coupling` body. Required `file` string; optional `direction` in
/// `{outgoing(default), incoming, both}`; optional `offset`/`limit`
/// pagination.
///
/// `incoming` / `outgoing` return a [`Page<CouplingEntry>`] — rows sorted
/// by `count` descending then `file` ascending, then sliced+byte-budgeted
/// via `byte_budget_take`. `both` returns a [`CouplingBoth`] carrying two
/// independently-paginated pages; the budget is allocated sequentially
/// (incoming first against the full `max_bytes`, outgoing against the
/// remainder after the incoming page plus a fixed wrapper overhead).
/// When incoming exhausts the budget, outgoing is an empty page flagged
/// `truncated: true` with `next_offset: Some(0)` so a client can
/// re-request the outgoing side fresh via `direction=outgoing offset=0`.
///
/// Defaults: `limit = 50` per side (zero-or-missing resolves to the
/// default; mirrors `get_orphans` / `search_symbols`), clamped at 1000;
/// `offset = 0`. An unknown file is not an error — it yields empty
/// page(s), matching the previous empty-object contract.
///
/// Unknown direction returns
/// `"invalid direction: <direction>. Expected one of: outgoing, incoming, both"`
/// — this is a deliberate divergence from the Go wording
/// `"'direction' must be 'incoming', 'outgoing', or 'both'"`. The Rust
/// form matches the `invalid kind: <kind>` and `invalid format: <fmt>`
/// shapes used elsewhere in the handler suite (and `generate_diagram`'s
/// own `invalid direction:` message), and includes the bad value
/// verbatim so users can self-correct.
pub fn get_coupling(
    graph: &RwLock<Graph>,
    file: &str,
    direction: Option<&str>,
    offset: Option<u32>,
    limit: Option<u32>,
    max_bytes: usize,
) -> CallToolResult {
    if file.is_empty() {
        return tool_error("'file' is required");
    }

    // Resolve direction up front so an invalid spelling errors before any
    // graph work. Empty / absent resolves to "outgoing". Accepted
    // spellings and the error wording mirror `generate_diagram`'s
    // direction validation idiom.
    let direction = match direction.unwrap_or("") {
        "" | "outgoing" => "outgoing",
        "incoming" => "incoming",
        "both" => "both",
        other => {
            return tool_error(format!(
                "invalid direction: {other}. Expected one of: outgoing, incoming, both"
            ));
        }
    };

    // Resolve defaults: zero-or-missing limit -> 50; clamp at 1000.
    // Matches the pagination convention used by `get_orphans` /
    // `search_symbols` for limit resolution.
    let resolved_limit = limit.filter(|&n| n != 0).unwrap_or(50).min(1000);
    let resolved_offset = offset.unwrap_or(0);

    // PathNormalization Phase 3.2: normalize the user-supplied `file` argument
    // before graph lookup. Mirrors `get_file_symbols` (Phase 3.1): canonical
    // form when the path exists on disk (resolving `.` / `..` and stripping
    // the Windows `\\?\` extended-path prefix), lexical fallback otherwise.
    // On Linux with an already-canonical path this is effectively identity.
    let path = paths::normalize_user_path(file);

    if direction == "both" {
        // Sequential budget allocation. Incoming is sized first against
        // the full `max_bytes`. The incoming page's serialized cost plus
        // a conservative fixed wrapper overhead is then subtracted from
        // `max_bytes` (floored at 0) and the remainder is passed to a
        // second `byte_budget_take` for outgoing. This guarantees the
        // `{"incoming":<page>,"outgoing":<page>}` envelope stays within
        // `max_bytes` even when incoming is large.
        let g = graph.read();
        let incoming_rows = coupling_rows(g.incoming_coupling(&path));
        let outgoing_rows = coupling_rows(g.coupling(&path));
        drop(g);

        let incoming_total = incoming_rows.len() as u32;
        let outgoing_total = outgoing_rows.len() as u32;

        let (in_results, _in_kept, in_truncated, in_next) =
            byte_budget_take(incoming_rows, resolved_offset, resolved_limit, max_bytes);
        let incoming = Page::<CouplingEntry> {
            results: in_results,
            total: incoming_total,
            offset: resolved_offset,
            limit: resolved_limit,
            truncated: in_truncated,
            next_offset: in_next,
        };

        // Bytes already spent by the serialized incoming page, plus the
        // fixed outer-wrapper reserve. `to_string` on plain owned data is
        // infallible in practice; on the unreachable failure path fall
        // back to the full budget so `remaining` saturates to 0 and the
        // outgoing side is starved rather than handed a budget that
        // could overflow `max_bytes` (a `0` fallback would do the
        // opposite — the conservative direction is "assume incoming
        // consumed everything").
        let incoming_bytes = serde_json::to_string(&incoming)
            .map(|s| s.len())
            .unwrap_or(max_bytes);
        let remaining = max_bytes
            .saturating_sub(incoming_bytes)
            .saturating_sub(COUPLING_BOTH_WRAPPER_OVERHEAD);

        let outgoing = if remaining == 0 {
            // Incoming ate the whole budget. Emit an empty outgoing page
            // flagged truncated with `next_offset: Some(0)` — the
            // start-fresh marker telling the client to re-call with
            // `direction=outgoing offset=0`.
            Page::<CouplingEntry> {
                results: vec![],
                total: outgoing_total,
                offset: resolved_offset,
                limit: resolved_limit,
                truncated: true,
                next_offset: Some(0),
            }
        } else {
            let (out_results, _out_kept, out_truncated, out_next) =
                byte_budget_take(outgoing_rows, resolved_offset, resolved_limit, remaining);
            Page::<CouplingEntry> {
                results: out_results,
                total: outgoing_total,
                offset: resolved_offset,
                limit: resolved_limit,
                truncated: out_truncated,
                next_offset: out_next,
            }
        };

        return tool_success_json(&CouplingBoth { incoming, outgoing });
    }

    let rows = {
        let g = graph.read();
        let counts = if direction == "incoming" {
            g.incoming_coupling(&path)
        } else {
            g.coupling(&path)
        };
        drop(g);
        coupling_rows(counts)
    };
    let total = rows.len() as u32;

    let (results, _kept, truncated, next_offset) =
        byte_budget_take(rows, resolved_offset, resolved_limit, max_bytes);

    let response = Page::<CouplingEntry> {
        results,
        total,
        offset: resolved_offset,
        limit: resolved_limit,
        truncated,
        next_offset,
    };
    tool_success_json(&response)
}

// ----- generate_diagram -----

/// Inputs to [`generate_diagram`]. Bundled into a struct so the handler
/// signature stays under clippy's `too_many_arguments` threshold without
/// reaching for an `allow` attribute (same pattern as `SearchSymbolsInput`).
#[derive(Debug, Default)]
pub struct GenerateDiagramInput<'a> {
    pub symbol: Option<&'a str>,
    pub file: Option<&'a str>,
    pub class: Option<&'a str>,
    pub depth: Option<u32>,
    pub max_nodes: Option<u32>,
    pub format: Option<&'a str>,
    pub styled: bool,
    /// Which arms of the call graph the `symbol` mode walks, as the raw
    /// wire spelling (`"callees"` / `"callers"` / `"both"`). Absent or
    /// empty resolves to both arms so callers predating the direction
    /// filter keep the original behavior. An unrecognized spelling is a
    /// handler-level error. Ignored by the `file` and `class` modes.
    pub direction: Option<&'a str>,
}

/// `generate_diagram` body. Dispatches on the exclusive parameter
/// (`symbol` | `file` | `class`) to the matching `Graph::diagram_*`
/// method, then formats the result as either JSON edges or a Mermaid
/// flowchart.
///
/// **Direction**: hardcoded to `"TD"` for all three diagram types. The
/// Go reference uses `"BT"` for inheritance and `"TD"` otherwise; the
/// Rust port unifies on `"TD"` per the task brief. This is a Rust-idiom
/// divergence — having a single direction makes diagrams visually
/// consistent regardless of which view a user requested. The snapshot
/// suite in 3.7 will lock this in.
///
/// **Exactly-one-of**: when 0 or >1 of `symbol`/`file`/`class` are set,
/// returns an error. The Go reference accepted multiple parameters and
/// silently picked one by precedence (class > symbol > file); the Rust
/// port rejects ambiguous calls so silent precedence ambiguity can't
/// produce surprising results.
///
/// Empty edges in `edges` format serialize as `[]` (never `null`) —
/// `DiagramResult::edges` is a `Vec`, not `Option`, so this falls out
/// of the type system.
pub fn generate_diagram(graph: &RwLock<Graph>, input: GenerateDiagramInput<'_>) -> CallToolResult {
    // Exactly-one-of validation. Empty strings count as absent so a
    // client passing `{"symbol": ""}` doesn't pass the check.
    let symbol = input.symbol.filter(|s| !s.is_empty());
    let file = input.file.filter(|s| !s.is_empty());
    let class = input.class.filter(|s| !s.is_empty());
    let count =
        usize::from(symbol.is_some()) + usize::from(file.is_some()) + usize::from(class.is_some());
    if count != 1 {
        return tool_error("exactly one of 'symbol', 'file', or 'class' is required");
    }

    let depth = input.depth.filter(|&d| d > 0).unwrap_or(1);
    let max_nodes = input.max_nodes.filter(|&m| m > 0).unwrap_or(30);

    // Only the `symbol` (call graph) branch consults `direction`, so it is
    // resolved and validated solely for that mode — a `direction` passed
    // alongside `file=`/`class=` is ignored rather than rejected, keeping
    // the "symbol mode only" contract honest. Absent or empty means "both
    // arms" so callers predating the direction filter keep the original
    // who-calls-X-and-who-X-calls behavior. Accepted spellings mirror the
    // serde renames on `DiagramDirection`.
    let direction = if symbol.is_some() {
        match input.direction.unwrap_or("") {
            "" | "both" => DiagramDirection::Both,
            "callees" => DiagramDirection::Callees,
            "callers" => DiagramDirection::Callers,
            other => {
                return tool_error(format!(
                    "invalid direction: {other}. Expected one of: callees, callers, both"
                ));
            }
        }
    } else {
        // Unused by the file/class branches; the value never reaches a
        // traversal so the choice is irrelevant.
        DiagramDirection::Both
    };

    let format = input.format.unwrap_or("");
    let format = if format.is_empty() { "edges" } else { format };

    // Validate format up front so an invalid format with valid dispatch
    // params still produces the format error (not a not-found from the
    // graph lookup).
    if format != "edges" && format != "mermaid" {
        return tool_error(format!(
            "invalid format: {format}. Expected 'edges' or 'mermaid'"
        ));
    }

    let g = graph.read();
    let dr_opt = if let Some(id) = symbol {
        g.diagram_call_graph(id, direction, depth, max_nodes)
    } else if let Some(path) = file {
        // PathNormalization Phase 3.2: same normalize wrap as `get_coupling`
        // and `get_file_symbols`. Only the file-mode branch needs it — the
        // `symbol` and `class` branches take symbol IDs, not file paths.
        let normalized = paths::normalize_user_path(path);
        g.diagram_file_graph(&normalized, depth, max_nodes)
    } else if let Some(name) = class {
        g.diagram_inheritance(name, depth, max_nodes)
    } else {
        // Unreachable: the exactly-one-of check above guarantees one is
        // Some. `unreachable!()` documents the invariant; if a future
        // edit weakens the check, the panic surfaces in tests.
        unreachable!("exactly-one-of validation guarantees one branch is taken");
    };

    let dr = match dr_opt {
        Some(d) => d,
        None => {
            // Did-you-mean for symbol/class on miss; bare not-found
            // for file (no useful suggestion source for filenames).
            if let Some(id) = symbol {
                let suggestions = suggest_symbols(&g, id, 5);
                drop(g);
                return if suggestions.is_empty() {
                    tool_error(format!("symbol not found: {id:?}"))
                } else {
                    tool_error(format!(
                        "symbol not found: {id:?}. Did you mean: {suggestions}?"
                    ))
                };
            }
            if let Some(name) = class {
                let class_like = suggest_class_symbols(&g, name, 5);
                drop(g);
                return if class_like.is_empty() {
                    tool_error(format!("class not found: {name:?}"))
                } else {
                    let suggestions = class_like.join(", ");
                    tool_error(format!(
                        "class not found: {name:?}. Did you mean: {suggestions}?"
                    ))
                };
            }
            // file branch: no did-you-mean.
            let path = file.expect("exactly-one-of guarantees file is Some on this branch");
            drop(g);
            return tool_error(format!("file not found: {path:?}"));
        }
    };
    drop(g);

    match format {
        "edges" => {
            // DiagramResult.edges is already Vec<DiagramEdge>; serialize directly.
            let edges: &Vec<DiagramEdge> = &dr.edges;
            tool_success_json(edges)
        }
        "mermaid" => {
            // Hardcode "TD" — see fn-level doc comment for rationale.
            let rendered = dr.render_mermaid("TD", input.styled);
            CallToolResult::success(vec![Content::text(rendered)])
        }
        _ => unreachable!("format validation rejects everything else above"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::{body_text, page_parts};
    use super::super::NO_BYTE_BUDGET;
    use super::*;
    use code_graph_core::{Edge, EdgeKind, FileGraph, Language, Symbol, SymbolKind};

    fn sym(name: &str, kind: SymbolKind, file: &str) -> Symbol {
        sym_full(name, kind, file, "")
    }

    fn sym_full(name: &str, kind: SymbolKind, file: &str, parent: &str) -> Symbol {
        Symbol {
            name: name.to_string(),
            kind,
            file: file.to_string(),
            line: 1,
            column: 0,
            end_line: 1,
            signature: format!("sig {name}"),
            namespace: String::new(),
            parent: parent.to_string(),
            language: Language::Cpp,
        }
    }

    fn call_edge(from: &str, to: &str, file: &str) -> Edge {
        Edge {
            from: from.to_string(),
            to: to.to_string(),
            kind: EdgeKind::Calls,
            file: file.to_string(),
            line: 1,
        }
    }

    fn include_edge(from: &str, to: &str) -> Edge {
        Edge {
            from: from.to_string(),
            to: to.to_string(),
            kind: EdgeKind::Includes,
            file: from.to_string(),
            line: 1,
        }
    }

    fn inherit_edge(from: &str, to: &str, file: &str) -> Edge {
        Edge {
            from: from.to_string(),
            to: to.to_string(),
            kind: EdgeKind::Inherits,
            file: file.to_string(),
            line: 0,
        }
    }

    fn locked(g: Graph) -> RwLock<Graph> {
        RwLock::new(g)
    }

    // --- detect_cycles ---

    /// Build a graph with `n` independent 2-node cycles: each pair
    /// `cycle_NNN_a.h <-> cycle_NNN_b.h` includes the other but no
    /// other file. Used by the pagination tests to assert page-1+page-2
    /// partitioning across a known cycle count.
    fn graph_with_n_cycles(n: usize) -> Graph {
        let mut g = Graph::new();
        for i in 0..n {
            let a = format!("/cycle_{i:03}_a.h");
            let b = format!("/cycle_{i:03}_b.h");
            g.merge_file_graph(FileGraph {
                path: a.clone(),
                language: Language::Cpp,
                symbols: vec![],
                edges: vec![include_edge(&a, &b)],
            });
            g.merge_file_graph(FileGraph {
                path: b.clone(),
                language: Language::Cpp,
                symbols: vec![],
                edges: vec![include_edge(&b, &a)],
            });
        }
        g
    }

    #[test]
    fn detect_cycles_empty_graph_returns_empty_envelope() {
        let g = locked(Graph::new());
        let r = detect_cycles(&g, None, None);
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        let (arr, total, offset, limit) = page_parts(&r);
        assert!(arr.is_empty());
        assert_eq!(total, 0);
        assert_eq!(offset, 0);
        assert_eq!(limit, 20);
    }

    #[test]
    fn detect_cycles_acyclic_graph_returns_empty_envelope() {
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/a.h".to_string(),
            language: Language::Cpp,
            symbols: vec![],
            edges: vec![include_edge("/a.h", "/b.h")],
        });
        g.merge_file_graph(FileGraph {
            path: "/b.h".to_string(),
            language: Language::Cpp,
            symbols: vec![],
            edges: vec![],
        });
        let g = locked(g);
        let r = detect_cycles(&g, None, None);
        let (arr, total, _, _) = page_parts(&r);
        assert!(arr.is_empty());
        assert_eq!(total, 0);
    }

    #[test]
    fn detect_cycles_two_node_cycle_returns_envelope_with_one_cycle() {
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/a.h".to_string(),
            language: Language::Cpp,
            symbols: vec![],
            edges: vec![include_edge("/a.h", "/b.h")],
        });
        g.merge_file_graph(FileGraph {
            path: "/b.h".to_string(),
            language: Language::Cpp,
            symbols: vec![],
            edges: vec![include_edge("/b.h", "/a.h")],
        });
        let g = locked(g);
        let r = detect_cycles(&g, None, None);
        let (arr, total, _, _) = page_parts(&r);
        assert_eq!(arr.len(), 1, "exactly one cycle in results");
        assert_eq!(total, 1, "total reports the full cycle count");
        let cycle = arr[0]["files"].as_array().unwrap();
        assert_eq!(cycle.len(), 2);
        // Inner cycle paths sorted in canonical order, no need to sort here.
        let names: Vec<&str> = cycle.iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(names, vec!["/a.h", "/b.h"]);
        // Each cycle is now a {files, truncated} object; an untruncated
        // cycle emits truncated:false and omits original_len.
        assert_eq!(arr[0]["truncated"], serde_json::json!(false));
        assert!(
            arr[0].as_object().unwrap().get("original_len").is_none(),
            "original_len absent when the cycle is not truncated"
        );
    }

    #[test]
    fn detect_cycles_default_limit_is_20() {
        let g = locked(graph_with_n_cycles(25));
        let r = detect_cycles(&g, None, None);
        let (arr, total, _, limit) = page_parts(&r);
        assert_eq!(arr.len(), 20);
        assert_eq!(total, 25);
        assert_eq!(limit, 20);
        // Default-limited first page of a 25-cycle graph is partial:
        // the envelope must report more pages remain (0+20 < 25).
        let (truncated, next_offset) = super::super::test_helpers::page_extras(&r);
        assert!(truncated, "20-of-25 first page must be truncated");
        assert_eq!(next_offset, Some(20));
    }

    #[test]
    fn detect_cycles_page_1_and_page_2_cover_full_set_no_overlap() {
        let g = locked(graph_with_n_cycles(30));
        let r1 = detect_cycles(&g, Some(20), Some(0));
        let (arr1, total1, _, _) = page_parts(&r1);
        let r2 = detect_cycles(&g, Some(20), Some(20));
        let (arr2, total2, _, _) = page_parts(&r2);
        assert_eq!(total1, 30);
        assert_eq!(total2, 30, "total invariant across pages");
        assert_eq!(arr1.len(), 20);
        assert_eq!(arr2.len(), 10);
        // Outer sort is by each cycle's first path; concat must produce no
        // duplicates and span the full 30-cycle set.
        let mut all_first_paths: Vec<String> = arr1
            .iter()
            .chain(arr2.iter())
            .map(|c| {
                c["files"].as_array().unwrap()[0]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        let len_before_dedup = all_first_paths.len();
        all_first_paths.sort();
        all_first_paths.dedup();
        assert_eq!(
            all_first_paths.len(),
            len_before_dedup,
            "no overlap between pages"
        );
        assert_eq!(all_first_paths.len(), 30, "pages cover the full cycle set");

        // The envelope must now tell the truth: page 1 stopped short of
        // the 30-cycle total, so truncated=true and next_offset points
        // at the start of page 2. Page 2 is the natural tail, so it must
        // report truncated=false / next_offset=None.
        let (t1, n1) = super::super::test_helpers::page_extras(&r1);
        assert!(t1, "page 1 of a 30-cycle set with limit 20 is truncated");
        assert_eq!(n1, Some(20), "next_offset resumes exactly at page 2");
        let (t2, n2) = super::super::test_helpers::page_extras(&r2);
        assert!(!t2, "page 2 reaches the end of the cycle set");
        assert_eq!(n2, None, "no further page after the tail");
    }

    #[test]
    fn detect_cycles_partial_first_page_envelope_reports_truncated_with_next_offset() {
        // 100 cycles, limit 10, offset 0: the page is a strict prefix of
        // the full set, so the envelope must advertise that more cycles
        // exist (truncated=true) and where to resume (next_offset=10).
        let g = locked(graph_with_n_cycles(100));
        let r = detect_cycles(&g, Some(10), Some(0));
        let (arr, total, offset, limit) = page_parts(&r);
        assert_eq!(arr.len(), 10, "limit caps the page at 10 cycles");
        assert_eq!(total, 100, "total is the pre-pagination cycle count");
        assert_eq!(offset, 0);
        assert_eq!(limit, 10);
        let (truncated, next_offset) = super::super::test_helpers::page_extras(&r);
        assert!(truncated, "90 cycles remain past this page");
        assert_eq!(next_offset, Some(10), "client resumes at offset 10");
    }

    #[test]
    fn detect_cycles_final_partial_page_envelope_reports_not_truncated() {
        // Same 100-cycle fixture, offset 95: only 5 cycles remain, fewer
        // than the limit. offset(95) + emitted(5) == total(100), so this
        // is the natural tail — truncated=false, next_offset=None.
        let g = locked(graph_with_n_cycles(100));
        let r = detect_cycles(&g, Some(10), Some(95));
        let (arr, total, offset, _) = page_parts(&r);
        assert_eq!(arr.len(), 5, "only the trailing 5 cycles remain");
        assert_eq!(total, 100);
        assert_eq!(offset, 95);
        let (truncated, next_offset) = super::super::test_helpers::page_extras(&r);
        assert!(!truncated, "no cycles remain past the final page");
        assert_eq!(next_offset, None, "tail page has no resume offset");
    }

    #[test]
    fn detect_cycles_offset_beyond_total_returns_empty_envelope() {
        let g = locked(graph_with_n_cycles(3));
        let r = detect_cycles(&g, None, Some(999));
        let (arr, total, offset, _) = page_parts(&r);
        assert!(arr.is_empty());
        assert_eq!(total, 3, "total still reports full cycle count");
        assert_eq!(offset, 999);
        // Over-offset empty page is the natural end of the set, not a
        // truncated page: no further page exists to fetch.
        let (truncated, next_offset) = super::super::test_helpers::page_extras(&r);
        assert!(!truncated, "over-offset empty page must not be truncated");
        assert_eq!(next_offset, None);
    }

    #[test]
    fn detect_cycles_limit_clamps_at_1000() {
        let g = locked(graph_with_n_cycles(3));
        let r = detect_cycles(&g, Some(999_999), None);
        let (arr, _, _, limit) = page_parts(&r);
        assert_eq!(limit, 1000, "echo the clamped limit");
        assert_eq!(arr.len(), 3, "all 3 cycles returned when data < cap");
    }

    #[test]
    fn detect_cycles_zero_limit_uses_default() {
        let g = locked(graph_with_n_cycles(3));
        let r = detect_cycles(&g, Some(0), None);
        let (_, _, _, limit) = page_parts(&r);
        assert_eq!(limit, 20);
    }

    #[test]
    fn untruncated_cycle_serializes_with_files_and_truncated_only() {
        // A non-truncated cycle carries files and an explicit
        // truncated:false (always emitted, mirroring the Page envelope's
        // always-present truncated bool); original_len is absent because
        // it is None. The exact byte shape is a wire-format contract.
        let c = Cycle {
            files: vec!["a".into(), "b".into()],
            truncated: false,
            original_len: None,
        };
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, r#"{"files":["a","b"],"truncated":false}"#);
    }

    // --- get_orphans ---

    fn graph_with_orphans() -> Graph {
        // foo calls bar; baz is uncalled (orphan); cls is a class with no callers.
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/x.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![
                sym("foo", SymbolKind::Function, "/x.cpp"),
                sym("bar", SymbolKind::Function, "/x.cpp"),
                sym("baz", SymbolKind::Function, "/x.cpp"),
                sym("cls", SymbolKind::Class, "/x.cpp"),
            ],
            edges: vec![call_edge("/x.cpp:foo", "/x.cpp:bar", "/x.cpp")],
        });
        g
    }

    #[test]
    fn orphans_default_returns_callables() {
        let g = locked(graph_with_orphans());
        let r = get_orphans(&g, None, None, None, None, false, NO_BYTE_BUDGET);
        let (arr, total, offset, limit) = page_parts(&r);
        // foo and baz have no callers; bar is called by foo. cls is a Class
        // and is excluded by the default callable-only filter.
        let names: Vec<&str> = arr.iter().map(|e| e["name"].as_str().unwrap()).collect();
        assert_eq!(arr.len(), 2, "got {names:?}");
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"baz"));
        assert!(!names.contains(&"bar"));
        assert!(!names.contains(&"cls"));
        assert_eq!(total, 2);
        assert_eq!(offset, 0);
        assert_eq!(limit, 20);
    }

    #[test]
    fn orphans_kind_class_returns_only_classes() {
        let g = locked(graph_with_orphans());
        let r = get_orphans(&g, Some("class"), None, None, None, false, NO_BYTE_BUDGET);
        let (arr, total, _, _) = page_parts(&r);
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], serde_json::json!("cls"));
        assert_eq!(arr[0]["kind"], serde_json::json!("class"));
        assert_eq!(total, 1);
    }

    #[test]
    fn orphans_invalid_kind_errors() {
        let g = locked(Graph::new());
        let r = get_orphans(&g, Some("widget"), None, None, None, false, NO_BYTE_BUDGET);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "invalid kind: widget");
    }

    #[test]
    fn orphans_empty_graph_returns_empty_envelope() {
        let g = locked(Graph::new());
        let r = get_orphans(&g, None, None, None, None, false, NO_BYTE_BUDGET);
        let (arr, total, offset, limit) = page_parts(&r);
        assert!(arr.is_empty());
        assert_eq!(total, 0);
        assert_eq!(offset, 0);
        assert_eq!(limit, 20);
    }

    #[test]
    fn orphans_empty_string_kind_treated_as_default() {
        // A client passing `kind=""` should behave the same as omitting
        // kind — Go's `req.GetArguments()["kind"].(string)` ignores empty
        // strings via the `&& k != ""` check.
        let g = locked(graph_with_orphans());
        let r = get_orphans(&g, Some(""), None, None, None, false, NO_BYTE_BUDGET);
        let (arr, _, _, _) = page_parts(&r);
        assert_eq!(arr.len(), 2, "empty kind => default callables-only");
    }

    #[test]
    fn orphans_brief_mode_omits_signature() {
        // Output is brief by default — assert signature is dropped from
        // the serialized form even though our test fixture has a non-empty
        // signature on each symbol.
        let g = locked(graph_with_orphans());
        let r = get_orphans(&g, None, None, None, None, false, NO_BYTE_BUDGET);
        let (arr, _, _, _) = page_parts(&r);
        for entry in arr {
            assert!(
                entry.get("signature").is_none(),
                "brief output must omit signature: {entry:?}",
            );
        }
    }

    // --- Phase 2 pagination invariants ------------------------------------

    /// Build a graph with exactly `n` orphan functions named `func_000`,
    /// `func_001`, ..., zero-padded to 3 digits so the natural sort order
    /// (`symbol_id` ascending) is predictable for assertions. All symbols
    /// live in `/big.cpp` so the symbol_id format is `[/big.cpp:func_000`,
    /// `/big.cpp:func_001`, ...]`.
    fn graph_with_n_orphan_functions(n: usize) -> Graph {
        let mut g = Graph::new();
        let mut symbols: Vec<Symbol> = Vec::with_capacity(n);
        for i in 0..n {
            symbols.push(sym(
                &format!("func_{i:03}"),
                SymbolKind::Function,
                "/big.cpp",
            ));
        }
        g.merge_file_graph(FileGraph {
            path: "/big.cpp".to_string(),
            language: Language::Cpp,
            symbols,
            edges: vec![],
        });
        g
    }

    #[test]
    fn orphans_default_limit_is_20() {
        // 25 orphans: default limit (20) returns the first 20; total = 25.
        let g = locked(graph_with_n_orphan_functions(25));
        let r = get_orphans(&g, None, None, None, None, false, NO_BYTE_BUDGET);
        let (arr, total, offset, limit) = page_parts(&r);
        assert_eq!(arr.len(), 20);
        assert_eq!(total, 25);
        assert_eq!(offset, 0);
        assert_eq!(limit, 20);
    }

    #[test]
    fn orphans_page_1_and_page_2_cover_full_set() {
        // 30 orphans: page 1 (offset=0, limit=20) ∪ page 2 (offset=20, limit=20)
        // covers all 30 with no overlap.
        let g = locked(graph_with_n_orphan_functions(30));

        let p1 = get_orphans(&g, None, Some(20), Some(0), None, false, NO_BYTE_BUDGET);
        let (a1, t1, _, _) = page_parts(&p1);
        let p2 = get_orphans(&g, None, Some(20), Some(20), None, false, NO_BYTE_BUDGET);
        let (a2, t2, _, _) = page_parts(&p2);

        assert_eq!(a1.len(), 20);
        assert_eq!(a2.len(), 10);
        assert_eq!(t1, 30);
        assert_eq!(t2, 30);

        // Union covers all 30, no duplicates.
        let mut ids: Vec<String> = a1
            .iter()
            .chain(a2.iter())
            .map(|e| e["id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 30, "page1 ∪ page2 must cover all 30 with no dup");
    }

    #[test]
    fn orphans_total_is_pre_pagination_count() {
        // Same fixture, three different pages — total is identical across all.
        let g = locked(graph_with_n_orphan_functions(30));
        let r1 = get_orphans(&g, None, Some(20), Some(0), None, false, NO_BYTE_BUDGET);
        let r2 = get_orphans(&g, None, Some(20), Some(20), None, false, NO_BYTE_BUDGET);
        let r3 = get_orphans(&g, None, Some(5), Some(10), None, false, NO_BYTE_BUDGET);
        let (_, t1, _, _) = page_parts(&r1);
        let (_, t2, _, _) = page_parts(&r2);
        let (_, t3, _, _) = page_parts(&r3);
        assert_eq!(t1, 30);
        assert_eq!(t2, 30);
        assert_eq!(t3, 30);
    }

    #[test]
    fn orphans_limit_clamps_at_1000() {
        // limit = 999_999 silently clamps to 1000; the response echoes the
        // clamped value so the agent sees what was actually used. The
        // 5-item fixture also verifies all 5 results return — confirming
        // take(1000) doesn't accidentally drop entries on a small set.
        let g = locked(graph_with_n_orphan_functions(5));
        let r = get_orphans(&g, None, Some(999_999), None, None, false, NO_BYTE_BUDGET);
        let (arr, _, _, limit) = page_parts(&r);
        assert_eq!(limit, 1000);
        assert_eq!(arr.len(), 5);
    }

    #[test]
    fn orphans_zero_limit_uses_default() {
        // limit = 0 is treated as "unset"; resolves to default 20.
        let g = locked(graph_with_n_orphan_functions(5));
        let r = get_orphans(&g, None, Some(0), None, None, false, NO_BYTE_BUDGET);
        let (_, _, _, limit) = page_parts(&r);
        assert_eq!(limit, 20);
    }

    #[test]
    fn orphans_offset_beyond_total_returns_empty() {
        // offset >= total returns empty results with the correct total.
        let g = locked(graph_with_orphans());
        let r = get_orphans(&g, None, None, Some(999), None, false, NO_BYTE_BUDGET);
        let (arr, total, offset, limit) = page_parts(&r);
        assert!(arr.is_empty());
        assert_eq!(total, 2);
        assert_eq!(offset, 999);
        assert_eq!(limit, 20);
    }

    #[test]
    fn orphans_kind_filter_combined_with_pagination() {
        // Mixed-kind fixture: 12 class orphans + 5 function orphans. With
        // kind="class" and limit=10, we get 10 class entries (all "class"
        // kind) and total=12.
        let mut g = Graph::new();
        let mut symbols: Vec<Symbol> = Vec::new();
        for i in 0..12 {
            symbols.push(sym(&format!("Class_{i:03}"), SymbolKind::Class, "/m.cpp"));
        }
        for i in 0..5 {
            symbols.push(sym(&format!("func_{i:03}"), SymbolKind::Function, "/m.cpp"));
        }
        g.merge_file_graph(FileGraph {
            path: "/m.cpp".to_string(),
            language: Language::Cpp,
            symbols,
            edges: vec![],
        });
        let g = locked(g);
        let r = get_orphans(
            &g,
            Some("class"),
            Some(10),
            None,
            None,
            false,
            NO_BYTE_BUDGET,
        );
        let (arr, total, _, _) = page_parts(&r);
        assert_eq!(arr.len(), 10);
        assert_eq!(total, 12);
        for entry in &arr {
            assert_eq!(entry["kind"], serde_json::json!("class"));
        }
    }

    #[test]
    fn orphans_brief_false_includes_signature() {
        // brief=false surfaces signature/column/end_line on each row.
        let g = locked(graph_with_orphans());
        let r = get_orphans(&g, None, None, None, Some(false), false, NO_BYTE_BUDGET);
        let (arr, _, _, _) = page_parts(&r);
        assert!(!arr.is_empty());
        for entry in &arr {
            assert!(
                entry.get("signature").is_some(),
                "brief=false must include signature: {entry:?}",
            );
        }
    }

    // --- Phase 2 byte-budget invariants -----------------------------------

    #[test]
    fn orphans_byte_budget_truncates_oversized_page() {
        // Phase 2 of PaginatedResponseSizeSafety: a tight `max_bytes` must
        // make `get_orphans` stop emitting records before reaching `limit`,
        // surface `truncated=true`, and report a usable `next_offset`.
        //
        // Fixture: 30 orphan functions named `func_000`..`func_029` in
        // `/big.cpp`. Each serialized SymbolResult in brief mode is ~60-70
        // bytes (`{"id":"/big.cpp:func_NNN","name":"func_NNN","kind":
        // "function","line":1}` plus the helper's +1 inter-record comma).
        // Phase 3.4 of PaginatedResponseSizeSafety dropped the `file`
        // field from SymbolResult — the `id` already encodes it.
        //
        // Pick `max_bytes = ENVELOPE_OVERHEAD_BYTES + 300`: budget after
        // overhead reservation is 300 bytes, which fits ~4 records before
        // the 5th would push past. Asks for `limit=20` so the byte budget
        // (not the count cap) is what bites. Asserts the documented
        // truncation semantics: `truncated=true`, `next_offset=Some(n)`
        // with `n > offset=0`, `results.len() < limit=20`, and
        // `total >= results.len() + offset`.
        use super::super::ENVELOPE_OVERHEAD_BYTES;
        let g = locked(graph_with_n_orphan_functions(30));
        let max_bytes = ENVELOPE_OVERHEAD_BYTES + 300;
        let r = get_orphans(&g, None, Some(20), Some(0), None, false, max_bytes);

        let (arr, total, offset, limit) = page_parts(&r);
        let (truncated, next_offset) = super::super::test_helpers::page_extras(&r);

        assert!(truncated, "tight max_bytes must produce truncated=true");
        assert!(
            (arr.len() as u32) < limit,
            "truncation must stop before hitting the count cap: results.len()={} >= limit={}",
            arr.len(),
            limit,
        );
        assert!(
            !arr.is_empty(),
            "budget should still admit at least one record",
        );
        match next_offset {
            Some(n) => assert!(
                n > offset,
                "next_offset must point past the current page: next_offset={n} <= offset={offset}",
            ),
            None => panic!("truncated=true must set next_offset=Some(n)"),
        }
        assert!(
            total >= arr.len() as u32 + offset,
            "total must be at least the records seen so far: total={total} < results.len()+offset={}",
            arr.len() as u32 + offset,
        );
        // Sanity: total still reflects the full pre-pagination match count.
        assert_eq!(total, 30, "total is the pre-pagination match count");
    }

    #[test]
    fn orphans_byte_budget_no_truncation_with_no_budget() {
        // Mirror anti-regression: with NO_BYTE_BUDGET (= usize::MAX), the
        // handler's existing behavior is preserved exactly — no truncation,
        // no next_offset. Locks the contract that the byte-budget wiring
        // does not affect callers that opt out.
        let g = locked(graph_with_n_orphan_functions(30));
        let r = get_orphans(&g, None, Some(20), Some(0), None, false, NO_BYTE_BUDGET);
        let (arr, total, _, _) = page_parts(&r);
        let (truncated, next_offset) = super::super::test_helpers::page_extras(&r);
        assert_eq!(arr.len(), 20);
        assert_eq!(total, 30);
        assert!(!truncated);
        assert_eq!(next_offset, None);
    }

    // --- Phase 3 count_only invariants ------------------------------------

    #[test]
    fn orphans_count_only_returns_sentinel_envelope_under_1kb() {
        // Phase 3.2 of PaginatedResponseSizeSafety: when count_only=true, the
        // handler returns Page { results: [], total: <real count>, offset: 0,
        // limit: 0, truncated: false, next_offset: None } regardless of how
        // many records WOULD have been returned. Serialized envelope size
        // must be < 1KB even at the 1000-orphan scale.
        //
        // Asserts: (a) results is empty, (b) total reflects the true match
        // count (not zero), (c) limit=0 (deliberate exception to the
        // "envelope echoes resolved limit" contract per plan Decision 9),
        // (d) truncated=false and next_offset is None, (e) serialized body
        // is well under 1024 bytes regardless of input scale.
        let g = locked(graph_with_n_orphan_functions(1000));
        let r = get_orphans(&g, None, None, None, None, true, NO_BYTE_BUDGET);

        let body = body_text(&r);
        assert!(
            body.len() < 1024,
            "count_only response must be < 1KB; got {} bytes",
            body.len(),
        );

        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        let results = parsed["results"].as_array().unwrap();
        let total = parsed["total"].as_u64().unwrap();
        let offset = parsed["offset"].as_u64().unwrap();
        let limit = parsed["limit"].as_u64().unwrap();
        let truncated = parsed["truncated"].as_bool().unwrap();
        let next_offset = parsed["next_offset"].clone();

        assert!(results.is_empty(), "count_only must emit empty results");
        assert_eq!(total, 1000, "total must reflect true match count");
        assert_eq!(offset, 0, "count_only emits offset=0");
        assert_eq!(
            limit, 0,
            "count_only emits limit=0 (Decision 9 exception to envelope echo rule)"
        );
        assert!(!truncated, "count_only must never set truncated=true");
        assert_eq!(
            next_offset,
            serde_json::Value::Null,
            "count_only must emit next_offset=null"
        );
    }

    #[test]
    fn orphans_count_only_respects_kind_filter() {
        // The count_only short-circuit must apply the same kind filter as
        // the materializing path, so total reflects the post-filter count.
        // Fixture: 2 orphan functions + 1 orphan class. kind=function -> 2;
        // kind=class -> 1.
        let g = locked(graph_with_orphans());

        // kind=function => 2 orphans (foo, baz).
        let r = get_orphans(&g, Some("function"), None, None, None, true, NO_BYTE_BUDGET);
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(parsed["total"].as_u64().unwrap(), 2);
        assert!(parsed["results"].as_array().unwrap().is_empty());

        // kind=class => 1 orphan (cls).
        let r = get_orphans(&g, Some("class"), None, None, None, true, NO_BYTE_BUDGET);
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(parsed["total"].as_u64().unwrap(), 1);
        assert!(parsed["results"].as_array().unwrap().is_empty());
    }

    #[test]
    fn orphans_count_only_invalid_kind_still_errors() {
        // The count_only check runs AFTER kind validation; bad kinds still
        // surface the canonical "invalid kind: <s>" tool error.
        let g = locked(Graph::new());
        let r = get_orphans(&g, Some("widget"), None, None, None, true, NO_BYTE_BUDGET);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "invalid kind: widget");
    }

    // --- get_class_hierarchy ---

    fn class_graph() -> Graph {
        // Base <- Mid <- Leaf chain.
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/cls.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![
                sym("Base", SymbolKind::Class, "/cls.cpp"),
                sym("Mid", SymbolKind::Class, "/cls.cpp"),
                sym("Leaf", SymbolKind::Class, "/cls.cpp"),
                sym(
                    "looks_like_a_class_but_isnt",
                    SymbolKind::Function,
                    "/cls.cpp",
                ),
            ],
            edges: vec![
                inherit_edge("Mid", "Base", "/cls.cpp"),
                inherit_edge("Leaf", "Mid", "/cls.cpp"),
            ],
        });
        g
    }

    #[test]
    fn class_hierarchy_missing_class_param_errors() {
        let g = locked(Graph::new());
        let r = get_class_hierarchy(&g, "", None, None);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "'class' is required");
    }

    #[test]
    fn class_hierarchy_returns_node_tree() {
        let g = locked(class_graph());
        let r = get_class_hierarchy(&g, "Mid", Some(1), None);
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        // Phase 4: response is wrapped in {hierarchy, truncated, max_nodes,
        // total_nodes_seen}; the tree itself lives under `hierarchy`.
        let hierarchy = &parsed["hierarchy"];
        assert_eq!(hierarchy["name"], serde_json::json!("Mid"));
        let bases = hierarchy["bases"].as_array().unwrap();
        assert_eq!(bases.len(), 1);
        assert_eq!(bases[0]["name"], serde_json::json!("Base"));
        let derived = hierarchy["derived"].as_array().unwrap();
        assert_eq!(derived.len(), 1);
        assert_eq!(derived[0]["name"], serde_json::json!("Leaf"));
        // Envelope meta: small fixture fits well under the default budget.
        assert_eq!(parsed["truncated"], serde_json::json!(false));
        assert_eq!(parsed["max_nodes"], serde_json::json!(250));
        // 3 unique names: Mid, Base, Leaf.
        assert_eq!(parsed["total_nodes_seen"], serde_json::json!(3));
    }

    #[test]
    fn class_hierarchy_unknown_with_no_suggestions() {
        let g = locked(Graph::new());
        let r = get_class_hierarchy(&g, "Nope", None, None);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "class not found: \"Nope\"");
    }

    #[test]
    fn class_hierarchy_unknown_with_suggestions_filters_to_class_like() {
        // "B" is a substring of "Base" (Class) and of nothing else. The
        // function `looks_like_a_class_but_isnt` does not contain "B".
        let g = locked(class_graph());
        let r = get_class_hierarchy(&g, "B", None, None);
        assert_eq!(r.is_error, Some(true));
        let text = body_text(&r);
        assert!(text.starts_with("class not found: \"B\""), "got: {text}");
        assert!(text.contains("Base"), "got: {text}");
        assert!(text.contains("Did you mean: "), "got: {text}");
    }

    #[test]
    fn class_hierarchy_function_kind_not_suggested() {
        // "looks_like_a_class_but_isnt" has SymbolKind::Function. A query
        // that matches it via substring should NOT receive a function as
        // a "class did you mean" suggestion. (Confirmed via separate text
        // assertion to make the divergence from suggest_symbols visible.)
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/x.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym(
                "looks_like_a_class_but_isnt",
                SymbolKind::Function,
                "/x.cpp",
            )],
            edges: vec![],
        });
        let g = locked(g);
        let r = get_class_hierarchy(&g, "looks", None, None);
        assert_eq!(r.is_error, Some(true));
        let text = body_text(&r);
        // No class-like candidates → bare not-found.
        assert_eq!(text, "class not found: \"looks\"");
    }

    #[test]
    fn class_hierarchy_depth_zero_normalized_to_one() {
        // A None depth and a Some(0) both become 1.
        let g = locked(class_graph());
        let with_zero = get_class_hierarchy(&g, "Mid", Some(0), None);
        let with_none = get_class_hierarchy(&g, "Mid", None, None);
        assert_eq!(body_text(&with_zero), body_text(&with_none));
    }

    #[test]
    fn class_hierarchy_handler_zero_max_nodes_uses_default_250() {
        // max_nodes=0 is the "unset" sentinel — the handler resolves it
        // to the documented default of 250 before forwarding to the
        // Graph layer. Matches the convention used by
        // `orphans_zero_limit_uses_default`. The Graph layer always
        // receives a non-zero u32; this assertion belongs to the
        // handler, not the Graph layer.
        let g = locked(class_graph());
        let r = get_class_hierarchy(&g, "Mid", Some(1), Some(0));
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(
            parsed["max_nodes"],
            serde_json::json!(250),
            "max_nodes=0 must resolve to default 250 (echoed in response)"
        );
        assert_eq!(parsed["truncated"], serde_json::json!(false));
    }

    #[test]
    fn class_hierarchy_handler_max_nodes_clamps_at_1000() {
        // Mirrors the orphan/limit clamp test; max_nodes=999_999 silently
        // resolves to the 1000 ceiling and the response echoes the
        // clamped value.
        let g = locked(class_graph());
        let r = get_class_hierarchy(&g, "Mid", Some(1), Some(999_999));
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(parsed["max_nodes"], serde_json::json!(1000));
    }

    #[test]
    fn class_hierarchy_handler_truncates_when_budget_exceeded() {
        // Budget of 2 on the 3-class fixture: Mid + Base reachable via
        // the up-walk but the budget exhausts before adding Leaf to the
        // derived side. Asserts the handler propagates `truncated=true`
        // and the budget cap echo.
        let g = locked(class_graph());
        let r = get_class_hierarchy(&g, "Mid", Some(1), Some(2));
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(parsed["truncated"], serde_json::json!(true));
        assert_eq!(parsed["max_nodes"], serde_json::json!(2));
        assert_eq!(parsed["total_nodes_seen"], serde_json::json!(2));
    }

    // --- get_coupling ---

    fn coupling_graph() -> Graph {
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/a.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("caller", SymbolKind::Function, "/a.cpp")],
            edges: vec![
                call_edge("/a.cpp:caller", "/b.cpp:target", "/a.cpp"),
                include_edge("/a.cpp", "/b.cpp"),
            ],
        });
        g.merge_file_graph(FileGraph {
            path: "/b.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("target", SymbolKind::Function, "/b.cpp")],
            edges: vec![],
        });
        g
    }

    /// Parse a `Page<CouplingEntry>` body into `(rows, total, truncated,
    /// next_offset)`. Each row is `(file, count)`. Mirrors the
    /// `page_parts` convention but specialized for the `{file, count}`
    /// record shape so tests can assert ordering and the file-asc
    /// tiebreak directly.
    fn coupling_page(r: &CallToolResult) -> (Vec<(String, u32)>, u32, bool, Option<u32>) {
        let parsed: serde_json::Value = serde_json::from_str(&body_text(r)).unwrap();
        let rows = parsed["results"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|v| {
                (
                    v["file"].as_str().unwrap().to_string(),
                    v["count"].as_u64().unwrap() as u32,
                )
            })
            .collect();
        let total = parsed["total"].as_u64().unwrap_or(0) as u32;
        let truncated = parsed["truncated"].as_bool().unwrap_or(false);
        let next_offset = parsed["next_offset"].as_u64().map(|n| n as u32);
        (rows, total, truncated, next_offset)
    }

    #[test]
    fn coupling_missing_file_param_errors() {
        let g = locked(Graph::new());
        let r = get_coupling(&g, "", None, None, None, NO_BYTE_BUDGET);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "'file' is required");
    }

    #[test]
    fn coupling_outgoing_default_returns_page() {
        let g = locked(coupling_graph());
        let r = get_coupling(&g, "/a.cpp", None, None, None, NO_BYTE_BUDGET);
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        let (rows, total, truncated, next) = coupling_page(&r);
        // 1 call + 1 include into /b.cpp -> a single row with count 2.
        assert_eq!(rows, vec![("/b.cpp".to_string(), 2)]);
        assert_eq!(total, 1);
        assert!(!truncated);
        assert_eq!(next, None);
    }

    #[test]
    fn coupling_incoming_returns_callers_and_includers_page() {
        let g = locked(coupling_graph());
        let r = get_coupling(&g, "/b.cpp", Some("incoming"), None, None, NO_BYTE_BUDGET);
        let (rows, total, _, _) = coupling_page(&r);
        assert_eq!(rows, vec![("/a.cpp".to_string(), 2)]);
        assert_eq!(total, 1);
    }

    /// Three files all couple to the query file with the SAME count (2),
    /// so the primary count-desc key is a tie. The secondary file-asc
    /// key must order them deterministically `/a < /b < /c`. Proves the
    /// `then_with(file.cmp)` tiebreak.
    #[test]
    fn coupling_sorts_desc_count_then_asc_file() {
        let mut g = Graph::new();
        // /hub.cpp includes /c.cpp, /b.cpp, /a.cpp twice each -> count 2
        // per target, deliberately added out of file order so the sort,
        // not insertion order, decides the result sequence.
        g.merge_file_graph(FileGraph {
            path: "/hub.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![],
            edges: vec![
                include_edge("/hub.cpp", "/c.cpp"),
                include_edge("/hub.cpp", "/c.cpp"),
                include_edge("/hub.cpp", "/b.cpp"),
                include_edge("/hub.cpp", "/b.cpp"),
                include_edge("/hub.cpp", "/a.cpp"),
                include_edge("/hub.cpp", "/a.cpp"),
            ],
        });
        let g = locked(g);
        let r = get_coupling(&g, "/hub.cpp", Some("outgoing"), None, None, NO_BYTE_BUDGET);
        let (rows, total, _, _) = coupling_page(&r);
        assert_eq!(
            rows,
            vec![
                ("/a.cpp".to_string(), 2),
                ("/b.cpp".to_string(), 2),
                ("/c.cpp".to_string(), 2),
            ],
            "equal counts must tiebreak by file ascending"
        );
        assert_eq!(total, 3);
    }

    #[test]
    fn coupling_both_returns_both_pages_populated() {
        // /a.cpp has 1 outgoing call to /b.cpp and /c.cpp includes
        // /a.cpp (incoming). "both" must populate both pages with the
        // direction-appropriate file.
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/a.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("caller", SymbolKind::Function, "/a.cpp")],
            edges: vec![call_edge("/a.cpp:caller", "/b.cpp:target", "/a.cpp")],
        });
        g.merge_file_graph(FileGraph {
            path: "/b.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("target", SymbolKind::Function, "/b.cpp")],
            edges: vec![],
        });
        g.merge_file_graph(FileGraph {
            path: "/c.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![],
            edges: vec![include_edge("/c.cpp", "/a.cpp")],
        });
        let g = locked(g);
        let r = get_coupling(&g, "/a.cpp", Some("both"), None, None, NO_BYTE_BUDGET);
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let incoming = &parsed["incoming"]["results"];
        let outgoing = &parsed["outgoing"]["results"];
        // Outgoing: the call /a -> /b -> /b.cpp count 1.
        assert_eq!(outgoing[0]["file"], serde_json::json!("/b.cpp"));
        assert_eq!(outgoing[0]["count"], serde_json::json!(1));
        // Incoming: /c.cpp includes /a.cpp -> /c.cpp count 1.
        assert_eq!(incoming[0]["file"], serde_json::json!("/c.cpp"));
        assert_eq!(incoming[0]["count"], serde_json::json!(1));
        assert_eq!(parsed["outgoing"]["total"], serde_json::json!(1));
        assert_eq!(parsed["incoming"]["total"], serde_json::json!(1));
    }

    /// Sequential budget: incoming has many rows; outgoing has one. With
    /// a `max_bytes` tuned so the incoming `byte_budget_take` (full
    /// `max_bytes`) consumes essentially all of it, the remaining budget
    /// after subtracting the serialized incoming page plus the 48-byte
    /// wrapper overhead floors at 0, so outgoing must be the empty
    /// start-fresh page: `truncated: true, next_offset: Some(0)`.
    ///
    /// Byte math: `ENVELOPE_OVERHEAD_BYTES` is 512 (mod.rs), and the
    /// incoming page wrapper itself is ~100 bytes. We set `max_bytes =
    /// 560`. The incoming `byte_budget_take` reserves 512 for its own
    /// envelope (560 - 512 = 48 bytes of record budget) — enough for at
    /// least one short `{"file":"/x","count":N}` row but not many. The
    /// full serialized incoming `Page` (envelope + the kept row(s)) then
    /// runs to ~120+ bytes. `560 - incoming_bytes - 48` saturates to 0
    /// long before outgoing can be sized, so the handler emits the empty
    /// outgoing page. This pins the "incoming exhausts the budget" branch.
    #[test]
    fn coupling_both_sequential_budget_starves_outgoing() {
        let mut g = Graph::new();
        // Several files include /target.cpp (incoming rows) and
        // /target.cpp includes one file (an outgoing row that must be
        // starved out).
        g.merge_file_graph(FileGraph {
            path: "/target.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![],
            edges: vec![include_edge("/target.cpp", "/out_dep.cpp")],
        });
        for name in ["/in_a.cpp", "/in_b.cpp", "/in_c.cpp", "/in_d.cpp"] {
            g.merge_file_graph(FileGraph {
                path: name.to_string(),
                language: Language::Cpp,
                symbols: vec![],
                edges: vec![include_edge(name, "/target.cpp")],
            });
        }
        let g = locked(g);
        let r = get_coupling(&g, "/target.cpp", Some("both"), None, None, 560);
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        // Incoming has all 4 includers as `total`; whatever fits is fine,
        // the discriminator is the outgoing starvation.
        assert_eq!(parsed["incoming"]["total"], serde_json::json!(4));
        let outgoing = &parsed["outgoing"];
        assert_eq!(
            outgoing["results"],
            serde_json::json!([]),
            "incoming should exhaust the budget, leaving outgoing empty"
        );
        assert_eq!(
            outgoing["truncated"],
            serde_json::json!(true),
            "starved outgoing page must flag truncated"
        );
        assert_eq!(
            outgoing["next_offset"],
            serde_json::json!(0),
            "starved outgoing page must carry the start-fresh marker next_offset=0"
        );
        // `total` is still the true pre-pagination count even when starved.
        assert_eq!(outgoing["total"], serde_json::json!(1));
    }

    #[test]
    fn coupling_invalid_direction_errors() {
        let g = locked(Graph::new());
        let r = get_coupling(&g, "/a.cpp", Some("sideways"), None, None, NO_BYTE_BUDGET);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(
            body_text(&r),
            "invalid direction: sideways. Expected one of: outgoing, incoming, both"
        );
    }

    #[test]
    fn coupling_unknown_file_returns_empty_page() {
        let g = locked(Graph::new());
        let r = get_coupling(&g, "/never.cpp", None, None, None, NO_BYTE_BUDGET);
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        let (rows, total, truncated, next) = coupling_page(&r);
        assert!(rows.is_empty());
        assert_eq!(total, 0);
        assert!(!truncated);
        assert_eq!(next, None);
    }

    /// Type-level shape pin for `DependencyEntry`: construct and serialize
    /// one so every field is exercised, keeping the wire shape
    /// `{file, kind, line}` frozen. `kind` is `"includes"` — the value
    /// `edge_kind_str(EdgeKind::Includes)` produces, matching `EdgeKind`'s
    /// serde serialization across every surface.
    #[test]
    fn dependency_entry_serializes_with_expected_shape() {
        let entry = super::super::DependencyEntry {
            file: "/dep.cpp".to_string(),
            kind: "includes",
            line: 7,
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&entry).unwrap()).unwrap();
        assert_eq!(json["file"], serde_json::json!("/dep.cpp"));
        assert_eq!(json["kind"], serde_json::json!("includes"));
        assert_eq!(json["line"], serde_json::json!(7));
    }

    // --- generate_diagram ---

    fn diagram_graph() -> Graph {
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/x.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![
                sym("a", SymbolKind::Function, "/x.cpp"),
                sym("b", SymbolKind::Function, "/x.cpp"),
            ],
            edges: vec![call_edge("/x.cpp:a", "/x.cpp:b", "/x.cpp")],
        });
        g.merge_file_graph(FileGraph {
            path: "/y.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![],
            edges: vec![include_edge("/y.cpp", "/x.cpp")],
        });
        g.merge_file_graph(FileGraph {
            path: "/cls.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![
                sym("Base", SymbolKind::Class, "/cls.cpp"),
                sym("Derived", SymbolKind::Class, "/cls.cpp"),
            ],
            edges: vec![inherit_edge("Derived", "Base", "/cls.cpp")],
        });
        g
    }

    #[test]
    fn diagram_no_param_errors() {
        let g = locked(Graph::new());
        let r = generate_diagram(&g, GenerateDiagramInput::default());
        assert_eq!(r.is_error, Some(true));
        assert_eq!(
            body_text(&r),
            "exactly one of 'symbol', 'file', or 'class' is required"
        );
    }

    #[test]
    fn diagram_two_params_errors() {
        let g = locked(Graph::new());
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                symbol: Some("/x.cpp:a"),
                file: Some("/x.cpp"),
                ..GenerateDiagramInput::default()
            },
        );
        assert_eq!(r.is_error, Some(true));
        assert_eq!(
            body_text(&r),
            "exactly one of 'symbol', 'file', or 'class' is required"
        );
    }

    #[test]
    fn diagram_three_params_errors() {
        let g = locked(Graph::new());
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                symbol: Some("a"),
                file: Some("/x.cpp"),
                class: Some("Base"),
                ..GenerateDiagramInput::default()
            },
        );
        assert_eq!(r.is_error, Some(true));
    }

    #[test]
    fn diagram_empty_strings_treated_as_absent() {
        // Three empty strings count as 0 set parameters.
        let g = locked(Graph::new());
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                symbol: Some(""),
                file: Some(""),
                class: Some(""),
                ..GenerateDiagramInput::default()
            },
        );
        assert_eq!(r.is_error, Some(true));
        assert_eq!(
            body_text(&r),
            "exactly one of 'symbol', 'file', or 'class' is required"
        );
    }

    #[test]
    fn diagram_symbol_edges_format() {
        let g = locked(diagram_graph());
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                symbol: Some("/x.cpp:a"),
                ..GenerateDiagramInput::default()
            },
        );
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["from"], serde_json::json!("a"));
        assert_eq!(arr[0]["to"], serde_json::json!("b"));
        assert_eq!(arr[0]["label"], serde_json::json!("calls"));
    }

    #[test]
    fn diagram_file_edges_format() {
        let g = locked(diagram_graph());
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                file: Some("/x.cpp"),
                ..GenerateDiagramInput::default()
            },
        );
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let arr = parsed.as_array().unwrap();
        // /y.cpp -> /x.cpp via include, found via reverse-include scan.
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["label"], serde_json::json!("includes"));
    }

    #[test]
    fn diagram_class_edges_format() {
        let g = locked(diagram_graph());
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                class: Some("Base"),
                ..GenerateDiagramInput::default()
            },
        );
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["from"], serde_json::json!("Derived"));
        assert_eq!(arr[0]["to"], serde_json::json!("Base"));
        assert_eq!(arr[0]["label"], serde_json::json!("inherits"));
    }

    #[test]
    fn diagram_mermaid_format() {
        let g = locked(diagram_graph());
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                symbol: Some("/x.cpp:a"),
                format: Some("mermaid"),
                ..GenerateDiagramInput::default()
            },
        );
        let text = body_text(&r);
        assert!(text.starts_with("graph TD\n"), "got: {text}");
        assert!(text.contains("calls"), "must include label: {text}");
    }

    #[test]
    fn diagram_mermaid_styled_marks_center() {
        let g = locked(diagram_graph());
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                symbol: Some("/x.cpp:a"),
                format: Some("mermaid"),
                styled: true,
                ..GenerateDiagramInput::default()
            },
        );
        let text = body_text(&r);
        assert!(text.contains(":::center"), "styled must tag center: {text}");
        assert!(text.contains("classDef center"), "got: {text}");
    }

    #[test]
    fn diagram_invalid_format_errors() {
        let g = locked(diagram_graph());
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                symbol: Some("/x.cpp:a"),
                format: Some("svg"),
                ..GenerateDiagramInput::default()
            },
        );
        assert_eq!(r.is_error, Some(true));
        assert_eq!(
            body_text(&r),
            "invalid format: svg. Expected 'edges' or 'mermaid'"
        );
    }

    #[test]
    fn diagram_invalid_direction_errors_in_symbol_mode() {
        let g = locked(diagram_graph());
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                symbol: Some("/x.cpp:a"),
                direction: Some("calle_es"),
                ..GenerateDiagramInput::default()
            },
        );
        assert_eq!(r.is_error, Some(true));
        assert_eq!(
            body_text(&r),
            "invalid direction: calle_es. Expected one of: callees, callers, both"
        );
    }

    #[test]
    fn diagram_invalid_direction_ignored_in_file_mode() {
        // `direction` is symbol-mode-only. A bad spelling alongside
        // `file=` must NOT surface a direction error — the file diagram
        // is produced as if no direction were given.
        let g = locked(diagram_graph());
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                file: Some("/x.cpp"),
                direction: Some("not-a-direction"),
                ..GenerateDiagramInput::default()
            },
        );
        assert_ne!(
            r.is_error,
            Some(true),
            "file-mode diagram must ignore an invalid direction, got error: {}",
            body_text(&r)
        );
    }

    #[test]
    fn diagram_unknown_symbol_did_you_mean() {
        let g = locked(diagram_graph());
        // "a" is a substring of `/x.cpp:a` — should suggest.
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                symbol: Some("a"),
                ..GenerateDiagramInput::default()
            },
        );
        assert_eq!(r.is_error, Some(true));
        let text = body_text(&r);
        assert!(text.starts_with("symbol not found: \"a\""), "got: {text}");
        assert!(text.contains("Did you mean: "), "got: {text}");
    }

    #[test]
    fn diagram_unknown_file_no_did_you_mean() {
        let g = locked(diagram_graph());
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                file: Some("/never.cpp"),
                ..GenerateDiagramInput::default()
            },
        );
        assert_eq!(r.is_error, Some(true));
        // No did-you-mean for files.
        assert_eq!(body_text(&r), "file not found: \"/never.cpp\"");
    }

    #[test]
    fn diagram_unknown_class_with_suggestion() {
        let g = locked(diagram_graph());
        // "B" → "Base" (Class).
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                class: Some("B"),
                ..GenerateDiagramInput::default()
            },
        );
        assert_eq!(r.is_error, Some(true));
        let text = body_text(&r);
        assert!(text.starts_with("class not found: \"B\""), "got: {text}");
        assert!(text.contains("Base"), "got: {text}");
    }

    #[test]
    fn diagram_empty_edges_serializes_as_array() {
        // Class with no inheritance edges → empty Vec<DiagramEdge> → "[]".
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/x.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("Solo", SymbolKind::Class, "/x.cpp")],
            edges: vec![],
        });
        let g = locked(g);
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                class: Some("Solo"),
                ..GenerateDiagramInput::default()
            },
        );
        assert_eq!(body_text(&r), "[]");
    }

    /// Fixture: `c -> a -> b`. `a` calls `b`; `c` calls `a`. Used by the
    /// direction-filter tests below, exercised through the handler so the
    /// `direction` string arg parsing is covered end to end.
    fn directional_call_graph() -> Graph {
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/x.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![
                sym("a", SymbolKind::Function, "/x.cpp"),
                sym("b", SymbolKind::Function, "/x.cpp"),
                sym("c", SymbolKind::Function, "/x.cpp"),
            ],
            edges: vec![
                call_edge("/x.cpp:a", "/x.cpp:b", "/x.cpp"),
                call_edge("/x.cpp:c", "/x.cpp:a", "/x.cpp"),
            ],
        });
        g
    }

    #[test]
    fn generate_diagram_direction_callees_only() {
        // Centered on `a` with direction=callees: only the forward arm
        // is walked, so the client sees exactly the a -> b edge and
        // never the c -> a inbound edge.
        let g = locked(directional_call_graph());
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                symbol: Some("/x.cpp:a"),
                direction: Some("callees"),
                ..GenerateDiagramInput::default()
            },
        );
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1, "callees must surface only a -> b: {arr:?}");
        assert_eq!(arr[0]["from"], serde_json::json!("a"));
        assert_eq!(arr[0]["to"], serde_json::json!("b"));
        // No edge whose endpoints are the inbound caller `c`.
        assert!(
            !arr.iter()
                .any(|e| e["from"] == serde_json::json!("c") || e["to"] == serde_json::json!("c")),
            "callees must NOT surface the c -> a inbound edge: {arr:?}",
        );
    }

    #[test]
    fn generate_diagram_direction_callers_only() {
        // Same fixture, direction=callers: only the reverse arm is
        // walked, so the client sees exactly the c -> a edge and never
        // the a -> b outbound edge.
        let g = locked(directional_call_graph());
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                symbol: Some("/x.cpp:a"),
                direction: Some("callers"),
                ..GenerateDiagramInput::default()
            },
        );
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1, "callers must surface only c -> a: {arr:?}");
        assert_eq!(arr[0]["from"], serde_json::json!("c"));
        assert_eq!(arr[0]["to"], serde_json::json!("a"));
        // No edge whose endpoints are the outbound callee `b`.
        assert!(
            !arr.iter()
                .any(|e| e["from"] == serde_json::json!("b") || e["to"] == serde_json::json!("b")),
            "callers must NOT surface the a -> b outbound edge: {arr:?}",
        );
    }

    #[test]
    fn generate_diagram_label_dedupe_pins_user_report() {
        // The user-reported triple-duplicate scenario, pinned at the
        // handler boundary. Two free functions both named `Tick` live in
        // DIFFERENT files, so their SymbolIds are DISTINCT
        // (`/a.cpp:Tick` vs `/b.cpp:Tick`); both call a function
        // rendering to the label `Update`. Centered on `Update` with
        // direction=callers, the reverse walk surfaces two inbound edges
        // that reduce to the identical rendered pair ("Tick", "Update").
        // Under raw-SymbolId dedupe this emitted the edge more than once
        // (the user saw 3x with extra macro-blind collisions); with
        // label-keyed dedupe exactly one survives in `result.edges`.
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/a.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("Tick", SymbolKind::Function, "/a.cpp")],
            edges: vec![call_edge("/a.cpp:Tick", "/shared.cpp:Update", "/a.cpp")],
        });
        g.merge_file_graph(FileGraph {
            path: "/b.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("Tick", SymbolKind::Function, "/b.cpp")],
            edges: vec![call_edge("/b.cpp:Tick", "/shared.cpp:Update", "/b.cpp")],
        });
        g.merge_file_graph(FileGraph {
            path: "/shared.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("Update", SymbolKind::Function, "/shared.cpp")],
            edges: vec![],
        });
        let g = locked(g);
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                symbol: Some("/shared.cpp:Update"),
                direction: Some("callers"),
                ..GenerateDiagramInput::default()
            },
        );
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let arr = parsed.as_array().unwrap();
        let tick_update = arr
            .iter()
            .filter(|e| {
                e["from"] == serde_json::json!("Tick") && e["to"] == serde_json::json!("Update")
            })
            .count();
        assert_eq!(
            tick_update, 1,
            "two distinct `Tick` SymbolIds rendering to the same label \
             must collapse into exactly one Tick -> Update edge: {arr:?}",
        );
        assert_eq!(
            arr.len(),
            1,
            "exactly the single deduped edge survives: {arr:?}",
        );
    }

    #[test]
    fn generate_diagram_no_file_basename_leak() {
        // `a` calls a resolved `helper` AND an unresolved
        // `/missing.cpp:gone` (no symbol named `gone` is declared, so
        // that SymbolId is not a graph node). Before the fix the
        // unresolved endpoint rendered as a file-basename pseudo-node
        // (`missing.cpp`); now that edge is dropped while the resolved
        // `a -> helper` edge survives. Pinning BOTH halves: the resolved
        // edge is not collateral-dropped, and no surviving endpoint
        // renders as a source-file basename.
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/x.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![
                sym("a", SymbolKind::Function, "/x.cpp"),
                sym("helper", SymbolKind::Function, "/x.cpp"),
            ],
            edges: vec![
                call_edge("/x.cpp:a", "/x.cpp:helper", "/x.cpp"),
                call_edge("/x.cpp:a", "/missing.cpp:gone", "/x.cpp"),
            ],
        });
        let g = locked(g);
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                symbol: Some("/x.cpp:a"),
                ..GenerateDiagramInput::default()
            },
        );
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        let body = body_text(&r);
        let parsed: serde_json::Value = serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("handler returned malformed JSON ({e}): {body}"));
        let arr = parsed
            .as_array()
            .unwrap_or_else(|| panic!("edges body must be a JSON array, got: {body}"));
        // Resolved edge survives; unresolved one is dropped (not
        // leaked as `missing.cpp`).
        assert_eq!(
            arr.len(),
            1,
            "only the resolved a -> helper edge survives: {arr:?}",
        );
        assert_eq!(arr[0]["from"], "a");
        assert_eq!(arr[0]["to"], "helper");
        const SOURCE_EXTS: &[&str] = &[
            ".cpp", ".cc", ".cxx", ".c", ".h", ".hpp", ".hxx", ".rs", ".go", ".py", ".pyi", ".cs",
            ".java",
        ];
        let looks_like_basename = |v: &serde_json::Value| {
            v.as_str()
                .map(|s| SOURCE_EXTS.iter().any(|ext| s.ends_with(ext)))
                .unwrap_or(false)
        };
        for e in arr {
            assert!(
                !looks_like_basename(&e["from"]),
                "no edge endpoint may render as a file basename, got from={:?}",
                e["from"],
            );
            assert!(
                !looks_like_basename(&e["to"]),
                "no edge endpoint may render as a file basename, got to={:?}",
                e["to"],
            );
        }
    }

    // --- PathNormalization Phase 3.2 --------------------------------------

    #[test]
    fn coupling_resolves_dot_segments_to_canonical_lookup() {
        // PathNormalization Phase 3.2: `get_coupling` wraps the user-supplied
        // `file` argument with `paths::normalize_user_path` before the graph
        // lookup. Mirrors the Phase 3.1 test in `symbols.rs`. Plant a coupling
        // edge keyed by a real canonical filesystem path, then query the
        // handler twice — once with the canonical form, once with a
        // `./sub/../` injected form that resolves to the same canonical via
        // `dunce::canonicalize`. Both calls must return the same coupling map.
        //
        // The path must exist on disk so the canonicalize branch is exercised
        // (the lexical-fallback branch on a non-existent path would NOT
        // resolve dot segments, per `paths.rs` test `(d)`).
        let tmp = tempfile::TempDir::new().expect("create tempdir");
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).expect("create sub dir");
        let file_path = tmp.path().join("a.cpp");
        std::fs::write(&file_path, "// empty\n").expect("write file");

        // Capture the canonical form the graph will be keyed by. On Linux
        // this is identity for an already-canonical path; the explicit
        // canonicalize step keeps the test correct under symlinked tempdirs
        // (e.g. macOS `/var` -> `/private/var`).
        let canonical = paths::canonicalize(&file_path).expect("canonicalize file");
        let canonical_str = canonical
            .to_str()
            .expect("canonical path is valid UTF-8 on Linux");

        // Build a graph with an include edge from the canonical path to
        // `/b.cpp` so the outgoing-coupling lookup has a record to find.
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: canonical_str.to_string(),
            language: Language::Cpp,
            symbols: vec![],
            edges: vec![include_edge(canonical_str, "/b.cpp")],
        });
        let g = locked(g);

        // (1) Canonical form — the baseline. Asserts the fixture is sound
        // before we exercise the normalize path.
        let r_canonical = get_coupling(&g, canonical_str, None, None, None, NO_BYTE_BUDGET);
        assert!(r_canonical.is_error.is_none() || r_canonical.is_error == Some(false));
        let (rows_canonical, _, _, _) = coupling_page(&r_canonical);
        assert_eq!(rows_canonical, vec![("/b.cpp".to_string(), 1)]);

        // (2) `./sub/../a.cpp` form — the load-bearing assertion. Without
        // `normalize_user_path`, this string would fail an exact-match graph
        // lookup against the canonical key and return an empty object.
        let messy = tmp.path().join(".").join("sub").join("..").join("a.cpp");
        let messy_str = messy.to_str().expect("messy path is valid UTF-8 on Linux");
        assert_ne!(
            messy_str, canonical_str,
            "messy fixture must differ from canonical for the test to be meaningful"
        );

        let r_messy = get_coupling(&g, messy_str, None, None, None, NO_BYTE_BUDGET);
        assert!(
            r_messy.is_error.is_none() || r_messy.is_error == Some(false),
            "messy form must succeed after normalize: body={}",
            body_text(&r_messy),
        );
        let (rows_messy, _, _, _) = coupling_page(&r_messy);
        assert_eq!(
            rows_messy,
            vec![("/b.cpp".to_string(), 1)],
            "messy form must return the same coupling page as canonical",
        );
    }

    #[test]
    fn diagram_file_mode_resolves_dot_segments_to_canonical_lookup() {
        // PathNormalization Phase 3.2: `generate_diagram` (file mode) wraps
        // the user-supplied `file` argument with `paths::normalize_user_path`
        // before the graph lookup. Mirrors the Phase 3.1 test in `symbols.rs`.
        // Plant an include edge keyed by a real canonical filesystem path,
        // then query the handler twice — once with the canonical form, once
        // with a `./sub/../` injected form — and assert both produce a
        // non-empty edge set.
        let tmp = tempfile::TempDir::new().expect("create tempdir");
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).expect("create sub dir");
        let file_path = tmp.path().join("x.cpp");
        std::fs::write(&file_path, "// empty\n").expect("write file");

        let canonical = paths::canonicalize(&file_path).expect("canonicalize file");
        let canonical_str = canonical
            .to_str()
            .expect("canonical path is valid UTF-8 on Linux");

        // Build a graph with /y.cpp -> canonical via include, so the file-mode
        // diagram (reverse-include scan) returns one edge.
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: canonical_str.to_string(),
            language: Language::Cpp,
            symbols: vec![],
            edges: vec![],
        });
        g.merge_file_graph(FileGraph {
            path: "/y.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![],
            edges: vec![include_edge("/y.cpp", canonical_str)],
        });
        let g = locked(g);

        // (1) Canonical form — baseline.
        let r_canonical = generate_diagram(
            &g,
            GenerateDiagramInput {
                file: Some(canonical_str),
                ..GenerateDiagramInput::default()
            },
        );
        assert!(r_canonical.is_error.is_none() || r_canonical.is_error == Some(false));
        let parsed_canonical: serde_json::Value =
            serde_json::from_str(&body_text(&r_canonical)).unwrap();
        let arr_canonical = parsed_canonical.as_array().unwrap();
        assert_eq!(arr_canonical.len(), 1);
        assert_eq!(arr_canonical[0]["label"], serde_json::json!("includes"));

        // (2) `./sub/../x.cpp` form — load-bearing.
        let messy = tmp.path().join(".").join("sub").join("..").join("x.cpp");
        let messy_str = messy.to_str().expect("messy path is valid UTF-8 on Linux");
        assert_ne!(
            messy_str, canonical_str,
            "messy fixture must differ from canonical for the test to be meaningful"
        );

        let r_messy = generate_diagram(
            &g,
            GenerateDiagramInput {
                file: Some(messy_str),
                ..GenerateDiagramInput::default()
            },
        );
        assert!(
            r_messy.is_error.is_none() || r_messy.is_error == Some(false),
            "messy form must succeed after normalize: body={}",
            body_text(&r_messy),
        );
        let parsed_messy: serde_json::Value = serde_json::from_str(&body_text(&r_messy)).unwrap();
        let arr_messy = parsed_messy.as_array().unwrap();
        assert_eq!(
            arr_messy.len(),
            1,
            "messy form must return the same edge set as canonical",
        );
        assert_eq!(arr_messy[0]["label"], serde_json::json!("includes"));
    }
}
