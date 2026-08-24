//! Typed core for `detect_cycles`, `get_orphans`, `get_class_hierarchy`,
//! `find_class_candidates`, `get_coupling`, `generate_diagram`, and phase
//! 1's `detect_communities`.
//!
//! Indexed-state behavior is defined once by [`crate::core::require_indexed`].
//! Each public operation receives the caller's real state and checks it at
//! entry; handler adapters are not typed-core entry points.
//!
//! **`generate_diagram(format="mermaid")` is the second plain-text
//! producer in the codebase** (Design Decision 2) — `format="edges"` maps
//! to `Ok(ToolOk::Value(_))`, `format="mermaid"` maps to
//! `Ok(ToolOk::Text(rendered))`. Proves `ToolOk::Text` was modeled
//! generally rather than around the non-callable advisory alone.
//!
//! **`detect_cycles` stays by-COUNT pagination only, deliberately NOT
//! byte-budgeted** — this is not "harmonised" onto `byte_budget_take`
//! while moving; the asymmetry is intentional and documented in
//! CLAUDE.md.
//!
//! **`get_coupling(direction="both")` preserves the sequential
//! byte-budget allocation exactly**: incoming is sized first against the
//! full budget, outgoing against what remains after incoming plus a
//! fixed wrapper reserve.
//!
//! `GenerateDiagramInput<'a>` stays in `handlers::structure` (Decision 4);
//! this module imports it rather than moving it.
//!
//! `ReliabilityMode` and `is_unreliable_orphan` stay in
//! `handlers::structure` rather than moving here, mirroring Decision 4's
//! precedent for `core::query`'s and `core::symbols`'s pure-logic
//! exceptions: `handlers::structure`'s own `#[cfg(test)]` module asserts
//! against `is_unreliable_orphan`/`ReliabilityMode` directly via
//! `use super::*` (the `is_unreliable_orphan_predicate_table` and
//! `is_unreliable_orphan_language_gates` tests), and moving them would
//! force edits to those (deliberately unmodified, Decision 6) tests.
//! `suggest_class_symbols`, `sort_coupling_rows`, `coupling_rows`,
//! `COUPLING_BOTH_WRAPPER_OVERHEAD`, and `ClassHierarchyResponse` are NOT
//! directly unit-tested, so they move here in full.

use std::collections::HashMap;
use std::path::PathBuf;

use code_graph_core::{paths, symbol_id, SymbolKind};
use code_graph_graph::{DiagramDirection, DiagramEdge, Graph, HierarchyNode};
use parking_lot::RwLock;
use serde::Serialize;

use crate::core::{require_indexed, ToolError, ToolOk, ToolResult};
use crate::handlers::structure::{is_unreliable_orphan, ReliabilityMode};

/// Re-exported so a second front-end (the CLI, Designs/CommandLineInterface
/// Decision 1) can construct the input without importing `handlers`.
pub use crate::handlers::structure::GenerateDiagramInput as DiagramInput;
use crate::handlers::{
    byte_budget_take, kind_str, parse_kind, parse_min_confidence, suggest_symbols, symbol_to_result,
};
pub use crate::handlers::{
    Community, CouplingBoth, CouplingEntry, Cycle, DegenerateInfo, DetectCommunitiesResponse, Page,
    SymbolResult,
};

// ----- detect_cycles -----

