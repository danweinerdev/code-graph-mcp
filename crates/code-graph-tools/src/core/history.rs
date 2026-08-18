//! Typed core for the history tools (Track D, phase 5).
//!
//! `blame_symbol` is the first history feature: a graph span lookup plus a
//! line-range blame through the provider registry (Designs/VcsHistory
//! Decision 4). The provider is reached through `code-graph-vcs` only —
//! no VCS backend dependency enters this crate (AC-23).

use std::path::{Path, PathBuf};

use code_graph_graph::Graph;
use code_graph_vcs::{VcsError, VcsRegistry};
use parking_lot::RwLock;
use serde::Serialize;

use crate::handlers::suggest_symbols;

use super::{require_indexed, ToolError, ToolOk, ToolResult};

/// One contiguous attribution range within the symbol's span.
#[derive(Debug, Serialize)]
pub struct BlameHunkRow {
    /// Provider-native revision identity (opaque; display-only).
    pub rev: String,
    /// Provider-reported author display value.
    pub author: String,
    /// Provider-reported UTC Unix timestamp in seconds.
    pub timestamp_utc: i64,
    /// One-based first line of this range in the blamed file.
    pub start_line: u32,
    /// Number of lines in this range.
    pub line_count: u32,
}

/// `blame_symbol` response body — a single JSON object, not a `Page`.
///
/// Unavailability is a SUCCESS shape (FR-36): `available: false` with a
/// `reason` covers both "no supported VCS here" and "this path has no
/// history at the blamed revision"; the tool error channel is reserved for
/// bad input (unknown symbol, unresolvable `at`) and genuine provider
/// failures.
#[derive(Debug, Serialize)]
pub struct BlameSymbolResponse {
    /// `false` when history cannot be attributed for this symbol — no
    /// supported VCS at the indexed root, or the file is untracked at the
    /// blamed revision. Always paired with `reason`.
    pub available: bool,
    /// Why history is unavailable. Absent (not `null`) when `available`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The requested symbol ID, echoed.
    pub symbol_id: String,
    /// Absolute file path the span refers to (the graph's stored path).
    pub file: String,
    /// One-based first line of the symbol's span, from the graph.
    pub start_line: u32,
    /// One-based last line of the symbol's span, from the graph.
    pub end_line: u32,
    /// The revision attribution reflects: the resolved `at` argument, or
    /// the provider's default revision (Git: `HEAD`). `null` when
    /// unavailable or when the default could not be resolved (blame then
    /// still ran against the provider default).
    pub rev: Option<String>,
    /// `true` when the on-disk file bytes differ from the file at the
    /// blamed revision. The graph's span comes from the on-disk file while
    /// attribution reflects the committed state, so a `true` here means
    /// line numbers may misalign between the two. `false` means no
    /// divergence was detected.
    pub stale: bool,
    /// Human-readable explanation when `stale` is `true`. Absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_reason: Option<String>,
    /// Attribution ranges clipped to `[start_line, end_line]`, in file
    /// order. Empty when `available` is `false`.
    pub hunks: Vec<BlameHunkRow>,
}

/// The graph facts `blame_symbol` needs, extracted under the read guard so
/// no lock is held across a provider `await`.
struct SymbolSpan {
    file: String,
    start_line: u32,
    end_line: u32,
}

fn unavailable(
    symbol_id: &str,
    span: &SymbolSpan,
    reason: String,
) -> ToolResult<BlameSymbolResponse> {
    Ok(ToolOk::Value(BlameSymbolResponse {
        available: false,
        reason: Some(reason),
        symbol_id: symbol_id.to_string(),
        file: span.file.clone(),
        start_line: span.start_line,
        end_line: span.end_line,
        rev: None,
        stale: false,
        stale_reason: None,
        hunks: Vec::new(),
    }))
}

