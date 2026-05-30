//! Watch-mode handlers — the `watch_start` / `watch_stop` lifecycle plus
//! the reindex loop: each batch of debounced filesystem events drives
//! a per-file reindex through [`try_reindex_file`], which is index-lock-aware
//! so an in-flight `analyze_codebase` can never race a watch-driven merge.
//!
//! The wire-format contract for this module:
//! - `watch mode is already active` — second `watch_start` while watching.
//! - `watch mode is not active` — `watch_stop` when nothing is watching.
//! - The require_indexed envelope on either handler when no codebase is
//!   indexed yet.
//!
//! `WatchHandle` lives on [`crate::server::ServerInner`], so `watch_start`
//! constructs it and writes it; `watch_stop` takes it back out and drops
//! the debouncer to tear down the OS watch. The async watch_loop task gets
//! its own [`tokio::sync::oneshot::Receiver`] for shutdown — `watch_stop`
//! sends `()` on the paired sender to end the loop cleanly.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use code_graph_core::{paths, symbol_id, EdgeKind, FileGraph, SymbolId};
use code_graph_lang::CallContext;
use notify_debouncer_full::notify::{EventKind, RecursiveMode};
use notify_debouncer_full::{new_debouncer, DebounceEventResult, DebouncedEvent};
use tokio::sync::{mpsc, oneshot};

use crate::indexer::{build_file_index, build_symbol_index};
use crate::server::{ServerInner, WatchHandle};

use super::{tool_error, tool_success_json};

/// Debounce window for the filesystem watcher. The `notify-debouncer-full`
/// API coalesces every event for a given path that arrives within this
/// window into a single `DebouncedEvent`. 250 ms is the design's pick: it
/// rides through editor save patterns (atomic-rename, multi-event saves)
/// while still feeling instant for an interactive `watch_start` user.
pub(crate) const DEBOUNCE_TIMEOUT: Duration = Duration::from_millis(250);

/// Bound for the in-process channel between the debouncer's notify thread
/// and the watch_loop tokio task. Events are best-effort — when the
/// channel is full, the debouncer's notify thread will fall back to
/// `blocking_send` (see [`forward_events`]) so no events are silently
/// dropped at the producer.
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// JSON body for both `watch_start` and `watch_stop`. The single boolean
/// carries the difference: `true` from `watch_start`, `false` from
/// `watch_stop`. Mirrors the minimal-shape success body from Go (an empty
/// object) — Go returned `{}`, but the boolean costs nothing on the wire
/// and gives clients a single field to assert against, locked in by the
/// snapshot tests.
#[derive(serde::Serialize)]
struct WatchResponse {
    watching: bool,
}

/// `watch_start` body. Caller must already have passed `require_indexed`.
///
/// Steps:
/// 1. Acquire `inner.watch` for write and refuse if a [`WatchHandle`] is
///    already installed. The check-and-store happens under one lock so two
///    concurrent `watch_start` calls cannot both observe `None` and race
///    to overwrite each other (TOCTOU).
/// 2. Read the indexed `root_path` (set by the most recent successful
///    `analyze_codebase`).
/// 3. Construct the debouncer with [`DEBOUNCE_TIMEOUT`].
/// 4. Recursively watch `root_path`.
/// 5. Spawn the watch_loop task and store the resulting [`WatchHandle`]
///    on `inner.watch` — still under the same write lock.
///
/// Holding the parking_lot write lock across `new_debouncer` +
/// `Debouncer::watch` is intentional: those are bounded OS operations
/// (no IO on user input, no blocking on async work), and the alternative
/// (two-phase lock-build-lock) reintroduces the very race we're closing.
pub fn watch_start(inner: &Arc<ServerInner>) -> rmcp::model::CallToolResult {
    let mut watch_guard = inner.watch.write();
    if watch_guard.is_some() {
        return tool_error("watch mode is already active");
    }

    let root_path = match inner.root_path.read().clone() {
        Some(p) => p,
        None => {
            // require_indexed passed (the indexed atomic flag is set) but
            // root_path is empty — this means the index was loaded by some
            // path that didn't populate root_path. Today's analyze_codebase
            // always populates it, so this branch is defensive only.
            return tool_error("no codebase indexed — call analyze_codebase first");
        }
    };

    // Channel: notify-debouncer-full's notify thread (non-tokio) →
    // watch_loop tokio task. The closure passed to `new_debouncer` is
    // `Fn(DebounceEventResult)` and may run on a worker thread that has
    // no tokio runtime — `mpsc::Sender::try_send` is blocking-thread
    // safe, so the closure forwards events without needing to be inside
    // a tokio context.
    let (events_tx, events_rx) = mpsc::channel::<Vec<DebouncedEvent>>(EVENT_CHANNEL_CAPACITY);

    let mut debouncer = match new_debouncer(DEBOUNCE_TIMEOUT, None, forward_events(events_tx)) {
        Ok(d) => d,
        Err(e) => return tool_error(format!("failed to start watcher: {e}")),
    };

    if let Err(e) = debouncer.watch(&root_path, RecursiveMode::Recursive) {
        return tool_error(format!("failed to watch {}: {e}", root_path.display()));
    }

    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();

    tokio::spawn(watch_loop(Arc::clone(inner), events_rx, cancel_rx));

    *watch_guard = Some(WatchHandle {
        debouncer,
        cancel: cancel_tx,
    });

    tool_success_json(&WatchResponse { watching: true })
}

/// `watch_stop` body. Caller must already have passed `require_indexed`.
///
/// Takes the live [`WatchHandle`] out of `inner.watch`, sends the cancel
/// signal so the watch_loop task exits, then drops the debouncer (which
/// tears down the OS watch).
pub fn watch_stop(inner: &Arc<ServerInner>) -> rmcp::model::CallToolResult {
    let handle = match inner.watch.write().take() {
        Some(h) => h,
        None => return tool_error("watch mode is not active"),
    };

    let WatchHandle { debouncer, cancel } = handle;
    // Best-effort: if the watch_loop task has already exited (e.g. its
    // future was cancelled at runtime shutdown), the receiver is gone and
    // `send` returns Err. That's fine — the goal is "stop watching", and
    // dropping the debouncer below achieves that regardless.
    let _ = cancel.send(());
    drop(debouncer);

    tool_success_json(&WatchResponse { watching: false })
}

/// Build the `Fn(DebounceEventResult)` closure that the debouncer's notify
/// thread will invoke. Bridges synchronous notify events into the tokio
/// receiver owned by [`watch_loop`].
///
/// The closure is `Fn`, so it must be cheap to call repeatedly — captures
/// `events_tx` by move, then `clone`s for each invocation. Errors from
/// notify itself are swallowed: the contract is "do not crash the
/// watcher thread".
fn forward_events(events_tx: mpsc::Sender<Vec<DebouncedEvent>>) -> impl Fn(DebounceEventResult) {
    move |result| {
        let events = match result {
            Ok(e) => e,
            Err(_errors) => return,
        };
        if events.is_empty() {
            return;
        }
        // Try non-blocking first; fall through to blocking_send when the
        // channel is full so we don't drop events at the producer.
        match events_tx.try_send(events) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(events)) => {
                // We're on a notify worker thread (non-tokio); a blocking
                // send is safe here and bounded by the watch_loop's drain
                // rate.
                let _ = events_tx.blocking_send(events);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                // The receiver has been dropped (watch_stop already ran).
                // Discarding here is the right behavior — there's no one
                // left to consume the events.
            }
        }
    }
}