/// Typed `detect_cycles` operation. Pagination is purely by count and is
/// deliberately not byte-budgeted.
pub fn detect_cycles(
    graph: &RwLock<Graph>,
    indexed: bool,
    subtree: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
    max_cycle_size: Option<u32>,
) -> ToolResult<Page<Cycle>> {
    require_indexed(indexed)?;

    let resolved_limit = limit.filter(|&n| n != 0).unwrap_or(20).min(1000);
    let resolved_offset = offset.unwrap_or(0);
    let resolved_max = max_cycle_size.filter(|&n| n != 0).unwrap_or(50).min(500);

    let subtree_prefix: Option<std::path::PathBuf> = subtree
        .filter(|s| !s.is_empty())
        .map(code_graph_core::paths::normalize_user_path);

    let raw_cycles: Vec<Vec<PathBuf>> = graph.read().detect_cycles();
    let cycles: Vec<Vec<PathBuf>> = match subtree_prefix.as_deref() {
        Some(p) => raw_cycles
            .into_iter()
            .filter(|cycle| cycle.iter().all(|file| file.starts_with(p)))
            .collect(),
        None => raw_cycles,
    };

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

    stringified.sort_by(|a, b| a.first().cmp(&b.first()));

    let total = stringified.len() as u32;
    let mut results: Vec<Cycle> = stringified
        .into_iter()
        .skip(resolved_offset as usize)
        .take(resolved_limit as usize)
        .map(|files| Cycle {
            files,
            truncated: false,
            original_len: None,
        })
        .collect();

    for cycle in &mut results {
        if cycle.files.len() as u32 > resolved_max {
            let original = cycle.files.len() as u32;
            cycle.files.truncate(resolved_max as usize);
            cycle.truncated = true;
            cycle.original_len = Some(original);
        }
    }

    // Cycle pagination is by COUNT, not by serialized byte size — see the
    // module doc comment. Do not route this through `byte_budget_take`.
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
    Ok(ToolOk::Value(response))
}

// ----- get_orphans -----

/// Typed `get_orphans` operation with reliability, subtree, count-only, and
/// byte-budgeted pagination filters.
#[allow(clippy::too_many_arguments)]
pub fn get_orphans(
    graph: &RwLock<Graph>,
    indexed: bool,
    kind: Option<&str>,
    subtree: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
    brief: Option<bool>,
    count_only: bool,
    reliability: Option<&str>,
    max_bytes: usize,
) -> ToolResult<Page<SymbolResult>> {
    require_indexed(indexed)?;

    let parsed_kind: Option<SymbolKind> = match kind.filter(|s| !s.is_empty()) {
        None => None,
        Some(s) => match parse_kind(s) {
            Some(k) => Some(k),
            None => return Err(ToolError(format!("invalid kind: {s}"))),
        },
    };

    let reliability_mode = match reliability {
        None | Some("") | Some("all") => ReliabilityMode::All,
        Some("high") => ReliabilityMode::High,
        Some("very_high") => ReliabilityMode::VeryHigh,
        Some(other) => {
            return Err(ToolError(format!(
                "invalid reliability: {other:?} (expected \"all\", \"high\", or \"very_high\")"
            )))
        }
    };

    let subtree_prefix: Option<std::path::PathBuf> = subtree
        .filter(|s| !s.is_empty())
        .map(code_graph_core::paths::normalize_user_path);

    if count_only {
        let g = graph.read();
        let raw = match subtree_prefix.as_deref() {
            Some(p) => g.orphans_under(p, parsed_kind),
            None => g.orphans(parsed_kind),
        };
        let total = match reliability_mode {
            ReliabilityMode::All => raw.len() as u32,
            ReliabilityMode::High | ReliabilityMode::VeryHigh => raw
                .iter()
                .filter(|s| !is_unreliable_orphan(s, reliability_mode, &g))
                .count() as u32,
        };
        drop(g);
        let response = Page::<SymbolResult> {
            results: vec![],
            total,
            offset: 0,
            limit: 0,
            truncated: false,
            next_offset: None,
        };
        return Ok(ToolOk::Value(response));
    }

    let resolved_limit = limit.filter(|&n| n != 0).unwrap_or(20).min(1000);
    let resolved_offset = offset.unwrap_or(0);
    let resolved_brief = brief.unwrap_or(true);

    let g = graph.read();
    let mut matches = match subtree_prefix.as_deref() {
        Some(p) => g.orphans_under(p, parsed_kind),
        None => g.orphans(parsed_kind),
    };
    if !matches!(reliability_mode, ReliabilityMode::All) {
        matches.retain(|s| !is_unreliable_orphan(s, reliability_mode, &g));
    }
    drop(g);
    let total = matches.len() as u32;

    matches.sort_by_key(symbol_id);

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
    Ok(ToolOk::Value(response))
}

