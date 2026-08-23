//! Typed core for the history tools (Track D, phase 5).
//!
//! `blame_symbol` is the first history feature: a graph span lookup plus a
//! line-range blame through the provider registry (Designs/VcsHistory
//! Decision 4). The provider is reached through `code-graph-vcs` only —
//! no VCS backend dependency enters this crate (AC-23).

use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(debug_assertions)]
use std::sync::{Mutex, OnceLock};

use code_graph_graph::Graph;
use code_graph_lang::FingerprintMode;
use code_graph_vcs::{VcsDetection, VcsError, VcsProvider, VcsRegistry};
use parking_lot::RwLock;
use serde::Serialize;

use crate::handlers::{kind_str, suggest_symbols};
use crate::server::ServerInner;

use super::fingerprint_cache::{Cached, FingerprintCache, FingerprintKey};
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
    /// the provider's default revision. `null` when
    /// unavailable or when the default could not be resolved (blame then
    /// still ran against the provider default).
    pub rev: Option<String>,
    /// `true` when the on-disk file contents diverge from the file at the
    /// blamed revision (compared line-ending-insensitively, so autocrlf
    /// checkouts stay clean). The graph's span comes from the on-disk file
    /// while attribution reflects the committed state, so `true` means line
    /// numbers may misalign between the two. `false` means no divergence
    /// was detected — which is only "verified clean" when `stale_reason`
    /// is absent.
    pub stale: bool,
    /// Present when `stale` is `true` (explains the divergence), when the
    /// comparison could not be performed (`stale` stays `false` but the
    /// span is unverified), or when the span has no attributable lines at
    /// the blamed revision. Absent means verified clean.
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

