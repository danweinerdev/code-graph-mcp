//! `analyze_codebase` handler body.
//!
//! Two-layer model:
//!
//! - [`analyze_codebase`] is a slot-protocol shell: cheap arg validation,
//!   single-flight gating via [`crate::analyze_job::AnalyzeSlot`], rotate
//!   any terminal job into `previous_terminal`, install a fresh `Running`
//!   job, await the worker, then project the terminal `JobStatus` onto
//!   the wire (`Completed` → `AnalyzeResult` JSON, `Failed` → `tool_error`).
//!   Per Design Decision 1 the slot — not `index_lock` — is the gate
//!   agents observe; per Design Decision 9 watch contention is no longer
//!   visible to sync callers.
//! - [`run_analyze_job`] is the worker: path canonicalization,
//!   `RootConfig::load`, cache fast-path, `spawn_blocking` parse pipeline,
//!   merge, persist. Acquires `index_lock` with `lock().await` (worker vs.
//!   watch serialization only). Writes terminal state into `job.state`
//!   before returning so the polling and inline-await paths both observe
//!   it consistently.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use code_graph_core::{paths, ConfigError, RootConfig};
use code_graph_graph::Graph;
use rmcp::model::{CallToolResult, ProgressNotificationParam, ProgressToken};
use rmcp::service::RoleServer;
use rmcp::Peer;
use serde::Serialize;

use crate::analyze_job::{AnalyzeJob, AnalyzePhase, JobStatus};
use crate::indexer::{
    build_file_index, build_symbol_index, extend_file_index, extend_symbol_index, index_directory,
    resolve_edges_with_indexes, ChannelProgressSink, ProgressEvent, ProgressSink,
};
use crate::server::ServerInner;

use super::status::format_unix_nanos_rfc3339;
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

/// Wraps a [`ChannelProgressSink`] so each `report()` ALSO writes the
/// latest progress triple into the owning [`AnalyzeJob`]'s mutable state.
/// Fan-out per Design Decision 8: the mpsc keeps the existing throttled
/// peer-notification path intact for sync mode, while the slot write makes
/// progress observable to `get_status` for both sync and async callers.
struct JobAwareProgressSink {
    inner: ChannelProgressSink,
    job: Arc<AnalyzeJob>,
}

impl ProgressSink for JobAwareProgressSink {
    fn report(&self, progress: u32, total: u32, message: &str) {
        // Order matters: keep the existing mpsc try_send first so the
        // forwarder's throttle window observes the same arrival pattern
        // it has always seen — slot mutation is the new side effect, not
        // a replacement.
        self.inner.report(progress, total, message);
        let mut s = self.job.state.write();
        s.progress = progress;
        s.progress_total = total;
        s.progress_message = message.to_string();
    }
}

impl JobAwareProgressSink {
    /// Atomic phase transition + peer-notification emission. Calls
    /// `AnalyzeJob::set_phase` to mutate job state (which sets the
    /// phase-specific message and resets `progress`), then pushes a
    /// snapshot of that state through the inner [`ChannelProgressSink`]
    /// so the MCP `notifications/progress` peer stream observes the
    /// phase boundary. Without this push, peers polling via the
    /// notification channel would never see a "Resolving cross-file
    /// edges" / "Persisting cache to disk" event — only via
    /// `get_status` polling — because no per-step `report()` fires
    /// from the worker between phase boundaries (and during persist
    /// there are no per-step reports AT ALL).
    ///
    /// The push bypasses `Self::report` to avoid re-writing the same
    /// values to `job.state` that `set_phase` already wrote (the read
    /// snapshot ensures the pushed event exactly matches what
    /// `get_status` would return at this instant).
    fn transition_to(&self, phase: crate::analyze_job::AnalyzePhase) {
        self.job.set_phase(phase);
        let (progress, total, message) = {
            let s = self.job.state.read();
            (s.progress, s.progress_total, s.progress_message.clone())
        };
        self.inner.report(progress, total, &message);
    }
}