// ----- get_class_hierarchy -----

/// Wire-format envelope for `get_class_hierarchy`. Body moved verbatim
/// from `handlers::structure::ClassHierarchyResponse`.
#[derive(Debug, Serialize)]
pub struct ClassHierarchyResponse {
    pub hierarchy: HierarchyNode,
    pub truncated: bool,
    pub max_nodes: u32,
    pub total_nodes_seen: u32,
}

/// Did-you-mean helper for class-like lookups. Body moved verbatim from
/// `handlers::structure::suggest_class_symbols`.
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

/// `get_class_hierarchy` body. Body moved verbatim from
/// `handlers::structure::get_class_hierarchy`, plus the core
/// `require_indexed` call at entry (Decision 8).
pub fn get_class_hierarchy(
    graph: &RwLock<Graph>,
    indexed: bool,
    class: &str,
    depth: Option<u32>,
    max_nodes: Option<u32>,
) -> ToolResult<ClassHierarchyResponse> {
    require_indexed(indexed)?;

    if class.is_empty() {
        return Err(ToolError("'class' is required".to_string()));
    }

    let depth = depth.filter(|&d| d > 0).unwrap_or(1);
    let resolved_max_nodes = max_nodes.filter(|&n| n != 0).unwrap_or(250).min(1000);

    let g = graph.read();

    if let Some(sym) = g.symbol_detail(class) {
        if !matches!(
            sym.kind,
            SymbolKind::Class | SymbolKind::Struct | SymbolKind::Interface | SymbolKind::Trait
        ) {
            let kind = kind_str(sym.kind);
            drop(g);
            return Err(ToolError(format!(
                "symbol {class:?} is a {kind}, not a class-like symbol; \
                 get_class_hierarchy requires a Class, Struct, Interface, or Trait"
            )));
        }
        if let Some((hierarchy, total_nodes_seen, truncated)) =
            g.class_hierarchy_for_symbol(class, depth, resolved_max_nodes)
        {
            let response = ClassHierarchyResponse {
                hierarchy,
                truncated,
                max_nodes: resolved_max_nodes,
                total_nodes_seen,
            };
            return Ok(ToolOk::Value(response));
        }
        drop(g);
        return Err(ToolError(format!(
            "internal: symbol {class:?} resolved but hierarchy walk returned no tree"
        )));
    }

    let candidates = g.find_classes_named(class);
    if candidates.len() > 1 {
        let mut listed: Vec<String> = candidates
            .iter()
            .map(|s| code_graph_core::symbol_id(s))
            .collect();
        listed.sort();
        let bullet_list = listed
            .iter()
            .map(|s| format!("  - {s}"))
            .collect::<Vec<_>>()
            .join("\n");
        drop(g);
        return Err(ToolError(format!(
            "ambiguous class name {class:?} ({n} candidates):\n{bullet_list}\nRe-run \
             get_class_hierarchy with one of the symbol_ids listed above, or call \
             find_class_candidates for full details.",
            n = listed.len()
        )));
    }

    if let Some((hierarchy, total_nodes_seen, truncated)) =
        g.class_hierarchy(class, depth, resolved_max_nodes)
    {
        let response = ClassHierarchyResponse {
            hierarchy,
            truncated,
            max_nodes: resolved_max_nodes,
            total_nodes_seen,
        };
        return Ok(ToolOk::Value(response));
    }
    let class_like = suggest_class_symbols(&g, class, 5);
    drop(g);

    if class_like.is_empty() {
        Err(ToolError(format!("class not found: {class:?}")))
    } else {
        let suggestions = class_like.join(", ");
        Err(ToolError(format!(
            "class not found: {class:?}. Did you mean: {suggestions}?"
        )))
    }
}