/// Byte comparison with `\r` stripped from both sides. autocrlf/eol-filtered
/// checkouts materialize LF blobs as CRLF on disk; that is not a divergence
/// in the lines blame attributes, and treating it as one would make `stale`
/// permanently true on Windows-normalized repositories (gate review F2).
/// Other smudge filters (ident, LFS) can still report stale — rare for
/// source files, and honest when they do.
fn eol_insensitive_eq(on_disk: &[u8], committed: &[u8]) -> bool {
    on_disk
        .iter()
        .filter(|&&byte| byte != b'\r')
        .eq(committed.iter().filter(|&&byte| byte != b'\r'))
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

/// Select a provider at the async detection boundary. Detection is deliberately
/// repeated for each history request: a registry never caches a selection
/// across repository/worktree changes.
enum SelectionFailure {
    Unavailable(String),
    Operational(VcsError),
}

async fn select_provider(
    vcs: &VcsRegistry,
    root: &Path,
) -> Result<Arc<dyn VcsProvider>, SelectionFailure> {
    match vcs.detect(root).await.map_err(SelectionFailure::Operational)? {
        VcsDetection::Selected(provider) => Ok(provider),
        VcsDetection::NoProvider => Err(SelectionFailure::Unavailable(format!(
            "no supported version-control system detected at {}",
            root.display()
        ))),
        VcsDetection::ProviderBoundElsewhere { provider } => Err(SelectionFailure::Unavailable(format!(
            "provider bound elsewhere: {provider} recognizes this root but is bound to a different repository"
        ))),
        VcsDetection::DifferentCheckout { provider } => Err(SelectionFailure::Unavailable(format!(
            "different checkout: {provider} is bound to another linked worktree of this repository"
        ))),
    }
}

/// `blame_symbol` body: resolve the symbol's span from the graph, then
/// attribute it through the detected provider.
///
/// Attribution reflects the blamed revision's committed state (task 5.5's
/// `blame` contract), while the span comes from the on-disk file the graph
/// indexed; `stale` reports when those two file states diverge, detected by
/// comparing on-disk bytes against [`code_graph_vcs::VcsProvider::read_at`]
/// for the blamed revision. The default-revision echo resolves the
/// provider's default revision and degrades to `rev: null` (with blame still
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
    let provider = match select_provider(vcs, &root).await {
        Ok(provider) => provider,
        Err(SelectionFailure::Unavailable(reason)) => return unavailable(symbol, &span, reason),
        Err(SelectionFailure::Operational(error)) => {
            return Err(ToolError(format!("detect VCS provider failed: {error}")))
        }
    };

    // Resolve the revision attribution will reflect. An explicit `at` that
    // does not resolve is a caller error; the default-revision echo is
    // best-effort (the provider's default is resolved purely so the response can NAME the
    // revision — blame itself falls back to the provider default).
    let resolved = match at.filter(|spec| !spec.is_empty()) {
        Some(spec) => match provider.resolve_rev(Some(spec)).await {
            Ok(rev) => Some(rev),
            Err(error) => {
                return Err(ToolError(format!(
                    "cannot resolve revision {spec:?}: {error}"
                )))
            }
        },
        None => provider.resolve_rev(None).await.ok(),
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
    // misaligned attribution (Designs/VcsHistory Decision 4). When the
    // comparison itself is impossible, say so explicitly instead of
    // letting `stale: false` read as "verified clean" (gate review F6).
    let (stale, mut stale_reason) = match &resolved {
        Some(rev) => match provider.read_at(rev, path).await {
            Ok(committed) => match tokio::fs::read(path).await {
                Ok(on_disk) if eol_insensitive_eq(&on_disk, &committed) => (false, None),
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
            // no divergence was DETECTED, which is not the same as
            // verified clean.
            Err(error) => (
                false,
                Some(format!(
                    "staleness not verified: the blamed revision's contents could \
                     not be read for comparison ({error})"
                )),
            ),
        },
        None => (
            false,
            Some(
                "staleness not verified: the provider's default revision did not \
                 resolve, so on-disk contents were not compared"
                    .to_string(),
            ),
        ),
    };

    // A span lying wholly beyond the blamed revision's EOF clamps to zero
    // attributable lines (gate review F4). Make the empty page carry its
    // own signal rather than looking like a quietly successful blame.
    if hunks.is_empty() {
        let empty_span_reason =
            "the symbol's span has no attributable lines at the blamed revision \
             (the file is shorter there than the on-disk span)";
        match &mut stale_reason {
            Some(reason) => {
                reason.push_str("; ");
                reason.push_str(empty_span_reason);
            }
            None => stale_reason = Some(empty_span_reason.to_string()),
        }
    }

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

/// Default revision window for `symbol_history` (OQ-D2: the bound and the
/// partial-result flags are fixed; the default is tunable).
pub const SYMBOL_HISTORY_DEFAULT_WINDOW: u32 = 50;
/// Ceiling on the revision window; larger requests clamp silently and the
/// resolved value is echoed.
pub const SYMBOL_HISTORY_MAX_WINDOW: u32 = 500;

/// One reported transition in a symbol's history, oldest first.
#[derive(Debug, Serialize)]
pub struct SymbolHistoryEntry {
    /// Provider-native revision identity (display-only).
    pub rev: String,
    /// Provider-reported author display value.
    pub author: String,
    /// Provider-reported UTC Unix timestamp in seconds.
    pub timestamp_utc: i64,
    /// Provider-reported one-line revision summary.
    pub summary: String,
    /// `"introduced"`, `"modified"`, or `"removed"`.
    pub change: &'static str,
    /// `true` only on an `introduced` entry at the window's oldest
    /// examined revision when older history may exist — the window filled,
    /// the provider truncated, or every older windowed revision was
    /// skipped — the symbol was *present at the window boundary*, which is
    /// indistinguishable from a genuine introduction.
    pub at_window_boundary: bool,
}

/// A revision the walk could not evaluate; the transition state carries
/// over it unchanged.
#[derive(Debug, Serialize)]
pub struct SkippedRevision {
    pub rev: String,
    pub reason: String,
}

/// `symbol_history` response body — a single JSON object, not a `Page`.
///
/// Unavailability is a SUCCESS shape (FR-36), exactly like `blame_symbol`:
/// `available: false` + `reason` covers "no supported VCS", "no committed
/// history for this path", and "no indexed root". The tool-error channel is
/// reserved for bad input (unknown symbol, unknown mode, a mode the
/// language cannot fingerprint yet) and genuine provider failures.
#[derive(Debug, Serialize)]
pub struct SymbolHistoryResponse {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The requested symbol ID, echoed.
    pub symbol_id: String,
    /// Absolute file path the history was walked for.
    pub file: String,
    /// Resolved fingerprint mode (`"normalized"` today).
    pub mode: String,
    /// Resolved window size (requests clamp to the ceiling; `0` means the
    /// default).
    pub window: u32,
    /// Revisions the walk considered, including skipped ones.
    pub revisions_examined: u32,
    /// The window filled to `window` revisions — older revisions touching
    /// this file MAY exist beyond the window (a history exactly `window`
    /// revisions long also sets this; the flag is conservative).
    pub window_filled: bool,
    /// The provider stopped before exhausting older history, such as at an
    /// internal examination cap or shallow boundary (distinct from
    /// `window_filled`).
    pub history_truncated: bool,
    /// Transitions only, oldest first. Revisions where the symbol did not
    /// change are deliberately absent — that filtering is the tool's value.
    pub entries: Vec<SymbolHistoryEntry>,
    /// Revisions skipped as unreadable or unparseable, in walk order.
    pub skipped: Vec<SkippedRevision>,
}

fn history_unavailable(
    symbol_id: &str,
    file: &str,
    mode: FingerprintMode,
    window: u32,
    reason: String,
) -> ToolResult<SymbolHistoryResponse> {
    Ok(ToolOk::Value(SymbolHistoryResponse {
        available: false,
        reason: Some(reason),
        symbol_id: symbol_id.to_string(),
        file: file.to_string(),
        mode: mode_wire(mode).to_string(),
        window,
        revisions_examined: 0,
        window_filled: false,
        history_truncated: false,
        entries: Vec::new(),
        skipped: Vec::new(),
    }))
}

fn mode_wire(mode: FingerprintMode) -> &'static str {
    match mode {
        FingerprintMode::Normalized => "normalized",
        FingerprintMode::LiteralInsensitive => "literal_insensitive",
    }
}

/// Revision records and cache probes are batched at eight. Because `read_at`
/// returns an opaque whole buffer, a batch retains at most one source: it is
/// flushed before another source read begins. This makes the 32 MiB target
/// honest without changing the four-operation provider trait.
const SOURCE_BATCH_MAX_SNAPSHOTS: usize = 8;
const SOURCE_BATCH_MAX_BYTES: usize = 32 * 1024 * 1024;

/// Internal debug-test instrumentation; it is not part of any tool response.
#[cfg(debug_assertions)]
#[derive(Clone, Copy, Debug, Default)]
#[doc(hidden)]
pub struct HistoryWorkMetrics {
    pub retained_sources_high_water: usize,
    pub retained_source_bytes_high_water: usize,
    pub non_oversized_source_bytes_high_water: usize,
    pub oversized_sources_admitted_alone: usize,
    pub active_ast_high_water: usize,
}

#[cfg(debug_assertions)]
#[derive(Default)]
struct MutableHistoryWorkMetrics {
    metrics: HistoryWorkMetrics,
    retained_sources: usize,
    retained_source_bytes: usize,
    active_ast: usize,
}

#[cfg(debug_assertions)]
type WorkMeter = Arc<Mutex<MutableHistoryWorkMetrics>>;

#[cfg(debug_assertions)]
static LAST_COMPLETED_WORK_METRICS: OnceLock<Mutex<HistoryWorkMetrics>> = OnceLock::new();

#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn reset_history_work_metrics_for_test() {
    *LAST_COMPLETED_WORK_METRICS
        .get_or_init(|| Mutex::new(HistoryWorkMetrics::default()))
        .lock()
        .expect("history work metrics mutex poisoned") = HistoryWorkMetrics::default();
}

#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn history_work_metrics_for_test() -> HistoryWorkMetrics {
    *LAST_COMPLETED_WORK_METRICS
        .get_or_init(|| Mutex::new(HistoryWorkMetrics::default()))
        .lock()
        .expect("history work metrics mutex poisoned")
}