/// Outcome of [`try_reindex_file`]. Surfaced for unit-test assertions and
/// for the watch loop's debug logging — production callers don't branch on
/// it (a re-index that fails for any reason gets logged and the watcher
/// keeps running).
///
/// Exposed `pub` so `tests/watch_dangling_edges.rs` (an integration test
/// that drives a single `try_reindex_file` call to deterministically
/// exercise the rename path) can match on the variants.
#[derive(Debug)]
pub enum ReindexOutcome {
    /// File was re-parsed (or removed) and merged into the graph.
    Reindexed,
    /// `index_lock` was held (an `analyze_codebase` is in flight). The event
    /// is **dropped** — the in-flight analyze will pick up the file's
    /// current state. Design Decision (`Designs/RustRewrite/README.md`,
    /// "Concurrency Model"): we don't queue, retry, or block.
    LockContended,
    /// Path didn't resolve to any registered language plugin (e.g. a
    /// `.txt` file inside the watched root). Defensive — the loop already
    /// pre-filters non-source paths.
    NotASource,
    /// Per-file failure (read error, parse error, …). The loop logs this
    /// and continues; the graph is left untouched on the failed path.
    Error(String),
}

/// Per-file reindex routine driven by [`watch_loop`].
///
/// Invariant — index-lock-aware: takes `inner.index_lock.try_lock()` first
/// and short-circuits on contention so a concurrent `analyze_codebase`
/// (which holds the same lock) cannot race an incremental merge. The lock
/// is held for the entire snapshot+resolve+merge sequence; an analyze that
/// arrives after the snapshot but before the merge is serialized behind us.
/// Design Decision (`Designs/RustRewrite/README.md`, "Concurrency Model"):
/// when contention is observed, the event is dropped — the in-flight
/// analyze will pick up the file's current state anyway, and queuing would
/// produce unbounded growth on a busy editor session.
///
/// Pipeline:
///
/// 1. `try_lock` — drop the event on contention.
/// 2. Resolve the file's plugin via `inner.registry.for_path`.
/// 3. Read the cached `inner.config` (canonical contract: the watch path
///    uses the config most recently loaded by `analyze_codebase`, not a
///    fresh disk read).
/// 4. On `is_remove`: drop the file from the graph.
/// 5. On create/modify: parse, reconstruct symbol/file indexes from the
///    current graph plus the new file, language-aware-resolve the new
///    file's edges, and merge.
pub async fn try_reindex_file(
    inner: &Arc<ServerInner>,
    path: &Path,
    is_remove: bool,
) -> ReindexOutcome {
    // Design Decision: drop the event on contention rather than queue or
    // retry. The in-flight `analyze_codebase` will pick up the file's
    // current state, so the user-observable graph eventually converges.
    let Ok(_index_guard) = inner.index_lock.try_lock() else {
        return ReindexOutcome::LockContended;
    };

    // Canonical contract: the watch path uses the cached `inner.config`,
    // not a fresh `RootConfig::load(<root>/.code-graph.toml)` per event.
    // The most recent successful `analyze_codebase` is the source of truth
    // for parsing/discovery settings; re-reading on every event would let a
    // stale on-disk config diverge from the live indexer state.
    //
    // Bind the cached `RootConfig` so we can pass
    // it into `LanguagePlugin::preprocess` below — `[cpp].macro_strip` is
    // a per-file knob and watch-mode reindex must apply the same
    // substitution the most-recent `analyze_codebase` did. The clone is
    // cheap (`RootConfig` is small) and isolates us from the read-lock.
    //
    // The clone now also drives the early-out `for_path_with_config` check
    // below so `[extensions]` overrides take effect on the watch path
    // (e.g. a file claimed via `[extensions].cpp = [".cu"]` reindexes;
    // a file in `[extensions].disabled` does not).
    let cfg_for_blocking = inner.config.read().clone();
    // Pre-clone the extensions slice so the post-spawn_blocking section
    // (which re-looks up the plugin to resolve edges) sees the same
    // override snapshot. `cfg_for_blocking` itself is moved into the
    // blocking task and cannot be borrowed afterwards.
    let extensions_for_resolve = cfg_for_blocking.extensions.clone();

    if inner
        .registry
        .for_path_with_config(path, &cfg_for_blocking.extensions)
        .is_none()
    {
        return ReindexOutcome::NotASource;
    }

    if is_remove {
        let mut g = inner.graph.write();
        // Capture the file's pre-existing symbol IDs *before* dropping the
        // file from the graph — they're the truly-removed set for the
        // dangling-edge prune. If the path was unknown to the graph, this
        // is empty and the prune is a no-op.
        let removed_ids: HashSet<SymbolId> = g
            .file_symbols(path)
            .into_iter()
            .map(|s| symbol_id(&s))
            .collect();
        g.remove_file(path);
        g.prune_dangling_edges(&removed_ids);
        return ReindexOutcome::Reindexed;
    }

    // Read + parse on the blocking pool. Parity with `analyze_codebase`,
    // which wraps its parse phase in `spawn_blocking` (handlers/analyze.rs)
    // for the same reason: `std::fs::read` and `plugin.parse_file` are
    // synchronous and can stall a tokio worker thread (slow disk, network
    // mounts, large templated headers). The `index_lock` guard above is
    // held across the await — `tokio::sync::Mutex` permits this, and the
    // canonical contract is "the watch path serializes behind any in-flight
    // analyze for the entire snapshot+resolve+merge sequence", so releasing
    // the lock here would re-open the merge race.
    //
    // We move `Arc<ServerInner>` into the blocking task so it can re-lookup
    // the plugin via `registry.for_path` (the registry stores
    // `Box<dyn LanguagePlugin>` and only hands out borrows). The for_path
    // call is O(extension-count) and re-checks defensively — the same path
    // we already accepted above won't have changed plugin between here and
    // the blocking task.
    let inner_for_blocking = Arc::clone(inner);
    let path_owned = path.to_path_buf();
    let parse_result: Result<FileGraph, ReindexOutcome> = tokio::task::spawn_blocking(move || {
        let plugin = match inner_for_blocking
            .registry
            .for_path_with_config(&path_owned, &cfg_for_blocking.extensions)
        {
            Some(p) => p,
            None => return Err(ReindexOutcome::NotASource),
        };
        let content = match std::fs::read(&path_owned) {
            Ok(b) => b,
            Err(e) => {
                return Err(ReindexOutcome::Error(format!(
                    "read {}: {e}",
                    path_owned.display()
                )))
            }
        };
        let cleaned = plugin.preprocess(&content, &cfg_for_blocking);
        match plugin.parse_file(&path_owned, &cleaned) {
            Ok(mut fg) => {
                // Per-file post-parse synthesis hook — parity with the
                // analyze path (`indexer::index_directory`). Sees the
                // ORIGINAL bytes (NOT the preprocessed `cleaned`) so a
                // plugin's secondary extractor over source the preprocess
                // pass would have rewritten still fires — concretely,
                // `[cpp].macro_define_function` invocations that
                // `macro_strip` could otherwise blank. Without this call
                // a watched edit of an affected file silently drops every
                // synthesized symbol that a cold `analyze_codebase` would
                // have produced (split-brain indexing).
                plugin.synthesize_symbols(&path_owned, &content, &cfg_for_blocking, &mut fg);
                Ok(fg)
            }
            Err(e) => Err(ReindexOutcome::Error(format!(
                "parse {}: {e}",
                path_owned.display()
            ))),
        }
    })
    .await
    .unwrap_or_else(|join_err| {
        Err(ReindexOutcome::Error(format!(
            "blocking task panicked while parsing {}: {join_err}",
            path.display()
        )))
    });
    let mut new_fg = match parse_result {
        Ok(fg) => fg,
        Err(outcome) => return outcome,
    };

    // Compute the IDs that *truly* disappeared from this file: the previous
    // snapshot's IDs minus the IDs the freshly-parsed file produces. On a
    // routine modify (no rename) this is empty; on a rename it's typically
    // 1; on a wholesale rewrite it's the full pre-existing set. We snapshot
    // pre-existing IDs from `Graph::files` while we still hold the read
    // lock for the file_graphs_snapshot below — same critical section, no
    // extra walk.
    let mut all_graphs;
    let pre_existing_ids: HashSet<SymbolId>;
    {
        let g = inner.graph.read();
        pre_existing_ids = g
            .file_symbols(path)
            .into_iter()
            .map(|s| symbol_id(&s))
            .collect();
        // Snapshot every existing FileGraph (symbols only — edges aren't
        // needed for index construction) and append the freshly-parsed
        // file's symbols. With the new file's symbols included in the
        // index, the resolver can rewrite calls/includes that point INTO
        // the new file from elsewhere on subsequent reindexes, and calls
        // FROM the new file resolve against the rest of the graph in one
        // pass.
        let mut snapshot = g.file_graphs_snapshot();
        // Drop the stale entry for this path (if any) so the index built
        // below sees only the new file's symbols, not both old and new.
        snapshot.retain(|fg| Path::new(&fg.path) != path);
        all_graphs = snapshot;
    }
    all_graphs.push(new_fg.clone());
    let file_index = build_file_index(&all_graphs);

    // Whole-graph post-parse pass. The watch path is a SECOND re-index
    // path (separate from `index_directory`) that must run the same hook
    // — otherwise plugins with crate-aware analysis (e.g. Rust's namespace
    // assignment) silently regress on every watched file save. We run it
    // BEFORE the per-edge resolve loop below so any in-place rewrites the
    // hook performs (namespaces, mod-edge targets) are visible to the
    // resolver — and BEFORE building the symbol index so the rewrites are
    // reflected in resolver candidate scoring. Plugins with no
    // post-processing inherit the trait's no-op default.
    for plugin in inner.registry.plugins() {
        plugin.post_index(&mut all_graphs, &file_index);
    }
    // The freshly-parsed `FileGraph` is the LAST entry in `all_graphs`
    // (pushed just above). After the hook may have rewritten its symbols
    // in place, copy the post-hook version back into `new_fg` so the
    // downstream merge sees the rewritten state. `expect` (not `if let`)
    // because the invariant is unconditional: `new_fg.clone()` was pushed
    // unconditionally a few lines up, so `all_graphs.last()` must be
    // `Some`. A silent stale-state slip here would defeat the whole
    // copy-back; surface a panic-with-context if the invariant ever
    // breaks.
    let last = all_graphs
        .last()
        .expect("all_graphs is non-empty: new_fg.clone() was pushed unconditionally above");
    new_fg = last.clone();
    let symbol_index = build_symbol_index(&all_graphs);

    let new_ids: HashSet<SymbolId> = new_fg.symbols.iter().map(symbol_id).collect();
    let removed_ids: HashSet<SymbolId> = pre_existing_ids
        .into_iter()
        .filter(|id| !new_ids.contains(id))
        .collect();

    // Resolve only the new file's edges in place. The existing graph's
    // edges are already stored as resolved edge entries (in adj/radj/
    // includes); they don't need re-resolution. The `resolve_all_edges`
    // helper walks every graph in its slice, but since we own only
    // `new_fg`, we inline the per-edge dispatch here.
    //
    // The plugin re-lookup here mirrors the one in the blocking parse
    // task above. It's bounded (HashMap-of-extensions probe) and avoids
    // borrowing the registry across the spawn_blocking boundary. Uses
    // the pre-cloned `extensions_for_resolve` so the override snapshot
    // matches the one the blocking task used.
    let Some(plugin) = inner
        .registry
        .for_path_with_config(path, &extensions_for_resolve)
    else {
        return ReindexOutcome::NotASource;
    };
    let path_for_ctx = std::path::PathBuf::from(&new_fg.path);
    new_fg.edges.retain_mut(|edge| {
        match edge.kind {
            EdgeKind::Includes => {
                match plugin.resolve_include(&edge.to, &file_index) {
                    Some((resolved, confidence))
                        if inner
                            .registry
                            .language_for_path_with_config(&resolved, &extensions_for_resolve)
                            .is_some() =>
                    {
                        edge.to = resolved.to_string_lossy().into_owned();
                        edge.confidence = confidence;
                    }
                    // Unresolved, or resolved to a non-source target: this
                    // include does not point at an indexed source file
                    // (system headers like `stdio.h`, external paths never
                    // in the FileIndex, config files like `.ini`/`.cfg`,
                    // plain `.txt`). It is not a graph edge — drop it
                    // rather than leak a raw/unresolvable string into the
                    // dependency graph. Not logged: this fires constantly
                    // in real C++ codebases and would flood stderr.
                    _ => return false,
                }
            }
            EdgeKind::Calls => {
                let ctx = CallContext {
                    caller_id: &edge.from,
                    caller_file: &path_for_ctx,
                    language: new_fg.language,
                };
                if let Some((id, confidence)) = plugin.resolve_call(&edge.to, &ctx, &symbol_index) {
                    edge.to = id;
                    edge.confidence = confidence;
                }
            }
            // Bare derived class names are the canonical form for inherits
            // edges; the graph engine resolves them at hierarchy-query time.
            EdgeKind::Inherits => {}
            _ => {}
        }
        true
    });

    // Merge + dangling-edge prune under one write lock. `merge_file_graph`
    // calls `remove_file_unsafe` internally which scrubs *outbound* edges
    // (those with `file == path`) but leaves *inbound* cross-file edges
    // pointing at any symbol that disappeared from this file — e.g. a
    // rename of `A:old_fn → A:new_fn` leaves `B:caller → A:old_fn` in B's
    // adjacency. `prune_dangling_edges` cleans those, scoped to the
    // truly-removed symbol IDs so it's O(edges-touching-removed-IDs), not
    // O(all edges). Inbound re-resolution (rebinding B's call to the new
    // name) is intentionally out of scope — that requires re-parsing B,
    // which the watch event for A doesn't warrant.
    let mut g = inner.graph.write();
    g.merge_file_graph(new_fg);
    g.prune_dangling_edges(&removed_ids);
    ReindexOutcome::Reindexed
}