/// `find_class_candidates` body. Body moved verbatim from
/// `handlers::structure::find_class_candidates`, plus the core
/// `require_indexed` call at entry (Decision 8).
pub fn find_class_candidates(
    graph: &RwLock<Graph>,
    indexed: bool,
    name: &str,
) -> ToolResult<Vec<SymbolResult>> {
    require_indexed(indexed)?;

    if name.is_empty() {
        return Err(ToolError("'name' is required".to_string()));
    }
    let g = graph.read();
    let mut candidates: Vec<_> = g.find_classes_named(name).into_iter().cloned().collect();
    candidates.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.line.cmp(&b.line)));
    drop(g);
    let results: Vec<_> = candidates
        .iter()
        .map(|s| symbol_to_result(s, false))
        .collect();
    Ok(ToolOk::Value(results))
}

// ----- get_coupling -----

/// Fixed reserve, in bytes, for the [`CouplingBoth`] outer wrapper. Body
/// moved verbatim from `handlers::structure::COUPLING_BOTH_WRAPPER_OVERHEAD`.
const COUPLING_BOTH_WRAPPER_OVERHEAD: usize = 48;

/// Body moved verbatim from `handlers::structure::sort_coupling_rows`.
fn sort_coupling_rows(rows: &mut [CouplingEntry]) {
    rows.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.file.cmp(&b.file)));
}

/// Body moved verbatim from `handlers::structure::coupling_rows`.
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

/// Marker return shape for `get_coupling`, since `outgoing`/`incoming`
/// return `Page<CouplingEntry>` and `both` returns `CouplingBoth` — two
/// distinct response types on one function, exactly like the handler
/// today (`CallToolResult` erased the distinction; the core keeps it
/// explicit via an enum rather than serializing early).
pub enum CouplingResult {
    Single(Page<CouplingEntry>),
    Both(CouplingBoth),
}

impl Serialize for CouplingResult {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            CouplingResult::Single(p) => p.serialize(serializer),
            CouplingResult::Both(b) => b.serialize(serializer),
        }
    }
}