#[cfg(debug_assertions)]
struct SourceSnapshotGuard {
    meter: WorkMeter,
    bytes: usize,
}

#[cfg(debug_assertions)]
impl SourceSnapshotGuard {
    fn admit(meter: WorkMeter, bytes: usize) -> Self {
        let mut state = meter.lock().expect("history work metrics mutex poisoned");
        debug_assert_eq!(state.retained_sources, 0);
        state.retained_sources = 1;
        state.retained_source_bytes += bytes;
        state.metrics.retained_sources_high_water = state
            .metrics
            .retained_sources_high_water
            .max(state.retained_sources);
        state.metrics.retained_source_bytes_high_water = state
            .metrics
            .retained_source_bytes_high_water
            .max(state.retained_source_bytes);
        if bytes <= SOURCE_BATCH_MAX_BYTES {
            state.metrics.non_oversized_source_bytes_high_water = state
                .metrics
                .non_oversized_source_bytes_high_water
                .max(state.retained_source_bytes);
        }
        debug_assert!(state.retained_sources <= SOURCE_BATCH_MAX_SNAPSHOTS);
        debug_assert!(
            state.retained_source_bytes <= SOURCE_BATCH_MAX_BYTES || state.retained_sources == 1
        );
        if bytes > SOURCE_BATCH_MAX_BYTES {
            state.metrics.oversized_sources_admitted_alone += 1;
        }
        drop(state);
        Self { meter, bytes }
    }
}