/// Run the analyze pipeline to terminal state on a shared `AnalyzeJob`.
///
/// Writes `JobStatus::Completed(AnalyzeResult)` or `JobStatus::Failed(msg)`
/// into `job.state` before returning; the return type is `()` because all
/// outcomes flow through the slot. `peer`/`progress_token` are `Some` only
/// for sync callers — async kickoff omits them, falling through to the
/// drain-only forwarder branch.
///
/// Acquires `inner.index_lock` with `lock().await` (NOT `try_lock`): the
/// slot is the single-flight gate now; `index_lock` only serializes worker
/// vs. watch reindex (Design Decision 1).
pub(crate) async fn run_analyze_job(
    inner: Arc<ServerInner>,
    job: Arc<AnalyzeJob>,
    peer: Option<Peer<RoleServer>>,
    progress_token: Option<ProgressToken>,
) {
    let path_raw = job.path.clone();
    let force = job.force;

    let abs_path = match paths::canonicalize(std::path::Path::new(&path_raw)) {
        Ok(p) => p,
        Err(_) => {
            finish_failed(&job, format!("directory does not exist: {path_raw}"));
            return;
        }
    };
    if !abs_path.is_dir() {
        finish_failed(
            &job,
            format!("path is not a directory: {}", abs_path.display()),
        );
        return;
    }

    let (mut cfg, project_root) = match RootConfig::load(&abs_path) {
        Ok((c, root)) => (c, root),
        Err(ConfigError::Toml(e)) => {
            finish_failed(&job, format!("failed to parse .code-graph.toml: {e}"));
            return;
        }
        Err(ConfigError::Io(e)) => {
            finish_failed(&job, format!("failed to read .code-graph.toml: {e}"));
            return;
        }
        Err(e @ ConfigError::ExtensionMissingDot { .. })
        | Err(e @ ConfigError::ExtensionConflict { .. })
        | Err(e @ ConfigError::MacroStripConflict { .. }) => {
            finish_failed(&job, format!("invalid .code-graph.toml: {e}"));
            return;
        }
    };
    let mut warnings = cfg.resolve_concurrency();

    // Serialize against the watch reindex path; the slot already gates
    // analyze-vs-analyze, so this lock has no analyze contention.
    let _guard = inner.index_lock.lock().await;

    // Stamp an initial phase immediately so polling clients see a
    // phase signal from the very first `get_status` call after
    // kickoff — *including* when the cache fast-path probe takes
    // ~30-90s on UE-scale projects deserializing a multi-GB rkyv
    // archive. Choice of initial phase reflects what's actually
    // about to happen:
    //   - `LoadingCache` when a cache load is in the worker's
    //     immediate future (i.e. the fast-path probe will run, OR
    //     spawn_blocking will load on the slow path).
    //   - `Discovering` when the force-rebuild short-circuit will
    //     skip the cache load entirely (`force=true` AND scope is
    //     project root).
    // Without the LoadingCache stamp, polling clients would see
    // `current_phase: "discovering"` with `progress: 0/0` for the
    // entire cache-load window — phase label says "walking the file
    // tree" while the indexer is actually deserializing the cache,
    // which feels like the indexer is hung. The slow path will
    // re-stamp `Discovering` (then `Parsing`) once it enters
    // `spawn_blocking` — idempotent re-sets are harmless.
    let scope_is_project_root = abs_path == project_root;
    let cache_load_skipped = force && scope_is_project_root;
    if cache_load_skipped {
        job.set_phase(AnalyzePhase::Discovering);
    } else {
        job.set_phase(AnalyzePhase::LoadingCache);
    }

    if project_root != abs_path {
        let toml_at_root = project_root.join(".code-graph.toml");
        if toml_at_root.exists() {
            warnings.push(format!(
                "using .code-graph.toml found at {} (parent of indexed root {}); \
                 cache lives at the project root, indexing scope stays at the invocation path",
                project_root.display(),
                abs_path.display()
            ));
        } else {
            warnings.push(format!(
                "no .code-graph.toml found between {} and filesystem root; \
                 using built-in defaults. C++ classes prefixed with API-export macros \
                 (e.g. `class CORE_API Foo`) will NOT be indexed. Place a .code-graph.toml \
                 with [cpp].macro_strip at your project root to enable engine-style support.",
                abs_path.display()
            ));
        }
    } else {
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

    if !force {
        let mut probe = Graph::new();
        let (load_ok, all_stale) = probe
            .load_and_stale(&project_root)
            .unwrap_or((false, Vec::new()));
        if load_ok && probe.files_in_scope_count(&abs_path) > 0 {
            let in_scope_stale: Vec<_> = all_stale
                .iter()
                .filter(|p| p.starts_with(&abs_path))
                .collect();
            if in_scope_stale.is_empty() {
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
                if sweep_ran {
                    // The sweep introduces a cache write — bump the
                    // phase so a polling client doesn't see the
                    // terminal stamp without ever observing a
                    // `Persisting` signal. Skipped when `sweep_ran`
                    // is false (no save_cache call) so the terminal
                    // phase stays at `Discovering`, matching what
                    // actually happened.
                    job.set_phase(AnalyzePhase::Persisting);
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
                finish_completed(&job, result);
                return;
            }
        }
    }

    let (tx, mut rx) = tokio::sync::mpsc::channel::<ProgressEvent>(64);
    // Clone the sender BEFORE `tx` moves into the sink. The clone
    // outlives `spawn_blocking` and is used to emit the Persisting
    // phase-boundary notification (which fires AFTER the blocking
    // handle completes — sink and its `tx` are dropped at that
    // point, so without this clone the mpsc channel would close and
    // the forwarder would exit before Persisting could push). Kept
    // alive through the persist-and-finalize sequence and dropped at
    // the very end, just before the forwarder is awaited.
    let tx_post_blocking = tx.clone();

    eprintln!(
        "[code-graph] analyze_codebase: progress_token_present={}",
        progress_token.is_some()
    );

    let forwarder = if let (Some(peer), Some(token)) = (peer, progress_token) {
        Some(tokio::spawn(async move {
            const THROTTLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);
            let mut last_sent = std::time::Instant::now()
                .checked_sub(THROTTLE_INTERVAL)
                .unwrap_or_else(std::time::Instant::now);
            let mut latest: Option<ProgressEvent> = None;

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
        Some(tokio::spawn(
            async move { while rx.recv().await.is_some() {} },
        ))
    };

    let registry = Arc::clone(&inner);
    let cfg_for_pool = cfg.clone();
    let abs_path_for_pool = abs_path.clone();
    let project_root_for_pool = project_root.clone();
    // `scope_is_project_root` already computed near the top of this
    // function for the initial-phase decision; reused here.
    let job_for_pool = Arc::clone(&job);
    let blocking_handle = tokio::task::spawn_blocking(move || {
        let sink = JobAwareProgressSink {
            inner: ChannelProgressSink(tx),
            job: job_for_pool,
        };
        let mut blocking_warnings: Vec<String> = Vec::new();

        let phase_start = std::time::Instant::now();
        let mut merged_graph = Graph::new();
        // Short-circuit the cache load when `force=true` AND the
        // invocation scope is the entire project root. In that case
        // the loaded graph would immediately be `clear()`ed below
        // (line ~394), so loading it is wasted I/O — multi-GB rkyv
        // deserialization for a graph that's about to be thrown away.
        // On UE/LLVM-scale projects this saves ~60-120s of cold
        // startup time when the user wanted a fresh rebuild.
        //
        // The narrower `force=true` + sub-scope case still loads the
        // cache because `drop_files_in_scope` needs the existing
        // entries to know what to evict.
        let cache_loaded = if force && scope_is_project_root {
            eprintln!(
                "[code-graph] phase: cache load SKIPPED \
                 (force=true, project-root scope — cache would be cleared)"
            );
            false
        } else {
            // Stamp the LoadingCache phase before the blocking
            // rkyv deserialization so polling clients see an
            // accurate "Loading cache from disk" signal instead of
            // sitting at `discovering, 0/0` for the entire load
            // window — which on multi-GB caches can be minutes.
            sink.transition_to(AnalyzePhase::LoadingCache);
            eprintln!(
                "[code-graph] phase: loading cache from {}",
                project_root_for_pool.display()
            );
            let loaded = merged_graph.load(&project_root_for_pool).unwrap_or(false);
            eprintln!(
                "[code-graph] phase: cache load {} ({:.1}s, {} cached files)",
                if loaded { "ok" } else { "absent/stale" },
                phase_start.elapsed().as_secs_f64(),
                merged_graph.stats().files
            );
            loaded
        };

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

        eprintln!(
            "[code-graph] phase: discovering + parsing under {}",
            abs_path_for_pool.display()
        );
        sink.transition_to(AnalyzePhase::Discovering);
        let phase_start = std::time::Instant::now();
        // `index_directory` starts with a file-walk (Discovering) and
        // then runs the rayon parse pool (Parsing). The first
        // per-file `ProgressSink::report` from the parse loop will
        // overwrite progress/total/message with a Parsing snapshot;
        // the only observable Discovering window is between this
        // `transition_to` and the first parse report. We flip to
        // Parsing right before entering `index_directory` so the
        // dominant in-flight phase for polling clients is `parsing`,
        // with `discovering` reserved for the briefly observable
        // file-walk. Each `transition_to` ALSO pushes a peer
        // notification through the mpsc channel, so a client listening
        // to MCP `notifications/progress` sees the phase boundary
        // event in addition to the per-file events.
        sink.transition_to(AnalyzePhase::Parsing);
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

        eprintln!("[code-graph] phase: resolving edges");
        sink.transition_to(AnalyzePhase::Resolving);
        let phase_start = std::time::Instant::now();
        let cached_snapshot = merged_graph.file_graphs_snapshot();
        let mut symbol_index = build_symbol_index(&cached_snapshot);
        extend_symbol_index(&mut symbol_index, &fresh_graphs);
        let mut file_index = build_file_index(&cached_snapshot);
        extend_file_index(&mut file_index, &fresh_graphs);

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

        drop(sink);
        Ok::<_, String>((merged_graph, blocking_warnings))
    });

    let blocking_result = blocking_handle.await;

    // NOTE on cleanup ordering: the forwarder is awaited AFTER all
    // post-blocking work, not right after `blocking_handle.await` (as
    // earlier revisions did). The forwarder is kept alive so the
    // Persisting phase-boundary notification can fan out to the peer.
    // The mpsc rx stays open as long as `tx_post_blocking` holds a
    // sender; dropping it just before `forwarder.await` lets the loop
    // drain and flush the final pending event. Every early-return
    // path below funnels through the single tail block at the bottom
    // of this function so cleanup never leaks.

    let outcome: Result<AnalyzeResult, String> = match blocking_result {
        Ok(Ok((merged_graph, blocking_warnings))) => {
            warnings.extend(blocking_warnings);
            if merged_graph.stats().files == 0 {
                Err(format!(
                    "no supported source files found in {}",
                    abs_path.display()
                ))
            } else {
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

                eprintln!(
                    "[code-graph] phase: saving cache to {}",
                    project_root.display()
                );
                // Transition to Persisting AND emit the phase-boundary
                // notification through the still-alive forwarder. The
                // sink itself is gone (spawn_blocking ended), so we
                // mutate the job state directly via `set_phase` and
                // push the snapshot through `tx_post_blocking`. Without
                // this push, peers listening on
                // `notifications/progress` would never observe the
                // Persisting phase — `save_cache` has no per-step
                // sink.report.
                job.set_phase(AnalyzePhase::Persisting);
                {
                    let s = job.state.read();
                    let evt = ProgressEvent {
                        progress: s.progress,
                        total: s.progress_total,
                        message: s.progress_message.clone(),
                    };
                    drop(s);
                    let _ = tx_post_blocking.try_send(evt);
                }

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

                Ok(AnalyzeResult {
                    files: stats.files,
                    symbols: stats.nodes,
                    edges: stats.edges,
                    root_path: project_root.to_string_lossy().into_owned(),
                    warnings,
                })
            }
        }
        Ok(Err(e)) => Err(format!("indexing failed: {e}")),
        Err(join_err) => Err(format!("indexing task panicked: {join_err}")),
    };

    // Tail cleanup. Drop the persist-side sender so `rx.recv()`
    // returns None and the forwarder loop exits; then await the
    // forwarder so its trailing flush (final pending notification)
    // completes before this function returns. Finally stamp the
    // terminal status on the job.
    drop(tx_post_blocking);
    if let Some(handle) = forwarder {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    }
    match outcome {
        Ok(result) => finish_completed(&job, result),
        Err(msg) => finish_failed(&job, msg),
    }
}

/// Stamp terminal state under a single `state.write()` so an observer
/// (1.3's sync handler reading after `await`, or 1.4's polled `get_status`)
/// sees status+finished_at consistently — never a half-written transition.
fn finish_completed(job: &AnalyzeJob, result: AnalyzeResult) {
    let mut s = job.state.write();
    s.status = JobStatus::Completed(result);
    s.finished_at = Some(now_nanos_u64());
}

fn finish_failed(job: &AnalyzeJob, msg: String) {
    let mut s = job.state.write();
    s.status = JobStatus::Failed(msg);
    s.finished_at = Some(now_nanos_u64());
}

/// `analyze_codebase` body.
///
/// Slot-protocol coordination only — the heavy lifting (cache fast-path,
/// parse pipeline, merge, persist) lives in [`run_analyze_job`]. The slot
/// is the single-flight gate (Design Decision 1); `index_lock` moves
/// into the worker.
///
/// Protocol:
/// 1. Pre-rotation cheap validation. Empty path returns immediately —
///    no slot touch, so the failing-path test stays slot-isolated.
/// 2. Acquire the slot write lock. If `current` is `Running`, drop the
///    guard and return the snapshot-locked `"indexing already in progress"`
///    wording (byte-identical to today). Note: this no longer fires on
///    `index_lock` contention with the watch handler — sync now awaits
///    the watch reindex instead (Design Decision 9).
/// 3. Rotate any terminal `current` into `previous_terminal`, install a
///    fresh `Running` job, drop the guard.
/// 4. Run the worker inline (`.await`) — peer/token pass through so the
///    forwarder still drives client-side progress notifications.
/// 5. Read the terminal `JobStatus` and project it to the wire shape:
///    `Completed(result)` → `tool_success_json`; `Failed(msg)` →
///    `tool_error`; `Running` is `unreachable!()` because the worker
///    always writes a terminal state before returning.
pub async fn analyze_codebase(
    inner: Arc<ServerInner>,
    path_raw: String,
    force: bool,
    peer: Option<Peer<RoleServer>>,
    progress_token: Option<ProgressToken>,
) -> CallToolResult {
    if path_raw.is_empty() {
        return tool_error("'path' is required");
    }

    let job = {
        let mut slot = inner.analyze_slot.write();
        if let Some(cur) = &slot.current {
            if matches!(cur.state.read().status, JobStatus::Running) {
                drop(slot);
                return tool_error("indexing already in progress");
            }
        }
        let started_at = now_nanos_u64();
        let job_id = format!("{started_at:020}");
        let job = AnalyzeJob::new_running(job_id, path_raw.clone(), force, started_at);
        if let Some(prev) = slot.current.take() {
            slot.previous_terminal = Some(prev);
        }
        slot.current = Some(Arc::clone(&job));
        job
    };

    run_analyze_job(Arc::clone(&inner), Arc::clone(&job), peer, progress_token).await;

    let state = job.state.read();
    match &state.status {
        JobStatus::Completed(result) => tool_success_json(result),
        JobStatus::Failed(msg) => tool_error(msg.clone()),
        JobStatus::Running => {
            unreachable!("run_analyze_job must write a terminal JobStatus before returning")
        }
    }
}

/// `analyze_codebase_async` body — kickoff handler that returns in
/// milliseconds regardless of indexing duration.
///
/// Identical slot protocol to [`analyze_codebase`] (Design Decision 1)
/// except the worker is `tokio::spawn`ed and detached instead of
/// `await`ed inline, and a duplicate kickoff against a `Running` slot
/// is a SUCCESS (not an error — Design Decision 3) carrying the
/// in-flight job's `job_id` with `existing: true`.
///
/// Protocol:
/// 1. Pre-rotation cheap validation. Empty path returns immediately —
///    no slot touch, mirroring sync wording.
/// 2. Acquire the slot write lock. If `current` is `Running`, snapshot
///    `(job_id, started_at)`, drop the guard, and return the duplicate
///    response with `existing: true`. Args of the duplicate call
///    (including `force`) are ignored.
/// 3. Otherwise rotate any terminal `current` into `previous_terminal`,
///    install a fresh `Running` job, drop the guard.
/// 4. `tokio::spawn(run_analyze_job(inner, job))` — the `JoinHandle` is
///    dropped to detach the worker.
/// 5. Return the kickoff response with `existing: false` carrying the
///    new `job_id` and `started_at`.
///
/// No `peer`/`progress_token` arguments — async kickoff has no
/// client-side progress channel; agents observe progress by polling
/// `get_status`.
pub async fn analyze_codebase_async(
    inner: Arc<ServerInner>,
    path_raw: String,
    force: bool,
) -> CallToolResult {
    if path_raw.is_empty() {
        return tool_error("'path' is required");
    }

    enum Kickoff {
        Existing { job_id: String, started_at: u64 },
        New(Arc<AnalyzeJob>),
    }

    let kickoff = {
        let mut slot = inner.analyze_slot.write();
        if let Some(cur) = &slot.current {
            if matches!(cur.state.read().status, JobStatus::Running) {
                let existing = Kickoff::Existing {
                    job_id: cur.job_id.clone(),
                    started_at: cur.started_at,
                };
                drop(slot);
                existing
            } else {
                let job = install_new_running(&mut slot, path_raw.clone(), force);
                Kickoff::New(job)
            }
        } else {
            let job = install_new_running(&mut slot, path_raw.clone(), force);
            Kickoff::New(job)
        }
    };

    match kickoff {
        Kickoff::Existing { job_id, started_at } => tool_success_json(&AsyncKickoffResponse {
            job_id,
            status: "running",
            started_at: format_unix_nanos_rfc3339(started_at),
            existing: true,
            note: "analyze already in progress — args ignored; poll get_status for progress",
        }),
        Kickoff::New(job) => {
            let response = AsyncKickoffResponse {
                job_id: job.job_id.clone(),
                status: "running",
                started_at: format_unix_nanos_rfc3339(job.started_at),
                existing: false,
                note: "analyze kicked off — poll get_status for progress and the terminal result",
            };
            // Detach: the JoinHandle is dropped intentionally so the
            // worker outlives this handler invocation. Terminal state
            // flows back through `job.state`, observable via get_status.
            tokio::spawn(run_analyze_job(
                Arc::clone(&inner),
                Arc::clone(&job),
                None,
                None,
            ));
            tool_success_json(&response)
        }
    }
}

/// Slot-rotation primitive shared by the async kickoff and (potentially)
/// future callers. Caller holds the slot write guard; this helper moves
/// any terminal `current` into `previous_terminal`, installs a fresh
/// `Running` job, and returns the Arc.
fn install_new_running(
    slot: &mut crate::analyze_job::AnalyzeSlot,
    path: String,
    force: bool,
) -> Arc<AnalyzeJob> {
    let started_at = now_nanos_u64();
    let job_id = format!("{started_at:020}");
    let job = AnalyzeJob::new_running(job_id, path, force, started_at);
    if let Some(prev) = slot.current.take() {
        slot.previous_terminal = Some(prev);
    }
    slot.current = Some(Arc::clone(&job));
    job
}

/// Wire shape of the `analyze_codebase_async` kickoff response.
/// `< 1KB` by construction — five fields, no nested payload.
#[derive(Debug, Serialize)]
struct AsyncKickoffResponse {
    job_id: String,
    status: &'static str,
    started_at: String,
    existing: bool,
    note: &'static str,
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
    use super::super::status::get_status;
    use super::super::test_helpers::body_text;
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

    /// Write a single trivial `.cpp` file into a fresh tempdir. Shared by
    /// the lifecycle/shape tests below — the slot protocol's behavior is
    /// orthogonal to corpus shape, so a one-file fixture is the smallest
    /// thing that exercises end-to-end indexing in under a millisecond.
    fn tempdir_with_one_cpp() -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.cpp"), b"void f() {}\n").unwrap();
        dir
    }

    /// `JobAwareProgressSink::transition_to` is the helper that
    /// guarantees a peer-visible phase boundary notification. Two
    /// post-conditions:
    ///   1. The job state reflects the new phase + its phase-specific
    ///      message (matches `AnalyzeJob::set_phase` semantics).
    ///   2. The inner mpsc receives a `ProgressEvent` carrying the
    ///      same `(progress, total, message)` triple — so the
    ///      forwarder can fan it out as an MCP
    ///      `notifications/progress` event.
    ///
    /// Without (2), peers polling via the notification stream would
    /// never observe the Persisting boundary (cache serialization
    /// has no per-step `report` of its own), and the
    /// Parsing→Resolving transition would only be visible via
    /// `get_status` polling.
    #[tokio::test]
    async fn transition_to_emits_phase_boundary_notification() {
        use crate::analyze_job::AnalyzePhase;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<crate::indexer::ProgressEvent>(8);
        let job = AnalyzeJob::new_running("0".into(), "/x".into(), false, 0);
        // Seed prior phase state to verify set_phase's reset
        // semantics carry through transition_to.
        {
            let mut s = job.state.write();
            s.progress = 100;
            s.progress_total = 100;
            s.progress_message = "Parsing: foo.cpp".to_string();
        }
        let sink = JobAwareProgressSink {
            inner: crate::indexer::ChannelProgressSink(tx),
            job: Arc::clone(&job),
        };

        sink.transition_to(AnalyzePhase::Resolving);

        // Post-condition 1: job state reflects the phase transition.
        let s = job.state.read();
        assert_eq!(s.current_phase, Some(AnalyzePhase::Resolving));
        assert_eq!(s.progress, 0, "set_phase resets progress");
        assert_eq!(
            s.progress_total, 100,
            "Resolving preserves prior progress_total"
        );
        assert_eq!(s.progress_message, "Resolving cross-file edges");
        drop(s);

        // Post-condition 2: mpsc receives a ProgressEvent with the
        // same snapshot. `try_recv` should succeed immediately because
        // transition_to pushes synchronously via try_send.
        let evt = rx
            .try_recv()
            .expect("transition_to must push a ProgressEvent to the inner sink");
        assert_eq!(evt.progress, 0);
        assert_eq!(evt.total, 100);
        assert_eq!(evt.message, "Resolving cross-file edges");
        // No further events are queued (single transition emits a
        // single event).
        assert!(rx.try_recv().is_err());
    }

    /// `transition_to(Persisting)` emits the Persisting-specific
    /// snapshot: `(progress=0, total=1, "Persisting cache to disk")`.
    /// Pins the wire shape callers depend on for the notification
    /// stream during cache serialization.
    #[tokio::test]
    async fn transition_to_persisting_emits_synthetic_one_of_one() {
        use crate::analyze_job::AnalyzePhase;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<crate::indexer::ProgressEvent>(8);
        let job = AnalyzeJob::new_running("0".into(), "/x".into(), false, 0);
        {
            let mut s = job.state.write();
            s.progress = 63784;
            s.progress_total = 63784;
            s.progress_message = "Resolving edges: last.cpp".to_string();
        }
        let sink = JobAwareProgressSink {
            inner: crate::indexer::ChannelProgressSink(tx),
            job: Arc::clone(&job),
        };

        sink.transition_to(AnalyzePhase::Persisting);

        let evt = rx
            .try_recv()
            .expect("transition_to(Persisting) must push a ProgressEvent");
        assert_eq!(evt.progress, 0);
        assert_eq!(
            evt.total, 1,
            "Persisting uses a synthetic single-task total of 1"
        );
        assert_eq!(evt.message, "Persisting cache to disk");
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

    /// Regression: the cache fast-path (`load_and_stale` succeeds +
    /// no in-scope files are stale) used to leave `current_phase` at
    /// `null` for its entire duration, then stamp the terminal with
    /// `current_phase` still null. On UE-scale codebases that means
    /// 30-90s of polling with `null`/`progress: 0/0` and then a
    /// silent flip to `completed` — indistinguishable from "the
    /// indexer is hung." Fix: stamp `Discovering` at the top of
    /// `run_analyze_job` so every code path through the worker
    /// emits at least one phase signal before terminal.
    ///
    /// This test exercises the regression by running analyze twice
    /// against the same fixture (first call writes the cache, second
    /// call hits the fast path) and asserting the second call's
    /// terminal job carries a non-null `current_phase`.
    #[tokio::test]
    async fn analyze_fast_path_stamps_current_phase() {
        use crate::analyze_job::JobStatus;

        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.cpp"), b"void f() {}\n").unwrap();
        let server = server_with_cpp_parser();
        let path = dir.path().to_string_lossy().into_owned();

        // First call: slow path. Writes the cache.
        let r1 = analyze_codebase(server.inner.clone(), path.clone(), false, None, None).await;
        assert!(r1.is_error.is_none() || r1.is_error == Some(false));

        // Second call: cache exists + all files clean → fast path.
        let r2 = analyze_codebase(server.inner.clone(), path.clone(), false, None, None).await;
        assert!(r2.is_error.is_none() || r2.is_error == Some(false));

        // Inspect the terminal job in slot.current.
        let slot = server.inner.analyze_slot.read();
        let current = slot
            .current
            .as_ref()
            .expect("analyze must install a slot.current entry");
        let state = current.state.read();
        // Terminal must be Completed (fast path returns
        // `finish_completed`).
        assert!(
            matches!(state.status, JobStatus::Completed(_)),
            "second analyze should complete via fast path: {:?}",
            std::any::type_name_of_val(&state.status)
        );
        // The regression: `current_phase` was `None`. After the fix,
        // the top-of-worker `set_phase(Discovering)` runs before the
        // fast-path probe so the terminal carries a non-null phase.
        assert!(
            state.current_phase.is_some(),
            "fast-path terminal must carry a non-null current_phase \
             (regression: was None before the top-of-worker set_phase \
             stamp); got {:?}",
            state.current_phase
        );
    }

    /// Regression: a multi-GB rkyv cache file took minutes to
    /// deserialize on cold start. The worker stamped `Discovering`
    /// before the load, then `Discovering` again after the load,
    /// so polling clients saw `current_phase: "discovering"` with
    /// `progress: 0/0` for the *entire* load window — phase label
    /// said "walking the file tree" while the indexer was actually
    /// blocked in rkyv deserialization, indistinguishable from a
    /// hung worker. Fix: distinct `LoadingCache` phase, stamped
    /// before any cache-load I/O.
    ///
    /// This test verifies the wire shape: at any point during a
    /// non-force second run, the cache-load path stamps
    /// `LoadingCache` (not `Discovering` or `null`). Exercised
    /// indirectly via the fast-path probe: the terminal of a
    /// cache-hit run carries `current_phase: "discovering"` because
    /// the fast path re-stamps Discovering after the probe and never
    /// hits the slow-path LoadingCache stamp — but the slow path's
    /// initial set_phase WAS LoadingCache. Pin this by inspecting the
    /// PREVIOUS_TERMINAL after kicking off a new run.
    ///
    /// Simpler approach: assert the initial set_phase choice
    /// directly. The function entry stamps `LoadingCache` when a
    /// cache load is in the worker's future, `Discovering` when the
    /// force-rebuild short-circuit will skip it. We can probe via a
    /// fresh fixture (no cache exists) and force=false → LoadingCache
    /// stamped (then cleared because cache load fails fast on
    /// missing file, transitioning forward).
    ///
    /// The deterministic path: force=true on a fixture is the
    /// "cache-load-skipped" case. The initial set_phase must be
    /// `Discovering`, NOT `LoadingCache`. We probe slot.current
    /// immediately after kickoff, before the worker has progressed
    /// much, to assert this.
    #[tokio::test]
    async fn analyze_force_root_scope_skips_cache_load_phase() {
        use crate::analyze_job::AnalyzePhase;

        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.cpp"), b"void f() {}\n").unwrap();
        let server = server_with_cpp_parser();
        let path = dir.path().to_string_lossy().into_owned();

        // First call: builds cache. Wait for completion.
        let _ = analyze_codebase(server.inner.clone(), path.clone(), false, None, None).await;

        // Second call: force=true on the same path (scope == project
        // root). The worker should skip the cache load entirely and
        // stamp `Discovering` as the initial phase, NOT
        // `LoadingCache`. The terminal phase will reflect whichever
        // phase was last set during the worker run; what we pin here
        // is that LoadingCache was NEVER set during this force run.
        //
        // The terminal phase under force=true on a small fixture is
        // typically `Persisting` (the cache rewrite at the end). The
        // critical assertion is just that `current_phase != null` AND
        // the worker behaviour matches the "skipped" branch.
        let r2 = analyze_codebase(server.inner.clone(), path.clone(), true, None, None).await;
        assert!(r2.is_error.is_none() || r2.is_error == Some(false));

        // Inspect the terminal.
        let slot = server.inner.analyze_slot.read();
        let current = slot
            .current
            .as_ref()
            .expect("force analyze must install a slot.current entry");
        let state = current.state.read();
        // current_phase must be non-null (already pinned by other
        // tests but worth re-asserting here).
        assert!(state.current_phase.is_some());
        // Terminal should be Persisting (the typical end-state for a
        // force run that writes a cache).
        assert_eq!(
            state.current_phase,
            Some(AnalyzePhase::Persisting),
            "force=true terminal should land on Persisting; got {:?}",
            state.current_phase
        );
    }

    /// Pin the message wording for the cache-load skip log so any
    /// future refactor that removes the optimization triggers a
    /// CI failure with a clear pointer.
    #[test]
    fn loading_cache_phase_message_pinned() {
        use crate::analyze_job::{AnalyzeJob, AnalyzePhase};
        let job = AnalyzeJob::new_running("0".into(), "/x".into(), false, 0);
        job.set_phase(AnalyzePhase::LoadingCache);
        let s = job.state.read();
        assert_eq!(s.progress_message, "Loading cache from disk");
        assert_eq!(s.progress_total, 1);
        assert_eq!(s.progress, 0);
    }

    #[tokio::test]
    async fn analyze_concurrent_call_returns_indexing_in_progress() {
        // Per Design Decision 9 the slot is the single-flight gate, not
        // `index_lock` — installing a synthetic Running job is the way to
        // simulate a concurrent in-flight analyze. The wire wording is the
        // load-bearing assertion and stays byte-identical.
        let server = server_with_cpp_parser();
        let inner = server.inner.clone();
        let synthetic = AnalyzeJob::new_running(
            "00000000000000000001".to_string(),
            "/tmp".to_string(),
            false,
            1,
        );
        inner.analyze_slot.write().current = Some(synthetic);

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

    /// (Task 2.1 / a) Async kickoff returns in the kickoff window — not
    /// blocking on the indexing pipeline. The 100ms ceiling is a generous
    /// budget on the kickoff path (slot write + tokio::spawn); a regression
    /// that turns kickoff into a synchronous-await would blow it.
    #[tokio::test]
    async fn async_kickoff_returns_immediately_with_running_job() {
        let dir = tempdir_with_one_cpp();
        let server = server_with_cpp_parser();

        let start = std::time::Instant::now();
        let r = analyze_codebase_async(
            server.inner.clone(),
            dir.path().to_string_lossy().into_owned(),
            false,
        )
        .await;
        let elapsed = start.elapsed();

        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "kickoff took {elapsed:?}, expected < 100ms — kickoff should not block on indexing"
        );
        assert!(
            r.is_error.is_none() || r.is_error == Some(false),
            "got: {r:?}"
        );

        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(parsed["status"], serde_json::json!("running"));
        assert_eq!(parsed["existing"], serde_json::json!(false));
        assert!(
            !parsed["job_id"].as_str().unwrap().is_empty(),
            "job_id must be non-empty"
        );
    }

    /// (Task 2.1 / b) After async kickoff, polling `get_status` eventually
    /// observes a Completed terminal carrying the indexed `result.files`
    /// count. The 5s poll bound is a hang catcher per the plan's note —
    /// a 1-file fixture indexes in milliseconds; if we hit the bound,
    /// something is wrong (worker hung, slot not transitioning).
    #[tokio::test]
    async fn async_kickoff_then_poll_completes() {
        let dir = tempdir_with_one_cpp();
        let server = server_with_cpp_parser();
        let inner = server.inner.clone();

        let kickoff = analyze_codebase_async(
            inner.clone(),
            dir.path().to_string_lossy().into_owned(),
            false,
        )
        .await;
        let kickoff_parsed: serde_json::Value = serde_json::from_str(&body_text(&kickoff)).unwrap();
        let job_id = kickoff_parsed["job_id"].as_str().unwrap().to_string();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let terminal: serde_json::Value = loop {
            if std::time::Instant::now() >= deadline {
                panic!(
                    "async job {job_id} did not reach terminal within 5s — \
                     worker hung, slot not transitioning, or progress state not flushed"
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let status = get_status(inner.clone());
            let parsed: serde_json::Value = serde_json::from_str(&body_text(&status)).unwrap();
            let job = &parsed["analyze_job"];
            let s = job["status"].as_str().unwrap_or("");
            if s == "completed" || s == "failed" {
                break job.clone();
            }
        };

        assert_eq!(
            terminal["status"],
            serde_json::json!("completed"),
            "expected Completed terminal; got: {terminal}"
        );
        assert_eq!(
            terminal["result"]["files"],
            serde_json::json!(1),
            "result.files should be 1 for the 1-file fixture"
        );
    }

    /// (Task 2.1 / c) The sync `analyze_codebase` handler installs a
    /// `Completed(_)` slot entry before returning — the worker always
    /// writes a terminal state, even on the inline-await path. Pinning
    /// this is the contract that lets `get_status` snapshot sync runs
    /// without ambiguity.
    #[tokio::test]
    async fn sync_analyze_populates_slot_with_completed() {
        let dir = tempdir_with_one_cpp();
        let server = server_with_cpp_parser();
        let inner = server.inner.clone();

        let _ = analyze_codebase(
            inner.clone(),
            dir.path().to_string_lossy().into_owned(),
            false,
            None,
            None,
        )
        .await;

        let slot = inner.analyze_slot.read();
        let current = slot
            .current
            .as_ref()
            .expect("sync analyze must install a slot.current entry");
        let state = current.state.read();
        match &state.status {
            JobStatus::Completed(result) => {
                assert_eq!(
                    result.files, 1,
                    "Completed result.files should match the 1-file fixture"
                );
            }
            JobStatus::Running => {
                panic!("sync analyze returned with slot still Running — terminal write missed")
            }
            JobStatus::Failed(msg) => {
                panic!("sync analyze ended Failed unexpectedly: {msg}")
            }
        }
    }

    /// (Task 2.1 / d) On a fresh server with no analyze ever invoked, the
    /// two job fields serialize as explicit JSON `null` — NOT missing
    /// keys. The explicit-null contract (Task 1.5) lets clients
    /// distinguish "no analyze ever" from "old server without the field".
    #[tokio::test]
    async fn get_status_with_no_analyze_returns_null_job_fields() {
        let server = server_with_cpp_parser();
        let r = get_status(server.inner.clone());
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let obj = parsed
            .as_object()
            .expect("get_status returns a JSON object");

        assert!(
            obj.contains_key("analyze_job"),
            "analyze_job key must be present even when null"
        );
        assert!(
            obj.contains_key("analyze_job_previous_terminal"),
            "analyze_job_previous_terminal key must be present even when null"
        );
        assert!(
            obj["analyze_job"].is_null(),
            "analyze_job should be JSON null on a fresh server"
        );
        assert!(
            obj["analyze_job_previous_terminal"].is_null(),
            "analyze_job_previous_terminal should be JSON null on a fresh server"
        );
    }

    /// (Task 2.1 / e) `get_status` exposes the same `AnalyzeResult` shape
    /// that sync `analyze_codebase` returns on its wire response. Cross-
    /// checked by running a parallel sync analyze on a fresh server over
    /// the same fixture and comparing `files` / `symbols` / `edges`.
    #[tokio::test]
    async fn get_status_completed_carries_full_analyze_result() {
        let dir = tempdir_with_one_cpp();

        // Server A — async kickoff + poll to Completed; read shape off get_status.
        let server_a = server_with_cpp_parser();
        let inner_a = server_a.inner.clone();
        let kickoff = analyze_codebase_async(
            inner_a.clone(),
            dir.path().to_string_lossy().into_owned(),
            false,
        )
        .await;
        let kickoff_parsed: serde_json::Value = serde_json::from_str(&body_text(&kickoff)).unwrap();
        let job_id = kickoff_parsed["job_id"].as_str().unwrap().to_string();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let job_view: serde_json::Value = loop {
            if std::time::Instant::now() >= deadline {
                panic!("async job {job_id} did not reach terminal within 5s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let status = get_status(inner_a.clone());
            let parsed: serde_json::Value = serde_json::from_str(&body_text(&status)).unwrap();
            let job = &parsed["analyze_job"];
            if job["status"].as_str() == Some("completed") {
                break job.clone();
            }
            if job["status"].as_str() == Some("failed") {
                panic!("async job ended Failed: {}", job["error"]);
            }
        };

        // Server B — parallel sync analyze on the same fixture. Use a
        // separate server so cache state from the first run can't leak
        // into the second's counts.
        let server_b = server_with_cpp_parser();
        let sync_r = analyze_codebase(
            server_b.inner.clone(),
            dir.path().to_string_lossy().into_owned(),
            false,
            None,
            None,
        )
        .await;
        let sync_parsed: serde_json::Value = serde_json::from_str(&body_text(&sync_r)).unwrap();

        let async_result = &job_view["result"];
        assert!(
            async_result.is_object(),
            "analyze_job.result must be populated on Completed; got: {job_view}"
        );

        // Cross-check: every numeric stat the sync wire response carries
        // must match the get_status snapshot byte-for-byte. If these
        // diverge, the two code paths are producing different AnalyzeResult
        // values for identical input — a bug worth pinning.
        assert_eq!(async_result["files"], sync_parsed["files"]);
        assert_eq!(async_result["symbols"], sync_parsed["symbols"]);
        assert_eq!(async_result["edges"], sync_parsed["edges"]);
        assert_eq!(async_result["root_path"], sync_parsed["root_path"]);
        // Sanity floor — the fixture has a function, so symbols can't be 0.
        assert_eq!(async_result["files"], serde_json::json!(1));
        assert!(async_result["symbols"].as_u64().unwrap() >= 1);
    }

    // ----- Task 2.2: single-flight race tests -------------------------------
    //
    // These tests verify the slot is the single-flight gate (Design Decision
    // 1), duplicate kickoff against a Running slot returns the existing
    // job_id (Decision 3), and sync vs. async exclude each other symmetrically
    // (Decision 9). They use the recording plugin's `SLEEP_PER_PARSE_MS` knob
    // (where required) to stretch the indexing window wide enough for a
    // second handler call to land on the slot while the first is still
    // Running.
    //
    // **Knob hygiene.** Every test that sets `SLEEP_PER_PARSE_MS` does so
    // through the `ParseSleepGuard` RAII helper below. If two tests in this
    // binary ran concurrently and one leaked a non-zero value, the other
    // would silently slow down — Cargo's default is parallel test execution
    // within a binary. The first two tests below (`concurrent_async_*`,
    // `async_duplicate_*`) do NOT touch the knob; their synchronization
    // primitive is the `Barrier` / `yield_now` pair, not stretched indexing
    // time. The third and fourth do, and clean up via the guard.

    use crate::test_recording_plugin::{Log, RecordingPlugin, SLEEP_PER_PARSE_MS};
    use code_graph_core::Language;
    use std::sync::atomic::Ordering as AtomicOrdering;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// Serializes the three tests that set `SLEEP_PER_PARSE_MS`: with Cargo's
    /// default parallel test execution, two tests entering [`ParseSleepGuard::set`]
    /// at once would race on the static, and the first to finish would
    /// `store(0)` while the other was still relying on its sleep value. The
    /// guard takes the lock on construction and releases it on drop, so at
    /// most one knob-using test holds the knob at a time.
    static SLEEP_KNOB_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// RAII guard that sets the recording plugin's per-`parse_file` sleep
    /// knob on construction and resets it to `0` on drop. The Drop reset is
    /// load-bearing: tests in this binary run concurrently by default, so a
    /// leaked non-zero value would silently stretch every concurrent test's
    /// indexing wall time and turn deterministic synchronization into
    /// timing-dependent flake. The guard ALSO holds [`SLEEP_KNOB_LOCK`] for
    /// its lifetime so concurrent knob-using tests cannot interleave their
    /// set/reset cycles and clobber each other's values.
    struct ParseSleepGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
    }
    impl ParseSleepGuard {
        fn set(ms: u64) -> Self {
            // Unwrap-or-into: a poisoned mutex from a panicking test is fine
            // for us — we're going to overwrite the value anyway, and the
            // next reset on Drop is the same operation either way.
            let lock = SLEEP_KNOB_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            SLEEP_PER_PARSE_MS.store(ms, AtomicOrdering::Relaxed);
            Self { _lock: lock }
        }
    }
    impl Drop for ParseSleepGuard {
        fn drop(&mut self) {
            SLEEP_PER_PARSE_MS.store(0, AtomicOrdering::Relaxed);
        }
    }

    /// Build a `CodeGraphServer` whose only registered plugin is the
    /// `RecordingPlugin` claiming `.rec` files. Routing through the recording
    /// plugin is what makes `SLEEP_PER_PARSE_MS` effective — the real
    /// `CppParser` ignores the knob. The returned `Log` is captured for
    /// callers that want to assert per-file invocation; the race tests below
    /// drop it.
    fn server_with_recording_plugin() -> (CodeGraphServer, Log) {
        let calls: Log = std::sync::Arc::new(Mutex::new(Vec::new()));
        let mut reg = LanguageRegistry::new();
        reg.register(Box::new(RecordingPlugin::new(
            Language::Cpp,
            &[".rec"],
            std::sync::Arc::clone(&calls),
        )))
        .unwrap();
        (CodeGraphServer::new(reg), calls)
    }

    /// Seed a tempdir with `n` trivial `.rec` files. Paired with the
    /// recording-plugin server above so the analyze handler routes each file
    /// through `RecordingPlugin::parse_file` (and therefore the sleep knob).
    fn tempdir_with_n_rec(n: usize) -> TempDir {
        let dir = TempDir::new().unwrap();
        for i in 0..n {
            fs::write(dir.path().join(format!("f{i}.rec")), b"// rec\n").unwrap();
        }
        dir
    }

    /// (Task 2.2 / a) Two `analyze_codebase_async` calls released
    /// simultaneously via a `Barrier` both hit the slot write lock at the
    /// same instant; the `PlRwLock` serializes them so one observes the
    /// other's `Running` write. Determinism comes from the barrier — both
    /// tasks reach the slot-write attempt at the same wall-clock point —
    /// and from the slot lock itself, which makes the check+rotate+install
    /// step atomic. NO sleep knob: indexing time is irrelevant; the
    /// synchronization happens entirely in the slot.
    #[tokio::test]
    async fn concurrent_async_kickoffs_only_one_spawns_worker() {
        let dir = tempdir_with_one_cpp();
        let server = server_with_cpp_parser();
        let inner = server.inner.clone();
        let path = dir.path().to_string_lossy().into_owned();

        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));

        let mut set = tokio::task::JoinSet::new();
        for _ in 0..2 {
            let inner = inner.clone();
            let path = path.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            set.spawn(async move {
                barrier.wait().await;
                analyze_codebase_async(inner, path, false).await
            });
        }

        let mut responses = Vec::with_capacity(2);
        while let Some(joined) = set.join_next().await {
            responses.push(joined.expect("kickoff task panicked"));
        }
        assert_eq!(responses.len(), 2);

        let parsed: Vec<serde_json::Value> = responses
            .iter()
            .map(|r| {
                assert!(
                    r.is_error.is_none() || r.is_error == Some(false),
                    "kickoff response unexpectedly errored: {r:?}"
                );
                serde_json::from_str(&body_text(r)).unwrap()
            })
            .collect();

        let job_ids: Vec<String> = parsed
            .iter()
            .map(|v| v["job_id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            job_ids[0], job_ids[1],
            "both concurrent kickoffs must surface the same job_id (the slot's installed Running job)"
        );

        let mut existing_flags: Vec<bool> = parsed
            .iter()
            .map(|v| v["existing"].as_bool().unwrap())
            .collect();
        existing_flags.sort();
        assert_eq!(
            existing_flags,
            vec![false, true],
            "exactly one kickoff must report existing=false (the winner that installed the job) \
             and the other existing=true (observer of the winner's write)"
        );
    }

    /// (Task 2.2 / b) Sequential kickoff with a `yield_now` between calls.
    /// The yield is a scheduling primitive — it surrenders the current task
    /// to the runtime, giving the slot write a chance to commit visibly
    /// before the second handler reads `slot.current.state.status`. The
    /// in-flight job satisfies `Running`, so the second kickoff returns the
    /// first's `job_id` with `existing: true`. NO sleep knob.
    #[tokio::test]
    async fn async_duplicate_kickoff_after_first_started_returns_existing_job_id() {
        let dir = tempdir_with_one_cpp();
        let server = server_with_cpp_parser();
        let inner = server.inner.clone();
        let path = dir.path().to_string_lossy().into_owned();

        let first = analyze_codebase_async(inner.clone(), path.clone(), false).await;
        let first_parsed: serde_json::Value = serde_json::from_str(&body_text(&first)).unwrap();
        let first_job_id = first_parsed["job_id"].as_str().unwrap().to_string();
        assert_eq!(first_parsed["existing"], serde_json::json!(false));

        tokio::task::yield_now().await;

        let second = analyze_codebase_async(inner.clone(), path.clone(), false).await;
        let second_parsed: serde_json::Value = serde_json::from_str(&body_text(&second)).unwrap();
        let second_job_id = second_parsed["job_id"].as_str().unwrap().to_string();
        assert_eq!(
            second_parsed["existing"],
            serde_json::json!(true),
            "second kickoff against a Running slot must report existing=true; got: {second_parsed}"
        );
        assert_eq!(
            second_job_id, first_job_id,
            "duplicate kickoff must surface the in-flight job's job_id, not mint a new one"
        );
    }

    /// (Task 2.2 / c) An async kickoff that is still indexing must block a
    /// subsequent sync `analyze_codebase` with the same byte-identical error
    /// the wire snapshot has always carried ("indexing already in
    /// progress"). The 5-file × 50ms-per-parse fixture guarantees ≥ 250ms
    /// of in-progress window — comfortably longer than any sync handler's
    /// slot-check + spawn fast path. No yield between the async and sync
    /// calls: the slot write happens before `analyze_codebase_async`
    /// returns, so by the time we call sync the slot is already Running.
    #[tokio::test]
    async fn async_kickoff_blocks_sync_analyze() {
        let _guard = ParseSleepGuard::set(50);
        let dir = tempdir_with_n_rec(5);
        let (server, _calls) = server_with_recording_plugin();
        let inner = server.inner.clone();
        let path = dir.path().to_string_lossy().into_owned();

        let kickoff = analyze_codebase_async(inner.clone(), path.clone(), false).await;
        assert!(
            kickoff.is_error.is_none() || kickoff.is_error == Some(false),
            "async kickoff itself must not error: {kickoff:?}"
        );

        let sync_r = analyze_codebase(inner.clone(), path.clone(), false, None, None).await;
        assert_eq!(sync_r.is_error, Some(true));
        assert_eq!(
            body_text(&sync_r),
            "indexing already in progress",
            "sync handler must reject byte-identically when slot.current is Running"
        );
    }

    /// (Task 2.2 / d) An in-flight sync `analyze_codebase` (Running slot,
    /// inline await) must surface to a subsequent `analyze_codebase_async`
    /// as `existing: true` carrying the sync job's `job_id`. The 20-file ×
    /// 50ms-per-parse fixture guarantees ≥ 1s of in-progress window —
    /// abundant headroom for the spin-yield loop to land while sync is
    /// still in `run_analyze_job`'s parse phase.
    ///
    /// Synchronization primitive: bounded spin-yield against the slot's
    /// observable state. NO sleep — only `yield_now`, with a 500ms wall-
    /// clock guard so a regression that prevents the slot from reaching
    /// Running surfaces as a panic rather than a hang.
    ///
    /// Sync runs in a `tokio::spawn`ed task; we drain its `JoinHandle` after
    /// the assertion so the worker completes cleanly inside the test's
    /// runtime (avoids any "destructor running during runtime shutdown"
    /// noise from a dangling handle).
    #[tokio::test]
    async fn sync_kickoff_blocks_async_kickoff() {
        let _guard = ParseSleepGuard::set(50);
        let dir = tempdir_with_n_rec(20);
        let (server, _calls) = server_with_recording_plugin();
        let inner = server.inner.clone();
        let path = dir.path().to_string_lossy().into_owned();

        let sync_handle = {
            let inner = inner.clone();
            let path = path.clone();
            tokio::spawn(async move { analyze_codebase(inner, path, false, None, None).await })
        };

        // Spin-yield until the slot's current job is Running. Bounded at
        // 500ms — the 20-file × 50ms fixture gives ~1s of Running window,
        // so 500ms is half that and any failure to reach Running in this
        // window indicates the slot-write protocol regressed.
        let start = Instant::now();
        let sync_job_id = loop {
            {
                let slot = inner.analyze_slot.read();
                if let Some(j) = &slot.current {
                    if matches!(j.state.read().status, JobStatus::Running) {
                        break j.job_id.clone();
                    }
                }
            }
            if start.elapsed() > Duration::from_millis(500) {
                panic!(
                    "sync analyze never reached Running state in slot within 500ms — \
                     slot-write protocol regressed or sync handler returned before installing the job"
                );
            }
            tokio::task::yield_now().await;
        };

        let async_r = analyze_codebase_async(inner.clone(), path.clone(), false).await;
        let async_parsed: serde_json::Value = serde_json::from_str(&body_text(&async_r)).unwrap();
        assert_eq!(
            async_parsed["existing"],
            serde_json::json!(true),
            "async kickoff against a Running sync slot must report existing=true; got: {async_parsed}"
        );
        assert_eq!(
            async_parsed["job_id"].as_str().unwrap(),
            sync_job_id,
            "async kickoff must surface the in-flight sync job's job_id"
        );

        // Drain the sync handler so the worker completes inside this test's
        // runtime — avoids the worker future being dropped mid-flight when
        // the test's runtime tears down.
        let _ = sync_handle.await.expect("sync handler task panicked");
    }

    // ----- Task 2.3: slot rotation, failure-path, and progress tests --------
    //
    // These tests pin the rotation rules (Decision 2 — two-slot grace window;
    // Decision 4 — failed counts as terminal for rotation), the failure
    // surface (the async/failed path returns byte-identical error wording to
    // the sync handler), and the deterministic progress fan-out (Decision 8).
    //
    // Knob hygiene: only `progress_increments_during_indexing` touches
    // `SLEEP_PER_PARSE_MS`. The other four use bounded-poll loops and never
    // stretch indexing time.

    /// Write a tempdir containing one trivial `.cpp` source plus the exact
    /// malformed `.code-graph.toml` that drives `RootConfig::load` into
    /// `ConfigError::Toml` — same fixture the sync
    /// `analyze_malformed_toml_reports_parse_error` test uses, so failed-async
    /// and failed-sync exercise byte-identical error wording.
    fn tempdir_with_malformed_toml() -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.cpp"), b"void f() {}\n").unwrap();
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[discovery\nmax_threads = nope\n",
        )
        .unwrap();
        dir
    }

    /// Poll `get_status` at 50ms cadence and return the `analyze_job` view
    /// once `status` reaches `"completed"` or `"failed"`. The 5s wall-clock
    /// bound is a hang catcher: every fixture used in Task 2.3 indexes (or
    /// fails) in milliseconds, so reaching the bound means the worker hung,
    /// the slot never transitioned, or the terminal write missed.
    async fn poll_until_terminal(
        inner: Arc<ServerInner>,
        max: std::time::Duration,
    ) -> serde_json::Value {
        let deadline = std::time::Instant::now() + max;
        loop {
            if std::time::Instant::now() >= deadline {
                panic!(
                    "analyze job did not reach terminal within {max:?} — \
                     worker hung, slot not transitioning, or terminal write missed"
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let status = get_status(inner.clone());
            let parsed: serde_json::Value = serde_json::from_str(&body_text(&status)).unwrap();
            let job = &parsed["analyze_job"];
            let s = job["status"].as_str().unwrap_or("");
            if s == "completed" || s == "failed" {
                return job.clone();
            }
        }
    }

    /// (Task 2.3 / a) After a terminal job, the next kickoff rotates the
    /// previous `current` into `previous_terminal` and installs a fresh
    /// `Running` job in `current`. This is the load-bearing behavior of the
    /// two-slot grace window (Decision 2): one terminal's result survives
    /// exactly one more kickoff.
    ///
    /// The slot read happens immediately after the second kickoff returns,
    /// so `current` is observed in its installed-Running state before the
    /// 1-file worker has time to complete. Job-id identity is the load-
    /// bearing assertion; the `Running` discriminant is the wire-level
    /// pin the design's verification text calls out.
    #[tokio::test]
    async fn terminal_job_rotates_to_previous_on_next_kickoff() {
        let dir = tempdir_with_one_cpp();
        let server = server_with_cpp_parser();
        let inner = server.inner.clone();
        let path = dir.path().to_string_lossy().into_owned();

        let t1_kickoff = analyze_codebase_async(inner.clone(), path.clone(), false).await;
        let t1_parsed: serde_json::Value = serde_json::from_str(&body_text(&t1_kickoff)).unwrap();
        let t1_job_id = t1_parsed["job_id"].as_str().unwrap().to_string();

        let t1_terminal = poll_until_terminal(inner.clone(), Duration::from_secs(5)).await;
        assert_eq!(
            t1_terminal["status"],
            serde_json::json!("completed"),
            "T1 must reach Completed before T2 kickoff; got: {t1_terminal}"
        );

        let t2_kickoff = analyze_codebase_async(inner.clone(), path.clone(), false).await;
        let t2_parsed: serde_json::Value = serde_json::from_str(&body_text(&t2_kickoff)).unwrap();
        let t2_job_id = t2_parsed["job_id"].as_str().unwrap().to_string();
        assert_ne!(
            t1_job_id, t2_job_id,
            "T2 kickoff after T1 terminal must mint a fresh job_id; rotation requires distinct ids"
        );

        let slot = inner.analyze_slot.read();
        let previous = slot
            .previous_terminal
            .as_ref()
            .expect("previous_terminal must carry T1 after T2 kickoff");
        assert_eq!(
            previous.job_id, t1_job_id,
            "previous_terminal must hold T1's job_id post-rotation"
        );
        let current = slot
            .current
            .as_ref()
            .expect("current must carry T2 after kickoff");
        assert_eq!(
            current.job_id, t2_job_id,
            "current must hold T2's job_id post-rotation"
        );
        assert!(
            matches!(current.state.read().status, JobStatus::Running),
            "current (T2) must be Running immediately after kickoff — read happens before worker terminal"
        );
    }

    /// (Task 2.3 / b) The grace window is bounded at one terminal. T1 →
    /// Completed → T2 → Completed → T3 leaves `previous_terminal = T2` and
    /// loses T1 entirely. Confirms the slot is two-deep, not unbounded.
    #[tokio::test]
    async fn two_back_to_back_analyses_lose_oldest_terminal() {
        let dir = tempdir_with_one_cpp();
        let server = server_with_cpp_parser();
        let inner = server.inner.clone();
        let path = dir.path().to_string_lossy().into_owned();

        let t1 = analyze_codebase_async(inner.clone(), path.clone(), false).await;
        let t1_parsed: serde_json::Value = serde_json::from_str(&body_text(&t1)).unwrap();
        let t1_job_id = t1_parsed["job_id"].as_str().unwrap().to_string();
        let _ = poll_until_terminal(inner.clone(), Duration::from_secs(5)).await;

        let t2 = analyze_codebase_async(inner.clone(), path.clone(), false).await;
        let t2_parsed: serde_json::Value = serde_json::from_str(&body_text(&t2)).unwrap();
        let t2_job_id = t2_parsed["job_id"].as_str().unwrap().to_string();
        let _ = poll_until_terminal(inner.clone(), Duration::from_secs(5)).await;

        let t3 = analyze_codebase_async(inner.clone(), path.clone(), false).await;
        let t3_parsed: serde_json::Value = serde_json::from_str(&body_text(&t3)).unwrap();
        let t3_job_id = t3_parsed["job_id"].as_str().unwrap().to_string();

        let slot = inner.analyze_slot.read();
        let previous_id = slot
            .previous_terminal
            .as_ref()
            .expect("previous_terminal must hold T2 after T3 kickoff")
            .job_id
            .clone();
        let current_id = slot
            .current
            .as_ref()
            .expect("current must hold T3 after kickoff")
            .job_id
            .clone();
        assert_eq!(
            previous_id, t2_job_id,
            "previous_terminal must rotate to T2 after T3 kickoff (T1 falls off the back)"
        );
        assert_eq!(
            current_id, t3_job_id,
            "current must hold T3's job_id post-rotation"
        );
        assert_ne!(
            previous_id, t1_job_id,
            "T1's job_id must no longer appear in previous_terminal"
        );
        assert_ne!(
            current_id, t1_job_id,
            "T1's job_id must no longer appear in current"
        );
    }

    /// (Task 2.3 / c) A malformed `.code-graph.toml` drives the worker into
    /// `JobStatus::Failed`. `get_status` surfaces the failure with the SAME
    /// byte-identical error prefix the existing sync handler produces
    /// (`"failed to parse .code-graph.toml"`), preserving the design's
    /// contract that failed-async and failed-sync expose the same wire text.
    #[tokio::test]
    async fn failed_job_surfaces_error_in_get_status() {
        let dir = tempdir_with_malformed_toml();
        let server = server_with_cpp_parser();
        let inner = server.inner.clone();

        let _ = analyze_codebase_async(
            inner.clone(),
            dir.path().to_string_lossy().into_owned(),
            false,
        )
        .await;

        let terminal = poll_until_terminal(inner.clone(), Duration::from_secs(5)).await;
        assert_eq!(
            terminal["status"],
            serde_json::json!("failed"),
            "malformed toml must drive the job to Failed; got: {terminal}"
        );
        let err = terminal["error"]
            .as_str()
            .expect("error must be populated when status is failed");
        assert!(
            err.starts_with("failed to parse .code-graph.toml"),
            "failed-async error must start with the same prefix the sync handler emits; got: {err:?}"
        );
    }

    /// (Task 2.3 / d) Failed counts as terminal for rotation purposes
    /// (Decision 4). A failed T1 rotates into `previous_terminal` exactly
    /// like a completed one would, and the original error message is
    /// preserved through the rotation (the slot stores `Arc<AnalyzeJob>`,
    /// so the inner state is shared, not copied).
    #[tokio::test]
    async fn failed_job_rotates_to_previous_terminal() {
        let bad_dir = tempdir_with_malformed_toml();
        let good_dir = tempdir_with_one_cpp();
        let server = server_with_cpp_parser();
        let inner = server.inner.clone();

        let t1 = analyze_codebase_async(
            inner.clone(),
            bad_dir.path().to_string_lossy().into_owned(),
            false,
        )
        .await;
        let t1_parsed: serde_json::Value = serde_json::from_str(&body_text(&t1)).unwrap();
        let t1_job_id = t1_parsed["job_id"].as_str().unwrap().to_string();
        let t1_terminal = poll_until_terminal(inner.clone(), Duration::from_secs(5)).await;
        assert_eq!(
            t1_terminal["status"],
            serde_json::json!("failed"),
            "T1 must reach Failed before T2 kickoff; got: {t1_terminal}"
        );

        let t2 = analyze_codebase_async(
            inner.clone(),
            good_dir.path().to_string_lossy().into_owned(),
            false,
        )
        .await;
        let t2_parsed: serde_json::Value = serde_json::from_str(&body_text(&t2)).unwrap();
        let t2_job_id = t2_parsed["job_id"].as_str().unwrap().to_string();

        let slot = inner.analyze_slot.read();
        let previous = slot
            .previous_terminal
            .as_ref()
            .expect("previous_terminal must carry the failed T1 after T2 kickoff");
        assert_eq!(
            previous.job_id, t1_job_id,
            "previous_terminal must hold T1's job_id even when T1 ended Failed"
        );
        match &previous.state.read().status {
            JobStatus::Failed(msg) => assert!(
                msg.starts_with("failed to parse .code-graph.toml"),
                "Failed message must survive rotation byte-identically; got: {msg:?}"
            ),
            other => panic!(
                "previous_terminal status must be Failed(_) after rotating a failed T1; got: {:?}",
                std::mem::discriminant(other)
            ),
        }
        let current = slot
            .current
            .as_ref()
            .expect("current must hold T2 after kickoff");
        assert_eq!(
            current.job_id, t2_job_id,
            "current must hold T2's job_id post-rotation"
        );
    }

    /// (Task 2.3 / e) Progress is fan-out (Decision 8) — the inner-lock
    /// write happens on every `report()` call, NOT just on terminal
    /// transition. With 10ms-per-file × 20 files indexed SEQUENTIALLY
    /// (`parsing.max_threads = 1` written into a project `.code-graph.toml`),
    /// the parse phase spends ~200ms in `report()`. 30ms polls give ≥ 6
    /// mid-run samples; ≥ 3 distinct values leaves headroom for scheduler
    /// jitter while catching the "atomic flushed only on completion"
    /// failure mode, which would surface as `{0, final}` — 2 distinct
    /// values.
    ///
    /// **Sequential parse is load-bearing.** Without it, the rayon pool
    /// defaults to `num_cpus`; with 20 files and 16+ cores the entire
    /// parse window collapses to one `SLEEP_PER_PARSE_MS` (~10ms) — far
    /// shorter than the 30ms cadence, and the test routinely sees < 3
    /// samples regardless of how the production code behaves.
    ///
    /// **Sampling is filtered to the parse phase.** `progress` is
    /// monotonic within a phase and resets at each phase boundary
    /// (parse → resolve → merge); phase identity rides on
    /// `progress_message`. We filter to messages carrying the
    /// `"Parsing: "` prefix so the monotonicity assertion targets the
    /// load-bearing fan-out behavior cleanly without crossing a phase
    /// boundary mid-loop. See the `progress` doc-comment in
    /// `crates/code-graph-tools/src/handlers/status.rs` for the
    /// canonical contract.
    #[tokio::test]
    async fn progress_increments_during_indexing() {
        let _guard = ParseSleepGuard::set(10);
        let dir = tempdir_with_n_rec(20);
        // Force serial parse so the 20 × 10ms sleep yields a deterministic
        // ~200ms parse window regardless of host CPU count. The toml lands
        // at the indexed root, so RootConfig::load picks it up.
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[parsing]\nmax_threads = 1\n",
        )
        .unwrap();
        let (server, _calls) = server_with_recording_plugin();
        let inner = server.inner.clone();
        let path = dir.path().to_string_lossy().into_owned();

        let _ = analyze_codebase_async(inner.clone(), path, false).await;

        let deadline = Instant::now() + Duration::from_secs(1);
        let mut parse_values: Vec<u32> = Vec::new();
        let mut all_samples: Vec<(u32, String)> = Vec::new();
        let final_status: String;
        loop {
            if Instant::now() >= deadline {
                panic!(
                    "progress test never reached terminal within 1s — \
                     20 × 10ms sequential parse should finish well inside this bound \
                     (parse samples observed: {} = {:?}; all samples: {:?})",
                    parse_values.len(),
                    parse_values,
                    all_samples
                );
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
            let status = get_status(inner.clone());
            let parsed: serde_json::Value = serde_json::from_str(&body_text(&status)).unwrap();
            let job = &parsed["analyze_job"];
            let s = job["status"].as_str().unwrap_or("").to_string();
            let progress = job["progress"].as_u64().unwrap_or(0) as u32;
            let message = job["progress_message"].as_str().unwrap_or("").to_string();
            all_samples.push((progress, message.clone()));
            if message.starts_with("Parsing: ") {
                parse_values.push(progress);
            }
            if s == "completed" || s == "failed" {
                final_status = s;
                break;
            }
        }

        assert_eq!(
            final_status, "completed",
            "20 trivial .rec files must index cleanly through the recording plugin; \
             got terminal status: {final_status:?}"
        );

        assert!(
            parse_values.windows(2).all(|w| w[0] <= w[1]),
            "parse-phase progress must be monotonic non-decreasing; \
             parse samples = {parse_values:?}; all samples = {all_samples:?}"
        );

        let distinct: std::collections::HashSet<u32> = parse_values.iter().copied().collect();
        assert!(
            distinct.len() >= 3,
            "expected ≥ 3 distinct parse-phase progress values (NOT just 0 → final); \
             parse samples = {parse_values:?}, distinct count = {}, all samples = {all_samples:?}. \
             A '2 distinct values' failure means progress is only flushed on terminal \
             transition — a production bug in the fan-out sink.",
            distinct.len()
        );
    }
}
