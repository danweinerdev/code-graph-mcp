//! `analyze_codebase` handler body.
//!
//! Coordinates the full pipeline:
//! 1. Single-flight lock (`index_lock.try_lock` — second concurrent call gets
//!    `"indexing already in progress"`).
//! 2. Path validation + canonicalization.
//! 3. `RootConfig::load` + `resolve_concurrency` (warnings flow into the
//!    response).
//! 4. Cache fast-path: `Graph::load` + `stale_paths`. Cache hit + zero stale
//!    files short-circuits without re-parsing.
//! 5. Cache miss / force / stale path: spawn the rayon parse pool inside
//!    `tokio::task::spawn_blocking`, forward progress events to
//!    `peer.notify_progress` from a sibling task, merge into the in-memory
//!    graph under a write lock, persist to cache (best-effort).
//! 6. Return `AnalyzeResult` JSON matching the Go shape (`files`, `symbols`,
//!    `edges`, `root_path`, `warnings`).

use std::sync::atomic::Ordering;
use std::sync::Arc;

use code_graph_core::{paths, ConfigError, RootConfig};
use code_graph_graph::Graph;
use rmcp::model::{CallToolResult, ProgressNotificationParam, ProgressToken};
use rmcp::service::RoleServer;
use rmcp::Peer;
use serde::Serialize;

use crate::indexer::{
    build_file_index, build_symbol_index, extend_file_index, extend_symbol_index, index_directory,
    resolve_edges_with_indexes, ChannelProgressSink, ProgressEvent,
};
use crate::server::ServerInner;

use super::{tool_error, tool_success_json};

/// JSON-shape mirror of Go's `analyzeResult` in `internal/tools/analyze.go`.
/// Field order, names, and `omitempty` semantics match the Go struct exactly.
#[derive(Debug, Serialize, Clone)]
pub struct AnalyzeResult {
    pub files: u32,
    pub symbols: u32,
    pub edges: u32,
    pub root_path: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Wall-clock nanoseconds since UNIX_EPOCH, suitable for cache mtimes
/// and sweep cadence math. Encodes the two failure modes explicitly so
/// they don't silently produce garbage:
///
/// - Clock before UNIX_EPOCH (pre-1970 system clock): `duration_since`
///   returns `Err`. Fall back to `0` — matches the `last_sweep_at`
///   "never swept" sentinel.
/// - `Duration::as_nanos()` (u128) overflows `u64` (~year 2554):
///   saturate to `u64::MAX` instead of the silent `as u64` truncation
///   the predecessor pattern used. Saturating up keeps `now > prior`
///   for any sane previously-stored value, so `elapsed_since_sweep`
///   evaluates large and the sweep runs (conservative). A truncating
///   cast would wrap to a small value and the cadence check would
///   skip the sweep indefinitely.
///
/// Every `analyze_codebase` time read goes through this so the
/// failure semantics stay uniform. Direct
/// `SystemTime::now().as_nanos() as u64` is the silent-truncation
/// footgun this helper exists to forbid.
pub(crate) fn now_nanos_u64() -> u64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Err(_) => 0,
        Ok(d) => u64::try_from(d.as_nanos()).unwrap_or(u64::MAX),
    }
}