/// Normalize a filesystem-event path so its shape matches what the
/// indexer stored.
///
/// **Why this exists (Windows correctness).** On Windows,
/// `notify-debouncer-full` propagates paths from `ReadDirectoryChangesW`
/// in the verbatim extended form (`\\?\D:\proj\foo.cpp`), while
/// `analyze_codebase` canonicalizes through `paths::canonicalize`
/// which dunce-strips the verbatim prefix (storing the
/// `D:\proj\foo.cpp` form). The two are distinct `PathBuf`s — distinct
/// `PathTrie` keys — and a downstream `merge_file_graph`'s
/// `contains_path` check would return `false` on the raw event path,
/// causing the watch reindex to INSERT a duplicate entry alongside
/// the indexed one. Within a few watch events the graph silently
/// doubles every touched file. Identical mechanism to the
/// stale-cache-pollution bug fixed in the rebase commit; this is the
/// production trigger.
///
/// **Behavior.** For create/modify events (`exists_hint = true`)
/// `paths::canonicalize` is preferred because it produces the same
/// on-disk shape the indexer would record. On failure (file deleted
/// between debounce-window expiry and this call) the pure-lexical
/// `paths::simplify` is the fallback. For remove events
/// (`exists_hint = false`) `canonicalize` would error anyway, so go
/// straight to `simplify` — strips the `\\?\` verbatim disk prefix
/// without an IO probe.
///
/// **No-op on Linux/macOS.** `dunce::simplified` and
/// `dunce::canonicalize` collapse to standard library calls on non-
/// Windows, and absolute event paths produced by `notify` are
/// already in the form `paths::canonicalize` produces. The function
/// is unconditional only because Rust workspace lint policy forbids
/// `#[cfg(windows)]` correctness shims when an unconditional
/// always-identity helper is available.
fn canonicalize_event_path(path: &Path, exists_hint: bool) -> PathBuf {
    if exists_hint {
        if let Ok(c) = paths::canonicalize(path) {
            return c;
        }
    }
    paths::simplify(path)
}