/// `get_coupling` body. Body moved verbatim from
/// `handlers::structure::get_coupling`, plus the core `require_indexed`
/// call at entry (Decision 8). Preserves the `direction="both"`
/// sequential byte-budget allocation exactly: incoming is sized first
/// against the full `max_bytes`; outgoing gets what remains after
/// incoming plus [`COUPLING_BOTH_WRAPPER_OVERHEAD`].
pub fn get_coupling(
    graph: &RwLock<Graph>,
    indexed: bool,
    file: &str,
    direction: Option<&str>,
    offset: Option<u32>,
    limit: Option<u32>,
    max_bytes: usize,
) -> ToolResult<CouplingResult> {
    require_indexed(indexed)?;

    if file.is_empty() {
        return Err(ToolError("'file' is required".to_string()));
    }

    let direction = match direction.unwrap_or("") {
        "" | "outgoing" => "outgoing",
        "incoming" => "incoming",
        "both" => "both",
        other => {
            return Err(ToolError(format!(
                "invalid direction: {other}. Expected one of: outgoing, incoming, both"
            )));
        }
    };

    let resolved_limit = limit.filter(|&n| n != 0).unwrap_or(50).min(1000);
    let resolved_offset = offset.unwrap_or(0);

    let path = paths::normalize_user_path(file);

    if direction == "both" {
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

        let incoming_bytes = serde_json::to_string(&incoming)
            .map(|s| s.len())
            .unwrap_or(max_bytes);
        let remaining = max_bytes
            .saturating_sub(incoming_bytes)
            .saturating_sub(COUPLING_BOTH_WRAPPER_OVERHEAD);

        let outgoing = if remaining == 0 {
            Page::<CouplingEntry> {
                results: vec![],
                total: outgoing_total,
                offset: resolved_offset,
                limit: resolved_limit,
                truncated: true,
                next_offset: Some(resolved_offset),
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

        return Ok(ToolOk::Value(CouplingResult::Both(CouplingBoth {
            incoming,
            outgoing,
        })));
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
    Ok(ToolOk::Value(CouplingResult::Single(response)))
}

// ----- generate_diagram -----

/// `generate_diagram` body. Body moved verbatim from
/// `handlers::structure::generate_diagram`, plus the core
/// `require_indexed` call at entry (Decision 8). Maps `format="edges"` to
/// `Ok(ToolOk::Value(_))` and `format="mermaid"` to
/// `Ok(ToolOk::Text(rendered))` — see the module doc comment for why this
/// is the case that proves `ToolOk::Text` was modeled generally.
pub fn generate_diagram(
    graph: &RwLock<Graph>,
    indexed: bool,
    input: DiagramInput<'_>,
) -> ToolResult<Vec<DiagramEdge>> {
    require_indexed(indexed)?;

    let symbol = input.symbol.filter(|s| !s.is_empty());
    let file = input.file.filter(|s| !s.is_empty());
    let class = input.class.filter(|s| !s.is_empty());
    let count =
        usize::from(symbol.is_some()) + usize::from(file.is_some()) + usize::from(class.is_some());
    if count != 1 {
        return Err(ToolError(
            "exactly one of 'symbol', 'file', or 'class' is required".to_string(),
        ));
    }

    let depth = input.depth.filter(|&d| d > 0).unwrap_or(1);
    let max_nodes = input.max_nodes.filter(|&m| m > 0).unwrap_or(30);

    let direction = if symbol.is_some() {
        match input.direction.unwrap_or("") {
            "" | "both" => DiagramDirection::Both,
            "callees" => DiagramDirection::Callees,
            "callers" => DiagramDirection::Callers,
            other => {
                return Err(ToolError(format!(
                    "invalid direction: {other}. Expected one of: callees, callers, both"
                )));
            }
        }
    } else {
        DiagramDirection::Both
    };

    let format = input.format.unwrap_or("");
    let format = if format.is_empty() { "edges" } else { format };

    if format != "edges" && format != "mermaid" {
        return Err(ToolError(format!(
            "invalid format: {format}. Expected 'edges' or 'mermaid'"
        )));
    }

    let min_confidence_filter = match parse_min_confidence(input.min_confidence) {
        Ok(v) => v,
        Err(e) => return Err(ToolError(e)),
    };

    let g = graph.read();
    let dr_opt = if let Some(id) = symbol {
        g.diagram_call_graph(id, direction, depth, max_nodes, min_confidence_filter)
    } else if let Some(path) = file {
        let normalized = paths::normalize_user_path(path);
        g.diagram_file_graph(&normalized, depth, max_nodes)
    } else if let Some(name) = class {
        g.diagram_inheritance(name, depth, max_nodes)
    } else {
        unreachable!("exactly-one-of validation guarantees one branch is taken");
    };

    let dr = match dr_opt {
        Some(d) => d,
        None => {
            if let Some(id) = symbol {
                let suggestions = suggest_symbols(&g, id, 5);
                drop(g);
                return if suggestions.is_empty() {
                    Err(ToolError(format!("symbol not found: {id:?}")))
                } else {
                    Err(ToolError(format!(
                        "symbol not found: {id:?}. Did you mean: {suggestions}?"
                    )))
                };
            }
            if let Some(name) = class {
                let class_like = suggest_class_symbols(&g, name, 5);
                drop(g);
                return if class_like.is_empty() {
                    Err(ToolError(format!("class not found: {name:?}")))
                } else {
                    let suggestions = class_like.join(", ");
                    Err(ToolError(format!(
                        "class not found: {name:?}. Did you mean: {suggestions}?"
                    )))
                };
            }
            let path = file.expect("exactly-one-of guarantees file is Some on this branch");
            drop(g);
            return Err(ToolError(format!("file not found: {path:?}")));
        }
    };
    drop(g);

    match format {
        "edges" => Ok(ToolOk::Value(dr.edges)),
        "mermaid" => {
            let rendered = dr.render_mermaid("TD", input.styled);
            Ok(ToolOk::Text(rendered))
        }
        _ => unreachable!("format validation rejects everything else above"),
    }
}

// ----- detect_communities -----

/// `detect_communities` body (`Designs/GraphQueries` Decision 4/9). Body
/// moved verbatim from `handlers::structure::detect_communities`, plus
/// the core `require_indexed` call at entry (Decision 8).
#[allow(clippy::too_many_arguments)]
pub fn detect_communities(
    graph: &RwLock<Graph>,
    indexed: bool,
    granularity: Option<&str>,
    max_iterations: Option<u32>,
    members_per_community: Option<u32>,
    limit: Option<u32>,
    offset: Option<u32>,
    max_bytes: usize,
) -> ToolResult<DetectCommunitiesResponse> {
    require_indexed(indexed)?;

    let resolved_granularity = match granularity.filter(|s| !s.is_empty()) {
        None | Some("file") => "file",
        Some(other) => {
            return Err(ToolError(format!(
                "invalid granularity: {other:?}; expected \"file\""
            )))
        }
    };

    let resolved_max_iterations = max_iterations.filter(|&n| n != 0).unwrap_or(50).min(500);
    let resolved_members_cap = members_per_community
        .filter(|&n| n != 0)
        .unwrap_or(10)
        .min(100);
    let resolved_limit = limit.filter(|&n| n != 0).unwrap_or(100).min(1000);
    let resolved_offset = offset.unwrap_or(0);

    let result = graph.read().file_communities(resolved_max_iterations);

    let total = result.communities.len() as u32;

    let communities: Vec<Community> = result
        .communities
        .iter()
        .map(|c| {
            let original_len = c.members.len() as u32;
            let mut members: Vec<String> = c
                .members
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            let truncated = original_len > resolved_members_cap;
            if truncated {
                members.truncate(resolved_members_cap as usize);
            }
            Community {
                label: c.label.clone(),
                size: original_len,
                members,
                truncated,
                original_len: if truncated { Some(original_len) } else { None },
            }
        })
        .collect();

    let (results, _total_kept, truncated, next_offset) =
        byte_budget_take(communities, resolved_offset, resolved_limit, max_bytes);

    let (termination, iterations) = match result.termination {
        code_graph_graph::Termination::Converged { iterations } => ("converged", iterations),
        code_graph_graph::Termination::IterationCeiling { iterations } => {
            ("iteration_ceiling", iterations)
        }
    };

    let degenerate = result.degeneracy.map(|d| match d {
        code_graph_graph::Degeneracy::Giant { share_permille } => DegenerateInfo {
            kind: "giant",
            share_permille: Some(share_permille),
        },
        code_graph_graph::Degeneracy::Atomized => DegenerateInfo {
            kind: "atomized",
            share_permille: None,
        },
    });

    let response = DetectCommunitiesResponse {
        page: Page::<Community> {
            results,
            total,
            offset: resolved_offset,
            limit: resolved_limit,
            truncated,
            next_offset,
        },
        granularity: resolved_granularity,
        members_per_community: resolved_members_cap,
        termination,
        iterations,
        node_count: result.node_count,
        edge_count: result.edge_count,
        degenerate,
    };
    Ok(ToolOk::Value(response))
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_graph_core::{Confidence, Edge, EdgeKind, FileGraph, Language, Symbol};

    fn include_edge(from: &str, to: &str) -> Edge {
        Edge {
            from: from.to_string(),
            to: to.to_string(),
            kind: EdgeKind::Includes,
            file: from.to_string(),
            line: 1,
            confidence: Confidence::Resolved,
            candidates: 1,
            shape: Default::default(),
        }
    }

    fn sym(name: &str, kind: SymbolKind, file: &str) -> Symbol {
        Symbol {
            name: name.to_string(),
            kind,
            file: file.to_string(),
            line: 1,
            column: 0,
            end_line: 1,
            signature: format!("sig {name}"),
            namespace: String::new(),
            parent: String::new(),
            language: Language::Cpp,
        }
    }

    fn locked(g: Graph) -> RwLock<Graph> {
        RwLock::new(g)
    }

    fn assert_unindexed_error<T>(result: ToolResult<T>) {
        match result {
            Err(e) => assert_eq!(e.0, "no codebase indexed — call analyze_codebase first"),
            Ok(_) => panic!("unindexed call must error"),
        }
    }

    // --- AC-28: unindexed => Err(ToolError) for every gated function ---

    #[test]
    fn detect_cycles_unindexed_returns_typed_error() {
        let g = locked(Graph::new());
        assert_unindexed_error(detect_cycles(&g, false, None, None, None, None));
    }

    #[test]
    fn get_orphans_unindexed_returns_typed_error() {
        let g = locked(Graph::new());
        assert_unindexed_error(get_orphans(
            &g,
            false,
            None,
            None,
            None,
            None,
            None,
            false,
            None,
            usize::MAX,
        ));
    }

    #[test]
    fn get_class_hierarchy_unindexed_returns_typed_error() {
        let g = locked(Graph::new());
        assert_unindexed_error(get_class_hierarchy(&g, false, "Foo", None, None));
    }

    #[test]
    fn find_class_candidates_unindexed_returns_typed_error() {
        let g = locked(Graph::new());
        assert_unindexed_error(find_class_candidates(&g, false, "Foo"));
    }

    #[test]
    fn get_coupling_unindexed_returns_typed_error() {
        let g = locked(Graph::new());
        assert_unindexed_error(get_coupling(
            &g,
            false,
            "/a.cpp",
            None,
            None,
            None,
            usize::MAX,
        ));
    }

    #[test]
    fn generate_diagram_unindexed_returns_typed_error() {
        let g = locked(Graph::new());
        let input = DiagramInput {
            symbol: Some("/a.cpp:foo"),
            ..Default::default()
        };
        assert_unindexed_error(generate_diagram(&g, false, input));
    }

    #[test]
    fn detect_communities_unindexed_returns_typed_error() {
        let g = locked(Graph::new());
        assert_unindexed_error(detect_communities(
            &g,
            false,
            None,
            None,
            None,
            None,
            None,
            usize::MAX,
        ));
    }

    #[test]
    fn detect_communities_echoes_resolved_members_per_community() {
        let g = locked(graph_with_a_calls_b());
        for (requested, expected) in [(None, 10), (Some(0), 10), (Some(101), 100)] {
            match detect_communities(&g, true, None, None, requested, None, None, usize::MAX) {
                Ok(ToolOk::Value(response)) => {
                    assert_eq!(response.members_per_community, expected);
                }
                Ok(ToolOk::Text(text)) => panic!("detect_communities must not return text: {text}"),
                Err(error) => panic!("detect_communities must succeed: {}", error.0),
            }
        }
    }

    // --- generate_diagram: mermaid -> Text, edges -> Value (Decision 2) ---

    fn graph_with_a_calls_b() -> Graph {
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/x.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![
                sym("a", SymbolKind::Function, "/x.cpp"),
                sym("b", SymbolKind::Function, "/x.cpp"),
            ],
            edges: vec![Edge {
                from: "/x.cpp:a".to_string(),
                to: "/x.cpp:b".to_string(),
                kind: EdgeKind::Calls,
                file: "/x.cpp".to_string(),
                line: 1,
                confidence: Confidence::Resolved,
                candidates: 1,
                shape: Default::default(),
            }],
        });
        g
    }

    #[test]
    fn generate_diagram_mermaid_returns_text_success() {
        let g = locked(graph_with_a_calls_b());
        let input = DiagramInput {
            symbol: Some("/x.cpp:a"),
            format: Some("mermaid"),
            ..Default::default()
        };
        match generate_diagram(&g, true, input) {
            Ok(ToolOk::Text(s)) => assert!(s.contains("flowchart") || s.contains("graph")),
            Ok(ToolOk::Value(_)) => panic!("mermaid format must not yield Value"),
            Err(e) => panic!("mermaid format must not error: {}", e.0),
        }
    }

    #[test]
    fn generate_diagram_edges_returns_value_success() {
        let g = locked(graph_with_a_calls_b());
        let input = DiagramInput {
            symbol: Some("/x.cpp:a"),
            format: Some("edges"),
            ..Default::default()
        };
        match generate_diagram(&g, true, input) {
            Ok(ToolOk::Value(edges)) => assert_eq!(edges.len(), 1),
            Ok(ToolOk::Text(s)) => panic!("edges format must not yield Text: {s}"),
            Err(e) => panic!("edges format must not error: {}", e.0),
        }
    }

    // --- get_coupling(direction="both"): sequential byte-budget split ---

    fn graph_with_coupled_files(n: usize) -> Graph {
        let mut g = Graph::new();
        let mut main_edges = Vec::new();
        for i in 0..n {
            let other = format!("/other_{i:03}.cpp");
            g.merge_file_graph(FileGraph {
                path: other.clone(),
                language: Language::Cpp,
                symbols: vec![],
                edges: vec![include_edge(&other, "/main.cpp")],
            });
            // /main.cpp also has its own outgoing edges into each `other`
            // file, so `direction=both` has non-empty pages on BOTH
            // sides — the budget-starvation assertion below distinguishes
            // "outgoing legitimately empty" from "outgoing starved by the
            // sequential split".
            main_edges.push(include_edge("/main.cpp", &other));
        }
        g.merge_file_graph(FileGraph {
            path: "/main.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![],
            edges: main_edges,
        });
        g
    }

    #[test]
    fn get_coupling_both_sequential_budget_split() {
        // Many incoming edges into /main.cpp, tight max_bytes so incoming
        // alone should consume most/all of the budget, forcing outgoing to
        // degrade to the start-fresh empty-page marker.
        let g = locked(graph_with_coupled_files(50));
        let result = get_coupling(&g, true, "/main.cpp", Some("both"), None, None, 200);
        match result {
            Ok(ToolOk::Value(CouplingResult::Both(both))) => {
                assert!(both.incoming.total > 0, "incoming should see edges");
                if both.incoming.truncated {
                    // Sequential split: outgoing must have been starved to
                    // the "start fresh" marker.
                    assert!(both.outgoing.results.is_empty());
                    assert!(both.outgoing.truncated);
                    assert_eq!(both.outgoing.next_offset, Some(0));
                }
            }
            Ok(ToolOk::Value(CouplingResult::Single(_))) => {
                panic!("direction=both must yield CouplingResult::Both")
            }
            Ok(ToolOk::Text(s)) => panic!("get_coupling must not yield Text: {s}"),
            Err(e) => panic!("get_coupling(both) must not error: {}", e.0),
        }
    }

    #[test]
    fn get_coupling_both_generous_budget_both_sides_present() {
        let g = locked(graph_with_coupled_files(3));
        let result = get_coupling(&g, true, "/main.cpp", Some("both"), None, None, usize::MAX);
        match result {
            Ok(ToolOk::Value(CouplingResult::Both(both))) => {
                assert_eq!(both.incoming.total, 3);
                assert!(!both.incoming.truncated);
                assert!(!both.outgoing.truncated);
            }
            _ => panic!("expected Ok(Value(Both(_)))"),
        }
    }
}