/// `analyze_codebase` body. See the module docstring for the full pipeline.
pub async fn analyze_codebase(
    inner: Arc<ServerInner>,
    path_raw: String,
    force: bool,
    peer: Option<Peer<RoleServer>>,
    progress_token: Option<ProgressToken>,
) -> CallToolResult {
    let Ok(_guard) = inner.index_lock.try_lock() else {
        return tool_error("indexing already in progress");
    };

    if path_raw.is_empty() {
        return tool_error("'path' is required");
    }

    let abs_path = match paths::canonicalize(std::path::Path::new(&path_raw)) {
        Ok(p) => p,
        Err(_) => {
            return tool_error(format!("directory does not exist: {path_raw}"));
        }
    };
    if !abs_path.is_dir() {
        // We deliberately distinguish "path doesn't resolve" from "path resolves
        // to a file, not a directory" — the Go binary collapses both into a single
        // "directory does not exist" message, but Rust's `paths::canonicalize`
        // already gave us the richer information and discarding it just for Go
        // byte-identity would make the error less helpful for no real benefit.
        // The snapshot suite locks in this Rust-specific wording.
        return tool_error(format!("path is not a directory: {}", abs_path.display()));
    }

    // `RootConfig::load` walks from `abs_path` upward looking for the
    // nearest `.code-graph.toml`. The returned `project_root` is the
    // directory containing the discovered toml, or `abs_path` itself
    // when no toml was found at any ancestor. Cache lookup,
    // indexing-scope warnings, and the AnalyzeResult key off this
    // `project_root`; the indexing SCOPE itself remains `abs_path`
    // (the user's invocation path), so a deep-subtree analyze does not
    // expand to cover the full project.
    let (mut cfg, project_root) = match RootConfig::load(&abs_path) {
        Ok((c, root)) => (c, root),
        Err(ConfigError::Toml(e)) => {
            return tool_error(format!("failed to parse .code-graph.toml: {e}"));
        }
        Err(ConfigError::Io(e)) => {
            return tool_error(format!("failed to read .code-graph.toml: {e}"));
        }
        // Any new `ConfigError` variant must be mapped here — the catch-all
        // path produces less-helpful errors.
        Err(e @ ConfigError::ExtensionMissingDot { .. })
        | Err(e @ ConfigError::ExtensionConflict { .. })
        | Err(e @ ConfigError::MacroStripConflict { .. }) => {
            return tool_error(format!("invalid .code-graph.toml: {e}"));
        }
    };
    let mut warnings = cfg.resolve_concurrency();

    // Surface discovery state to the user via warnings. Three cases:
    // - Config discovered at a parent (invocation_path is inside the
    //   project) → informational; the user should know which toml
    //   applied and that the project lives upstream of their cwd.
    // - No toml found anywhere up to filesystem root → the
    //   project-root falls back to the invocation path, and the user
    //   loses macro_strip / extensions / etc. defaults. Loudly call
    //   out the likely consequence (UE-style `class CORE_API Foo` not
    //   indexed) so the user can act.
    // - Toml at the invocation path itself → silent (the common case).
    if project_root != abs_path {
        // Discovery succeeded but at an ancestor. Distinguish by checking
        // whether the toml file exists at project_root (it does — that's
        // how we found it).
        let toml_at_root = project_root.join(".code-graph.toml");
        if toml_at_root.exists() {
            warnings.push(format!(
                "using .code-graph.toml found at {} (parent of indexed root {}); \
                 cache lives at the project root, indexing scope stays at the invocation path",
                project_root.display(),
                abs_path.display()
            ));
        } else {
            // project_root walked all the way to filesystem root without
            // finding a toml. `RootConfig::load` returns
            // `start.to_path_buf()` in this case, so `project_root ==
            // abs_path` and we shouldn't hit this branch — but guard
            // explicitly so a future change to the fallback semantics
            // doesn't silently produce confusing warnings.
            warnings.push(format!(
                "no .code-graph.toml found between {} and filesystem root; \
                 using built-in defaults. C++ classes prefixed with API-export macros \
                 (e.g. `class CORE_API Foo`) will NOT be indexed. Place a .code-graph.toml \
                 with [cpp].macro_strip at your project root to enable engine-style support.",
                abs_path.display()
            ));
        }
    } else {
        // No upward walk happened OR walk found toml right at abs_path.
        // Distinguish by checking if a toml file actually exists at the
        // location — absence means "no config anywhere" (the warn-loudly
        // case), presence means "toml right at invocation root" (silent).
        let toml_at_invocation = abs_path.join(".code-graph.toml");
        if !toml_at_invocation.exists() {
            warnings.push(format!(
                "no .code-graph.toml found between {} and filesystem root; \
                 using built-in defaults. C++ classes prefixed with API-export macros \
                 (e.g. `class CORE_API Foo`) will NOT be indexed. Place a .code-graph.toml \
                 with [cpp].macro_strip at your project root to enable engine-style support.",
                abs_path.display()
            ));
        }
    }

    // Orphan-cache detection: a pre-fix cache sitting at the invocation
    // path (where the old behaviour wrote it) is now orphaned by the
    // move to project-root co-location. Warn so the user can reclaim
    // disk. Only fires when invocation_path differs from project_root —
    // otherwise the two are the same location and there's no orphan.
    if abs_path != project_root {
        let invocation_cache = code_graph_graph::cache_path(&abs_path);
        if invocation_cache.exists() {
            warnings.push(format!(
                "orphan cache detected at {} — the indexer now caches at the project root ({}). \
                 The orphan is not used and can be deleted to reclaim disk.",
                invocation_cache.display(),
                project_root.display()
            ));
        }
    }

    // Cache fast-path: when not forced, attempt cache load + scoped
    // stale check at the project root. We only short-circuit if:
    //   (a) the cache contains AT LEAST ONE file in the invocation
    //       scope (otherwise the user is indexing a new subtree —
    //       a fast-path return would yield an empty-scope graph that
    //       hides whatever's actually under their cwd);
    //   (b) every in-scope cached file is mtime-fresh. Out-of-scope
    //       files might be stale but that's the lazy/scoped contract
    //       — the sweep below handles them.
    //
    // Even on the fast-path we still run the opportunistic
    // out-of-scope hygiene sweep if its cadence has elapsed: cache
    // ghost entries from deleted out-of-scope files should be cleaned
    // up regardless of whether the parse pipeline ran.
    //
    // Known limitation: this fast-path does NOT detect NEW files
    // added to an already-indexed scope. The cached mtimes only
    // include previously-indexed files, so a brand-new file in the
    // scope is invisible to the staleness check and the fast-path
    // returns without picking it up. Workaround: re-run with
    // `force=true` after adding files to a scope. A full discovery
    // walk inside the fast-path would close this gap but at the cost
    // of every fast-path hit doing a stat-walk of the scope.
    if !force {
        let mut probe = Graph::new();
        // Combined load + staleness check in one mmap+bytecheck pass.
        // The separate `Graph::load` + `stale_paths` calls would have
        // mmap'd + bytecheck'd the cache file twice; on a ~25 MB cache
        // each bytecheck is 10–50 ms, so the fast-path was paying
        // 20–100 ms of avoidable work per cache-hit invocation.
        let (load_ok, all_stale) = probe
            .load_and_stale(&project_root)
            .unwrap_or((false, Vec::new()));
        if load_ok && probe.files_in_scope_count(&abs_path) > 0 {
            // Filter stale to the invocation scope to honour
            // lazy/scoped semantics.
            let in_scope_stale: Vec<_> = all_stale
                .iter()
                .filter(|p| p.starts_with(&abs_path))
                .collect();
            if in_scope_stale.is_empty() {
                // Run the opportunistic out-of-scope sweep if cadence
                // elapsed. Even in the fast-path the sweep is the
                // right thing: deleted out-of-scope files should be
                // cleaned up regardless of whether parsing happened.
                let mut fast_path_warnings: Vec<String> = Vec::new();
                let now_nanos = now_nanos_u64();
                let elapsed_since_sweep = now_nanos.saturating_sub(probe.last_sweep_at());
                let sweep_ran = elapsed_since_sweep >= code_graph_graph::SWEEP_INTERVAL_NANOS;
                if sweep_ran {
                    let swept = probe.sweep_missing_out_of_scope(&abs_path);
                    if !swept.is_empty() {
                        fast_path_warnings.push(format!(
                            "out-of-scope sweep removed {} stale cache entry(ies) \
                             (files deleted in subtrees not touched by this invocation)",
                            swept.len()
                        ));
                    }
                    probe.set_last_sweep_at(now_nanos);
                }
                let stats = probe.stats();
                {
                    let mut g = inner.graph.write();
                    *g = probe;
                }
                *inner.root_path.write() = Some(project_root.clone());
                *inner.config.write() = cfg;
                inner.indexed.store(true, Ordering::Release);
                inner
                    .index_built_at
                    .store(now_nanos_u64(), Ordering::Release);
                inner.index_force_built.store(force, Ordering::Release);
                // Persist the swept graph so the cadence bump and any
                // removed entries survive. Skip the save when the
                // sweep was a no-op (cadence not elapsed AND nothing
                // changed); saving an unchanged cache is wasted I/O.
                if sweep_ran {
                    if let Err(e) = save_cache(&inner.graph, &project_root) {
                        fast_path_warnings.push(format!("cache save failed: {e}"));
                    }
                }
                warnings.extend(fast_path_warnings);
                let result = AnalyzeResult {
                    files: stats.files,
                    symbols: stats.nodes,
                    edges: stats.edges,
                    root_path: project_root.to_string_lossy().into_owned(),
                    warnings,
                };
                return tool_success_json(&result);
            }
            // In-scope stale files present: fall through to scoped
            // re-index. The probe we just loaded is the starting state;
            // we'll re-use it during the merge phase below rather than
            // re-loading from disk.
        }
    }

    // Full re-index path. Spawn the progress forwarder BEFORE
    // `spawn_blocking` so the receiver is alive when rayon workers start
    // sending — otherwise early events get dropped.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ProgressEvent>(64);

    // Diagnostic: surface whether the client passed a progressToken at
    // all. Investigated alongside the
    // `"[Tool result missing due to internal error]"` failure mode on
    // long analyses — knowing whether the client is asking for progress
    // is the first datum we need to decide if a notification flood is
    // implicated. Emitted at start so it lines up with the matching
    // request in any captured server transcript. Single line per call,
    // negligible cost, no PII.
    eprintln!(
        "[code-graph] analyze_codebase: progress_token_present={}",
        progress_token.is_some()
    );

    let forwarder = if let (Some(peer), Some(token)) = (peer, progress_token) {
        Some(tokio::spawn(async move {
            // Throttle outbound notifications to one per
            // `THROTTLE_INTERVAL`. Without this, the parallelized
            // resolve phase can fire ~3000 progress events/sec
            // (72k parse + 72k resolve over ~50s on LLVM), which
            // floods the Claude Code MCP client and is the leading
            // suspect for `"[Tool result missing due to internal
            // error]"` — Claude Code's
            // `ensureToolResultPairing` injects that synthetic
            // result when an assistant `tool_use` has no matching
            // `tool_result`, and a backed-up message queue is one
            // way the real tool result could be lost or processed
            // too late to count. The user-perceived progress UX
            // does not need every event; one update per 100ms is
            // 10/sec — plenty to feel responsive without
            // overwhelming the client. Coalesces to the LATEST
            // event, not the first — the user wants "where are we
            // now", not "where were we 100ms ago".
            const THROTTLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);
            let mut last_sent = std::time::Instant::now()
                .checked_sub(THROTTLE_INTERVAL)
                .unwrap_or_else(std::time::Instant::now);
            let mut latest: Option<ProgressEvent> = None;

            // Per-call timeout for `notify_progress`. The MCP client (e.g.
            // Claude Code hitting MCP_TOOL_TIMEOUT) may stop servicing the
            // stdio stream mid-analyze; the rmcp transport then stalls and
            // `peer.notify_progress(...).await` hangs on its responder
            // oneshot. Without this bound the forwarder would never drain,
            // and the parent's `handle.await` below would never resolve —
            // wedging `index_lock` for the lifetime of the process.
            const NOTIFY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

            while let Some(evt) = rx.recv().await {
                latest = Some(evt);
                let now = std::time::Instant::now();
                if now.duration_since(last_sent) >= THROTTLE_INTERVAL {
                    if let Some(e) = latest.take() {
                        last_sent = now;
                        let mut params =
                            ProgressNotificationParam::new(token.clone(), e.progress as f64);
                        if e.total > 0 {
                            params = params.with_total(e.total as f64);
                        }
                        params = params.with_message(e.message);
                        let _ = tokio::time::timeout(NOTIFY_TIMEOUT, peer.notify_progress(params))
                            .await;
                    }
                }
            }
            // Channel closed (indexer is done). Send the most recent
            // pending event so the client sees the final state
            // — otherwise the throttle could swallow the
            // last "100% done" tick if it landed inside the cooldown.
            if let Some(e) = latest {
                let mut params = ProgressNotificationParam::new(token.clone(), e.progress as f64);
                if e.total > 0 {
                    params = params.with_total(e.total as f64);
                }
                params = params.with_message(e.message);
                let _ = tokio::time::timeout(NOTIFY_TIMEOUT, peer.notify_progress(params)).await;
            }
        }))
    } else {
        // No progress token from the client → drain the channel locally so
        // `try_send` calls in the indexer don't fail-and-warn: a `recv()`
        // task with nothing else to do is the cheapest "/dev/null" sink.
        Some(tokio::spawn(async move {
            while rx.recv().await.is_some() {
                // Drain.
            }
        }))
    };

    let registry = Arc::clone(&inner);
    let cfg_for_pool = cfg.clone();
    let abs_path_for_pool = abs_path.clone();
    let project_root_for_pool = project_root.clone();
    let scope_is_project_root = abs_path == project_root;
    let blocking_handle = tokio::task::spawn_blocking(move || {
        let sink = ChannelProgressSink(tx);
        let mut blocking_warnings: Vec<String> = Vec::new();

        // Phase A: load any existing project-root cache as the starting
        // state. The merge model treats the cache as a project-wide
        // accumulator: prior scoped invocations contributed entries
        // that survive this invocation unless we explicitly evict them.
        eprintln!(
            "[code-graph] phase: loading cache from {}",
            project_root_for_pool.display()
        );
        let phase_start = std::time::Instant::now();
        let mut merged_graph = Graph::new();
        let cache_loaded = merged_graph.load(&project_root_for_pool).unwrap_or(false);
        eprintln!(
            "[code-graph] phase: cache load {} ({:.1}s, {} cached files)",
            if cache_loaded { "ok" } else { "absent/stale" },
            phase_start.elapsed().as_secs_f64(),
            merged_graph.stats().files
        );

        // Phase B: apply scope policy. Three cases:
        //  - force=true at project root → full clobber (today's
        //    behavior — clear everything and rebuild).
        //  - force=true at a subdir → drop in-scope entries only; the
        //    rest of the project cache survives.
        //  - !force → evict in-scope files that no longer exist on
        //    disk so the upcoming merge starts from a known state for
        //    the scope being re-parsed.
        if force {
            if scope_is_project_root {
                merged_graph.clear();
            } else if cache_loaded {
                let dropped = merged_graph.drop_files_in_scope(&abs_path_for_pool);
                if !dropped.is_empty() {
                    blocking_warnings.push(format!(
                        "force=true dropped {} cached file(s) under {} before re-index",
                        dropped.len(),
                        abs_path_for_pool.display()
                    ));
                }
            }
        } else if cache_loaded {
            let evicted = merged_graph.evict_missing_in_scope(&abs_path_for_pool);
            if !evicted.is_empty() {
                blocking_warnings.push(format!(
                    "evicted {} cached file(s) under {} (no longer present on disk)",
                    evicted.len(),
                    abs_path_for_pool.display()
                ));
            }
        }

        // Phase C: discover + parse files in scope. `index_directory`
        // walks `abs_path_for_pool` (the invocation path), not the
        // project root — scope follows the user's invocation, even
        // though the cache lives at the project root.
        eprintln!(
            "[code-graph] phase: discovering + parsing under {}",
            abs_path_for_pool.display()
        );
        let phase_start = std::time::Instant::now();
        let (mut fresh_graphs, parse_warnings) =
            match index_directory(&abs_path_for_pool, &registry.registry, &cfg_for_pool, &sink) {
                Ok(v) => v,
                Err(e) => return Err(e.to_string()),
            };
        blocking_warnings.extend(parse_warnings);
        eprintln!(
            "[code-graph] phase: discover+parse done ({:.1}s, {} files parsed)",
            phase_start.elapsed().as_secs_f64(),
            fresh_graphs.len()
        );

        // Phase D: build cross-scope indexes. Fresh edges resolve
        // against the union of cached symbols/files (from prior
        // scoped invocations) and freshly-parsed symbols/files (from
        // this invocation). Documented asymmetry: cached EDGES are
        // not re-resolved here, so old "unresolved" edges from prior
        // invocations stay unresolved even when their would-be target
        // is now in the cache. The user can `force=true` at the
        // originating subtree to re-resolve.
        eprintln!("[code-graph] phase: resolving edges");
        let phase_start = std::time::Instant::now();
        let cached_snapshot = merged_graph.file_graphs_snapshot();
        let mut symbol_index = build_symbol_index(&cached_snapshot);
        extend_symbol_index(&mut symbol_index, &fresh_graphs);
        let mut file_index = build_file_index(&cached_snapshot);
        extend_file_index(&mut file_index, &fresh_graphs);

        // Phase E: resolve fresh edges against the combined indexes.
        resolve_edges_with_indexes(
            &mut fresh_graphs,
            &symbol_index,
            &file_index,
            &registry.registry,
            &sink,
        );
        eprintln!(
            "[code-graph] phase: resolve done ({:.1}s)",
            phase_start.elapsed().as_secs_f64()
        );

        // Phase F: merge fresh FileGraphs into the project graph.
        // `merge_file_graph` is idempotent per-path: if a file was
        // already in the cache and we just re-parsed it (the
        // incremental-stale or force-scoped case), the merge replaces
        // its prior entries cleanly.
        eprintln!(
            "[code-graph] phase: merging {} fresh file(s) into project graph",
            fresh_graphs.len()
        );
        let phase_start = std::time::Instant::now();
        for fg in fresh_graphs {
            merged_graph.merge_file_graph(fg);
        }
        eprintln!(
            "[code-graph] phase: merge done ({:.1}s, total {} files in graph)",
            phase_start.elapsed().as_secs_f64(),
            merged_graph.stats().files
        );

        // Phase G: opportunistic out-of-scope sweep. If at least
        // SWEEP_INTERVAL_NANOS has elapsed since the last sweep, stat
        // every cached file OUTSIDE the invocation scope and drop the
        // ones missing on disk. Keeps the cache from accumulating
        // ghost entries from subtrees this invocation didn't touch.
        // Cost: O(files-out-of-scope) syscalls, but only on the sweep
        // cadence (default 24h).
        let now_nanos = now_nanos_u64();
        let elapsed_since_sweep = now_nanos.saturating_sub(merged_graph.last_sweep_at());
        if elapsed_since_sweep >= code_graph_graph::SWEEP_INTERVAL_NANOS {
            let swept = merged_graph.sweep_missing_out_of_scope(&abs_path_for_pool);
            if !swept.is_empty() {
                blocking_warnings.push(format!(
                    "out-of-scope sweep removed {} stale cache entry(ies) \
                     (files deleted in subtrees not touched by this invocation)",
                    swept.len()
                ));
            }
            merged_graph.set_last_sweep_at(now_nanos);
        }

        // Drop the sink (sender) so the forwarder task exits cleanly.
        drop(sink);
        Ok::<_, String>((merged_graph, blocking_warnings))
    });

    let blocking_result = blocking_handle.await;

    // Wait for the forwarder to finish draining the channel (the sink was
    // dropped at the end of the blocking task). Best-effort: a panic in the
    // forwarder is non-fatal because progress notifications are advisory.
    //
    // The wait is bounded so `index_lock` cannot be wedged by a stalled MCP
    // transport (the per-call timeout inside the forwarder is the primary
    // bound; this outer cap is defense-in-depth in case a future change
    // introduces a new unbounded await in the forwarder body). If the
    // deadline fires the JoinHandle is dropped, detaching the spawned task:
    // it stays alive in the background and is harmless because it holds no
    // server state, while the parent function continues to save the cache
    // and release `_guard`.
    if let Some(handle) = forwarder {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    }

    let (merged_graph, blocking_warnings) = match blocking_result {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            return tool_error(format!("indexing failed: {e}"));
        }
        Err(join_err) => {
            return tool_error(format!("indexing task panicked: {join_err}"));
        }
    };

    warnings.extend(blocking_warnings);

    if merged_graph.stats().files == 0 {
        return tool_error(format!(
            "no supported source files found in {}",
            abs_path.display()
        ));
    }

    // Install the merged graph under the write lock. Held briefly:
    // assignment-by-value moves the graph in; no further work happens
    // under the lock.
    let stats = {
        let mut g = inner.graph.write();
        *g = merged_graph;
        g.stats()
    };

    *inner.root_path.write() = Some(project_root.clone());
    *inner.config.write() = cfg;
    inner.indexed.store(true, Ordering::Release);
    inner
        .index_built_at
        .store(now_nanos_u64(), Ordering::Release);
    inner.index_force_built.store(force, Ordering::Release);

    // Surface a project-vs-scope size hint when the cache contains
    // entries outside the invocation scope. Users running scoped
    // analyses occasionally lose track of how much of the project is
    // already cached vs. how much they just refreshed; one line in
    // warnings keeps it visible without spamming queries.
    if abs_path != project_root {
        let in_scope_count = {
            let g = inner.graph.read();
            g.files_in_scope_count(&abs_path)
        };
        let out_of_scope_count = stats.files.saturating_sub(in_scope_count as u32);
        if out_of_scope_count > 0 {
            warnings.push(format!(
                "project cache contains {} file(s) outside the current scope ({}); \
                 they are preserved across this invocation. Run analyze_codebase at {} \
                 to refresh them, or force=true at any subtree to invalidate it.",
                out_of_scope_count,
                abs_path.display(),
                project_root.display()
            ));
        }
    }

    // Persist to cache (best-effort: a save failure becomes a warning, not
    // a fatal). The cache co-locates with the project root, NOT the
    // invocation path. Subsequent scoped invocations under the same
    // project hit the same cache file and accumulate into it.
    eprintln!(
        "[code-graph] phase: saving cache to {}",
        project_root.display()
    );
    let save_start = std::time::Instant::now();
    if let Err(e) = save_cache(&inner.graph, &project_root) {
        warnings.push(format!("cache save failed: {e}"));
        eprintln!(
            "[code-graph] phase: save FAILED ({:.1}s)",
            save_start.elapsed().as_secs_f64()
        );
    } else {
        eprintln!(
            "[code-graph] phase: save done ({:.1}s)",
            save_start.elapsed().as_secs_f64()
        );
    }

    let result = AnalyzeResult {
        files: stats.files,
        symbols: stats.nodes,
        edges: stats.edges,
        root_path: project_root.to_string_lossy().into_owned(),
        warnings,
    };
    tool_success_json(&result)
}

