//! Typed core for the history tools (Track D, phase 5).
//!
//! `blame_symbol` is the first history feature: a graph span lookup plus a
//! line-range blame through the provider registry (Designs/VcsHistory
//! Decision 4). The provider is reached through `code-graph-vcs` only —
//! no VCS backend dependency enters this crate (AC-23).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use code_graph_graph::Graph;
use code_graph_lang::FingerprintMode;
use code_graph_vcs::{VcsError, VcsRegistry};
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
    /// the provider's default revision (Git: `HEAD`). `null` when
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
    if hunks.is_empty() && stale_reason.is_none() {
        stale_reason = Some(
            "the symbol's span has no attributable lines at the blamed revision \
             (the file is shorter there than the on-disk span)"
                .to_string(),
        );
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
    /// this file exist beyond the window.
    pub window_filled: bool,
    /// The provider stopped examining history at its internal bound before
    /// exhausting reachable history (distinct from `window_filled`).
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

/// Per-revision input for the blocking walk, resolved in the async phase.
enum RevisionInput {
    /// The fingerprint (or tombstone) came from the cache; no bytes needed.
    Cached(Option<u64>),
    /// Cache miss: the file's bytes at this revision, fetched via
    /// `read_at` (in memory, FR-35 — no temporary file is ever written).
    Bytes(Vec<u8>),
    /// The provider reports the path does not exist at this revision —
    /// typically a commit that DELETED the file. That is an examined
    /// absence (it drives a `removed` transition), not a skip.
    Absent,
    /// The revision's bytes could not be read; the walk skips it.
    ReadFailed(String),
}

/// `symbol_history` body: when did this symbol's *content* actually change,
/// as opposed to when was its file touched (FR-33).
///
/// The walk runs oldest to newest over a bounded window of revisions
/// touching the symbol's file, computes the symbol's fingerprint at each
/// (exact, case-sensitive `(name, kind)` match — D-0005: any rename,
/// including case-only, reports as `removed` + `introduced`), and emits
/// only transitions. The CPU-bound half — parse + fingerprint per revision
/// — runs inside one `spawn_blocking` so a large window cannot starve the
/// runtime (NFR-10); revision bytes are prefetched through the provider's
/// own blocking-pool dispatch, and cache hits skip the fetch entirely.
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
    // Rejected up front so the outcome is data-independent: no plugin
    // supports the mode today, and deferring the check to the walk would
    // make the same request alternately error or succeed depending on
    // whether any windowed revision contains the symbol.
    if mode == FingerprintMode::LiteralInsensitive {
        return Err(ToolError(
            "fingerprint mode \"literal_insensitive\" is not supported for any language yet \
             (per-language overrides arrive in phase 8); use \"normalized\""
                .to_string(),
        ));
    }
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
    let Some(provider) = inner.vcs.detect(&root) else {
        return history_unavailable(
            symbol,
            &file,
            mode,
            window,
            format!(
                "no supported version-control system detected at {}",
                root.display()
            ),
        );
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

    // Async phase: consult the cache, prefetch bytes only for misses. The
    // provider dispatches each read on its own blocking pool.
    let mut inputs: Vec<RevisionInput> = Vec::with_capacity(commits.len());
    for commit in &commits {
        let key = FingerprintKey {
            provider: &provider_id,
            rev: commit.rev.as_str(),
            relative_path: &relative_path,
            symbol_name: &target_name,
            kind,
            mode,
            config: config_id,
        };
        if let Some(cached) = cache.get(&key) {
            inputs.push(RevisionInput::Cached(match cached {
                Cached::Fingerprint(fingerprint) => Some(fingerprint),
                Cached::Tombstone => None,
            }));
            continue;
        }
        match provider.read_at(&commit.rev, Path::new(&file)).await {
            Ok(bytes) => inputs.push(RevisionInput::Bytes(bytes)),
            // The path does not exist at this revision — a deletion commit.
            // Examined absence, not a skip: it drives `removed`.
            Err(VcsError::NotFound(_)) => inputs.push(RevisionInput::Absent),
            Err(error) => inputs.push(RevisionInput::ReadFailed(error.to_string())),
        }
    }

    run_transition_walk(WalkArgs {
        inner: Arc::clone(inner),
        symbol_id: symbol.to_string(),
        commits,
        inputs,
        file,
        target_name,
        target_kind,
        language,
        mode,
        window,
        window_filled,
        history_truncated,
        cache,
        relative_path,
        provider_id,
        kind,
        config,
        config_id,
    })
    .await
}

/// Everything the blocking walk needs, owned, so the closure is `'static`.
struct WalkArgs {
    inner: Arc<ServerInner>,
    symbol_id: String,
    commits: Vec<code_graph_vcs::Commit>,
    inputs: Vec<RevisionInput>,
    file: String,
    target_name: String,
    target_kind: code_graph_core::SymbolKind,
    language: code_graph_core::Language,
    mode: FingerprintMode,
    window: u32,
    window_filled: bool,
    history_truncated: bool,
    cache: FingerprintCache,
    relative_path: String,
    provider_id: String,
    kind: &'static str,
    /// The effective root config: historical parses must run the same
    /// preprocess + synthesis pipeline the indexer ran.
    config: code_graph_core::RootConfig,
    /// [`super::fingerprint_cache::config_identity`] of `config`.
    config_id: u64,
}

/// The CPU-bound half of `symbol_history`: parse + fingerprint each cache
/// miss and assemble the transition walk, inside one `spawn_blocking`.
async fn run_transition_walk(args: WalkArgs) -> ToolResult<SymbolHistoryResponse> {
    let revisions_examined = args.commits.len() as u32;
    let walk = tokio::task::spawn_blocking(move || {
        let WalkArgs {
            inner,
            symbol_id,
            commits,
            inputs,
            file,
            target_name,
            target_kind,
            language,
            mode,
            window,
            window_filled,
            history_truncated,
            cache,
            relative_path,
            provider_id,
            kind,
            config,
            config_id,
        } = args;
        let Some(plugin) = inner.registry.plugin_for(language) else {
            return Err(ToolError(format!("no parser registered for {language:?}")));
        };

        let mut entries: Vec<SymbolHistoryEntry> = Vec::new();
        let mut skipped: Vec<SkippedRevision> = Vec::new();
        // None = nothing examined yet; Some(None) = absent at the previous
        // examined revision; Some(Some(fp)) = present with that fingerprint.
        let mut previous: Option<Option<u64>> = None;

        for (commit, input) in commits.iter().zip(inputs) {
            let key = FingerprintKey {
                provider: &provider_id,
                rev: commit.rev.as_str(),
                relative_path: &relative_path,
                symbol_name: &target_name,
                kind,
                mode,
                config: config_id,
            };
            let current: Option<u64> = match input {
                RevisionInput::Cached(cached) => cached,
                RevisionInput::Absent => {
                    // The file does not exist at this revision (deletion
                    // commit): the symbol is absent, and that absence is as
                    // cacheable as any parsed tombstone.
                    cache.put(&key, Cached::Tombstone);
                    None
                }
                RevisionInput::ReadFailed(reason) => {
                    skipped.push(SkippedRevision {
                        rev: commit.rev.to_string(),
                        reason: format!("unreadable at revision: {reason}"),
                    });
                    continue;
                }
                RevisionInput::Bytes(bytes) => {
                    // Mirror the indexer's extraction pipeline exactly
                    // (indexer.rs parse phase): config-driven preprocess
                    // byte-rewrites feed the parse, and synthesis sees the
                    // ORIGINAL bytes. Without this, every symbol that only
                    // extracts under `[cpp].macro_*` config would silently
                    // report an empty history.
                    let cleaned = plugin.preprocess(&bytes, &config);
                    // Historical code may not parse with today's grammar —
                    // expected, not exceptional: skip and flag.
                    let mut parsed = match plugin.parse_file(Path::new(&file), &cleaned) {
                        Ok(parsed) => parsed,
                        Err(error) => {
                            skipped.push(SkippedRevision {
                                rev: commit.rev.to_string(),
                                reason: format!("parse failed: {error}"),
                            });
                            continue;
                        }
                    };
                    plugin.synthesize_symbols(Path::new(&file), &bytes, &config, &mut parsed);
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
                            match plugin.fingerprint_symbol(&bytes, historical, mode) {
                                Some(fingerprint) => {
                                    cache.put(&key, Cached::Fingerprint(fingerprint));
                                    Some(fingerprint)
                                }
                                None if mode == FingerprintMode::LiteralInsensitive => {
                                    // Never silently fall back to Normalized
                                    // (Designs/VcsHistory Decision 5).
                                    return Err(ToolError(format!(
                                        "fingerprint mode \"literal_insensitive\" is not \
                                         supported for {language:?} yet (per-language \
                                         overrides arrive in phase 8); use \"normalized\""
                                    )));
                                }
                                None => {
                                    skipped.push(SkippedRevision {
                                        rev: commit.rev.to_string(),
                                        reason: "span not fingerprintable at this revision"
                                            .to_string(),
                                    });
                                    continue;
                                }
                            }
                        }
                    }
                }
            };

            let first_examined = previous.is_none();
            let before = previous.flatten();
            previous = Some(current);
            let change = match (before, current) {
                (None, Some(_)) => "introduced",
                (Some(_), None) if !first_examined => "removed",
                (Some(a), Some(b)) if a != b && !first_examined => "modified",
                _ => continue,
            };
            entries.push(SymbolHistoryEntry {
                rev: commit.rev.to_string(),
                author: commit.author.clone(),
                timestamp_utc: commit.timestamp_utc,
                summary: commit.summary.clone(),
                change,
                // Present at the window's oldest examined revision with
                // older history possibly existing: indistinguishable from a
                // genuine introduction — say so instead of mislabelling.
                // Skips carry the same uncertainty: when every revision
                // older than the first examined one was skipped, the symbol
                // may have existed at the skipped revisions too (`skipped`
                // holds exactly the pre-first skips at this point).
                at_window_boundary: first_examined
                    && change == "introduced"
                    && (window_filled || history_truncated || !skipped.is_empty()),
            });
        }

        Ok(SymbolHistoryResponse {
            available: true,
            reason: None,
            symbol_id,
            file,
            mode: mode_wire(mode).to_string(),
            window,
            revisions_examined,
            window_filled,
            history_truncated,
            entries,
            skipped,
        })
    })
    .await
    .map_err(|error| ToolError(format!("symbol_history worker failed: {error}")))??;

    Ok(ToolOk::Value(walk))
}