#[cfg(debug_assertions)]
impl Drop for SourceSnapshotGuard {
    fn drop(&mut self) {
        let mut state = self
            .meter
            .lock()
            .expect("history work metrics mutex poisoned");
        state.retained_sources -= 1;
        state.retained_source_bytes -= self.bytes;
    }
}

#[cfg(debug_assertions)]
struct AstWorkGuard(WorkMeter);

#[cfg(debug_assertions)]
impl AstWorkGuard {
    fn begin(meter: WorkMeter) -> Self {
        let mut state = meter.lock().expect("history work metrics mutex poisoned");
        state.active_ast += 1;
        state.metrics.active_ast_high_water =
            state.metrics.active_ast_high_water.max(state.active_ast);
        drop(state);
        Self(meter)
    }
}

#[cfg(debug_assertions)]
impl Drop for AstWorkGuard {
    fn drop(&mut self) {
        self.0
            .lock()
            .expect("history work metrics mutex poisoned")
            .active_ast -= 1;
    }
}

/// Debug-test seam for deterministic slow parser/fingerprint coverage.
#[cfg(debug_assertions)]
type ParserHook = Arc<dyn Fn() + Send + Sync>;

#[cfg(debug_assertions)]
static PARSER_HOOK: OnceLock<Mutex<Option<ParserHook>>> = OnceLock::new();

#[cfg(debug_assertions)]
#[doc(hidden)]
pub struct ParserHookGuard(Option<ParserHook>);

#[cfg(debug_assertions)]
impl Drop for ParserHookGuard {
    fn drop(&mut self) {
        *PARSER_HOOK
            .get_or_init(|| Mutex::new(None))
            .lock()
            .expect("history parser test hook mutex poisoned") = self.0.take();
    }
}

#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn set_parser_hook_for_test(hook: ParserHook) -> ParserHookGuard {
    let previous = (*PARSER_HOOK
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("history parser test hook mutex poisoned"))
    .replace(hook);
    ParserHookGuard(previous)
}

enum SnapshotInput {
    Cached(Option<u64>),
    /// The provider reports the path does not exist at this revision. This is
    /// an examined absence, not uncertainty.
    Absent,
    /// The provider could not read an otherwise unknown snapshot.
    ReadFailed(String),
    /// One owned source buffer, metered from `read_at` return until the batch
    /// drops it after parsing.
    Bytes(SourceSnapshot),
}

struct SourceSnapshot {
    bytes: Vec<u8>,
    oversized: bool,
    #[cfg(debug_assertions)]
    _guard: SourceSnapshotGuard,
}

impl SourceSnapshot {
    fn new(bytes: Vec<u8>, #[cfg(debug_assertions)] meter: WorkMeter) -> Self {
        let oversized = bytes.len() > SOURCE_BATCH_MAX_BYTES;
        #[cfg(debug_assertions)]
        let guard = SourceSnapshotGuard::admit(meter, bytes.len());
        Self {
            bytes,
            oversized,
            #[cfg(debug_assertions)]
            _guard: guard,
        }
    }
}