/// Save the graph to `<dir>/.code-graph-cache.db`. Lifted to a helper so
/// the lock is held for the minimum span needed to serialize the cache —
/// a long save under the write lock would block all queries.
fn save_cache(
    graph: &parking_lot::RwLock<Graph>,
    dir: &std::path::Path,
) -> Result<(), code_graph_graph::PersistError> {
    let g = graph.read();
    g.save(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::CodeGraphServer;
    use code_graph_lang::LanguageRegistry;
    use code_graph_lang_cpp::CppParser;
    use std::fs;
    use tempfile::TempDir;

    fn server_with_cpp_parser() -> CodeGraphServer {
        let mut reg = LanguageRegistry::new();
        reg.register(Box::new(CppParser::new().expect("CppParser::new")))
            .unwrap();
        CodeGraphServer::new(reg)
    }

    #[tokio::test]
    async fn analyze_missing_path_errors() {
        let server = server_with_cpp_parser();
        let r = analyze_codebase(server.inner.clone(), String::new(), false, None, None).await;
        assert_eq!(r.is_error, Some(true));
        let body = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        assert_eq!(body, "'path' is required");
    }

    #[tokio::test]
    async fn analyze_nonexistent_directory_errors() {
        let server = server_with_cpp_parser();
        let r = analyze_codebase(
            server.inner.clone(),
            "/this/path/does/not/exist/abc123xyz".to_string(),
            false,
            None,
            None,
        )
        .await;
        assert_eq!(r.is_error, Some(true));
        let body = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        assert!(
            body.starts_with("directory does not exist:"),
            "expected 'directory does not exist:' wording, got: {body}"
        );
    }

    #[tokio::test]
    async fn analyze_path_is_file_errors() {
        // Deliberate divergence from Go's collapsed message; Rust keeps the
        // richer distinction between "path doesn't resolve" and "path
        // resolves to a file, not a directory".
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("a.cpp");
        fs::write(&file_path, b"void f() {}\n").unwrap();

        let server = server_with_cpp_parser();
        let r = analyze_codebase(
            server.inner.clone(),
            file_path.to_string_lossy().into_owned(),
            false,
            None,
            None,
        )
        .await;
        assert_eq!(r.is_error, Some(true));
        let body = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        assert!(
            body.starts_with("path is not a directory:"),
            "expected 'path is not a directory:' wording, got: {body}"
        );
    }

    #[tokio::test]
    async fn analyze_succeeds_on_small_directory_and_sets_indexed_flag() {
        let dir = TempDir::new().unwrap();
        for i in 0..3 {
            fs::write(
                dir.path().join(format!("f{i}.cpp")),
                format!("void f{i}() {{}}\n").as_bytes(),
            )
            .unwrap();
        }

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
            "got: {r:?}"
        );

        let body = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["files"], serde_json::json!(3));
        assert!(parsed["symbols"].as_u64().unwrap() >= 3);
        assert!(!parsed["root_path"].as_str().unwrap().is_empty());
        // Indexed flag is now set.
        assert!(server.inner.indexed.load(Ordering::Acquire));
        // Root path stored.
        assert!(server.inner.root_path.read().is_some());
    }

    #[tokio::test]
    async fn analyze_empty_directory_reports_no_files() {
        let dir = TempDir::new().unwrap();
        let server = server_with_cpp_parser();
        let r = analyze_codebase(
            server.inner.clone(),
            dir.path().to_string_lossy().into_owned(),
            false,
            None,
            None,
        )
        .await;
        assert_eq!(r.is_error, Some(true));
        let body = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        assert!(
            body.starts_with("no supported source files found in"),
            "got: {body}"
        );
        // indexed flag stays false.
        assert!(!server.inner.indexed.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn analyze_second_call_uses_cache_when_not_forced() {
        let dir = TempDir::new().unwrap();
        for i in 0..2 {
            fs::write(
                dir.path().join(format!("f{i}.cpp")),
                format!("void f{i}() {{}}\n").as_bytes(),
            )
            .unwrap();
        }
        let server = server_with_cpp_parser();
        let path = dir.path().to_string_lossy().into_owned();
        // First call: full re-index.
        let _ = analyze_codebase(server.inner.clone(), path.clone(), false, None, None).await;
        // Second call: cache hit (no force).
        let r2 = analyze_codebase(server.inner.clone(), path.clone(), false, None, None).await;
        assert!(r2.is_error.is_none() || r2.is_error == Some(false));
        let body = r2
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        // Same file count regardless of which path was taken.
        assert_eq!(parsed["files"], serde_json::json!(2));
    }

    #[tokio::test]
    async fn analyze_concurrent_call_returns_indexing_in_progress() {
        // Acquire the lock externally to simulate a concurrent in-flight
        // analyze_codebase. The handler should immediately error.
        let server = server_with_cpp_parser();
        let inner = server.inner.clone();
        let _held = inner.index_lock.try_lock().expect("first lock");
        let r = analyze_codebase(inner.clone(), "/tmp".to_string(), false, None, None).await;
        assert_eq!(r.is_error, Some(true));
        let body = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        assert_eq!(body, "indexing already in progress");
    }

    #[tokio::test]
    async fn analyze_force_skips_cache() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.cpp"), b"void f() {}\n").unwrap();
        let server = server_with_cpp_parser();
        let path = dir.path().to_string_lossy().into_owned();
        let _ = analyze_codebase(server.inner.clone(), path.clone(), false, None, None).await;
        let r2 = analyze_codebase(server.inner.clone(), path, true, None, None).await;
        assert!(r2.is_error.is_none() || r2.is_error == Some(false));
    }

    #[tokio::test]
    async fn analyze_malformed_toml_reports_parse_error() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.cpp"), b"void f() {}\n").unwrap();
        // Garbage TOML.
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[discovery\nmax_threads = nope\n",
        )
        .unwrap();

        let server = server_with_cpp_parser();
        let r = analyze_codebase(
            server.inner.clone(),
            dir.path().to_string_lossy().into_owned(),
            false,
            None,
            None,
        )
        .await;
        assert_eq!(r.is_error, Some(true));
        let body = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        assert!(
            body.starts_with("failed to parse .code-graph.toml:"),
            "got: {body}"
        );
    }
}
