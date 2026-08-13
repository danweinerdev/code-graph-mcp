//! Typed core for `analyze_codebase` / `analyze_codebase_async`.
//!
//! Both tools are **ungated by design** — `analyze_codebase` is what
//! *creates* the index, so there is no core `require_indexed` call here
//! (matches `core::status`, the other ungated module).
//!
//! ## The progress bridge (OQ-A3)
//!
//! `run_analyze_job` used to take `peer: Option<Peer<RoleServer>>` and
//! `progress_token: Option<ProgressToken>` (both rmcp types) and forward
//! parse-pool progress straight to `peer.notify_progress`. The core must
//! not reference rmcp, so it instead takes an abstract
//! `Arc<dyn ProgressSink>` — the same trait the indexer already defines
//! (`crate::indexer::ProgressSink`) — and the adapter in
//! `handlers::analyze` supplies the rmcp-forwarding implementation
//! (`RmcpProgressSink` + its background forwarder task) built from the
//! `Peer`/`ProgressToken` it receives. `JobAwareProgressSink<S>` is
//! generic over any `S: ProgressSink`, so it works identically whether
//! `S` is a `ChannelProgressSink` in a unit test or an
//! `Arc<dyn ProgressSink>` on the real MCP path — see
//! `crate::indexer`'s blanket `impl<T: ProgressSink + ?Sized> ProgressSink
//! for Arc<T>`, which is what makes `Arc<dyn ProgressSink>` itself a
//! valid `S`.
//!
//! Body moved verbatim from `handlers::analyze` (task 2.6), minus the
//! rmcp-specific forwarding machinery (channel + throttled forwarder
//! task), which now lives in the adapter — the core only ever calls
//! `sink.report(...)` / `JobAwareProgressSink::transition_to(...)`.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use code_graph_core::{paths, ConfigError, RootConfig};
use code_graph_graph::Graph;

use crate::analyze_job::{
    covers, CoverageIdentity, Job, JobKind, JobPhase, JobRequest, JobResult, JobSlot, JobStatus,
    PendingJob, JOB_PENDING_LIMIT, TERMINAL_HISTORY_LIMIT,
};
use crate::core::{ToolError, ToolOk, ToolResult};
use crate::handlers::analyze::{
    now_nanos_u64, AnalyzeResult, AsyncKickoffResponse, SyncAnalyzeResponse,
};
use crate::handlers::status::{format_unix_nanos_rfc3339, JobView};
use crate::handlers::ENVELOPE_OVERHEAD_BYTES;
use crate::indexer::{
    build_file_index, build_symbol_index, extend_file_index, extend_symbol_index, index_directory,
    resolve_edges_with_indexes, NoopProgressSink, ProgressSink,
};
use crate::server::ServerInner;

/// Wraps any [`ProgressSink`] so each `report()` ALSO writes the latest
/// progress triple into the owning [`Job`]'s mutable state.
/// Fan-out per Design Decision 8: the wrapped sink keeps the existing
/// throttled peer-notification path intact for sync mode, while the slot
/// write makes progress observable to `get_status` for both sync and
/// async callers.
///
/// Generic over `S: ProgressSink` (rather than a fixed
/// `ChannelProgressSink` field) so it composes with any sink shape,
/// including the `Arc<dyn ProgressSink>` the real MCP path passes and
/// the concrete sinks unit tests construct directly.
pub(crate) struct JobAwareProgressSink<S: ProgressSink> {
    pub(crate) inner: S,
    pub(crate) job: Arc<Job>,
}

impl<S: ProgressSink> ProgressSink for JobAwareProgressSink<S> {
    fn report(&self, progress: u32, total: u32, message: &str) {
        // Order matters: keep the existing inner-sink report first so the
        // forwarder's throttle window (when the inner sink is
        // peer-forwarding) observes the same arrival pattern it has
        // always seen — slot mutation is the new side effect, not a
        // replacement.
        self.inner.report(progress, total, message);
        let mut s = self.job.state.write();
        s.progress = progress;
        s.progress_total = total;
        s.progress_message = message.to_string();
    }
}

impl<S: ProgressSink> JobAwareProgressSink<S> {
    /// Atomic phase transition + inner-sink emission. Calls
    /// `Job::set_phase` to mutate job state (which sets the
    /// phase-specific message and resets `progress`), then pushes a
    /// snapshot of that state through the inner sink so a peer-forwarding
    /// sink observes the phase boundary. Without this push, a peer
    /// polling via the notification channel would never see a "Resolving
    /// cross-file edges" / "Persisting cache to disk" event — only via
    /// `get_status` polling — because no per-step `report()` fires from
    /// the worker between phase boundaries (and during persist there are
    /// no per-step reports AT ALL).
    ///
    /// The push bypasses `Self::report` to avoid re-writing the same
    /// values to `job.state` that `set_phase` already wrote (the read
    /// snapshot ensures the pushed event exactly matches what
    /// `get_status` would return at this instant).
    pub(crate) fn transition_to(&self, phase: JobPhase) {
        self.job.set_phase(phase);
        let (progress, total, message) = {
            let s = self.job.state.read();
            (s.progress, s.progress_total, s.progress_message.clone())
        };
        self.inner.report(progress, total, &message);
    }
}