/// Watch loop: receives debounced filesystem-event batches from the
/// debouncer's notify thread (via the bridge built in [`forward_events`])
/// and drives per-path reindexes through [`try_reindex_file`]. Cancellation
/// arrives on `cancel`; the loop also exits when the events channel
/// closes (the producing side of the channel — the debouncer — went away).
async fn watch_loop(
    inner: Arc<ServerInner>,
    mut events: mpsc::Receiver<Vec<DebouncedEvent>>,
    cancel: oneshot::Receiver<()>,
) {
    tokio::pin!(cancel);
    loop {
        tokio::select! {
            _ = &mut cancel => return,
            maybe_evts = events.recv() => match maybe_evts {
                Some(evts) => {
                    process_event_batch(&inner, evts).await;
                }
                None => return,
            },
        }
    }
}

/// Drive one debounced batch through the reindex pipeline. Factored out of
/// [`watch_loop`] so unit tests can exercise the dispatch logic without
/// constructing a debouncer or channel pair.
async fn process_event_batch(inner: &Arc<ServerInner>, evts: Vec<DebouncedEvent>) {
    // Snapshot the `[extensions]` override slice once per batch so the
    // upfront filter applies the same overrides `try_reindex_file` will
    // use. `try_reindex_file` re-reads `inner.config` itself, so any
    // mid-batch config change between this snapshot and the call still
    // produces a correct result; the snapshot here just keeps the cheap
    // filter consistent within the batch.
    let extensions = inner.config.read().extensions.clone();
    for evt in evts {
        let is_remove = matches!(evt.event.kind, EventKind::Remove(_));
        for raw_path in &evt.event.paths {
            // Canonicalize at the dispatch boundary so every downstream
            // graph lookup and merge sees the same shape the indexer
            // stored. On Windows the raw event path arrives in the
            // verbatim `\\?\D:\...` form (notify-debouncer-full
            // propagates `ReadDirectoryChangesW`'s output verbatim);
            // the indexer dunce-strips that prefix. Without the strip
            // here, every watched edit would land as a duplicate file
            // entry in the graph because `merge_file_graph`'s
            // `contains_path` check would miss the existing stripped-
            // form key.
            let path = canonicalize_event_path(raw_path, !is_remove);
            // Filter to source paths up-front so we don't pay
            // `index_lock.try_lock` for every random `.swp` an editor
            // touches. `try_reindex_file` re-checks defensively.
            if inner
                .registry
                .for_path_with_config(&path, &extensions)
                .is_none()
            {
                // Not a source file. But on a Remove event the path
                // may have been a DIRECTORY whose subtree contained
                // many indexed files (a `git checkout` that drops a
                // crate, a rebase that nukes a module dir, etc.). The
                // OS/notify-debouncer surfaces those as a single Remove
                // event on the parent dir whose extension doesn't
                // match any plugin. Check the graph: if any indexed
                // file lives under this path, drop the whole subtree
                // in one trie op via `Graph::remove_files_under`. This
                // is Phase E.3 (B)'s `PathTrie::remove_subtree`
                // payoff site.
                if is_remove {
                    try_reindex_subtree_removal(inner, &path).await;
                }
                continue;
            }
            let outcome = try_reindex_file(inner, &path, is_remove).await;
            if let ReindexOutcome::Error(msg) = outcome {
                // No `tracing` dep on this workspace; eprintln only for
                // hard errors so test output isn't spammed by routine
                // contention or non-source noise.
                eprintln!("watch: reindex failed for {}: {msg}", path.display());
            }
        }
    }
}