/// `symbol_history` body: when did this symbol's *content* actually change,
/// as opposed to when was its file touched (FR-33).
///
/// The walk runs oldest to newest over a bounded window of revisions
/// touching the symbol's file, computes the symbol's fingerprint at each
/// (exact, case-sensitive `(name, kind)` match — D-0005: any rename,
/// including case-only, reports as `removed` + `introduced`), and emits
/// only transitions. Cache shard reads and each parse/fingerprint run in
/// Tokio blocking tasks. Revision records are batched oldest-to-newest at
/// eight, but opaque whole-buffer reads retain at most one source at a time;
/// parsing drops that source's AST before the next source read is admitted.
pub async fn symbol_history(
    inner: &Arc<ServerInner>,
    indexed: bool,
    symbol: &str,
    mode: Option<&str>,
    window: Option<u32>,
) -> ToolResult<SymbolHistoryResponse> {
    require_indexed(indexed)?;

    if symbol.is_empty() {
        return Err(ToolError("'symbol' is required".to_string()));
    }
    let mode = match mode.unwrap_or("normalized") {
        "" | "normalized" => FingerprintMode::Normalized,
        "literal_insensitive" => FingerprintMode::LiteralInsensitive,
        other => {
            return Err(ToolError(format!(
                "unknown fingerprint mode {other:?}: use \"normalized\" or \
                 \"literal_insensitive\""
            )))
        }
    };
    // Phase 8 removed the upfront `literal_insensitive` rejection: every
    // shipped language plugin carries an AST override supporting both
    // modes, so the mode flows through. Per-span unavailability (an
    // unlocatable span — synthesized symbols, error-recovered regions — or
    // a hypothetical future plugin without an override) surfaces as a
    // VISIBLE skip in the walk, never as a silent fallback to Normalized
    // (Designs/VcsHistory Decision 5).
    let window = match window {
        None | Some(0) => SYMBOL_HISTORY_DEFAULT_WINDOW,
        Some(requested) => requested.min(SYMBOL_HISTORY_MAX_WINDOW),
    };

    // Symbol facts under the read guard, dropped before any await.
    let (target_name, target_kind, file, language) = {
        let g = inner.graph.read();
        match g.symbol_detail(symbol) {
            Some(s) => (s.name, s.kind, s.file, s.language),
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

    let root = inner.root_path.read().clone();
    let Some(root) = root else {
        return history_unavailable(
            symbol,
            &file,
            mode,
            window,
            "no indexed root recorded; re-run analyze_codebase".to_string(),
        );
    };
    let provider = match select_provider(&inner.vcs, &root).await {
        Ok(provider) => provider,
        Err(SelectionFailure::Unavailable(reason)) => {
            return history_unavailable(symbol, &file, mode, window, reason)
        }
        Err(SelectionFailure::Operational(error)) => {
            return Err(ToolError(format!("detect VCS provider failed: {error}")))
        }
    };

    let revision_window = match provider.revisions_touching(Path::new(&file), window).await {
        Ok(revision_window) => revision_window,
        Err(VcsError::NotFound(error)) => {
            return history_unavailable(
                symbol,
                &file,
                mode,
                window,
                format!("no history for this path: {error}"),
            )
        }
        Err(VcsError::Unavailable(error)) => {
            return history_unavailable(
                symbol,
                &file,
                mode,
                window,
                format!("history unavailable: {error}"),
            )
        }
        Err(error) => return Err(ToolError(format!("list revisions failed: {error}"))),
    };
    if revision_window.commits.is_empty() {
        return history_unavailable(
            symbol,
            &file,
            mode,
            window,
            "no committed history for this path (untracked, or never committed)".to_string(),
        );
    }

    let history_truncated = revision_window.truncated;
    let mut commits = revision_window.commits;
    commits.reverse(); // provider returns newest first; the walk needs oldest first
    let window_filled = commits.len() as u32 == window;

    // The cache lives at the project root that owns the graph cache.
    let cache_root = inner
        .cache_root
        .read()
        .clone()
        .unwrap_or_else(|| root.clone());
    let cache = FingerprintCache::open(&cache_root);
    let relative_path = Path::new(&file)
        .strip_prefix(&cache_root)
        .map(|relative| relative.to_string_lossy().into_owned())
        .unwrap_or_else(|_| file.clone());
    let provider_id = provider.id().to_string();
    let kind = kind_str(target_kind);

    // Historical extraction must see the same config-driven pipeline the
    // indexer used (preprocess byte-rewrites + symbol synthesis) — without
    // it every `[cpp].macro_*`-dependent symbol would silently report an
    // empty history. The config is part of the cache key for the same
    // reason (see `FingerprintKey.config`).
    let config = inner.config.read().clone();
    let config_id = super::fingerprint_cache::config_identity(&config);

    let mut state = TransitionState::default();
    #[cfg(debug_assertions)]
    let meter: WorkMeter = Arc::new(Mutex::new(MutableHistoryWorkMetrics::default()));
    // Probe cache shards in bounded ordered batches. This keeps disk I/O out
    // of runtime workers without turning a 500-revision walk into a 500-source
    // prefetch. A cache hit has no source snapshot at all.
    for batch in commits.chunks(SOURCE_BATCH_MAX_SNAPSHOTS) {
        let cached = cache_get_batch(
            cache.clone(),
            batch.iter().map(|commit| commit.rev.to_string()).collect(),
            provider_id.clone(),
            relative_path.clone(),
            target_name.clone(),
            kind,
            mode,
            config_id,
        )
        .await?;

        let mut snapshots = Vec::with_capacity(batch.len());
        for (commit, cached) in batch.iter().zip(cached) {
            let input = match cached {
                Some(cached) => SnapshotInput::Cached(cached_value(cached)),
                None => {
                    // `read_at` returns a whole opaque Vec. Flush any prior
                    // source BEFORE awaiting another read, so the returned
                    // bytes can never coexist with a retained source.
                    if snapshots.iter().any(|snapshot: &SnapshotArgs| {
                        matches!(snapshot.input, SnapshotInput::Bytes(_))
                    }) {
                        state = process_snapshot_batch(
                            std::mem::take(&mut snapshots),
                            state,
                            window_filled,
                            history_truncated,
                        )
                        .await?;
                    }
                    match provider.read_at(&commit.rev, Path::new(&file)).await {
                        Ok(bytes) => SnapshotInput::Bytes(SourceSnapshot::new(
                            bytes,
                            #[cfg(debug_assertions)]
                            Arc::clone(&meter),
                        )),
                        Err(VcsError::NotFound(_)) => SnapshotInput::Absent,
                        Err(error) => SnapshotInput::ReadFailed(error.to_string()),
                    }
                }
            };
            let oversized_source =
                matches!(&input, SnapshotInput::Bytes(source) if source.oversized);
            if oversized_source && !snapshots.is_empty() {
                state = process_snapshot_batch(
                    std::mem::take(&mut snapshots),
                    state,
                    window_filled,
                    history_truncated,
                )
                .await?;
            }
            snapshots.push(SnapshotArgs {
                inner: Arc::clone(inner),
                commit: commit.clone(),
                input,
                file: file.clone(),
                target_name: target_name.clone(),
                target_kind,
                language,
                mode,
                cache: cache.clone(),
                relative_path: relative_path.clone(),
                provider_id: provider_id.clone(),
                kind,
                config: config.clone(),
                config_id,
                #[cfg(debug_assertions)]
                meter: Arc::clone(&meter),
            });
            // An oversized source is admitted and processed alone; normal
            // sources may share the record batch only with non-source rows.
            if oversized_source {
                state = process_snapshot_batch(
                    std::mem::take(&mut snapshots),
                    state,
                    window_filled,
                    history_truncated,
                )
                .await?;
            }
        }
        if !snapshots.is_empty() {
            state =
                process_snapshot_batch(snapshots, state, window_filled, history_truncated).await?;
        }
    }

    #[cfg(debug_assertions)]
    {
        *LAST_COMPLETED_WORK_METRICS
            .get_or_init(|| Mutex::new(HistoryWorkMetrics::default()))
            .lock()
            .expect("history work metrics mutex poisoned") = meter
            .lock()
            .expect("history work metrics mutex poisoned")
            .metrics;
    }

    Ok(ToolOk::Value(SymbolHistoryResponse {
        available: true,
        reason: None,
        symbol_id: symbol.to_string(),
        file,
        mode: mode_wire(mode).to_string(),
        window,
        revisions_examined: commits.len() as u32,
        window_filled,
        history_truncated,
        entries: state.entries,
        skipped: state.skipped,
    }))
}

fn cached_value(cached: Cached) -> Option<u64> {
    match cached {
        Cached::Fingerprint(fingerprint) => Some(fingerprint),
        Cached::Tombstone => None,
    }
}

#[allow(clippy::too_many_arguments)]
async fn cache_get_batch(
    cache: FingerprintCache,
    revisions: Vec<String>,
    provider_id: String,
    relative_path: String,
    target_name: String,
    kind: &'static str,
    mode: FingerprintMode,
    config_id: u64,
) -> Result<Vec<Option<Cached>>, ToolError> {
    tokio::task::spawn_blocking(move || {
        revisions
            .iter()
            .map(|rev| {
                cache.get(&FingerprintKey {
                    provider: &provider_id,
                    rev,
                    relative_path: &relative_path,
                    symbol_name: &target_name,
                    kind,
                    mode,
                    config: config_id,
                })
            })
            .collect()
    })
    .await
    .map_err(|error| ToolError(format!("symbol_history cache worker failed: {error}")))
}

/// Everything one blocking parse/fingerprint task needs, owned, so the
/// closure is `'static`. It owns exactly one source snapshot.
struct SnapshotArgs {
    inner: Arc<ServerInner>,
    commit: code_graph_vcs::Commit,
    input: SnapshotInput,
    file: String,
    target_name: String,
    target_kind: code_graph_core::SymbolKind,
    language: code_graph_core::Language,
    mode: FingerprintMode,
    cache: FingerprintCache,
    relative_path: String,
    provider_id: String,
    kind: &'static str,
    /// The effective root config: historical parses must run the same
    /// preprocess + synthesis pipeline the indexer ran.
    config: code_graph_core::RootConfig,
    /// [`super::fingerprint_cache::config_identity`] of `config`.
    config_id: u64,
    #[cfg(debug_assertions)]
    meter: WorkMeter,
}

enum SnapshotResult {
    Current(Option<u64>),
    Skipped(String),
}

#[derive(Default)]
struct TransitionState {
    entries: Vec<SymbolHistoryEntry>,
    skipped: Vec<SkippedRevision>,
    previous: Option<Option<u64>>,
}

fn apply_current(
    state: &mut TransitionState,
    commit: &code_graph_vcs::Commit,
    current: Option<u64>,
    window_filled: bool,
    history_truncated: bool,
) {
    let first_examined = state.previous.is_none();
    let before = state.previous.flatten();
    state.previous = Some(current);
    let change = match (before, current) {
        (None, Some(_)) => "introduced",
        (Some(_), None) if !first_examined => "removed",
        (Some(a), Some(b)) if a != b && !first_examined => "modified",
        _ => return,
    };
    state.entries.push(SymbolHistoryEntry {
        rev: commit.rev.to_string(),
        author: commit.author.clone(),
        timestamp_utc: commit.timestamp_utc,
        summary: commit.summary.clone(),
        change,
        at_window_boundary: first_examined
            && change == "introduced"
            && (window_filled || history_truncated || !state.skipped.is_empty()),
    });
}

/// Process an ordered source batch in one blocking task. Each `ParsedFile`
/// stays local to [`process_snapshot`] and drops before the next entry.
async fn process_snapshot_batch(
    snapshots: Vec<SnapshotArgs>,
    mut state: TransitionState,
    window_filled: bool,
    history_truncated: bool,
) -> Result<TransitionState, ToolError> {
    tokio::task::spawn_blocking(move || {
        for snapshot in snapshots {
            let commit = snapshot.commit.clone();
            match process_snapshot(snapshot)? {
                SnapshotResult::Current(current) => apply_current(
                    &mut state,
                    &commit,
                    current,
                    window_filled,
                    history_truncated,
                ),
                SnapshotResult::Skipped(reason) => state.skipped.push(SkippedRevision {
                    rev: commit.rev.to_string(),
                    reason,
                }),
            }
        }
        Ok(state)
    })
    .await
    .map_err(|error| ToolError(format!("symbol_history worker failed: {error}")))?
}

/// Parse and fingerprint one admitted source snapshot. The `ParsedFile` is a
/// local variable in this blocking closure, so its AST drops before the next
/// source snapshot can be processed.
fn process_snapshot(args: SnapshotArgs) -> Result<SnapshotResult, ToolError> {
    (|| {
        let SnapshotArgs {
            inner,
            commit,
            input,
            file,
            target_name,
            target_kind,
            language,
            mode,
            cache,
            relative_path,
            provider_id,
            kind,
            config,
            config_id,
            #[cfg(debug_assertions)]
            meter,
        } = args;
        let Some(plugin) = inner.registry.plugin_for(language) else {
            return Err(ToolError(format!("no parser registered for {language:?}")));
        };
        let key = FingerprintKey {
            provider: &provider_id,
            rev: commit.rev.as_str(),
            relative_path: &relative_path,
            symbol_name: &target_name,
            kind,
            mode,
            config: config_id,
        };
        let current = match input {
            SnapshotInput::Cached(cached) => cached,
            SnapshotInput::Absent => {
                cache.put(&key, Cached::Tombstone);
                None
            }
            SnapshotInput::ReadFailed(reason) => {
                return Ok(SnapshotResult::Skipped(format!(
                    "unreadable at revision: {reason}"
                )));
            }
            SnapshotInput::Bytes(bytes) => {
                // The source was metered immediately after `read_at` and is
                // the only retained source in this revision-record batch.
                // Mirror the indexer's extraction pipeline exactly
                // (indexer.rs parse phase): config-driven preprocess
                // byte-rewrites feed the parse, and synthesis sees the
                // ORIGINAL bytes. Without this, every symbol that only
                // extracts under `[cpp].macro_*` config would silently
                // report an empty history.
                let cleaned = plugin.preprocess(&bytes.bytes, &config);
                // Historical code may not parse with today's grammar —
                // expected, not exceptional: skip and flag.
                #[cfg(debug_assertions)]
                let _ast_guard = AstWorkGuard::begin(Arc::clone(&meter));
                #[cfg(debug_assertions)]
                if let Some(hook) = PARSER_HOOK
                    .get_or_init(|| Mutex::new(None))
                    .lock()
                    .expect("history parser test hook mutex poisoned")
                    .clone()
                {
                    hook();
                }
                let mut parsed = match plugin.parse_file(Path::new(&file), &cleaned) {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        return Ok(SnapshotResult::Skipped(format!("parse failed: {error}")))
                    }
                };
                plugin.synthesize_symbols(Path::new(&file), &bytes.bytes, &config, &mut parsed);
                // Exact, case-sensitive (name, kind) — D-0005. Several
                // matches (overloads) resolve deterministically to the
                // earliest occurrence.
                let found = parsed
                    .symbols
                    .iter()
                    .filter(|s| s.name == target_name && s.kind == target_kind)
                    .min_by_key(|s| (s.line, s.column));
                match found {
                    None => {
                        cache.put(&key, Cached::Tombstone);
                        None
                    }
                    Some(historical) => {
                        // Fingerprint the SAME bytes the parse saw (the
                        // preprocessed form) — phase 8.1's settled
                        // contract: the AST overrides re-parse `content`
                        // to locate the span, and raw-vs-cleaned bytes
                        // would misalign macro-stripped spans. Preprocess
                        // is byte-preserving, so line spans are identical
                        // either way for the text default.
                        match plugin.fingerprint_symbol(&cleaned, historical, mode) {
                            Some(fingerprint) => {
                                cache.put(&key, Cached::Fingerprint(fingerprint));
                                Some(fingerprint)
                            }
                            None if mode == FingerprintMode::LiteralInsensitive => {
                                // Post-phase-8 this arm is per-SPAN
                                // unavailability (unlocatable span:
                                // synthesized symbols, error-recovered
                                // regions) or a future plugin without an
                                // AST override. A VISIBLE skip — never a
                                // silent fallback to Normalized
                                // (Designs/VcsHistory Decision 5): the
                                // revision lands in `skipped` with the
                                // mode named, and the transition state
                                // carries over it.
                                return Ok(SnapshotResult::Skipped(format!(
                                    "span not fingerprintable under mode \
                                     \"literal_insensitive\" at this revision ({language:?})"
                                )));
                            }
                            None => {
                                return Ok(SnapshotResult::Skipped(
                                    "span not fingerprintable at this revision".to_string(),
                                ))
                            }
                        }
                    }
                }
            }
        };
        Ok(SnapshotResult::Current(current))
    })()
}