/// `blame_symbol` body: resolve the symbol's span from the graph, then
/// attribute it through the detected provider.
///
/// Attribution reflects the blamed revision's committed state (task 5.5's
/// `blame` contract), while the span comes from the on-disk file the graph
/// indexed; `stale` reports when those two file states diverge, detected by
/// comparing on-disk bytes against [`code_graph_vcs::VcsProvider::read_at`]
/// for the blamed revision. The default-revision echo resolves the
/// provider's `"HEAD"` spec and degrades to `rev: null` (with blame still
/// running at the provider default) if that spec does not resolve.
pub async fn blame_symbol(
    graph: &RwLock<Graph>,
    vcs: &VcsRegistry,
    indexed: bool,
    root: Option<PathBuf>,
    symbol: &str,
    at: Option<&str>,
) -> ToolResult<BlameSymbolResponse> {
    require_indexed(indexed)?;

    if symbol.is_empty() {
        return Err(ToolError("'symbol' is required".to_string()));
    }

    // Span lookup under the read guard, dropped before any await: provider
    // calls must not run under the graph lock, and the returned future must
    // stay `Send` (parking_lot guards are not).
    let span = {
        let g = graph.read();
        match g.symbol_detail(symbol) {
            Some(s) => SymbolSpan {
                file: s.file.clone(),
                start_line: s.line,
                end_line: s.end_line.max(s.line),
            },
            None => {
                let suggestions = suggest_symbols(&g, symbol, 5);
                return Err(if suggestions.is_empty() {
                    ToolError(format!("symbol not found: {symbol:?}"))
                } else {
                    ToolError(format!(
                        "symbol not found: {symbol:?}. Did you mean: {suggestions}?"
                    ))
                });
            }
        }
    };

    let Some(root) = root else {
        return unavailable(
            symbol,
            &span,
            "no indexed root recorded; re-run analyze_codebase".to_string(),
        );
    };
    let Some(provider) = vcs.detect(&root) else {
        return unavailable(
            symbol,
            &span,
            format!(
                "no supported version-control system detected at {}",
                root.display()
            ),
        );
    };

    // Resolve the revision attribution will reflect. An explicit `at` that
    // does not resolve is a caller error; the default-revision echo is
    // best-effort ("HEAD" is resolved purely so the response can NAME the
    // revision — blame itself falls back to the provider default).
    let resolved = match at.filter(|spec| !spec.is_empty()) {
        Some(spec) => match provider.resolve_rev(spec).await {
            Ok(rev) => Some(rev),
            Err(error) => {
                return Err(ToolError(format!(
                    "cannot resolve revision {spec:?}: {error}"
                )))
            }
        },
        None => provider.resolve_rev("HEAD").await.ok(),
    };

    let path = Path::new(&span.file);
    let hunks = match provider
        .blame(
            path,
            Some((span.start_line, span.end_line)),
            resolved.as_ref(),
        )
        .await
    {
        Ok(hunks) => hunks,
        // Untracked path (or absent at the blamed revision): history is
        // unavailable for this file — a success, distinct from "no VCS
        // here" (Designs/VcsHistory error table).
        Err(VcsError::NotFound(error)) => {
            return unavailable(
                symbol,
                &span,
                format!("no history for this path at the blamed revision: {error}"),
            )
        }
        Err(VcsError::Unavailable(error)) => {
            return unavailable(symbol, &span, format!("history unavailable: {error}"))
        }
        Err(error) => return Err(ToolError(format!("blame failed: {error}"))),
    };

    // Staleness: the span is derived from the on-disk file; attribution
    // reflects the blamed revision. Divergence between the two means line
    // numbers may misalign — report it, never silently return possibly
    // misaligned attribution (Designs/VcsHistory Decision 4).
    let (stale, stale_reason) = match &resolved {
        Some(rev) => match provider.read_at(rev, path).await {
            Ok(committed) => match std::fs::read(path) {
                Ok(on_disk) if on_disk == committed => (false, None),
                Ok(_) => (
                    true,
                    Some(format!(
                        "on-disk contents differ from revision {rev}; the symbol span \
                         comes from the working tree, so attributed line numbers may \
                         misalign — commit the edits or blame with `at` set to a \
                         revision matching the indexed state"
                    )),
                ),
                Err(error) => (
                    true,
                    Some(format!(
                        "file unreadable on disk ({error}); attribution reflects \
                         revision {rev}"
                    )),
                ),
            },
            // Blame succeeded but the revision's bytes are unreadable —
            // nothing to compare against, so no divergence is *detected*.
            Err(_) => (false, None),
        },
        None => (false, None),
    };

    Ok(ToolOk::Value(BlameSymbolResponse {
        available: true,
        reason: None,
        symbol_id: symbol.to_string(),
        rev: resolved.map(|rev| rev.to_string()),
        stale,
        stale_reason,
        hunks: hunks
            .into_iter()
            .map(|hunk| BlameHunkRow {
                rev: hunk.rev.to_string(),
                author: hunk.author,
                timestamp_utc: hunk.timestamp_utc,
                start_line: hunk.start_line,
                line_count: hunk.line_count,
            })
            .collect(),
        file: span.file,
        start_line: span.start_line,
        end_line: span.end_line,
    }))
}