/// Handle a Remove event whose path is NOT a recognized source-file
/// extension — typically a directory removal that the OS surfaces as a
/// single event for the parent dir rather than per-child file events.
/// Uses [`Graph::remove_files_under`] (which uses
/// [`PathTrie::remove_subtree`]) to drop every indexed file under the
/// path in one trie operation.
///
/// No-op if the path matches no indexed file (e.g. a real non-source
/// file deletion that we ignored at index time anyway).
///
/// [`PathTrie::remove_subtree`]: code_graph_path_trie::PathTrie::remove_subtree
async fn try_reindex_subtree_removal(inner: &Arc<ServerInner>, path: &Path) {
    // Same lock + index-guard contract as `try_reindex_file`. Drop on
    // contention rather than queue or retry.
    let Ok(_index_guard) = inner.index_lock.try_lock() else {
        return;
    };
    let mut g = inner.graph.write();
    let removed_ids = g.remove_files_under(path);
    if removed_ids.is_empty() {
        return; // Path wasn't a directory containing indexed files.
    }
    g.prune_dangling_edges(&removed_ids);
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;
    use std::sync::atomic::Ordering;

    use code_graph_lang::LanguageRegistry;
    use code_graph_lang_cpp::CppParser;
    use rmcp::model::CallToolResult;
    use tempfile::TempDir;

    use crate::handlers::analyze::analyze_codebase;
    use crate::handlers::NO_BYTE_BUDGET;
    use crate::server::CodeGraphServer;

    fn first_text(r: &CallToolResult) -> String {
        r.content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default()
    }

    fn server_with_cpp_parser() -> CodeGraphServer {
        let mut reg = LanguageRegistry::new();
        reg.register(Box::new(CppParser::new().expect("CppParser::new")))
            .unwrap();
        CodeGraphServer::new(reg)
    }

    /// Index a TempDir holding a single trivial C++ source file; return the
    /// server (now indexed) and the dir handle (kept alive for the test).
    async fn indexed_server() -> (CodeGraphServer, TempDir) {
        let dir = TempDir::new().expect("TempDir");
        std::fs::write(dir.path().join("a.cpp"), b"void f() {}\n").expect("write fixture");
        let server = server_with_cpp_parser();
        let r = analyze_codebase(
            server.inner.clone(),
            dir.path().to_string_lossy().into_owned(),
            false,
            None,
            None,
        )
        .await;
        assert!(
            r.is_error.is_none() || r.is_error == Some(false),
            "fixture analyze must succeed: {r:?}"
        );
        assert!(server.inner.indexed.load(Ordering::Acquire));
        (server, dir)
    }

    /// Wait briefly for the spawned watch_loop tokio task to deposit its
    /// `WatchHandle` into `inner.watch`. `watch_start` installs the handle
    /// synchronously, so this is a defensive check — the assertion fires
    /// either way.
    async fn assert_watch_handle_present(server: &CodeGraphServer) {
        // No actual async wait is needed — `watch_start` writes the handle
        // before returning. Kept the helper signature async-ish so a
        // future change that defers handle install (e.g. waiting on the
        // spawned loop's startup) doesn't invalidate every call site.
        assert!(
            server.inner.watch.read().is_some(),
            "watch_start must populate inner.watch synchronously",
        );
    }

    fn assert_watch_handle_absent(server: &CodeGraphServer) {
        assert!(
            server.inner.watch.read().is_none(),
            "watch state must be cleared",
        );
    }

    /// Defensive guard: when `indexed=true` but `root_path` is empty
    /// (a state today's `analyze_codebase` never produces but a future
    /// loader might), `watch_start` falls back to the require_indexed
    /// envelope rather than panicking on the missing root.
    #[tokio::test]
    async fn watch_start_without_root_path_returns_require_indexed_envelope() {
        let server = server_with_cpp_parser();
        server.inner.indexed.store(true, Ordering::Release);
        // root_path stays None.
        let r = watch_start(&server.inner);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(
            first_text(&r),
            "no codebase indexed — call analyze_codebase first",
        );
        assert_watch_handle_absent(&server);
    }

    #[tokio::test]
    async fn watch_start_indexed_succeeds_and_sets_watch_state() {
        let (server, dir) = indexed_server().await;
        let r = watch_start(&server.inner);
        assert!(
            r.is_error.is_none() || r.is_error == Some(false),
            "watch_start happy path: {r:?}",
        );
        assert_eq!(first_text(&r), "{\"watching\":true}");
        assert_watch_handle_present(&server).await;
        // Tear down before the TempDir drops so the OS watch unwinds
        // cleanly (the debouncer holds an inotify handle on the dir).
        let _ = watch_stop(&server.inner);
        drop(dir);
    }

    #[tokio::test]
    async fn watch_start_double_start_errors() {
        let (server, dir) = indexed_server().await;
        let r1 = watch_start(&server.inner);
        assert!(r1.is_error.is_none() || r1.is_error == Some(false));
        let r2 = watch_start(&server.inner);
        assert_eq!(r2.is_error, Some(true));
        assert_eq!(first_text(&r2), "watch mode is already active");
        // First handle still installed.
        assert_watch_handle_present(&server).await;
        let _ = watch_stop(&server.inner);
        drop(dir);
    }

    #[tokio::test]
    async fn watch_stop_when_not_watching_errors() {
        let (server, dir) = indexed_server().await;
        let r = watch_stop(&server.inner);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(first_text(&r), "watch mode is not active");
        assert_watch_handle_absent(&server);
        drop(dir);
    }

    #[tokio::test]
    async fn watch_stop_after_start_succeeds_and_clears_state() {
        let (server, dir) = indexed_server().await;
        let r1 = watch_start(&server.inner);
        assert!(r1.is_error.is_none() || r1.is_error == Some(false));
        assert_watch_handle_present(&server).await;

        let r2 = watch_stop(&server.inner);
        assert!(r2.is_error.is_none() || r2.is_error == Some(false));
        assert_eq!(first_text(&r2), "{\"watching\":false}");
        assert_watch_handle_absent(&server);

        // Second stop is now an error.
        let r3 = watch_stop(&server.inner);
        assert_eq!(r3.is_error, Some(true));
        assert_eq!(first_text(&r3), "watch mode is not active");

        drop(dir);
    }

    /// Regression test for the TOCTOU race that the read-then-write split
    /// in `watch_start` previously exposed: two concurrent callers could
    /// both observe `inner.watch == None` and both proceed to install a
    /// handle, with the second silently overwriting the first.
    ///
    /// With the single-write-lock fix, exactly one of N concurrent
    /// `watch_start` calls succeeds and the rest return the
    /// "watch mode is already active" error. A `Barrier` forces all
    /// tasks to enter `watch_start` at the same time so the test has the
    /// best chance of catching a regression — without the fix this would
    /// be probabilistic; with the fix it is deterministic.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn watch_start_is_race_free_under_concurrent_callers() {
        use std::sync::Barrier;

        let (server, dir) = indexed_server().await;
        let inner = Arc::clone(&server.inner);

        const TASKS: usize = 4;
        let barrier = Arc::new(Barrier::new(TASKS));

        let mut handles = Vec::with_capacity(TASKS);
        for _ in 0..TASKS {
            let inner = Arc::clone(&inner);
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::task::spawn_blocking(move || {
                // Synchronous Barrier inside spawn_blocking — `watch_start`
                // is itself synchronous, so the race window we care about
                // is between the lock acquisitions, not across .await
                // points. The blocking pool gives us real OS threads.
                barrier.wait();
                watch_start(&inner)
            }));
        }

        let mut successes = 0;
        let mut already_active = 0;
        for h in handles {
            let r = h.await.expect("task join");
            if r.is_error == Some(true) {
                assert_eq!(first_text(&r), "watch mode is already active");
                already_active += 1;
            } else {
                assert_eq!(first_text(&r), "{\"watching\":true}");
                successes += 1;
            }
        }
        assert_eq!(successes, 1, "exactly one watch_start must win");
        assert_eq!(already_active, TASKS - 1);
        assert_watch_handle_present(&server).await;

        let _ = watch_stop(&server.inner);
        drop(dir);
    }

    /// Sanity check: the constants and types we lock in survive a
    /// round-trip through their published API surfaces. Failing here
    /// would indicate the dependency upgraded under us in a way that
    /// changed the debouncer-event shape.
    #[test]
    fn debounce_constants_are_well_formed() {
        assert_eq!(DEBOUNCE_TIMEOUT, Duration::from_millis(250));
        // Just a structural check that DebouncedEvent is still the type
        // we forward — compile-time only.
        let _: fn(DebounceEventResult) = |_| {};
        // And that we can name the path type the API hands back.
        let _: fn(&Path) = |_| {};
    }

    // -- try_reindex_file ----------------------------------------------

    use crate::handlers::symbols::get_file_symbols;

    /// Convert a `CallToolResult` body to a JSON value. Helper for the
    /// reindex tests that need to assert on symbol-list shapes.
    fn body_json(r: &CallToolResult) -> serde_json::Value {
        serde_json::from_str(&first_text(r)).expect("body is JSON")
    }

    /// When `index_lock` is held externally (modeling a concurrent
    /// `analyze_codebase`), `try_reindex_file` must drop the event without
    /// touching the graph. This is the design-canonical behavior — see
    /// the comment on the `try_lock` site.
    #[tokio::test]
    async fn try_reindex_file_drops_event_when_index_lock_held() {
        let (server, dir) = indexed_server().await;
        let inner = Arc::clone(&server.inner);

        // Take a snapshot of file count before the lock-contended call.
        let before_files = inner.graph.read().stats().files;

        // Hold the index_lock externally.
        let _held = inner.index_lock.try_lock().expect("first lock");

        // Modify a.cpp on disk so a successful reindex would change the
        // graph. The lock-contended path must NOT pick this up.
        let a_cpp = dir.path().join("a.cpp");
        std::fs::write(&a_cpp, b"void changed() {}\n").unwrap();

        let outcome = try_reindex_file(&inner, &a_cpp, false).await;
        match outcome {
            ReindexOutcome::LockContended => {}
            other => panic!("expected LockContended, got {other:?}"),
        }

        // Graph file count unchanged; the new symbol name didn't appear.
        let after_files = inner.graph.read().stats().files;
        assert_eq!(
            before_files, after_files,
            "lock contention must leave file count unchanged"
        );
        let abs_a_cpp = std::fs::canonicalize(&a_cpp).unwrap();
        let r = get_file_symbols(
            &inner.graph,
            &abs_a_cpp.to_string_lossy(),
            false,
            true,
            None,
            None,
            false,
            NO_BYTE_BUDGET,
        );
        let body = body_json(&r);
        // The response is a Page<SymbolResult> envelope.
        let arr = body["results"].as_array().expect("results array");
        assert!(
            arr.iter().all(|s| s["name"].as_str() != Some("changed")),
            "lock-contended call must NOT have re-parsed; got {body}"
        );

        drop(_held);
        drop(dir);
    }

    /// `try_reindex_file` must read `inner.config` (the cached snapshot
    /// from the most recent `analyze_codebase`) rather than re-loading
    /// `<root>/.code-graph.toml` from disk on every event. A direct probe
    /// is hard without instrumentation; the practical assertion is:
    /// (1) we mutate `inner.config` in-place; (2) we drop a different
    /// config on disk; (3) after a reindex, `inner.config` is unchanged
    /// — proving the watch path didn't replace it via a disk read.
    #[tokio::test]
    async fn try_reindex_file_uses_cached_config_not_disk_config() {
        let (server, dir) = indexed_server().await;
        let inner = Arc::clone(&server.inner);

        // Mutate the cached config in-process to a sentinel value.
        let sentinel_threads = 7usize;
        {
            let mut cfg = inner.config.write();
            cfg.parsing.max_threads = sentinel_threads;
            cfg.discovery.max_threads = sentinel_threads;
        }

        // Drop a different on-disk config that would override the cached
        // one if the watch path re-loaded it.
        std::fs::write(
            dir.path().join(".code-graph.toml"),
            "[parsing]\nmax_threads = 1\n[discovery]\nmax_threads = 1\n",
        )
        .unwrap();

        // Trigger a reindex of an existing file.
        let a_cpp = std::fs::canonicalize(dir.path().join("a.cpp")).unwrap();
        std::fs::write(&a_cpp, b"void post_change() {}\n").unwrap();
        let outcome = try_reindex_file(&inner, &a_cpp, false).await;
        match outcome {
            ReindexOutcome::Reindexed => {}
            other => panic!("expected Reindexed, got {other:?}"),
        }

        // Cached config still holds the sentinel — the watch path did
        // NOT replace it from disk.
        let cfg_after = inner.config.read();
        assert_eq!(
            cfg_after.parsing.max_threads, sentinel_threads,
            "watch path must NOT re-read .code-graph.toml; cached config must be preserved"
        );
        assert_eq!(cfg_after.discovery.max_threads, sentinel_threads);

        drop(dir);
    }

    /// Modify-path: re-parsing a changed file must surface the new symbol
    /// names through `get_file_symbols`.
    #[tokio::test]
    async fn try_reindex_file_modify_updates_graph() {
        let (server, dir) = indexed_server().await;
        let inner = Arc::clone(&server.inner);
        let a_cpp = std::fs::canonicalize(dir.path().join("a.cpp")).unwrap();

        // Replace the function body with a new function name.
        std::fs::write(&a_cpp, b"void brand_new_function() {}\n").unwrap();

        let outcome = try_reindex_file(&inner, &a_cpp, false).await;
        match outcome {
            ReindexOutcome::Reindexed => {}
            other => panic!("expected Reindexed, got {other:?}"),
        }

        let r = get_file_symbols(
            &inner.graph,
            &a_cpp.to_string_lossy(),
            false,
            true,
            None,
            None,
            false,
            NO_BYTE_BUDGET,
        );
        let body = body_json(&r);
        // The response is a Page<SymbolResult> envelope.
        let names: Vec<&str> = body["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|s| s["name"].as_str())
            .collect();
        assert!(
            names.contains(&"brand_new_function"),
            "modify reindex must surface new symbol name; got {names:?}"
        );
        assert!(
            !names.contains(&"f"),
            "old symbol must be gone; got {names:?}"
        );

        drop(dir);
    }

    /// Remove-path: deleting a file and reindexing with `is_remove=true`
    /// must drop the file's symbols. Subsequent `get_file_symbols` returns
    /// the wire-canonical "no symbols found" error.
    #[tokio::test]
    async fn try_reindex_file_remove_drops_file_from_graph() {
        let (server, dir) = indexed_server().await;
        let inner = Arc::clone(&server.inner);
        let a_cpp = std::fs::canonicalize(dir.path().join("a.cpp")).unwrap();

        // Sanity: the file exists in the graph before removal.
        assert!(!inner.graph.read().file_symbols(&a_cpp).is_empty());

        // Delete on disk and call try_reindex_file with is_remove=true.
        std::fs::remove_file(&a_cpp).unwrap();
        let outcome = try_reindex_file(&inner, &a_cpp, true).await;
        match outcome {
            ReindexOutcome::Reindexed => {}
            other => panic!("expected Reindexed for remove path, got {other:?}"),
        }

        // get_file_symbols now produces the canonical not-found wording.
        let path_str = a_cpp.to_string_lossy().into_owned();
        let r = get_file_symbols(
            &inner.graph,
            &path_str,
            false,
            true,
            None,
            None,
            false,
            NO_BYTE_BUDGET,
        );
        assert_eq!(r.is_error, Some(true));
        assert_eq!(
            first_text(&r),
            format!("no symbols found in file: {path_str}"),
        );

        drop(dir);
    }

    /// Watch-path call site: `try_reindex_file` must invoke `post_index`
    /// over the full graph set (existing graphs + the freshly-parsed
    /// `new_fg`) before the per-edge resolve loop. The watch path is a
    /// SEPARATE re-index path from `index_directory`, and omitting the
    /// hook here silently regresses plugins with crate-aware analysis
    /// on every watched file save.
    ///
    /// Uses a recording plugin that logs each `post_index` invocation;
    /// the test drives an initial analyze (which produces one hook call)
    /// followed by a watch reindex (which must produce another).
    #[tokio::test]
    async fn try_reindex_file_invokes_post_index_over_full_graph_set() {
        use code_graph_core::Language;
        use std::sync::{Arc, Mutex};

        use crate::test_recording_plugin::{Log, RecordingPlugin};

        let calls: Log = Arc::new(Mutex::new(Vec::new()));
        let mut reg = LanguageRegistry::new();
        // Use a synthetic `.rec` extension so the test doesn't fight
        // real C++ parser behavior.
        reg.register(Box::new(RecordingPlugin::new(
            Language::Cpp,
            &[".rec"],
            Arc::clone(&calls),
        )))
        .unwrap();

        let server = crate::server::CodeGraphServer::new(reg);

        // Seed two files so a missed iteration would be observable in
        // the post_index call's path list.
        let dir = TempDir::new().expect("TempDir");
        std::fs::write(dir.path().join("a.rec"), b"// a\n").unwrap();
        std::fs::write(dir.path().join("b.rec"), b"// b\n").unwrap();

        // Initial analyze produces one post_index call over both files.
        let r = analyze_codebase(
            server.inner.clone(),
            dir.path().to_string_lossy().into_owned(),
            true,
            None,
            None,
        )
        .await;
        assert!(
            r.is_error.is_none() || r.is_error == Some(false),
            "initial analyze must succeed: {r:?}"
        );
        let analyze_log = calls.lock().unwrap().clone();
        assert_eq!(
            analyze_log.len(),
            1,
            "analyze path fires post_index exactly once: {analyze_log:?}"
        );
        assert_eq!(
            analyze_log[0].len(),
            2,
            "analyze post_index must see both seeded files: {:?}",
            analyze_log[0]
        );

        // Modify one file on disk and drive try_reindex_file directly
        // (no debouncer wait — same determinism rationale as the other
        // watch-path tests in this module).
        let a_rec = std::fs::canonicalize(dir.path().join("a.rec")).unwrap();
        std::fs::write(&a_rec, b"// a edited\n").unwrap();

        let outcome = try_reindex_file(&server.inner, &a_rec, false).await;
        match outcome {
            ReindexOutcome::Reindexed => {}
            other => panic!("expected Reindexed, got {other:?}"),
        }

        let total_log = calls.lock().unwrap().clone();
        assert_eq!(
            total_log.len(),
            2,
            "watch path must invoke post_index exactly once per try_reindex_file \
             on top of the initial analyze call: {total_log:?}"
        );
        let watch_call = &total_log[1];
        // The hook must see BOTH files (existing b.rec + freshly-parsed
        // a.rec), not just the changed file.
        assert_eq!(
            watch_call.len(),
            2,
            "watch post_index must see the full graph set (existing + new), \
             not just the changed file: {watch_call:?}"
        );
        let want_a = a_rec.to_string_lossy().into_owned();
        let want_b = std::fs::canonicalize(dir.path().join("b.rec"))
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            watch_call.iter().any(|p| p == &want_a),
            "watch post_index must include the re-parsed file {want_a:?}; got {watch_call:?}"
        );
        assert!(
            watch_call.iter().any(|p| p == &want_b),
            "watch post_index must include the pre-existing file {want_b:?}; got {watch_call:?}"
        );

        drop(dir);
    }

    /// Copy-back round-trip: prove that mutations a `post_index` hook
    /// writes into `all_graphs[last]` actually reach `new_fg` (and thus
    /// the merged `Graph`). The sibling
    /// `try_reindex_file_invokes_post_index_over_full_graph_set` test only
    /// records that the hook fires — deleting the copy-back line that
    /// lifts the post-hook last-entry back into `new_fg` would not change
    /// that test's outcome. This test fails outright if the copy-back is
    /// removed.
    ///
    /// Mechanism: a mutating recording plugin rewrites the LAST graph's
    /// first symbol's `namespace` to a sentinel string during the watch
    /// reindex's `post_index` call. Without the copy-back, the merged
    /// `Graph::nodes` carries the pre-hook (empty) namespace; with the
    /// copy-back, it carries the sentinel. `Symbol::namespace` is keyed
    /// off the same `(file, name)` symbol ID either way — only the
    /// namespace field changes — so the assertion below is sensitive to
    /// the copy-back specifically, not to any structural difference.
    #[tokio::test]
    async fn mutating_post_index_writes_survive_copy_back() {
        use code_graph_core::Language;
        use std::sync::{Arc, Mutex};

        use crate::test_recording_plugin::{Log, RecordingPlugin};

        const SENTINEL: &str = "post_index_sentinel_ns";

        let calls: Log = Arc::new(Mutex::new(Vec::new()));
        let mut reg = LanguageRegistry::new();
        // Mutator: rewrite the LAST graph's first symbol's `namespace` to
        // SENTINEL. The last graph is the freshly-parsed `new_fg` clone
        // the watch path pushed at line 374 — this is the slot that
        // line 394's copy-back must lift back into `new_fg` for the
        // sentinel to survive merge.
        let mutator: crate::test_recording_plugin::Mutator =
            Box::new(|graphs: &mut [code_graph_core::FileGraph], _file_index| {
                if let Some(last) = graphs.last_mut() {
                    if let Some(sym) = last.symbols.first_mut() {
                        sym.namespace = SENTINEL.to_string();
                    }
                }
            });
        reg.register(Box::new(RecordingPlugin::with_mutator(
            Language::Cpp,
            &[".rec"],
            Arc::clone(&calls),
            mutator,
        )))
        .unwrap();

        let server = crate::server::CodeGraphServer::new(reg);

        // Seed two files so the `all_graphs` set has more than just the
        // re-parsed file in it during the watch hook (mirrors the
        // structure the existing post_index test uses).
        let dir = TempDir::new().expect("TempDir");
        std::fs::write(dir.path().join("a.rec"), b"// a\n").unwrap();
        std::fs::write(dir.path().join("b.rec"), b"// b\n").unwrap();

        let r = analyze_codebase(
            server.inner.clone(),
            dir.path().to_string_lossy().into_owned(),
            true,
            None,
            None,
        )
        .await;
        assert!(
            r.is_error.is_none() || r.is_error == Some(false),
            "initial analyze must succeed: {r:?}"
        );

        // Drive the watch path on a.rec. The mutator runs once during
        // the post_index hook and writes SENTINEL into the last graph's
        // first symbol. The copy-back at line 394 lifts that state into
        // `new_fg`, which `merge_file_graph` then inserts into the
        // Graph's node table.
        let a_rec = std::fs::canonicalize(dir.path().join("a.rec")).unwrap();
        std::fs::write(&a_rec, b"// a edited\n").unwrap();
        let outcome = try_reindex_file(&server.inner, &a_rec, false).await;
        match outcome {
            ReindexOutcome::Reindexed => {}
            other => panic!("expected Reindexed, got {other:?}"),
        }

        // Observation: pull a.rec's symbols out of the merged graph and
        // confirm the sentinel namespace is visible. Deleting line 394's
        // copy-back leaves `new_fg` carrying the pre-hook (empty)
        // namespace, and this assertion fails.
        let symbols = server.inner.graph.read().file_symbols(&a_rec);
        assert!(
            !symbols.is_empty(),
            "watch reindex must populate a.rec's symbols in the merged graph"
        );
        let observed: Vec<&str> = symbols.iter().map(|s| s.namespace.as_str()).collect();
        assert!(
            observed.contains(&SENTINEL),
            "post_index sentinel namespace must survive copy-back into new_fg; got: {observed:?}"
        );

        drop(dir);
    }

    /// Non-source paths (e.g. `.txt`) must short-circuit with `NotASource`
    /// and not mutate the graph. The watch loop pre-filters these too, but
    /// the per-file routine defends in case a future caller drops the
    /// pre-filter.
    #[tokio::test]
    async fn try_reindex_file_skips_non_source_files() {
        let (server, dir) = indexed_server().await;
        let inner = Arc::clone(&server.inner);

        let txt = dir.path().join("README.txt");
        std::fs::write(&txt, b"hello\n").unwrap();
        let txt = std::fs::canonicalize(&txt).unwrap();
        let stats_before = inner.graph.read().stats();

        let outcome = try_reindex_file(&inner, &txt, false).await;
        match outcome {
            ReindexOutcome::NotASource => {}
            other => panic!("expected NotASource, got {other:?}"),
        }

        let stats_after = inner.graph.read().stats();
        assert_eq!(
            stats_before, stats_after,
            "non-source path must not mutate graph"
        );

        drop(dir);
    }

    // --- canonicalize_event_path: Windows-correctness regression suite ---
    //
    // These tests pin the dispatch-boundary normalization that
    // `process_event_batch` calls on every event path. The actual
    // Windows-specific failure mode (verbatim `\\?\` prefix bypassing
    // `PathTrie::contains_path`) is `#[cfg(windows)]`-gated below; the
    // helper-shape tests above it run on every platform so a future
    // regression that deletes `canonicalize_event_path` or stops
    // calling it from `process_event_batch` breaks Linux CI.

    /// `canonicalize_event_path` on an existing file with
    /// `exists_hint = true` returns the canonical form (matches what
    /// the indexer would store). On Linux this is the dunce-identity
    /// path; the test pins the contract on both platforms.
    #[test]
    fn canonicalize_event_path_returns_canonical_form_when_file_exists() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a.cpp");
        std::fs::write(&file, b"void f() {}\n").unwrap();
        let canonical = paths::canonicalize(&file).unwrap();
        let got = canonicalize_event_path(&file, true);
        assert_eq!(
            got, canonical,
            "with exists_hint=true on a real file the result must match paths::canonicalize"
        );
    }

    /// `canonicalize_event_path` on a non-existent path with
    /// `exists_hint = false` (the remove-event case) falls back to
    /// `paths::simplify` — no IO probe, lexical strip only. Critical
    /// because remove events fire AFTER the file is gone; canonicalize
    /// would error and produce an inconsistent shape.
    #[test]
    fn canonicalize_event_path_uses_lexical_strip_on_remove_event() {
        let dir = TempDir::new().unwrap();
        let gone = dir.path().join("already_deleted.cpp");
        // Never written. canonicalize would fail. exists_hint=false
        // tells the helper to skip the IO probe.
        let got = canonicalize_event_path(&gone, false);
        let expected = paths::simplify(&gone);
        assert_eq!(got, expected);
    }

    /// `canonicalize_event_path` on a non-existent path with the
    /// optimistic `exists_hint = true` falls back to lexical strip
    /// (rather than panicking) when the IO probe fails. This is the
    /// race-window case: a create/modify event fires, the user
    /// deletes the file before the watch debounce expires, and the
    /// handler reaches this path with `exists_hint = true`. Must not
    /// crash; falling back to simplify keeps subsequent
    /// `contains_path` checks on a consistent shape.
    #[test]
    fn canonicalize_event_path_falls_back_to_simplify_on_failed_canonicalize() {
        let dir = TempDir::new().unwrap();
        let gone = dir.path().join("raced.cpp");
        let got = canonicalize_event_path(&gone, true);
        let expected = paths::simplify(&gone);
        assert_eq!(got, expected);
    }

    /// Windows-only: `canonicalize_event_path` strips the verbatim
    /// `\\?\D:\…` prefix that `notify-debouncer-full` delivers from
    /// `ReadDirectoryChangesW`. Without the strip, the post-canonicalize
    /// path would be a distinct `PathTrie` key from the indexer-stored
    /// (stripped) form and every watched edit would duplicate the
    /// file entry in the graph. Pins the production fix for the
    /// "Watch-mode path re-contamination on Windows" known limitation.
    #[cfg(windows)]
    #[test]
    fn canonicalize_event_path_strips_verbatim_disk_prefix_on_windows() {
        use std::path::PathBuf;
        // Use a real on-disk path so paths::canonicalize succeeds.
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a.cpp");
        std::fs::write(&file, b"void f() {}\n").unwrap();
        // Construct the verbatim form that notify would deliver.
        let canonical = paths::canonicalize(&file).unwrap();
        let canonical_str = canonical.to_string_lossy();
        let verbatim = PathBuf::from(format!(r"\\?\{}", canonical_str));
        let stripped = canonicalize_event_path(&verbatim, true);
        assert!(
            !stripped.to_string_lossy().contains(r"\\?\"),
            "verbatim prefix must be stripped: {}",
            stripped.display()
        );
        assert_eq!(
            stripped, canonical,
            "stripped form must match what paths::canonicalize produces on the bare path"
        );
    }
}