/// Run the analyze pipeline to terminal state on a shared [`Job`].
///
/// Writes `JobStatus::Completed(AnalyzeResult)` or `JobStatus::Failed(msg)`
/// into `job.state` before returning; the return type is `()` because all
/// outcomes flow through the slot. `sink` receives every progress event;
/// the adapter passes an `Arc<dyn ProgressSink>` — a real forwarding sink
/// for sync callers with a peer/token, `Arc::new(NoopProgressSink)`
/// otherwise (async kickoff has no client-side progress channel).
///
/// Acquires `inner.index_lock` with `lock().await` (NOT `try_lock`): the
/// slot is the single-flight gate now; `index_lock` only serializes worker
/// vs. watch reindex (Design Decision 1).
pub(crate) async fn run_analyze_job(
    inner: Arc<ServerInner>,
    job: Arc<Job>,
    sink: Arc<dyn ProgressSink>,
) {
    let path_raw = job.path.clone();
    let force = job.force;

    // A bound Linux daemon keeps logical paths for all index work, but must
    // reject the operation before its first filesystem lookup if that logical
    // root has been substituted since startup.
    if let Err(error) = inner.ensure_daemon_root_current() {
        finish_failed(&job, error);
        return;
    }

    // An existing directory was canonicalized at admission for coverage.
    // Reuse that identity for execution so a queued symlink cannot be
    // retargeted into a different indexing scope before promotion. Requests
    // without admission identity retain the established worker-time raw-path
    // canonicalization and validation errors.
    let abs_path = match job.coverage.as_ref() {
        Some(coverage) => coverage.invocation_path.clone(),
        None => match paths::canonicalize(std::path::Path::new(&path_raw)) {
            Ok(path) => path,
            Err(_) => {
                finish_failed(&job, format!("directory does not exist: {path_raw}"));
                return;
            }
        },
    };
    if !abs_path.is_dir() {
        finish_failed(
            &job,
            format!("path is not a directory: {}", abs_path.display()),
        );
        return;
    }

    if let Some(daemon_root) = inner.daemon_project_root.get() {
        if !abs_path.starts_with(daemon_root) {
            finish_failed(
                &job,
                format!(
                    "daemon is bound to project root {}; cannot analyze path {} outside that root",
                    daemon_root.display(),
                    abs_path.display()
                ),
            );
            return;
        }
    }

    // A valid admission identity carries the exact config/root pair used for
    // coverage. Re-discovering here would let a queued job cross a newly
    // created, removed, or replaced nested `.code-graph.toml` boundary. Jobs
    // with no admitted identity still load here so nonexistent and malformed
    // config requests retain their established execution-time errors.
    let (mut cfg, project_root, config_present) = match job.coverage.as_ref() {
        Some(coverage) => (
            coverage.config.clone(),
            coverage.project_root.clone(),
            coverage.config_present,
        ),
        None => match RootConfig::load_with_presence(&abs_path) {
            Ok((c, root, config_present)) => (c, root, config_present),
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
            | Err(e @ ConfigError::MacroStripConflict { .. })
            | Err(e @ ConfigError::MacroDefineTypeEmptyName)
            | Err(e @ ConfigError::MacroDefineTypeKeyword { .. }) => {
                finish_failed(&job, format!("invalid .code-graph.toml: {e}"));
                return;
            }
        },
    };
    if let Some(daemon_root) = inner.daemon_project_root.get() {
        if daemon_root != &project_root {
            finish_failed(
                &job,
                format!(
                    "daemon is bound to project root {}; cannot analyze project root {}",
                    daemon_root.display(),
                    project_root.display()
                ),
            );
            return;
        }
    }
    let mut warnings = cfg.resolve_concurrency();

    // Serialize against the watch reindex path; the slot already gates
    // analyze-vs-analyze, so this lock has no analyze contention.
    #[cfg(debug_assertions)]
    debug_write_before_index_lock_marker(&abs_path);
    let _guard = inner.index_lock.lock().await;
    if let Err(error) = inner.ensure_daemon_root_current() {
        finish_failed(&job, error);
        return;
    }

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
        job.set_phase(JobPhase::Discovering);
    } else {
        job.set_phase(JobPhase::LoadingCache);
    }

    if project_root != abs_path {
        if config_present {
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
        if !config_present {
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

    // `prebuilt_graph` holds the fast-path probe when the cache loaded
    // successfully but the fast path falls through (e.g. some in-scope
    // files are stale, or no in-scope files exist in the cache). The
    // slow path inside `spawn_blocking` reuses this Graph instead of
    // calling `merged_graph.load()` a second time — on UE-scale caches
    // (~3GB rkyv archive) the double-load was responsible for several
    // minutes of redundant I/O on every incremental analyze.
    //
    // Set to `None` when `force=true` (no fast-path probe runs), when
    // the cache file is absent / stale-version, or when `load_and_stale`
    // returns an error.
    let prebuilt_graph: Option<Graph> = if !force {
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
                if let Err(error) = inner.ensure_daemon_root_current() {
                    finish_failed(&job, error);
                    return;
                }
                let stats = {
                    // Lock order: status_publication is outermost, followed by
                    // graph and applied-index/config locks. Never await while
                    // this guard is held.
                    let _publication = inner.status_publication.write();
                    let mut g = inner.graph.write();
                    *g = probe;
                    let stats = g.stats();
                    drop(g);
                    pause_after_graph_replacement_before_metadata(&inner);
                    publish_applied_index_state(&inner, &project_root, config_present, cfg);
                    inner.indexed.store(true, Ordering::Release);
                    inner
                        .index_built_at
                        .store(now_nanos_u64(), Ordering::Release);
                    inner.index_force_built.store(force, Ordering::Release);
                    stats
                };
                if sweep_ran {
                    // The sweep introduces a cache write — bump the
                    // phase so a polling client doesn't see the
                    // terminal stamp without ever observing a
                    // `Persisting` signal. Skipped when `sweep_ran`
                    // is false (no save_cache call) so the terminal
                    // phase stays at `Discovering`, matching what
                    // actually happened.
                    job.set_phase(JobPhase::Persisting);
                    if let Err(e) = save_cache(&inner, &project_root) {
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
                    coalesced_by: None,
                };
                finish_completed(&job, result);
                return;
            }
            // Fall-through with stale in-scope files: hoist the
            // already-loaded probe into the slow path.
            Some(probe)
        } else if load_ok {
            // Cache loaded but contained no in-scope files (likely a
            // first-time scope under an existing project root). Still
            // worth reusing — the slow path's eviction is a no-op for
            // out-of-scope entries.
            Some(probe)
        } else {
            // No cache on disk, or load failed. Slow path will start
            // from a fresh Graph.
            None
        }
    } else {
        None
    };

    let sink_for_pool = Arc::clone(&sink);
    let registry = Arc::clone(&inner);
    let cfg_for_pool = cfg.clone();
    let abs_path_for_pool = abs_path.clone();
    let project_root_for_pool = project_root.clone();
    // `scope_is_project_root` already computed near the top of this
    // function for the initial-phase decision; reused here.
    let job_for_pool = Arc::clone(&job);
    let blocking_handle = tokio::task::spawn_blocking(move || {
        let sink = JobAwareProgressSink {
            inner: sink_for_pool,
            job: job_for_pool,
        };
        let mut blocking_warnings: Vec<String> = Vec::new();

        let phase_start = std::time::Instant::now();
        // Cache acquisition: three paths, mutually exclusive.
        //
        // 1. `prebuilt_graph: Some(g)` — the fast-path probe already
        //    loaded the cache and the slow path inherited it. This is
        //    the dominant `force=false` incremental case on UE-scale
        //    projects (~3GB cache); reusing the probe saves a full
        //    re-deserialize that previously cost minutes of redundant
        //    I/O. `cache_loaded = true` by construction.
        //
        // 2. `force=true && scope_is_project_root` — the loaded graph
        //    would immediately be `clear()`ed below, so loading is
        //    wasted I/O. Skip entirely; `cache_loaded = false`.
        //
        // 3. Anything else (`force=false` with no cache on disk,
        //    `force=true` on a sub-scope where `drop_files_in_scope`
        //    needs the cached entries) — load fresh inside
        //    spawn_blocking.
        let (mut merged_graph, cache_loaded) = if let Some(g) = prebuilt_graph {
            eprintln!(
                "[code-graph] phase: cache reused from fast-path probe \
                 ({} cached files, no re-load needed)",
                g.stats().files
            );
            (g, true)
        } else if force && scope_is_project_root {
            eprintln!(
                "[code-graph] phase: cache load SKIPPED \
                 (force=true, project-root scope — cache would be cleared)"
            );
            (Graph::new(), false)
        } else {
            // Stamp the LoadingCache phase before the blocking
            // rkyv deserialization so polling clients see an
            // accurate "Loading cache from disk" signal instead of
            // sitting at `discovering, 0/0` for the entire load
            // window — which on multi-GB caches can be minutes.
            sink.transition_to(JobPhase::LoadingCache);
            eprintln!(
                "[code-graph] phase: loading cache from {}",
                project_root_for_pool.display()
            );
            let mut g = Graph::new();
            let loaded = g.load(&project_root_for_pool).unwrap_or(false);
            eprintln!(
                "[code-graph] phase: cache load {} ({:.1}s, {} cached files)",
                if loaded { "ok" } else { "absent/stale" },
                phase_start.elapsed().as_secs_f64(),
                g.stats().files
            );
            (g, loaded)
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
        sink.transition_to(JobPhase::Discovering);
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
        // file-walk. Each `transition_to` ALSO pushes an event
        // through the inner sink, so a peer-forwarding sink observes
        // the phase boundary in addition to the per-file events.
        sink.transition_to(JobPhase::Parsing);
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
        sink.transition_to(JobPhase::Resolving);
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
            &cfg_for_pool.extensions,
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

    let outcome: Result<AnalyzeResult, String> = match blocking_result {
        Ok(Ok((merged_graph, blocking_warnings))) => {
            warnings.extend(blocking_warnings);
            if merged_graph.stats().files == 0 {
                Err(format!(
                    "no supported source files found in {}",
                    abs_path.display()
                ))
            } else if let Err(error) = inner.ensure_daemon_root_current() {
                Err(error)
            } else {
                let stats = {
                    // Lock order: status_publication is outermost, followed by
                    // graph and applied-index/config locks. Never await while
                    // this guard is held.
                    let _publication = inner.status_publication.write();
                    let mut g = inner.graph.write();
                    *g = merged_graph;
                    let stats = g.stats();
                    drop(g);
                    pause_after_graph_replacement_before_metadata(&inner);
                    publish_applied_index_state(&inner, &project_root, config_present, cfg);
                    inner.indexed.store(true, Ordering::Release);
                    inner
                        .index_built_at
                        .store(now_nanos_u64(), Ordering::Release);
                    inner.index_force_built.store(force, Ordering::Release);
                    stats
                };

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
                // event through `sink` directly. The spawn_blocking-owned
                // `JobAwareProgressSink` is gone by this point, but `sink`
                // (the `Arc<dyn ProgressSink>` passed into this function)
                // is still alive, so a fresh `JobAwareProgressSink` wrapping
                // it reproduces the exact same push `transition_to` always
                // does — no separate channel-draining hack needed now that
                // the sink is a shared `Arc` rather than a per-call mpsc
                // sender. Without this push, a peer-forwarding sink would
                // never observe the Persisting phase — `save_cache` has no
                // per-step `report`.
                JobAwareProgressSink {
                    inner: Arc::clone(&sink),
                    job: Arc::clone(&job),
                }
                .transition_to(JobPhase::Persisting);

                let save_start = std::time::Instant::now();
                if let Err(e) = save_cache(&inner, &project_root) {
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
                    coalesced_by: None,
                })
            }
        }
        Ok(Err(e)) => Err(format!("indexing failed: {e}")),
        Err(join_err) => Err(format!("indexing task panicked: {join_err}")),
    };

    match outcome {
        Ok(result) => finish_completed(&job, result),
        Err(msg) => finish_failed(&job, msg),
    }
}

/// Publish the configuration snapshot that governs the newly applied graph.
///
/// Call only while holding `inner.status_publication` after a successful
/// fast-path or merge has replaced `inner.graph`.
/// In particular, queued jobs and failed analyzes must not alter this snapshot:
/// `get_status` describes the active index, not the most recently admitted
/// request or the filesystem's current `.code-graph.toml` state.
fn publish_applied_index_state(
    inner: &ServerInner,
    project_root: &std::path::Path,
    config_present: bool,
    cfg: RootConfig,
) {
    let project_root = project_root.to_path_buf();
    let config_path = config_present.then(|| project_root.join(".code-graph.toml"));
    *inner.root_path.write() = Some(project_root.clone());
    *inner.cache_root.write() = Some(project_root.clone());
    *inner.config.write() = cfg.clone();
    *inner.applied_index.write() = crate::server::AppliedIndexState {
        root_path: Some(project_root),
        config_path,
        config: cfg,
    };
}

/// Invoke the deterministic test handoff while the publication write guard is
/// held. This is synchronous by design: the lock-order contract forbids an
/// `.await` while `status_publication` is held.
#[cfg(test)]
fn pause_after_graph_replacement_before_metadata(inner: &ServerInner) {
    if let Some(hook) = inner.publication_hook.lock().take() {
        let _ = hook.reached.send(());
        hook.proceed.wait();
    }
}

#[cfg(not(test))]
fn pause_after_graph_replacement_before_metadata(_inner: &ServerInner) {}

/// Stamp terminal state under a single `state.write()` so an observer
/// (the sync handler reading after `await`, or polled `get_status`)
/// sees status+finished_at+phase consistently — never a half-written
/// transition.
///
/// Also stamps `current_phase = Completed`, `progress = 1/1`, and
/// message `"Analyze complete"` atomically with the status flip so a
/// polling client observing `current_phase == "completed"` can treat
/// the analyze as finished without separately consulting `status`.
pub(crate) fn finish_completed(job: &Job, result: AnalyzeResult) {
    let mut s = job.state.write();
    s.current_phase = Some(JobPhase::Completed);
    s.progress = 1;
    s.progress_total = 1;
    s.progress_message = "Analyze complete".to_string();
    s.status = JobStatus::Completed(JobResult::Analyze(result));
    s.finished_at = Some(now_nanos_u64());
    drop(s);
    job.terminal_changed.notify_waiters();
}

pub(crate) fn finish_failed(job: &Job, msg: String) {
    // Intentionally do NOT touch `current_phase` — leave it at the
    // last in-flight phase so polling clients see WHERE the failure
    // happened (e.g. `current_phase: "parsing"` + `error: "..."`
    // tells the agent the failure was during parsing, not resolve
    // or persist). `Completed` is reserved for successful terminals.
    let mut s = job.state.write();
    s.status = JobStatus::Failed(msg);
    s.finished_at = Some(now_nanos_u64());
    drop(s);
    job.terminal_changed.notify_waiters();
}

/// `analyze_codebase` body.
///
/// Slot-protocol coordination only — the heavy lifting (cache fast-path,
/// parse pipeline, merge, persist) lives in [`run_analyze_job`]. The slot
/// is the FIFO admission gate; `index_lock` serializes promoted workers and
/// watch reindex work.
///
/// Ungated by design (Decision 8 / plan task 2.2 notes): this is what
/// creates the index, so there is no core `require_indexed` call here.
pub async fn analyze_codebase(
    inner: Arc<ServerInner>,
    path_raw: String,
    force: bool,
    sink: Arc<dyn ProgressSink>,
) -> ToolResult<SyncAnalyzeResponse> {
    if path_raw.is_empty() {
        return Err(ToolError("'path' is required".to_string()));
    }
    let admission = {
        // Coverage/config discovery performs filesystem reads. Keep this
        // async gate while its blocking probe runs so the snapshot and later
        // FIFO admission remain one linearizable operation, but never take a
        // parking_lot slot/persistence lock across that await.
        let _admission_lock = inner.admission_lock.lock().await;
        let coverage = probe_analyze_admission(Arc::clone(&inner), path_raw.clone()).await?;
        #[cfg(test)]
        pause_after_coverage_probe(&inner).await;
        let mut slot = inner.analyze_slot.write();
        // Decision 7 only lets a sync request escape the FIFO when another
        // request was already waiting before this admission. The first
        // distinct request behind a running worker keeps normal synchronous
        // semantics (including its real progress sink) and waits for itself.
        let pending_before_admission = !slot.pending.is_empty();
        match admit_job(&inner, &mut slot, path_raw, force, coverage)? {
            Admission::Covered { job, .. } => SyncAdmission::Covered(job),
            Admission::Queued { job, job_guard } => {
                slot.pending.push_back(PendingJob {
                    job: Arc::clone(&job),
                    job_guard,
                    sink: if pending_before_admission {
                        Arc::new(NoopProgressSink)
                    } else {
                        sink
                    },
                });
                if pending_before_admission {
                    SyncAdmission::QueuedImmediate(job)
                } else {
                    SyncAdmission::QueuedBlocking(job)
                }
            }
            Admission::Running { job, job_guard } => {
                spawn_supervised_job(Arc::clone(&inner), Arc::clone(&job), sink, job_guard);
                SyncAdmission::Running(job)
            }
        }
    };

    let (job, coalesced_by) = match admission {
        SyncAdmission::Covered(job) => (Arc::clone(&job), Some(job.job_id.clone())),
        SyncAdmission::Running(job) => (job, None),
        SyncAdmission::QueuedImmediate(job) => {
            return Ok(ToolOk::Value(SyncAnalyzeResponse::Queued(
                queued_kickoff_response(&job),
            )));
        }
        SyncAdmission::QueuedBlocking(job) => (job, None),
    };

    // The worker is detached before this call reaches an await point. A sync
    // caller therefore only observes its job and can be cancelled without
    // cancelling or stranding the worker/successors.
    wait_for_terminal(&job).await;

    let state = job.state.read();
    match &state.status {
        JobStatus::Completed(JobResult::Analyze(result)) => {
            let mut result = result.clone();
            result.coalesced_by = coalesced_by;
            Ok(ToolOk::Value(SyncAnalyzeResponse::Result(result)))
        }
        JobStatus::Completed(_) => unreachable!("analyze jobs always retain Analyze results"),
        JobStatus::Failed(msg) => {
            let message = match coalesced_by {
                Some(coverer_job_id) => format!("{msg} (coalesced_by: {coverer_job_id})"),
                None => msg.clone(),
            };
            Err(ToolError(message))
        }
        JobStatus::Running => {
            unreachable!("run_analyze_job must write a terminal JobStatus before returning")
        }
        JobStatus::Queued => unreachable!("queued sync analyze cannot complete its terminal wait"),
    }
}

enum SyncAdmission {
    Covered(Arc<Job>),
    Running(Arc<Job>),
    QueuedBlocking(Arc<Job>),
    QueuedImmediate(Arc<Job>),
}

/// `analyze_codebase_async` body — kickoff that returns before indexing begins.
/// Admission normally completes quickly, but its canonicalization and config
/// discovery can wait on a slow filesystem.
///
/// Identical FIFO slot protocol to [`analyze_codebase`] except the worker is
/// `tokio::spawn`ed and detached instead of awaited inline.
///
/// No progress sink parameter — async kickoff has no client-side
/// progress channel; agents observe progress by polling `get_status`.
/// The detached worker runs with `Arc::new(NoopProgressSink)`.
///
/// Ungated by design, same as [`analyze_codebase`].
pub async fn analyze_codebase_async(
    inner: Arc<ServerInner>,
    path_raw: String,
    force: bool,
) -> ToolResult<AsyncKickoffResponse> {
    if path_raw.is_empty() {
        return Err(ToolError("'path' is required".to_string()));
    }

    let kickoff = {
        // See synchronous admission: this lock intentionally covers the
        // blocking snapshot plus slot insertion, not a parking_lot slot or
        // persistence lock across the probe await.
        let _admission_lock = inner.admission_lock.lock().await;
        let coverage = probe_analyze_admission(Arc::clone(&inner), path_raw.clone()).await?;
        #[cfg(test)]
        pause_after_coverage_probe(&inner).await;
        let mut slot = inner.analyze_slot.write();
        match admit_job(&inner, &mut slot, path_raw, force, coverage)? {
            Admission::Covered { job, status } => (job, status, true),
            Admission::Queued { job, job_guard } => {
                slot.pending.push_back(PendingJob {
                    job: Arc::clone(&job),
                    job_guard,
                    sink: Arc::new(NoopProgressSink),
                });
                (job, "queued", false)
            }
            Admission::Running { job, job_guard } => {
                spawn_supervised_job(
                    Arc::clone(&inner),
                    Arc::clone(&job),
                    Arc::new(NoopProgressSink),
                    job_guard,
                );
                (job, "running", false)
            }
        }
    };

    let (job, status, existing) = kickoff;
    Ok(ToolOk::Value(AsyncKickoffResponse {
        job_id: job.job_id.clone(),
        status,
        started_at: format_unix_nanos_rfc3339(job.started_at),
        existing,
        note: if existing {
            "analyze request coalesced into an existing job — poll get_job_status(job_id) for progress and the terminal result"
        } else if status == "running" {
            "analyze kicked off — poll get_job_status(job_id) for progress and the terminal result"
        } else {
            "analyze queued — poll get_job_status(job_id) for progress and the terminal result; use get_status for FIFO position"
        },
    }))
}

/// Kick off whole-graph community detection on the shared FIFO. Indexed state
/// and validation are checked before admission so rejected requests never
/// consume an ID or appear in status. The server adapter enforces the same
/// indexed-state guard on the MCP path.
pub(crate) async fn detect_communities_async(
    inner: Arc<ServerInner>,
    granularity: Option<String>,
    max_iterations: Option<u32>,
    members_per_community: Option<u32>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> ToolResult<AsyncKickoffResponse> {
    crate::core::require_indexed(inner.indexed.load(Ordering::Acquire))?;
    crate::core::structure::validate_detect_communities_args(granularity.as_deref())?;
    let request = JobRequest::DetectCommunities {
        granularity,
        max_iterations,
        members_per_community,
        limit,
        offset,
    };
    let job = {
        // `analyze_slot` is the shared FIFO's linearization point. Community
        // jobs have already validated and need no coverage snapshot, so they
        // enter it directly rather than waiting for an analyze filesystem
        // probe that has not yet linearized.
        let max_bytes = inner.config.read().response.max_bytes;
        let mut slot = inner.analyze_slot.write();
        let guard = inner
            .persist
            .begin_job()
            .map_err(|message| ToolError(message.to_string()))?;
        let queued = slot_is_occupied(&slot);
        if queued && slot.pending.len() >= JOB_PENDING_LIMIT {
            return Err(queue_full_error());
        }
        let (job_id, started_at) = issue_job_id(&mut slot);
        let job = if queued {
            Job::new_queued_communities(job_id, request, started_at, max_bytes)
        } else {
            Job::new_running_communities(job_id, request, started_at, max_bytes)
        };
        if queued {
            slot.pending.push_back(PendingJob {
                job: Arc::clone(&job),
                job_guard: guard,
                sink: Arc::new(NoopProgressSink),
            });
        } else {
            // Publish the sole community-work phase before detaching the
            // worker, so immediate polling has a meaningful nonterminal
            // progress snapshot rather than a scheduling-dependent null.
            job.set_phase(JobPhase::DetectingCommunities);
            if let Some(previous) = slot.current.take() {
                archive_terminal(&mut slot, Arc::clone(&previous));
                slot.previous_terminal = Some(previous);
            }
            slot.current = Some(Arc::clone(&job));
            slot.current_completion_pending = true;
            spawn_supervised_job(
                Arc::clone(&inner),
                Arc::clone(&job),
                Arc::new(NoopProgressSink),
                guard,
            );
        }
        job
    };
    let status = if matches!(job.state.read().status, JobStatus::Queued) {
        "queued"
    } else {
        "running"
    };
    Ok(ToolOk::Value(AsyncKickoffResponse {
        job_id: job.job_id.clone(),
        status,
        started_at: format_unix_nanos_rfc3339(job.started_at),
        existing: false,
        note: if status == "queued" {
            "community detection queued — poll get_job_status(job_id) for progress and the terminal result; use get_status for FIFO position"
        } else {
            "community detection kicked off — poll get_job_status(job_id) for progress and the terminal result"
        },
    }))
}

fn queued_kickoff_response(job: &Job) -> AsyncKickoffResponse {
    AsyncKickoffResponse {
        job_id: job.job_id.clone(),
        status: "queued",
        started_at: format_unix_nanos_rfc3339(job.started_at),
        existing: false,
        note: "analyze queued — poll get_job_status(job_id) for progress and the terminal result; use get_status for FIFO position",
    }
}

enum Admission {
    Covered {
        job: Arc<Job>,
        /// Snapshot taken under the slot lock and job state lock. It is never
        /// a terminal state disguised as `running`.
        status: &'static str,
    },
    Running {
        job: Arc<Job>,
        job_guard: crate::server::JobGuard,
    },
    Queued {
        job: Arc<Job>,
        job_guard: crate::server::JobGuard,
    },
}

/// Admit an analyze request while the caller holds the slot write lock.
///
/// The coverage identity is deliberately computed only for canonical existing
/// directories with a successfully discovered config root. Invalid paths and
/// config errors retain no identity and proceed to their worker, preserving
/// established execution-time validation errors.
fn admit_job(
    inner: &ServerInner,
    slot: &mut JobSlot,
    path_raw: String,
    force: bool,
    coverage: Option<CoverageIdentity>,
) -> Result<Admission, ToolError> {
    // Every request participates in closed admission before it can be
    // coalesced. Covered requests drop this temporary guard below; only newly
    // admitted queued/running jobs retain one through their supervisor.
    let job_guard = inner
        .persist
        .begin_analyze()
        .map_err(|message| ToolError(message.to_string()))?;

    if let Some(coverage) = coverage.as_ref() {
        if let Some((job, status)) = covering_job(slot, coverage, force) {
            drop(job_guard);
            return Ok(Admission::Covered { job, status });
        }
    }

    if slot_is_occupied(slot) {
        if slot.pending.len() >= JOB_PENDING_LIMIT {
            return Err(queue_full_error());
        }
        let job = install_new_queued(slot, path_raw, force, coverage);
        Ok(Admission::Queued { job, job_guard })
    } else {
        let job = install_new_running(slot, path_raw, force, coverage);
        Ok(Admission::Running { job, job_guard })
    }
}

fn queue_full_error() -> ToolError {
    ToolError(format!(
        "job queue is full ({JOB_PENDING_LIMIT} pending jobs); wait for queued work to complete and retry, or poll get_status for FIFO diagnostics"
    ))
}

fn coverage_identity(path_raw: &str) -> Option<CoverageIdentity> {
    let path = paths::canonicalize(std::path::Path::new(path_raw)).ok()?;
    if !path.is_dir() {
        return None;
    }
    let (config, project_root, config_present) = RootConfig::load_with_presence(&path).ok()?;
    let config_identity = serde_json::to_string(&config).ok()?;
    Some(CoverageIdentity {
        invocation_path: path,
        project_root,
        config,
        config_identity,
        config_present,
    })
}

/// Run admission's filesystem snapshot on Tokio's blocking pool. The caller
/// deliberately holds only `admission_lock` while awaiting this task: that
/// preserves snapshot-to-FIFO ordering without stalling a runtime worker or
/// holding the slot/persistence locks across an await.
async fn probe_analyze_admission(
    inner: Arc<ServerInner>,
    path_raw: String,
) -> Result<Option<CoverageIdentity>, ToolError> {
    // Move the owned permit into the detached blocking closure rather than
    // retaining it in this MCP future. A cancelled request drops this future,
    // but the filesystem probe keeps the sole permit until it has returned.
    let permit = Arc::clone(&inner.admission_probe_permits)
        .acquire_owned()
        .await
        .map_err(|error| ToolError(format!("analyze admission probe semaphore closed: {error}")))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        // A bound daemon must reject a substituted root before coverage
        // canonicalization, including before a request can reuse a coverer.
        inner.ensure_daemon_root_current().map_err(ToolError)?;
        #[cfg(test)]
        pause_during_blocking_coverage_probe(&inner);
        Ok(coverage_identity(&path_raw))
    })
    .await
    .map_err(|error| ToolError(format!("analyze admission probe terminated: {error}")))?
}

#[cfg(test)]
fn pause_during_blocking_coverage_probe(inner: &ServerInner) {
    let hook = inner
        .admission_blocking_probe_hook
        .lock()
        .ok()
        .and_then(|mut hook| hook.take());
    if let Some(hook) = hook {
        let _ = hook.reached.send(());
        hook.proceed.wait();
    }
}

#[cfg(test)]
async fn pause_after_coverage_probe(inner: &ServerInner) {
    let hook = inner.admission_probe_hook.lock().await.take();
    if let Some(hook) = hook {
        let _ = hook.reached.send(());
        let _ = hook.proceed.await;
    }
}

/// Find a covering request and snapshot its nonterminal status. The current
/// job is checked first, then every pending job in FIFO order, under one slot
/// write lock. This follows promotion's slot-then-state lock order, while
/// terminal writers take only the state lock, so no lock cycle is possible.
fn covering_job(
    slot: &JobSlot,
    coverage: &CoverageIdentity,
    force: bool,
) -> Option<(Arc<Job>, &'static str)> {
    let mut candidates = slot
        .current
        .iter()
        .chain(slot.pending.iter().map(|pending| &pending.job));
    candidates.find_map(|job| {
        let state = job.state.read();
        let status = match &state.status {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Completed(_) | JobStatus::Failed(_) => return None,
        };
        let covers_request = job
            .coverage
            .as_ref()
            .is_some_and(|coverer| covers((coverer, job.force), (coverage, force)));
        if covers_request {
            Some((Arc::clone(job), status))
        } else {
            None
        }
    })
}

fn slot_is_occupied(slot: &JobSlot) -> bool {
    slot.current.is_some()
        && !(slot.pending.is_empty()
            && !slot.current_completion_pending
            && slot
                .current
                .as_ref()
                .is_some_and(|current| current.state.read().is_terminal()))
}

/// Install an immediately-running job. Caller holds the slot write guard.
fn install_new_running(
    slot: &mut JobSlot,
    path: String,
    force: bool,
    coverage: Option<CoverageIdentity>,
) -> Arc<Job> {
    let (job_id, started_at) = issue_job_id(slot);
    let job = Job::new_running_with_coverage(job_id, path, force, started_at, coverage);
    if let Some(prev) = slot.current.take() {
        archive_terminal(slot, Arc::clone(&prev));
        slot.previous_terminal = Some(prev);
    }
    slot.current = Some(Arc::clone(&job));
    slot.current_completion_pending = true;
    job
}

/// Install an admitted queued job. Caller holds the slot write guard.
fn install_new_queued(
    slot: &mut JobSlot,
    path: String,
    force: bool,
    coverage: Option<CoverageIdentity>,
) -> Arc<Job> {
    let (job_id, started_at) = issue_job_id(slot);
    Job::new_queued_with_coverage(job_id, path, force, started_at, coverage)
}

fn issue_job_id(slot: &mut JobSlot) -> (String, u64) {
    let issued = now_nanos_u64().max(slot.next_job_id);
    slot.next_job_id = issued.saturating_add(1);
    (format!("{issued:020}"), issued)
}

/// Detach a supervised worker. The supervisor, not any MCP handler, owns both
/// terminal recovery and promotion, so handler cancellation cannot orphan the
/// current slot entry.
fn spawn_supervised_job(
    inner: Arc<ServerInner>,
    job: Arc<Job>,
    sink: Arc<dyn ProgressSink>,
    job_guard: crate::server::JobGuard,
) {
    tokio::spawn(async move {
        let worker = tokio::spawn(run_job(Arc::clone(&inner), Arc::clone(&job), sink));
        if let Err(error) = worker.await {
            finish_failed_if_nonterminal(
                &job,
                format!("{} worker terminated: {error}", job_kind_name(job.kind)),
            );
        }
        #[cfg(test)]
        pause_before_completion_rotation(&inner).await;
        let successor = promote_pending(&inner, &job);
        drop(job_guard);
        if let Some(successor) = successor {
            spawn_supervised_job(inner, successor.job, successor.sink, successor.job_guard);
        }
    });
}

async fn run_job(inner: Arc<ServerInner>, job: Arc<Job>, sink: Arc<dyn ProgressSink>) {
    match job.kind {
        JobKind::Analyze => run_analyze_job(inner, job, sink).await,
        JobKind::DetectCommunities => run_detect_communities_job(inner, job).await,
    }
}

async fn run_detect_communities_job(inner: Arc<ServerInner>, job: Arc<Job>) {
    #[cfg(test)]
    {
        let should_panic = {
            let mut slot = inner.analyze_slot.write();
            std::mem::take(&mut slot.panic_next_community_job)
        };
        assert!(!should_panic, "test community worker panic");
    }
    let JobRequest::DetectCommunities {
        granularity,
        max_iterations,
        members_per_community,
        limit,
        offset,
    } = &job.request
    else {
        finish_failed(
            &job,
            "community job has invalid request payload".to_string(),
        );
        return;
    };
    job.set_phase(JobPhase::DetectingCommunities);
    let request = (
        granularity.clone(),
        *max_iterations,
        *members_per_community,
        *limit,
        *offset,
    );
    let worker_inner = Arc::clone(&inner);
    let worker_job = Arc::clone(&job);
    let result = tokio::task::spawn_blocking(move || {
        let mut finished_at = 0;
        let result = crate::core::structure::detect_communities_with_response_budget(
            &worker_inner.graph,
            true,
            request.0.as_deref(),
            request.1,
            request.2,
            request.3,
            request.4,
            |empty_response| {
                // Measure the actual generic fields while pessimistically
                // reserving the longest possible pagination continuation.
                // The real row list is intentionally not involved, avoiding
                // a row-budget/wrapper-size circular dependency.
                let mut response_for_measurement = empty_response.clone();
                response_for_measurement.page.truncated = true;
                response_for_measurement.page.next_offset = Some(u32::MAX);
                let nested_bytes = serde_json::to_string(&response_for_measurement)
                    .expect("community response must serialize")
                    .len();
                finished_at = now_nanos_u64();
                let wrapped_bytes = serde_json::to_string(&JobView::completed_community(
                    &worker_job,
                    response_for_measurement,
                    finished_at,
                ))
                .expect("community job view must serialize")
                .len();
                // `byte_budget_take` reserves ENVELOPE_OVERHEAD_BYTES from
                // its input before admitting rows. Give it a budget whose
                // remaining row allowance is exactly the outer JobView's
                // available space: `max_bytes - wrapped_empty`. This keeps
                // the nested response's real metadata (which need not fit
                // the generic envelope reserve) and all JobView fields in
                // the total. Saturation preserves the irreducible tiny-budget
                // start-fresh response when the wrapper alone cannot fit.
                let wrapper_overhead = wrapped_bytes.saturating_sub(nested_bytes);
                let fixed_overhead = wrapper_overhead.saturating_add(nested_bytes);
                worker_job
                    .max_bytes
                    .saturating_sub(fixed_overhead.saturating_sub(ENVELOPE_OVERHEAD_BYTES))
            },
        );
        (result, finished_at)
    })
    .await;
    match result {
        Ok((Ok(ToolOk::Value(response)), finished_at)) => {
            let mut state = job.state.write();
            state.progress = 1;
            state.progress_total = 1;
            state.progress_message = "Community detection complete".to_string();
            state.status = JobStatus::Completed(JobResult::DetectCommunities(response));
            state.finished_at = Some(finished_at);
            drop(state);
            job.terminal_changed.notify_waiters();
        }
        Ok((Ok(ToolOk::Text(_)), _)) => finish_failed(
            &job,
            "community detection returned unexpected text".to_string(),
        ),
        Ok((Err(error), _)) => finish_failed(&job, error.0),
        Err(error) => finish_failed(&job, format!("community worker terminated: {error}")),
    }
}

fn promote_pending(inner: &ServerInner, job: &Job) -> Option<PendingJob> {
    let mut slot = inner.analyze_slot.write();
    assert!(
        slot.current
            .as_ref()
            .is_some_and(|current| std::ptr::eq(Arc::as_ptr(current), job)),
        "only the current job's supervisor may promote the analyze FIFO"
    );
    slot.current_completion_pending = false;
    if slot.pending.is_empty() {
        return None;
    }
    let next = slot
        .pending
        .pop_front()
        .expect("non-empty pending queue must yield its FIFO head");
    let previous = slot
        .current
        .take()
        .expect("current job identity was checked");
    archive_terminal(&mut slot, Arc::clone(&previous));
    slot.previous_terminal = Some(previous);
    next.job.mark_running();
    slot.current = Some(Arc::clone(&next.job));
    slot.current_completion_pending = true;
    Some(next)
}

fn archive_terminal(slot: &mut JobSlot, job: Arc<Job>) {
    debug_assert!(job.state.read().is_terminal());
    slot.terminal_history.push_back(job);
    if slot.terminal_history.len() > TERMINAL_HISTORY_LIMIT {
        let _ = slot.terminal_history.pop_front();
    }
}

fn finish_failed_if_nonterminal(job: &Job, msg: String) {
    let mut state = job.state.write();
    if state.is_terminal() {
        return;
    }
    state.status = JobStatus::Failed(msg);
    state.finished_at = Some(now_nanos_u64());
    drop(state);
    job.terminal_changed.notify_waiters();
}

#[cfg(test)]
async fn pause_before_completion_rotation(inner: &ServerInner) {
    let hook = inner.analyze_slot.write().completion_hook.take();
    if let Some(hook) = hook {
        let _ = hook.reached.send(());
        let _ = hook.proceed.await;
    }
}

async fn wait_for_terminal(job: &Job) {
    loop {
        let notified = job.terminal_changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if job.state.read().is_terminal() {
            return;
        }
        notified.await;
    }
}

fn job_kind_name(kind: JobKind) -> &'static str {
    match kind {
        JobKind::Analyze => "indexing",
        JobKind::DetectCommunities => "community",
    }
}

/// Save the graph to `<dir>/.code-graph-cache.db`. Lifted to a helper so
/// the lock is held for the minimum span needed to serialize the cache —
/// a long save under the write lock would block all queries.
pub(crate) fn save_cache(inner: &ServerInner, dir: &std::path::Path) -> Result<(), String> {
    let _persist = inner.persist.begin_persist().map_err(str::to_owned)?;
    #[cfg(debug_assertions)]
    debug_write_persist_admitted_marker();
    #[cfg(debug_assertions)]
    debug_delay_persist(dir);
    let io_dir = inner.cache_io_root.read().clone();
    // A daemon without the retained procfd capability must not perform a
    // pathname-based save: validating the pathname and opening the cache are
    // separate operations, so root replacement could redirect the latter.
    // Direct servers are unbound and retain their existing logical-path save.
    #[cfg(target_os = "linux")]
    if io_dir.is_none() && inner.daemon_project_root.get().is_some() {
        return Err(
            "retained-root cache I/O is unavailable; refusing an unanchored daemon cache save"
                .to_string(),
        );
    }
    let io_dir = io_dir.unwrap_or_else(|| dir.to_path_buf());
    let g = inner.graph.read();
    let result = g.save(&io_dir).map_err(|error| error.to_string());
    #[cfg(debug_assertions)]
    if result.is_ok() {
        debug_write_persist_completion_marker();
    }
    result
}

#[cfg(debug_assertions)]
fn debug_delay_persist(dir: &std::path::Path) {
    let Ok(root) = std::env::var("CODE_GRAPH_TEST_PERSIST_DELAY_ROOT") else {
        return;
    };
    let Ok(delay_millis) = std::env::var("CODE_GRAPH_TEST_PERSIST_DELAY_MILLIS") else {
        return;
    };
    if std::path::Path::new(&root) == dir {
        if let Ok(delay_millis) = delay_millis.parse::<u64>() {
            std::thread::sleep(std::time::Duration::from_millis(delay_millis));
        }
    }
}

#[cfg(debug_assertions)]
fn debug_write_persist_admitted_marker() {
    if let Ok(marker) = std::env::var("CODE_GRAPH_TEST_PERSIST_ADMITTED_MARKER") {
        let _ = std::fs::write(marker, b"persist admitted\n");
    }
}

#[cfg(debug_assertions)]
fn debug_write_persist_completion_marker() {
    if let Ok(marker) = std::env::var("CODE_GRAPH_TEST_PERSIST_MARKER") {
        let _ = std::fs::write(marker, b"persist complete\n");
    }
}

/// Test-only synchronization point after config/root discovery but before an
/// analyze worker waits for `index_lock`. The root match keeps concurrent
/// test roots isolated; production release builds omit the marker entirely.
#[cfg(debug_assertions)]
fn debug_write_before_index_lock_marker(path: &std::path::Path) {
    let Ok(root) = std::env::var("CODE_GRAPH_TEST_ANALYZE_BEFORE_LOCK_ROOT") else {
        return;
    };
    let Ok(marker) = std::env::var("CODE_GRAPH_TEST_ANALYZE_BEFORE_LOCK_MARKER") else {
        return;
    };
    if std::path::Path::new(&root) == path {
        let _ = std::fs::write(marker, b"analyze reached index-lock wait\n");
    }
}

#[cfg(test)]
mod coalesce {
    use super::*;
    use code_graph_core::paths;
    use code_graph_lang::LanguageRegistry;

    fn server() -> crate::server::CodeGraphServer {
        crate::server::CodeGraphServer::new(LanguageRegistry::new())
    }

    fn graph_with_isolated_community_files(count: usize, path_padding: usize) -> Graph {
        let mut graph = Graph::new();
        for index in 0..count {
            graph.merge_file_graph(code_graph_core::FileGraph {
                path: format!("/communities/{index:02}/{}.cpp", "x".repeat(path_padding)),
                language: code_graph_core::Language::Cpp,
                symbols: Vec::new(),
                edges: Vec::new(),
            });
        }
        graph
    }

    mod admission_probe {
        use super::*;

        #[test]
        fn keeps_current_thread_runtime_responsive() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap();
            runtime.block_on(async {
                let fixture = tempfile::TempDir::new().unwrap();
                let server = server();
                let inner = Arc::clone(&server.inner);
                inner.analyze_slot.write().current = Some(Job::new_running(
                    "blocker".into(),
                    "/blocker".into(),
                    false,
                    0,
                ));

                let (probe_reached_tx, probe_reached_rx) = tokio::sync::oneshot::channel();
                let release_probe = Arc::new(std::sync::Barrier::new(2));
                *inner.admission_blocking_probe_hook.lock().unwrap() =
                    Some(crate::server::BlockingAdmissionProbeHook {
                        reached: probe_reached_tx,
                        proceed: Arc::clone(&release_probe),
                    });

                let path = fixture.path().to_string_lossy().into_owned();
                let kickoff_inner = Arc::clone(&inner);
                let kickoff = tokio::spawn(async move {
                    analyze_codebase_async(kickoff_inner, path, false).await
                });
                tokio::time::timeout(std::time::Duration::from_secs(1), probe_reached_rx)
                    .await
                    .expect("blocking probe must start")
                    .expect("blocking probe must signal its barrier");

                let (timer_tx, timer_rx) = tokio::sync::oneshot::channel();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    let _ = timer_tx.send(());
                });
                tokio::time::timeout(std::time::Duration::from_millis(250), timer_rx)
                    .await
                    .expect("independent timer must progress while filesystem probe blocks")
                    .expect("timer task must complete");

                release_probe.wait();
                let ToolOk::Value(kickoff) = kickoff.await.unwrap().unwrap() else {
                    panic!("analyze kickoff must return JSON")
                };
                assert_eq!(kickoff.status, "queued");
            });
        }

        #[tokio::test]
        async fn community_linearizes_before_analyze_blocked_in_filesystem_probe() {
            let fixture = tempfile::TempDir::new().unwrap();
            let server = server();
            let inner = Arc::clone(&server.inner);
            inner.indexed.store(true, Ordering::Release);

            let (probe_reached_tx, probe_reached_rx) = tokio::sync::oneshot::channel();
            let release_probe = Arc::new(std::sync::Barrier::new(2));
            *inner.admission_blocking_probe_hook.lock().unwrap() =
                Some(crate::server::BlockingAdmissionProbeHook {
                    reached: probe_reached_tx,
                    proceed: Arc::clone(&release_probe),
                });
            let (completion_reached_tx, completion_reached_rx) = tokio::sync::oneshot::channel();
            let (completion_proceed_tx, completion_proceed_rx) = tokio::sync::oneshot::channel();
            inner.analyze_slot.write().completion_hook = Some(crate::analyze_job::CompletionHook {
                reached: completion_reached_tx,
                proceed: completion_proceed_rx,
            });

            let analyze_inner = Arc::clone(&inner);
            let analyze_path = fixture.path().to_string_lossy().into_owned();
            let analyze = tokio::spawn(async move {
                analyze_codebase_async(analyze_inner, analyze_path, false).await
            });
            probe_reached_rx
                .await
                .expect("analyze probe must block before it can linearize");

            let ToolOk::Value(community) = tokio::time::timeout(
                std::time::Duration::from_millis(250),
                detect_communities_async(Arc::clone(&inner), None, None, None, None, None),
            )
            .await
            .expect("community kickoff must not wait for the blocked analyze probe")
            .unwrap() else {
                panic!("community kickoff must return JSON")
            };
            assert_eq!(community.status, "running");
            completion_reached_rx
                .await
                .expect("community worker must reach completion rotation");

            // Keep the completed community as the slot occupant until the
            // analyze has linearized, making the expected FIFO order explicit.
            release_probe.wait();
            let ToolOk::Value(analyze) = analyze.await.unwrap().unwrap() else {
                panic!("analyze kickoff must return JSON")
            };
            assert_eq!(analyze.status, "queued");
            let analyze_job = {
                let slot = inner.analyze_slot.read();
                assert_eq!(slot.current.as_ref().unwrap().job_id, community.job_id);
                assert_eq!(slot.pending.len(), 1);
                assert_eq!(slot.pending[0].job.job_id, analyze.job_id);
                Arc::clone(&slot.pending[0].job)
            };

            completion_proceed_tx
                .send(())
                .expect("community supervisor must be waiting to rotate");
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                wait_for_terminal(&analyze_job),
            )
            .await
            .expect("queued analyze must run after community rotation");
            assert!(analyze_job.state.read().is_terminal());
            assert!(inner.analyze_slot.read().pending.is_empty());
        }

        #[tokio::test(flavor = "current_thread")]
        async fn cancelled_probe_keeps_blocking_capacity_until_its_closure_returns() {
            let fixture = tempfile::TempDir::new().unwrap();
            let server = server();
            let inner = Arc::clone(&server.inner);
            inner.indexed.store(true, Ordering::Release);
            inner.analyze_slot.write().current = Some(Job::new_running(
                "blocker".into(),
                "/blocker".into(),
                false,
                0,
            ));

            let (first_reached_tx, first_reached_rx) = tokio::sync::oneshot::channel();
            let release_first = Arc::new(std::sync::Barrier::new(2));
            *inner.admission_blocking_probe_hook.lock().unwrap() =
                Some(crate::server::BlockingAdmissionProbeHook {
                    reached: first_reached_tx,
                    proceed: Arc::clone(&release_first),
                });

            let path = fixture.path().to_string_lossy().into_owned();
            let first = tokio::spawn(analyze_codebase_async(
                Arc::clone(&inner),
                path.clone(),
                false,
            ));
            first_reached_rx
                .await
                .expect("first filesystem probe must start");
            first.abort();
            assert!(first.await.is_err(), "first MCP future must be cancelled");

            let (second_reached_tx, mut second_reached_rx) = tokio::sync::oneshot::channel();
            let release_second = Arc::new(std::sync::Barrier::new(2));
            *inner.admission_blocking_probe_hook.lock().unwrap() =
                Some(crate::server::BlockingAdmissionProbeHook {
                    reached: second_reached_tx,
                    proceed: Arc::clone(&release_second),
                });
            let second = tokio::spawn(analyze_codebase_async(Arc::clone(&inner), path, false));

            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                loop {
                    if inner.admission_lock.try_lock().is_err() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("second request must await the occupied probe permit");
            assert!(matches!(
                second_reached_rx.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ));

            let (timer_tx, timer_rx) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                let _ = timer_tx.send(());
            });
            tokio::time::timeout(std::time::Duration::from_millis(250), timer_rx)
                .await
                .expect("Tokio scheduling must remain responsive")
                .expect("independent timer must complete");
            let ToolOk::Value(community) = tokio::time::timeout(
                std::time::Duration::from_millis(250),
                detect_communities_async(Arc::clone(&inner), None, None, None, None, None),
            )
            .await
            .expect("community kickoff must not wait for analyze probe capacity")
            .unwrap() else {
                panic!("community kickoff must return JSON")
            };
            assert_eq!(community.status, "queued");

            release_first.wait();
            second_reached_rx
                .await
                .expect("second probe must launch only after first closure releases");
            release_second.wait();
            let ToolOk::Value(second) = second.await.unwrap().unwrap() else {
                panic!("second analyze kickoff must return JSON")
            };
            assert_eq!(second.status, "queued");
        }
    }

    async fn terminal_community_view(
        inner: Arc<ServerInner>,
        kickoff: AsyncKickoffResponse,
    ) -> JobView {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let ToolOk::Value(view) =
                crate::core::status::get_job_status(Arc::clone(&inner), kickoff.job_id.clone())
                    .unwrap()
            else {
                panic!("community status must return JSON")
            };
            if view.status == "completed" || view.status == "failed" {
                return view;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "community job timed out"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn community_async_kickoff_poll_and_result_matches_sync_when_budget_does_not_bind() {
        let server = server();
        server.inner.indexed.store(true, Ordering::Release);
        let ToolOk::Value(sync) = crate::core::structure::detect_communities(
            &server.inner.graph,
            true,
            None,
            None,
            None,
            None,
            None,
            server.inner.config.read().response.max_bytes,
        )
        .unwrap() else {
            panic!("communities must return JSON")
        };
        let start = std::time::Instant::now();
        let ToolOk::Value(kickoff) =
            detect_communities_async(server.inner.clone(), None, None, None, None, None)
                .await
                .unwrap()
        else {
            panic!("kickoff must return JSON")
        };
        assert!(start.elapsed() < std::time::Duration::from_millis(100));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let view = loop {
            let ToolOk::Value(view) =
                crate::core::status::get_job_status(server.inner.clone(), kickoff.job_id.clone())
                    .unwrap()
            else {
                panic!("job status must return JSON")
            };
            if view.status == "completed" || view.status == "failed" {
                break view;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "community job timed out"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        };
        assert_eq!(view.kind, JobKind::DetectCommunities);
        assert_eq!(view.current_phase, Some(JobPhase::DetectingCommunities));
        let Some(JobResult::DetectCommunities(result)) = view.result else {
            panic!("expected completed community result: {view:?}")
        };
        assert_eq!(
            serde_json::to_string(&result).unwrap(),
            serde_json::to_string(&sync).unwrap(),
            "terminal result must match sync output when the budget does not bind"
        );
        let ToolOk::Value(status) = crate::core::status::get_status(server.inner.clone()).unwrap()
        else {
            panic!("status must return JSON")
        };
        assert_eq!(
            status.job.as_ref().map(|job| job.kind),
            Some(JobKind::DetectCommunities)
        );
        assert!(status.analyze_job.is_none());
    }

    #[tokio::test]
    async fn async_community_budget_boundary_search_keeps_every_feasible_terminal_within_limit() {
        let server = server();
        server.inner.indexed.store(true, Ordering::Release);
        *server.inner.graph.write() = graph_with_isolated_community_files(8, 80);
        let mut feasible_cases = 0;

        // Sweep tight budgets around the first-row boundary. Each feasible
        // result must fit as a standalone terminal JobView, not merely as its
        // nested DetectCommunitiesResponse.
        for max_bytes in (700..=1_400).step_by(5) {
            server.inner.config.write().response.max_bytes = max_bytes;
            let ToolOk::Value(kickoff) =
                detect_communities_async(server.inner.clone(), None, None, None, None, None)
                    .await
                    .unwrap()
            else {
                panic!("community kickoff must return JSON")
            };
            let view = terminal_community_view(server.inner.clone(), kickoff).await;
            let Some(JobResult::DetectCommunities(result)) = &view.result else {
                panic!("expected completed community result")
            };
            if !result.page.results.is_empty() {
                feasible_cases += 1;
                assert!(
                    serde_json::to_string(&view).unwrap().len() <= max_bytes,
                    "terminal JobView with rows must fit max_bytes={max_bytes}"
                );
            }
        }
        assert!(
            feasible_cases > 0,
            "boundary search must encounter at least one budget that admits a community row"
        );
    }

    #[tokio::test]
    async fn async_community_budget_tiny_limit_returns_only_resumable_envelope() {
        let server = server();
        server.inner.indexed.store(true, Ordering::Release);
        *server.inner.graph.write() = graph_with_isolated_community_files(2, 80);
        server.inner.config.write().response.max_bytes = 1;

        let ToolOk::Value(kickoff) =
            detect_communities_async(server.inner.clone(), None, None, None, None, None)
                .await
                .unwrap()
        else {
            panic!("community kickoff must return JSON")
        };
        let view = terminal_community_view(server.inner.clone(), kickoff).await;
        let serialized_view = serde_json::to_string(&view).unwrap();
        let Some(JobResult::DetectCommunities(result)) = view.result else {
            panic!("expected completed community result")
        };
        assert!(result.page.results.is_empty());
        assert!(result.page.truncated);
        assert_eq!(result.page.next_offset, Some(0));
        // The one-byte configuration is below the irreducible generic JobView
        // plus nested response envelope. The result contains no community row,
        // so that unavoidable wrapper is the only cap exceedance.
        assert!(serialized_view.len() > 1);
    }

    #[tokio::test]
    async fn async_community_budget_paging_recovers_every_community_once() {
        let server = server();
        server.inner.indexed.store(true, Ordering::Release);
        *server.inner.graph.write() = graph_with_isolated_community_files(8, 120);
        server.inner.config.write().response.max_bytes = 100_000;

        let expected = match crate::core::structure::detect_communities(
            &server.inner.graph,
            true,
            None,
            None,
            None,
            None,
            None,
            usize::MAX,
        )
        .unwrap()
        {
            ToolOk::Value(response) => response
                .page
                .results
                .into_iter()
                .map(|community| community.label)
                .collect::<Vec<_>>(),
            ToolOk::Text(_) => panic!("communities must return JSON"),
        };

        let mut offset = None;
        let mut actual = Vec::new();
        loop {
            let ToolOk::Value(kickoff) =
                detect_communities_async(server.inner.clone(), None, None, None, Some(2), offset)
                    .await
                    .unwrap()
            else {
                panic!("community kickoff must return JSON")
            };
            let view = terminal_community_view(server.inner.clone(), kickoff).await;
            let repeated = serde_json::to_string(&view).unwrap();
            let ToolOk::Value(polled_again) =
                crate::core::status::get_job_status(server.inner.clone(), view.job_id.clone())
                    .unwrap()
            else {
                panic!("community status must return JSON")
            };
            assert_eq!(repeated, serde_json::to_string(&polled_again).unwrap());
            let Some(JobResult::DetectCommunities(response)) = view.result else {
                panic!("expected completed community result")
            };
            assert!(response.page.total > response.page.limit);
            actual.extend(
                response
                    .page
                    .results
                    .into_iter()
                    .map(|community| community.label),
            );
            let Some(next_offset) = response.page.next_offset else {
                assert!(!response.page.truncated);
                break;
            };
            assert!(response.page.truncated);
            assert!(next_offset > response.page.offset);
            offset = Some(next_offset);
        }
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn community_progress_is_observable_while_running() {
        let server = server();
        server.inner.indexed.store(true, Ordering::Release);
        let ToolOk::Value(kickoff) =
            detect_communities_async(server.inner.clone(), None, None, None, None, None)
                .await
                .unwrap()
        else {
            panic!("kickoff must return JSON")
        };
        // The kickoff publishes this phase before it detaches the worker, so
        // this immediate poll is a deterministic nonterminal observation.
        let ToolOk::Value(view) =
            crate::core::status::get_job_status(server.inner.clone(), kickoff.job_id).unwrap()
        else {
            panic!("job status must return JSON")
        };
        assert_eq!(view.status, "running");
        assert_eq!(view.kind, JobKind::DetectCommunities);
        assert_eq!(view.current_phase, Some(JobPhase::DetectingCommunities));
        assert_eq!(view.progress_total, 1);
    }

    #[tokio::test]
    async fn unindexed_community_async_rejects_before_validation_or_admission() {
        let server = server();
        let before = {
            let slot = server.inner.analyze_slot.read();
            (
                slot.current.is_none(),
                slot.previous_terminal.is_none(),
                slot.pending.len(),
                slot.next_job_id,
            )
        };

        let result = detect_communities_async(
            server.inner.clone(),
            Some("not-file".to_string()),
            None,
            None,
            None,
            None,
        )
        .await;
        let Err(error) = result else {
            panic!("unindexed core entry must reject before argument validation");
        };
        assert_eq!(error.0, "no codebase indexed — call analyze_codebase first");

        let slot = server.inner.analyze_slot.read();
        assert_eq!(slot.current.is_none(), before.0);
        assert_eq!(slot.previous_terminal.is_none(), before.1);
        assert_eq!(slot.pending.len(), before.2);
        assert_eq!(slot.next_job_id, before.3);
    }

    #[tokio::test]
    async fn invalid_community_async_input_does_not_issue_job() {
        let server = server();
        server.inner.indexed.store(true, Ordering::Release);
        assert!(detect_communities_async(
            server.inner.clone(),
            Some("symbol".to_string()),
            None,
            None,
            None,
            None,
        )
        .await
        .is_err());
        let ToolOk::Value(status) = crate::core::status::get_status(server.inner.clone()).unwrap()
        else {
            panic!("status must return JSON")
        };
        assert!(status.job.is_none());
        assert_eq!(status.job_pending_count, 0);
    }

    #[test]
    fn mixed_kind_fifo_promotion_never_overtakes() {
        let server = server();
        let inner = Arc::clone(&server.inner);
        let current = Job::new_running("current".into(), "/current".into(), false, 1);
        finish_failed(&current, "done".into());
        let community = Job::new_queued_communities(
            "community".into(),
            JobRequest::DetectCommunities {
                granularity: None,
                max_iterations: None,
                members_per_community: None,
                limit: None,
                offset: None,
            },
            2,
            1024,
        );
        let analyze = Job::new_queued("analyze".into(), "/analyze".into(), false, 3);
        {
            let mut slot = inner.analyze_slot.write();
            slot.current = Some(Arc::clone(&current));
            slot.current_completion_pending = true;
            slot.pending.push_back(PendingJob {
                job: Arc::clone(&community),
                job_guard: inner.persist.begin_job().unwrap(),
                sink: Arc::new(NoopProgressSink),
            });
            slot.pending.push_back(PendingJob {
                job: Arc::clone(&analyze),
                job_guard: inner.persist.begin_job().unwrap(),
                sink: Arc::new(NoopProgressSink),
            });
        }

        let first = promote_pending(&inner, &current).expect("FIFO head must promote");
        assert!(Arc::ptr_eq(&first.job, &community));
        assert_eq!(inner.analyze_slot.read().pending[0].job.job_id, "analyze");
        finish_failed(&community, "done".into());
        let second = promote_pending(&inner, &community).expect("second FIFO head must promote");
        assert!(Arc::ptr_eq(&second.job, &analyze));
        drop(first.job_guard);
        drop(second.job_guard);
    }

    #[tokio::test]
    async fn panicking_community_job_fails_and_promotes_analyze_successor() {
        let server = server();
        server.inner.indexed.store(true, Ordering::Release);
        let inner = Arc::clone(&server.inner);
        inner.analyze_slot.write().panic_next_community_job = true;
        let held_index_lock = inner.index_lock.lock().await;
        let (completion_reached_tx, completion_reached_rx) = tokio::sync::oneshot::channel();
        let (completion_proceed_tx, completion_proceed_rx) = tokio::sync::oneshot::channel();
        inner.analyze_slot.write().completion_hook = Some(crate::analyze_job::CompletionHook {
            reached: completion_reached_tx,
            proceed: completion_proceed_rx,
        });
        let analyze_dir = tempfile::TempDir::new().unwrap();
        let ToolOk::Value(community) =
            detect_communities_async(Arc::clone(&inner), None, None, None, None, None)
                .await
                .unwrap()
        else {
            panic!("community kickoff must return JSON")
        };
        completion_reached_rx
            .await
            .expect("panicking community worker must pause before rotation");
        let ToolOk::Value(analyze) = analyze_codebase_async(
            Arc::clone(&inner),
            analyze_dir.path().to_string_lossy().into_owned(),
            false,
        )
        .await
        .unwrap() else {
            panic!("analyze kickoff must return JSON")
        };
        assert_eq!(analyze.status, "queued");
        completion_proceed_tx
            .send(())
            .expect("community supervisor must await rotation release");
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let ToolOk::Value(view) = crate::core::status::get_job_status(
                    Arc::clone(&inner),
                    community.job_id.clone(),
                )
                .unwrap() else {
                    panic!("community status must return JSON")
                };
                if view.status == "failed" {
                    assert!(view.error.unwrap().contains("community worker terminated"));
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("community panic must be surfaced by the generic supervisor");
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while inner
                .analyze_slot
                .read()
                .current
                .as_ref()
                .is_some_and(|current| current.job_id != analyze.job_id)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("community supervisor must promote its queued analyze successor");
        drop(held_index_lock);
    }

    fn job(id: &str, path: &std::path::Path, force: bool, queued: bool) -> Arc<Job> {
        let coverage = coverage_identity(&path.display().to_string());
        if queued {
            Job::new_queued_with_coverage(id.into(), path.display().to_string(), force, 0, coverage)
        } else {
            Job::new_running_with_coverage(
                id.into(),
                path.display().to_string(),
                force,
                0,
                coverage,
            )
        }
    }

    fn pending(inner: &ServerInner, job: Arc<Job>) -> PendingJob {
        PendingJob {
            job,
            job_guard: inner.persist.begin_job().unwrap(),
            sink: Arc::new(NoopProgressSink),
        }
    }

    fn fill_pending_to_limit(inner: &ServerInner, slot: &mut JobSlot) {
        for index in 0..JOB_PENDING_LIMIT {
            slot.pending.push_back(pending(
                inner,
                Job::new_queued(
                    format!("pending-{index}"),
                    format!("/pending-{index}"),
                    false,
                    index as u64,
                ),
            ));
        }
    }

    #[tokio::test]
    async fn pending_limit_rejects_distinct_jobs_but_keeps_covered_analyzes() {
        let root = tempfile::TempDir::new().unwrap();
        let blocker = root.path().join("blocker");
        let distinct = root.path().join("distinct");
        std::fs::create_dir(&blocker).unwrap();
        std::fs::create_dir(&distinct).unwrap();
        std::fs::write(root.path().join(".code-graph.toml"), "").unwrap();
        let server = server();
        server.inner.indexed.store(true, Ordering::Release);
        let current = job("current", &blocker, false, false);
        {
            let mut slot = server.inner.analyze_slot.write();
            slot.current = Some(Arc::clone(&current));
            slot.current_completion_pending = true;
            fill_pending_to_limit(&server.inner, &mut slot);
            let next_id_before = slot.next_job_id;

            let distinct_raw = distinct.to_string_lossy().into_owned();
            let Err(error) = admit_job(
                &server.inner,
                &mut slot,
                distinct_raw.clone(),
                false,
                coverage_identity(&distinct_raw),
            ) else {
                panic!("a 33rd distinct analyze must be rejected");
            };
            assert_eq!(
                error.0,
                "job queue is full (32 pending jobs); wait for queued work to complete and retry, or poll get_status for FIFO diagnostics"
            );
            assert_eq!(slot.pending.len(), JOB_PENDING_LIMIT);
            assert_eq!(slot.next_job_id, next_id_before);

            let blocker_raw = blocker.to_string_lossy().into_owned();
            let covered = admit_job(
                &server.inner,
                &mut slot,
                blocker_raw.clone(),
                false,
                coverage_identity(&blocker_raw),
            )
            .unwrap();
            assert!(
                matches!(covered, Admission::Covered { ref job, .. } if Arc::ptr_eq(job, &current))
            );
            assert_eq!(slot.pending.len(), JOB_PENDING_LIMIT);
            assert_eq!(slot.next_job_id, next_id_before);

            let Err(force_mismatch) = admit_job(
                &server.inner,
                &mut slot,
                blocker_raw.clone(),
                true,
                coverage_identity(&blocker_raw),
            ) else {
                panic!("a force-mismatched analyze must stay distinct and be rejected");
            };
            assert_eq!(force_mismatch.0, error.0);
            assert_eq!(slot.pending.len(), JOB_PENDING_LIMIT);
            assert_eq!(slot.next_job_id, next_id_before);

            std::fs::write(
                root.path().join(".code-graph.toml"),
                "[cpp]\nmacro_strip = [\"REPLACED_API\"]\n",
            )
            .unwrap();
            let Err(config_mismatch) = admit_job(
                &server.inner,
                &mut slot,
                blocker_raw.clone(),
                false,
                coverage_identity(&blocker_raw),
            ) else {
                panic!(
                    "a config-distinct analyze must stay distinct and be rejected at queue capacity"
                );
            };
            assert_eq!(config_mismatch.0, error.0);
            assert_eq!(slot.pending.len(), JOB_PENDING_LIMIT);
            assert_eq!(slot.next_job_id, next_id_before);
        }

        let Err(community_error) =
            detect_communities_async(Arc::clone(&server.inner), None, None, None, None, None).await
        else {
            panic!("a 33rd distinct community job must be rejected");
        };
        assert_eq!(
            community_error.0,
            "job queue is full (32 pending jobs); wait for queued work to complete and retry, or poll get_status for FIFO diagnostics"
        );
        assert_eq!(
            server.inner.analyze_slot.read().pending.len(),
            JOB_PENDING_LIMIT
        );
        assert_eq!(
            server.inner.analyze_slot.read().next_job_id,
            0,
            "rejected community work must not issue an ID"
        );

        finish_failed(&current, "done".into());
        let promoted = promote_pending(&server.inner, &current)
            .expect("promotion must free exactly one pending slot");
        drop(promoted.job_guard);
        let mut slot = server.inner.analyze_slot.write();
        assert_eq!(slot.pending.len(), JOB_PENDING_LIMIT - 1);
        let next_id_before = slot.next_job_id;
        let distinct_raw = distinct.to_string_lossy().into_owned();
        let admitted = admit_job(
            &server.inner,
            &mut slot,
            distinct_raw.clone(),
            false,
            coverage_identity(&distinct_raw),
        )
        .expect("one promoted entry must reopen one pending admission slot");
        let Admission::Queued { job, job_guard } = admitted else {
            panic!("distinct request must queue behind the promoted current job");
        };
        slot.pending.push_back(PendingJob {
            job,
            job_guard,
            sink: Arc::new(NoopProgressSink),
        });
        assert_eq!(slot.pending.len(), JOB_PENDING_LIMIT);
        assert!(slot.next_job_id > next_id_before);
    }

    #[tokio::test]
    async fn queue_full_rejection_does_not_extend_shutdown_drain() {
        let root = tempfile::TempDir::new().unwrap();
        let blocker = root.path().join("blocker");
        let distinct = root.path().join("distinct");
        std::fs::create_dir(&blocker).unwrap();
        std::fs::create_dir(&distinct).unwrap();
        std::fs::write(root.path().join(".code-graph.toml"), "").unwrap();
        let server = server();
        {
            let mut slot = server.inner.analyze_slot.write();
            slot.current = Some(job("current", &blocker, false, false));
            fill_pending_to_limit(&server.inner, &mut slot);
            let distinct_raw = distinct.to_string_lossy().into_owned();
            assert!(matches!(
                admit_job(
                    &server.inner,
                    &mut slot,
                    distinct_raw.clone(),
                    false,
                    coverage_identity(&distinct_raw),
                ),
                Err(ToolError(message)) if message.starts_with("job queue is full")
            ));
            slot.pending.clear();
        }
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            server.inner.persist.close_analyze_and_wait(),
        )
        .await
        .expect("a rejected request must not retain a shutdown-drain guard");
    }

    #[test]
    fn running_coverer_reuses_arc_without_pending_growth() {
        let root = tempfile::TempDir::new().unwrap();
        let child = root.path().join("child");
        std::fs::create_dir(&child).unwrap();
        std::fs::write(root.path().join(".code-graph.toml"), "").unwrap();
        let server = server();
        let coverer = job("current", root.path(), false, false);
        let mut slot = server.inner.analyze_slot.write();
        slot.current = Some(Arc::clone(&coverer));

        let admitted = admit_job(
            &server.inner,
            &mut slot,
            child.display().to_string(),
            false,
            coverage_identity(&child.display().to_string()),
        )
        .unwrap();
        assert!(
            matches!(admitted, Admission::Covered { ref job, status: "running" } if Arc::ptr_eq(job, &coverer))
        );
        assert!(slot.pending.is_empty());
        assert_eq!(
            slot.next_job_id, 0,
            "covered requests do not issue a new job"
        );
    }

    #[test]
    fn pending_coverer_including_non_head_reuses_arc_without_queue_growth() {
        let root = tempfile::TempDir::new().unwrap();
        let child = root.path().join("child");
        let other = root.path().join("other");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(root.path().join(".code-graph.toml"), "").unwrap();
        let server = server();
        let current = job("current", &other, false, false);
        let first = job("first", &other, false, true);
        let coverer = job("coverer", root.path(), false, true);
        let mut slot = server.inner.analyze_slot.write();
        slot.current = Some(current);
        slot.pending.push_back(pending(&server.inner, first));
        slot.pending
            .push_back(pending(&server.inner, Arc::clone(&coverer)));

        let admitted = admit_job(
            &server.inner,
            &mut slot,
            child.display().to_string(),
            false,
            coverage_identity(&child.display().to_string()),
        )
        .unwrap();
        assert!(
            matches!(admitted, Admission::Covered { ref job, status: "queued" } if Arc::ptr_eq(job, &coverer))
        );
        assert_eq!(slot.pending.len(), 2);
        assert_eq!(slot.pending[0].job.job_id, "first");
        assert_eq!(slot.pending[1].job.job_id, "coverer");
    }

    #[test]
    fn reverse_containment_and_disjoint_requests_do_not_coalesce() {
        let root = tempfile::TempDir::new().unwrap();
        let child = root.path().join("child");
        let disjoint = root.path().join("disjoint");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::create_dir_all(&disjoint).unwrap();
        let first_server = server();
        {
            let mut slot = first_server.inner.analyze_slot.write();
            slot.current = Some(job("current", &child, false, false));
            let raw = root.path().display().to_string();
            assert!(matches!(
                admit_job(
                    &first_server.inner,
                    &mut slot,
                    raw.clone(),
                    false,
                    coverage_identity(&raw)
                )
                .unwrap(),
                Admission::Queued { .. }
            ));
        }

        let server = server();
        let mut slot = server.inner.analyze_slot.write();
        slot.current = Some(job("current", &child, false, false));
        {
            let path = disjoint.as_path();
            let raw = path.display().to_string();
            assert!(matches!(
                admit_job(
                    &server.inner,
                    &mut slot,
                    raw.clone(),
                    false,
                    coverage_identity(&raw)
                )
                .unwrap(),
                Admission::Queued { .. }
            ));
        }
    }

    #[test]
    fn terminal_current_is_excluded_from_coalescing() {
        let root = tempfile::TempDir::new().unwrap();
        let server = server();
        let terminal = job("terminal", root.path(), false, false);
        finish_failed(&terminal, "done".into());
        let mut slot = server.inner.analyze_slot.write();
        slot.current = Some(Arc::clone(&terminal));
        let raw = root.path().display().to_string();

        let admitted = admit_job(
            &server.inner,
            &mut slot,
            raw.clone(),
            false,
            coverage_identity(&raw),
        )
        .unwrap();
        assert!(
            matches!(admitted, Admission::Running { ref job, .. } if !Arc::ptr_eq(job, &terminal))
        );
    }

    #[test]
    fn invalid_paths_do_not_coalesce() {
        let root = tempfile::TempDir::new().unwrap();
        let missing = root.path().join("missing");
        let server = server();
        let mut slot = server.inner.analyze_slot.write();
        slot.current = Some(job("current", root.path(), false, false));
        let raw = missing.display().to_string();
        assert!(coverage_identity(&raw).is_none());
        assert!(matches!(
            admit_job(&server.inner, &mut slot, raw, false, None).unwrap(),
            Admission::Queued { .. }
        ));
    }

    #[test]
    fn nested_config_request_is_queued_instead_of_coalescing_with_parent() {
        let root = tempfile::TempDir::new().unwrap();
        let child = root.path().join("child");
        std::fs::create_dir(&child).unwrap();
        std::fs::write(root.path().join(".code-graph.toml"), "").unwrap();
        std::fs::write(child.join(".code-graph.toml"), "").unwrap();
        let server = server();
        let parent = job("parent", root.path(), false, false);
        let mut slot = server.inner.analyze_slot.write();
        slot.current = Some(Arc::clone(&parent));

        let admitted = admit_job(
            &server.inner,
            &mut slot,
            child.display().to_string(),
            false,
            coverage_identity(&child.display().to_string()),
        )
        .unwrap();
        assert!(
            matches!(admitted, Admission::Queued { ref job, .. } if !Arc::ptr_eq(job, &parent)),
            "a child config shadows the parent project and must receive its own queued job"
        );
    }

    #[test]
    fn malformed_child_config_does_not_coalesce_with_parent() {
        let root = tempfile::TempDir::new().unwrap();
        let child = root.path().join("child");
        std::fs::create_dir(&child).unwrap();
        std::fs::write(root.path().join(".code-graph.toml"), "").unwrap();
        std::fs::write(
            child.join(".code-graph.toml"),
            "[response]\nmax_bytes = -1\n",
        )
        .unwrap();
        let server = server();
        let parent = job("parent", root.path(), false, false);
        let mut slot = server.inner.analyze_slot.write();
        slot.current = Some(Arc::clone(&parent));
        let child_raw = child.display().to_string();

        assert!(
            coverage_identity(&child_raw).is_none(),
            "invalid child config must not produce a coalescing identity"
        );
        assert!(matches!(
            admit_job(&server.inner, &mut slot, child_raw, false, None).unwrap(),
            Admission::Queued { .. }
        ));
    }

    /// A queued symlink request captures its target at admission. Retargeting
    /// the link before promotion must neither change the work it performs nor
    /// make a later request for the original target coalesce with work on the
    /// new target.
    #[cfg(unix)]
    #[tokio::test]
    async fn queued_symlink_executes_its_admission_target_after_retarget() {
        use code_graph_lang_cpp::CppParser;

        let root = tempfile::TempDir::new().unwrap();
        let target_a = root.path().join("target-a");
        let target_b = root.path().join("target-b");
        let link = root.path().join("queued-link");
        let blocker = root.path().join("blocker");
        std::fs::create_dir(&target_a).unwrap();
        std::fs::create_dir(&target_b).unwrap();
        std::fs::create_dir(&blocker).unwrap();
        std::fs::write(target_a.join("a.cpp"), b"void from_a() {}\n").unwrap();
        std::fs::write(target_b.join("b.cpp"), b"void from_b() {}\n").unwrap();
        std::os::unix::fs::symlink(&target_a, &link).unwrap();

        let mut registry = code_graph_lang::LanguageRegistry::new();
        registry
            .register(Box::new(CppParser::new().unwrap()))
            .unwrap();
        let server = crate::server::CodeGraphServer::new(registry);
        let inner = Arc::clone(&server.inner);
        let raw_link = link.to_string_lossy().into_owned();
        let admitted_target = coverage_identity(&raw_link).unwrap();
        let target_a_identity = paths::canonicalize(&target_a).unwrap();
        assert_eq!(admitted_target.invocation_path, target_a_identity);

        let (blocker_job, queued) = {
            let mut slot = inner.analyze_slot.write();
            // A non-covering current job is enough to make the symlink job
            // queue; the test drives that queued job directly below.
            let blocker_job = Job::new_running(
                "blocker".into(),
                blocker.to_string_lossy().into_owned(),
                false,
                0,
            );
            slot.current = Some(Arc::clone(&blocker_job));
            let admission =
                admit_job(&inner, &mut slot, raw_link, false, Some(admitted_target)).unwrap();
            let Admission::Queued { job, job_guard } = admission else {
                panic!("symlink request must queue behind the current job");
            };
            slot.pending.push_back(PendingJob {
                job: Arc::clone(&job),
                job_guard,
                sink: Arc::new(NoopProgressSink),
            });
            (blocker_job, job)
        };

        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&target_b, &link).unwrap();

        // A later request for A still coalesces with the queued job. This
        // proves the coverer's stable identity is A, not the retargeted link.
        {
            let mut slot = inner.analyze_slot.write();
            let admission = admit_job(
                &inner,
                &mut slot,
                target_a.to_string_lossy().into_owned(),
                false,
                coverage_identity(&target_a.to_string_lossy()),
            )
            .unwrap();
            assert!(
                matches!(admission, Admission::Covered { ref job, status: "queued" } if Arc::ptr_eq(job, &queued))
            );
        }

        finish_completed(
            &blocker_job,
            AnalyzeResult {
                files: 0,
                symbols: 0,
                edges: 0,
                root_path: blocker.to_string_lossy().into_owned(),
                warnings: Vec::new(),
                coalesced_by: None,
            },
        );
        let successor = promote_pending(&inner, &blocker_job)
            .expect("the queued symlink job must be promoted after the blocker completes");
        assert!(Arc::ptr_eq(&successor.job, &queued));
        run_analyze_job(
            Arc::clone(&inner),
            Arc::clone(&successor.job),
            Arc::new(NoopProgressSink),
        )
        .await;
        drop(successor.job_guard);

        let state = queued.state.read();
        let JobStatus::Completed(JobResult::Analyze(result)) = &state.status else {
            panic!("queued job must complete against its admission target");
        };
        assert_eq!(result.root_path, target_a_identity.to_string_lossy());
        drop(state);

        let a_source = paths::canonicalize(&target_a.join("a.cpp")).unwrap();
        let b_source = paths::canonicalize(&target_b.join("b.cpp")).unwrap();
        let graph = inner.graph.read();
        assert!(
            graph
                .file_symbols(&a_source)
                .iter()
                .any(|symbol| symbol.name == "from_a"),
            "the coalesced queued job must index target A"
        );
        assert!(
            graph.file_symbols(&b_source).is_empty(),
            "the retargeted symlink target B must not be indexed"
        );
    }

    mod config_identity {
        use super::*;
        use code_graph_lang_cpp::CppParser;

        fn server() -> crate::server::CodeGraphServer {
            let mut registry = code_graph_lang::LanguageRegistry::new();
            registry
                .register(Box::new(CppParser::new().unwrap()))
                .unwrap();
            crate::server::CodeGraphServer::new(registry)
        }

        fn queued_analyze(
            server: &crate::server::CodeGraphServer,
            path: &std::path::Path,
        ) -> Arc<Job> {
            let raw = path.to_string_lossy().into_owned();
            let coverage = coverage_identity(&raw).expect("fixture config must admit");
            let blocker = Job::new_running("blocker".into(), "/blocker".into(), false, 0);
            let mut slot = server.inner.analyze_slot.write();
            slot.current = Some(blocker);
            let Admission::Queued { job, job_guard } =
                admit_job(&server.inner, &mut slot, raw, true, Some(coverage)).unwrap()
            else {
                panic!("fixture analyze must queue behind blocker");
            };
            slot.pending.push_back(PendingJob {
                job: Arc::clone(&job),
                job_guard,
                sink: Arc::new(NoopProgressSink),
            });
            job
        }

        async fn run_queued(inner: &Arc<ServerInner>, job: &Arc<Job>) -> AnalyzeResult {
            job.mark_running();
            run_analyze_job(
                Arc::clone(inner),
                Arc::clone(job),
                Arc::new(NoopProgressSink),
            )
            .await;
            let state = job.state.read();
            let JobStatus::Completed(JobResult::Analyze(result)) = &state.status else {
                panic!("queued analyze must succeed");
            };
            result.clone()
        }

        #[tokio::test]
        async fn nested_config_created_while_queued_keeps_admitted_parent_boundary() {
            let temp = tempfile::TempDir::new().unwrap();
            let outer = paths::canonicalize(temp.path()).unwrap();
            let child = outer.join("child");
            std::fs::create_dir(&child).unwrap();
            std::fs::write(
                outer.join(".code-graph.toml"),
                "[cpp]\nmacro_strip = [\"OUTER_API\"]\n",
            )
            .unwrap();
            let source = child.join("subject.cpp");
            std::fs::write(&source, "class OUTER_API FromParent {};\n").unwrap();

            let server = server();
            let job = queued_analyze(&server, &child);
            std::fs::write(
                child.join(".code-graph.toml"),
                "[cpp]\nmacro_strip = [\"INNER_API\"]\n",
            )
            .unwrap();

            let result = run_queued(&server.inner, &job).await;
            assert_eq!(result.root_path, outer.to_string_lossy());
            assert!(code_graph_graph::cache_path(&outer).exists());
            assert!(!code_graph_graph::cache_path(&child).exists());
            assert!(server
                .inner
                .graph
                .read()
                .file_symbols(&source)
                .iter()
                .any(|symbol| symbol.name == "FromParent"));
        }

        #[test]
        fn same_root_config_provenance_change_prevents_coalescing() {
            let temp = tempfile::TempDir::new().unwrap();
            let root = paths::canonicalize(temp.path()).unwrap();
            let raw = root.to_string_lossy().into_owned();
            let without_file = coverage_identity(&raw).expect("default config identity");
            assert!(!without_file.config_present);

            std::fs::write(root.join(".code-graph.toml"), "").unwrap();
            let with_file = coverage_identity(&raw).expect("empty config identity");
            assert!(with_file.config_present);
            assert!(!covers((&without_file, false), (&with_file, false)));
            assert!(!covers((&with_file, false), (&without_file, false)));

            std::fs::remove_file(root.join(".code-graph.toml")).unwrap();
            let removed = coverage_identity(&raw).expect("restored default identity");
            assert!(!removed.config_present);
            assert!(covers((&without_file, false), (&removed, false)));
        }

        #[tokio::test]
        async fn nested_config_removed_while_queued_keeps_admitted_child_boundary() {
            let temp = tempfile::TempDir::new().unwrap();
            let outer = paths::canonicalize(temp.path()).unwrap();
            let child = outer.join("child");
            std::fs::create_dir(&child).unwrap();
            std::fs::write(outer.join(".code-graph.toml"), "[cpp]\nmacro_strip = []\n").unwrap();
            let child_config = child.join(".code-graph.toml");
            std::fs::write(&child_config, "[cpp]\nmacro_strip = [\"INNER_API\"]\n").unwrap();
            let source = child.join("subject.cpp");
            std::fs::write(&source, "class INNER_API FromChild {};\n").unwrap();

            let server = server();
            let job = queued_analyze(&server, &child);
            std::fs::remove_file(child_config).unwrap();

            // A request admitted after removal belongs to the outer project,
            // so it must not receive this queued child's eventual result.
            {
                let mut slot = server.inner.analyze_slot.write();
                let raw = child.to_string_lossy().into_owned();
                assert!(matches!(
                    admit_job(
                        &server.inner,
                        &mut slot,
                        raw.clone(),
                        true,
                        coverage_identity(&raw),
                    )
                    .unwrap(),
                    Admission::Queued { .. }
                ));
            }

            let result = run_queued(&server.inner, &job).await;
            assert_eq!(result.root_path, child.to_string_lossy());
            assert!(code_graph_graph::cache_path(&child).exists());
            assert!(!code_graph_graph::cache_path(&outer).exists());
            assert!(server
                .inner
                .graph
                .read()
                .file_symbols(&source)
                .iter()
                .any(|symbol| symbol.name == "FromChild"));
        }

        #[tokio::test]
        async fn nested_config_replaced_after_coverage_keeps_coverers_admitted_config() {
            let temp = tempfile::TempDir::new().unwrap();
            let outer = paths::canonicalize(temp.path()).unwrap();
            let child = outer.join("child");
            std::fs::create_dir(&child).unwrap();
            std::fs::write(outer.join(".code-graph.toml"), "[cpp]\nmacro_strip = []\n").unwrap();
            let child_config = child.join(".code-graph.toml");
            std::fs::write(&child_config, "[cpp]\nmacro_strip = [\"FIRST_API\"]\n").unwrap();
            let source = child.join("subject.cpp");
            std::fs::write(&source, "class FIRST_API FirstConfig {};\n").unwrap();

            let server = server();
            let job = queued_analyze(&server, &child);
            {
                let mut slot = server.inner.analyze_slot.write();
                let raw = child.to_string_lossy().into_owned();
                assert!(matches!(
                    admit_job(
                        &server.inner,
                        &mut slot,
                        raw.clone(),
                        true,
                        coverage_identity(&raw),
                    )
                    .unwrap(),
                    Admission::Covered { job: ref coverer, status: "queued" }
                        if Arc::ptr_eq(coverer, &job)
                ));
            }
            std::fs::write(&child_config, "[cpp]\nmacro_strip = [\"SECOND_API\"]\n").unwrap();

            // Same root but a different admitted effective config must not
            // coalesce into the queued FIRST_API job.
            {
                let mut slot = server.inner.analyze_slot.write();
                let raw = child.to_string_lossy().into_owned();
                assert!(matches!(
                    admit_job(
                        &server.inner,
                        &mut slot,
                        raw.clone(),
                        true,
                        coverage_identity(&raw),
                    )
                    .unwrap(),
                    Admission::Queued { .. }
                ));
            }

            let result = run_queued(&server.inner, &job).await;
            assert_eq!(result.root_path, child.to_string_lossy());
            assert!(code_graph_graph::cache_path(&child).exists());
            assert!(!code_graph_graph::cache_path(&outer).exists());
            assert!(server
                .inner
                .graph
                .read()
                .file_symbols(&source)
                .iter()
                .any(|symbol| symbol.name == "FirstConfig"));
        }

        #[tokio::test]
        async fn coverage_snapshot_and_fifo_admission_are_linearizable() {
            let temp = tempfile::TempDir::new().unwrap();
            let root = paths::canonicalize(temp.path()).unwrap();
            let config_path = root.join(".code-graph.toml");
            std::fs::write(&config_path, "[cpp]\nmacro_strip = [\"OLD_API\"]\n").unwrap();

            let server = server();
            let inner = Arc::clone(&server.inner);
            inner.analyze_slot.write().current = Some(Job::new_running(
                "blocker".into(),
                "/blocker".into(),
                false,
                0,
            ));
            let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
            let (proceed_tx, proceed_rx) = tokio::sync::oneshot::channel();
            *inner.admission_probe_hook.lock().await = Some(crate::server::AdmissionProbeHook {
                reached: reached_tx,
                proceed: proceed_rx,
            });

            let path = root.to_string_lossy().into_owned();
            let first_inner = Arc::clone(&inner);
            let first_path = path.clone();
            let first =
                tokio::spawn(
                    async move { analyze_codebase_async(first_inner, first_path, true).await },
                );
            reached_rx
                .await
                .expect("first request snapshots old config");

            std::fs::write(&config_path, "[cpp]\nmacro_strip = [\"NEW_API\"]\n").unwrap();
            let second_inner = Arc::clone(&inner);
            let second =
                tokio::spawn(async move { analyze_codebase_async(second_inner, path, true).await });
            tokio::task::yield_now().await;
            assert!(
                inner.analyze_slot.read().pending.is_empty(),
                "the newer snapshot must not enter FIFO while the older snapshot owns admission"
            );

            proceed_tx.send(()).unwrap();
            let ToolOk::Value(first) = first.await.unwrap().unwrap() else {
                panic!("first kickoff must return JSON")
            };
            let ToolOk::Value(second) = second.await.unwrap().unwrap() else {
                panic!("second kickoff must return JSON")
            };

            let slot = inner.analyze_slot.read();
            assert_eq!(slot.pending.len(), 2);
            assert_eq!(slot.pending[0].job.job_id, first.job_id);
            assert_eq!(slot.pending[1].job.job_id, second.job_id);
            let first_config = slot.pending[0]
                .job
                .coverage
                .as_ref()
                .unwrap()
                .config_identity
                .clone();
            let second_config = slot.pending[1]
                .job
                .coverage
                .as_ref()
                .unwrap()
                .config_identity
                .clone();
            assert_ne!(
                first_config, second_config,
                "the FIFO order must retain the old snapshot before the newer config snapshot"
            );
        }
    }

    mod config_path {
        use super::*;
        use code_graph_lang_cpp::CppParser;
        use std::sync::{mpsc, Barrier};
        use std::time::Duration;

        fn server() -> crate::server::CodeGraphServer {
            let mut registry = code_graph_lang::LanguageRegistry::new();
            registry
                .register(Box::new(CppParser::new().unwrap()))
                .unwrap();
            crate::server::CodeGraphServer::new(registry)
        }

        async fn apply_analyze(server: &crate::server::CodeGraphServer, root: &std::path::Path) {
            let job = Job::new_running(
                "status-config".into(),
                root.to_string_lossy().into_owned(),
                false,
                0,
            );
            run_analyze_job(
                Arc::clone(&server.inner),
                job.clone(),
                Arc::new(NoopProgressSink),
            )
            .await;
            assert!(
                matches!(
                    job.state.read().status,
                    JobStatus::Completed(JobResult::Analyze(_))
                ),
                "fixture analyze must succeed"
            );
        }

        fn status_config_path(server: &crate::server::CodeGraphServer) -> Option<String> {
            let ToolOk::Value(status) =
                crate::core::status::get_status(server.inner.clone()).expect("status must succeed")
            else {
                panic!("status must return a value")
            };
            status.config_path
        }

        #[tokio::test]
        async fn status_config_path_stays_none_when_created_after_successful_analyze() {
            let temp = tempfile::TempDir::new().unwrap();
            let root = paths::canonicalize(temp.path()).unwrap();
            std::fs::write(root.join("subject.cpp"), "void subject() {}\n").unwrap();
            let server = server();

            apply_analyze(&server, &root).await;
            assert_eq!(status_config_path(&server), None);

            std::fs::write(root.join(".code-graph.toml"), "[cpp]\nmacro_strip = []\n").unwrap();
            assert_eq!(
                status_config_path(&server),
                None,
                "a config created after indexing must not claim to have governed the active graph"
            );
        }

        #[tokio::test]
        async fn status_config_path_survives_removal_after_successful_analyze() {
            let temp = tempfile::TempDir::new().unwrap();
            let root = paths::canonicalize(temp.path()).unwrap();
            let config_path = root.join(".code-graph.toml");
            std::fs::write(root.join("subject.cpp"), "void subject() {}\n").unwrap();
            std::fs::write(&config_path, "[cpp]\nmacro_strip = []\n").unwrap();
            let server = server();

            apply_analyze(&server, &root).await;
            let expected = config_path.to_string_lossy().into_owned();
            assert_eq!(status_config_path(&server), Some(expected.clone()));

            std::fs::remove_file(&config_path).unwrap();
            assert_eq!(
                status_config_path(&server),
                Some(expected),
                "removing the applied config must not erase active-index provenance"
            );
        }

        /// The cache fast-path's publication guard must cover the gap between
        /// replacing the graph and writing the matching status metadata. The
        /// hook pauses exactly in that gap; a concurrent status read must
        /// remain blocked until release, then observe the complete snapshot.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn status_blocks_during_graph_publication_then_sees_complete_new_snapshot() {
            let temp = tempfile::TempDir::new().unwrap();
            let root = paths::canonicalize(temp.path()).unwrap();
            let source = root.join("subject.cpp");
            std::fs::write(&source, "void old_symbol() {}\n").unwrap();
            let server = server();
            apply_analyze(&server, &root).await;

            let config_path = root.join(".code-graph.toml");
            std::fs::write(&config_path, "[cpp]\nmacro_strip = []\n").unwrap();

            let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
            let proceed = Arc::new(Barrier::new(2));
            *server.inner.publication_hook.lock() = Some(crate::server::PublicationHook {
                reached: reached_tx,
                proceed: Arc::clone(&proceed),
            });

            let job = Job::new_running(
                "publication-consistency".into(),
                root.to_string_lossy().into_owned(),
                false,
                0,
            );
            let worker = tokio::spawn(run_analyze_job(
                Arc::clone(&server.inner),
                Arc::clone(&job),
                Arc::new(NoopProgressSink),
            ));
            reached_rx
                .await
                .expect("analyze must pause after graph replacement");

            let (status_tx, status_rx) = mpsc::channel();
            let status_inner = Arc::clone(&server.inner);
            let status_reader = std::thread::spawn(move || {
                status_tx
                    .send(crate::core::status::get_status(status_inner))
                    .expect("status receiver remains live");
            });
            assert!(
                status_rx.recv_timeout(Duration::from_millis(50)).is_err(),
                "get_status must wait for the in-progress publication"
            );

            proceed.wait();
            worker.await.expect("analyze worker must not panic");
            let status = status_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("get_status must complete after publication")
                .expect("get_status must succeed");
            status_reader.join().expect("status reader must not panic");
            let ToolOk::Value(status) = status else {
                panic!("status must return a value")
            };
            assert!(status.indexed);
            assert_eq!(status.index_symbols, 1);
            assert_eq!(
                status.config_path,
                Some(config_path.to_string_lossy().into_owned()),
                "status must pair the new graph with its applied config provenance"
            );
            assert!(
                matches!(
                    job.state.read().status,
                    JobStatus::Completed(JobResult::Analyze(_))
                ),
                "publication test analyze must complete"
            );
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::indexer::ProgressSink;
    use std::os::fd::AsRawFd;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct EnvVarGuard {
        name: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(name: &'static str, value: &std::path::Path) -> Self {
            let previous = std::env::var_os(name);
            std::env::set_var(name, value);
            Self { name, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = self.previous.take() {
                std::env::set_var(self.name, value);
            } else {
                std::env::remove_var(self.name);
            }
        }
    }

    struct PanicSink;

    impl ProgressSink for PanicSink {
        fn report(&self, _progress: u32, _total: u32, _message: &str) {
            panic!("test progress sink panic")
        }
    }

    #[tokio::test]
    async fn panicking_worker_is_failed_and_promotes_queued_successor() {
        use code_graph_lang_cpp::CppParser;

        let root = tempfile::TempDir::new().unwrap();
        let successor_root = tempfile::TempDir::new().unwrap();
        std::fs::write(root.path().join("a.cpp"), b"void f() {}\n").unwrap();
        std::fs::write(successor_root.path().join("b.cpp"), b"void g() {}\n").unwrap();
        let mut registry = code_graph_lang::LanguageRegistry::new();
        registry
            .register(Box::new(CppParser::new().unwrap()))
            .unwrap();
        let server = crate::server::CodeGraphServer::new(registry);
        let inner = Arc::clone(&server.inner);
        let path = root.path().to_string_lossy().into_owned();
        let held_index_lock = inner.index_lock.lock().await;

        let failed = tokio::spawn(analyze_codebase(
            Arc::clone(&inner),
            path.clone(),
            false,
            Arc::new(PanicSink),
        ));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            if inner.analyze_slot.read().current.is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "panicking job was not admitted before successor kickoff"
            );
            tokio::task::yield_now().await;
        }
        let successor = analyze_codebase_async(
            Arc::clone(&inner),
            successor_root.path().to_string_lossy().into_owned(),
            false,
        )
        .await
        .unwrap();
        let ToolOk::Value(successor) = successor else {
            panic!("async kickoff must return a response")
        };
        assert_eq!(successor.status, "queued");
        drop(held_index_lock);
        let failed = failed.await.unwrap();
        assert!(
            failed.is_err(),
            "supervisor must surface a panicked worker as Failed"
        );

        let successor_job = inner.analyze_slot.read().current.clone().unwrap();
        wait_for_terminal(&successor_job).await;
        let slot = inner.analyze_slot.read();
        assert_eq!(slot.current.as_ref().unwrap().job_id, successor.job_id);
        assert!(matches!(
            slot.current.as_ref().unwrap().state.read().status,
            JobStatus::Completed(_)
        ));
        assert!(matches!(
            slot.previous_terminal.as_ref().unwrap().state.read().status,
            JobStatus::Failed(_)
        ));
    }

    #[test]
    fn save_cache_uses_retained_root_io_after_visible_root_replacement() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "code-graph-cache-io-root-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        let retained_root = std::fs::File::open(&root).unwrap();
        let io_root = std::path::PathBuf::from(format!(
            "/proc/{}/fd/{}",
            std::process::id(),
            retained_root.as_raw_fd()
        ));
        let server = crate::server::CodeGraphServer::new(code_graph_lang::LanguageRegistry::new());
        *server.inner.cache_io_root.write() = Some(io_root);

        let relocated = root.with_extension("relocated");
        std::fs::rename(&root, &relocated).unwrap();
        std::fs::create_dir(&root).unwrap();
        save_cache(&server.inner, &root).unwrap();

        assert!(
            !code_graph_graph::cache_path(&root).exists(),
            "save never follows the replacement root path"
        );
        let mut loaded = Graph::new();
        assert!(
            loaded.load(&relocated).unwrap(),
            "cache written through retained I/O root is loadable at the original inode"
        );

        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(relocated).unwrap();
    }

    #[test]
    fn save_cache_without_alias_refuses_a_replaced_daemon_root() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "code-graph-cache-no-alias-root-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        let server = crate::server::CodeGraphServer::new(code_graph_lang::LanguageRegistry::new());
        server.bind_daemon_project_root(root.clone()).unwrap();
        let retained_root = std::fs::File::open(&root).unwrap();
        server
            .bind_daemon_retained_root(&retained_root.metadata().unwrap())
            .unwrap();

        let relocated = root.with_extension("relocated");
        std::fs::rename(&root, &relocated).unwrap();
        std::fs::create_dir(&root).unwrap();

        let error = save_cache(&server.inner, &root).unwrap_err();
        assert!(error.contains("retained-root cache I/O is unavailable"));
        assert!(
            !code_graph_graph::cache_path(&root).exists(),
            "an alias-less daemon must not save through a replacement pathname"
        );

        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(relocated).unwrap();
    }

    #[tokio::test]
    async fn replaced_daemon_root_is_rejected_before_analysis_filesystem_work() {
        use code_graph_lang_cpp::CppParser;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "code-graph-analyze-replaced-root-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("old.cpp"), b"void retained() {}\n").unwrap();
        let mut registry = code_graph_lang::LanguageRegistry::new();
        registry
            .register(Box::new(CppParser::new().unwrap()))
            .unwrap();
        let server = crate::server::CodeGraphServer::new(registry);
        server.bind_daemon_project_root(root.clone()).unwrap();
        let retained_root = std::fs::File::open(&root).unwrap();
        server
            .bind_daemon_retained_root(&retained_root.metadata().unwrap())
            .unwrap();

        let relocated = root.with_extension("relocated");
        std::fs::rename(&root, &relocated).unwrap();
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("replacement.cpp"), b"void replacement() {}\n").unwrap();

        let result = analyze_codebase(
            Arc::clone(&server.inner),
            root.to_string_lossy().into_owned(),
            true,
            Arc::new(NoopProgressSink),
        )
        .await;
        assert!(
            matches!(&result, Err(ToolError(message)) if message.contains("project root was replaced")),
            "replacement namespace must fail before indexing"
        );
        assert_eq!(server.inner.graph.read().stats().files, 0);
        assert!(
            !code_graph_graph::cache_path(&relocated).exists(),
            "replacement symbols must not be persisted into the retained root"
        );

        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(relocated).unwrap();
    }

    /// Admission must reject a substituted daemon root before coverage
    /// canonicalization can reuse a live job for the replacement pathname.
    #[tokio::test]
    async fn replaced_daemon_root_is_rejected_before_coverage_coalescing() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "code-graph-analyze-coverage-replaced-root-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        let server = crate::server::CodeGraphServer::new(code_graph_lang::LanguageRegistry::new());
        server.bind_daemon_project_root(root.clone()).unwrap();
        let retained_root = std::fs::File::open(&root).unwrap();
        server
            .bind_daemon_retained_root(&retained_root.metadata().unwrap())
            .unwrap();

        let coverage = coverage_identity(&root.to_string_lossy()).unwrap();
        let coverer = Job::new_running_with_coverage(
            "coverer".into(),
            root.to_string_lossy().into_owned(),
            false,
            0,
            Some(coverage),
        );
        server.inner.analyze_slot.write().current = Some(Arc::clone(&coverer));

        let relocated = root.with_extension("relocated");
        std::fs::rename(&root, &relocated).unwrap();
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("replacement.cpp"), b"void replacement() {}\n").unwrap();

        let path = root.to_string_lossy().into_owned();
        let sync = analyze_codebase(
            Arc::clone(&server.inner),
            path.clone(),
            false,
            Arc::new(NoopProgressSink),
        )
        .await;
        assert!(
            matches!(&sync, Err(ToolError(message)) if message.contains("project root was replaced")),
            "sync admission must reject before it can wait on the would-be coverer"
        );

        let asynchronous = analyze_codebase_async(Arc::clone(&server.inner), path, false).await;
        assert!(
            matches!(&asynchronous, Err(ToolError(message)) if message.contains("project root was replaced")),
            "async admission must reject before it can return the would-be coverer"
        );
        let slot = server.inner.analyze_slot.read();
        assert!(
            Arc::ptr_eq(slot.current.as_ref().unwrap(), &coverer),
            "rejected requests must not inspect coverage to replace or enqueue work"
        );
        assert!(slot.pending.is_empty());
        assert_eq!(slot.next_job_id, 0);

        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(relocated).unwrap();
    }

    #[tokio::test]
    async fn admitted_analyze_waiting_for_index_lock_cannot_publish_replacement_root_data() {
        use code_graph_lang_cpp::CppParser;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "code-graph-analyze-lock-replaced-root-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        let old_source = root.join("old.cpp");
        std::fs::write(&old_source, b"void retained() {}\n").unwrap();
        let mut registry = code_graph_lang::LanguageRegistry::new();
        registry
            .register(Box::new(CppParser::new().unwrap()))
            .unwrap();
        let server = crate::server::CodeGraphServer::new(registry);
        analyze_codebase(
            Arc::clone(&server.inner),
            root.to_string_lossy().into_owned(),
            true,
            Arc::new(NoopProgressSink),
        )
        .await
        .unwrap();
        let before = server.inner.graph.read().stats();
        server.bind_daemon_project_root(root.clone()).unwrap();
        let retained_root = std::fs::File::open(&root).unwrap();
        server
            .bind_daemon_retained_root(&retained_root.metadata().unwrap())
            .unwrap();

        let marker = root.join("before-index-lock.marker");
        let _root_env = EnvVarGuard::set("CODE_GRAPH_TEST_ANALYZE_BEFORE_LOCK_ROOT", &root);
        let _marker_env = EnvVarGuard::set("CODE_GRAPH_TEST_ANALYZE_BEFORE_LOCK_MARKER", &marker);
        let held_lock = server.inner.index_lock.lock().await;
        let inner = Arc::clone(&server.inner);
        let path = root.to_string_lossy().into_owned();
        let analyze = tokio::spawn(async move {
            analyze_codebase(inner, path, true, Arc::new(NoopProgressSink)).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !marker.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("analyze must finish config/root work before waiting on index_lock");

        let relocated = root.with_extension("relocated");
        std::fs::rename(&root, &relocated).unwrap();
        std::fs::create_dir(&root).unwrap();
        let replacement = root.join("replacement.cpp");
        std::fs::write(&replacement, b"void replacement() {}\n").unwrap();
        drop(held_lock);

        let result = analyze.await.unwrap();
        assert!(
            matches!(&result, Err(ToolError(message)) if message.contains("project root was replaced")),
            "the post-lock root check must fail the already-admitted job"
        );
        let after = server.inner.graph.read().stats();
        assert_eq!(after.files, before.files);
        assert_eq!(after.nodes, before.nodes);
        assert_eq!(after.edges, before.edges);
        assert!(server
            .inner
            .graph
            .read()
            .file_symbols(&replacement)
            .is_empty());
        let mut retained_cache = Graph::new();
        assert!(retained_cache.load(&relocated).unwrap());
        assert!(retained_cache.file_symbols(&replacement).is_empty());

        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(relocated).unwrap();
    }
}
