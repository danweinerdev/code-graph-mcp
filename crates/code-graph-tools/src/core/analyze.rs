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
    covers, AnalyzeJob, AnalyzePhase, AnalyzeSlot, CoverageIdentity, JobStatus, PendingAnalyze,
    TERMINAL_HISTORY_LIMIT,
};
use crate::core::{ToolError, ToolOk, ToolResult};
use crate::handlers::analyze::{
    now_nanos_u64, AnalyzeResult, AsyncKickoffResponse, SyncAnalyzeResponse,
};
use crate::handlers::status::format_unix_nanos_rfc3339;
use crate::indexer::{
    build_file_index, build_symbol_index, extend_file_index, extend_symbol_index, index_directory,
    resolve_edges_with_indexes, NoopProgressSink, ProgressSink,
};
use crate::server::ServerInner;

/// Wraps any [`ProgressSink`] so each `report()` ALSO writes the latest
/// progress triple into the owning [`AnalyzeJob`]'s mutable state.
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
    pub(crate) job: Arc<AnalyzeJob>,
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
    /// `AnalyzeJob::set_phase` to mutate job state (which sets the
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
    pub(crate) fn transition_to(&self, phase: AnalyzePhase) {
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
    job: Arc<AnalyzeJob>,
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
        | Err(e @ ConfigError::MacroStripConflict { .. })
        | Err(e @ ConfigError::MacroDefineTypeEmptyName)
        | Err(e @ ConfigError::MacroDefineTypeKeyword { .. }) => {
            finish_failed(&job, format!("invalid .code-graph.toml: {e}"));
            return;
        }
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
                let stats = probe.stats();
                if let Err(error) = inner.ensure_daemon_root_current() {
                    finish_failed(&job, error);
                    return;
                }
                {
                    let mut g = inner.graph.write();
                    *g = probe;
                }
                *inner.root_path.write() = Some(project_root.clone());
                *inner.cache_root.write() = Some(project_root.clone());
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
            sink.transition_to(AnalyzePhase::LoadingCache);
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
        // file-walk. Each `transition_to` ALSO pushes an event
        // through the inner sink, so a peer-forwarding sink observes
        // the phase boundary in addition to the per-file events.
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
                    let mut g = inner.graph.write();
                    *g = merged_graph;
                    g.stats()
                };

                *inner.root_path.write() = Some(project_root.clone());
                *inner.cache_root.write() = Some(project_root.clone());
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
                .transition_to(AnalyzePhase::Persisting);

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

/// Stamp terminal state under a single `state.write()` so an observer
/// (the sync handler reading after `await`, or polled `get_status`)
/// sees status+finished_at+phase consistently — never a half-written
/// transition.
///
/// Also stamps `current_phase = Completed`, `progress = 1/1`, and
/// message `"Analyze complete"` atomically with the status flip so a
/// polling client observing `current_phase == "completed"` can treat
/// the analyze as finished without separately consulting `status`.
pub(crate) fn finish_completed(job: &AnalyzeJob, result: AnalyzeResult) {
    let mut s = job.state.write();
    s.current_phase = Some(AnalyzePhase::Completed);
    s.progress = 1;
    s.progress_total = 1;
    s.progress_message = "Analyze complete".to_string();
    s.status = JobStatus::Completed(result);
    s.finished_at = Some(now_nanos_u64());
    drop(s);
    job.terminal_changed.notify_waiters();
}

pub(crate) fn finish_failed(job: &AnalyzeJob, msg: String) {
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
    // Coverage canonicalization performs filesystem lookups. A bound daemon
    // must reject a substituted root before doing that work, including before
    // a request can reuse an existing covering job.
    inner.ensure_daemon_root_current().map_err(ToolError)?;
    let coverage = coverage_identity(&path_raw);
    let admission = {
        let mut slot = inner.analyze_slot.write();
        // Decision 7 only lets a sync request escape the FIFO when another
        // request was already waiting before this admission. The first
        // distinct request behind a running worker keeps normal synchronous
        // semantics (including its real progress sink) and waits for itself.
        let pending_before_admission = !slot.pending.is_empty();
        match admit_job(&inner, &mut slot, path_raw, force, coverage)? {
            Admission::Covered { job, .. } => SyncAdmission::Covered(job),
            Admission::Queued { job, analyze_guard } => {
                slot.pending.push_back(PendingAnalyze {
                    job: Arc::clone(&job),
                    analyze_guard,
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
            Admission::Running { job, analyze_guard } => {
                spawn_supervised_job(Arc::clone(&inner), Arc::clone(&job), sink, analyze_guard);
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
        JobStatus::Completed(result) => {
            let mut result = result.clone();
            result.coalesced_by = coalesced_by;
            Ok(ToolOk::Value(SyncAnalyzeResponse::Result(result)))
        }
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
    Covered(Arc<AnalyzeJob>),
    Running(Arc<AnalyzeJob>),
    QueuedBlocking(Arc<AnalyzeJob>),
    QueuedImmediate(Arc<AnalyzeJob>),
}

/// `analyze_codebase_async` body — kickoff that returns in milliseconds
/// regardless of indexing duration.
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

    // Keep this before coverage canonicalization: it performs filesystem
    // lookups and could otherwise coalesce work in a replacement namespace.
    inner.ensure_daemon_root_current().map_err(ToolError)?;
    let coverage = coverage_identity(&path_raw);
    let kickoff = {
        let mut slot = inner.analyze_slot.write();
        match admit_job(&inner, &mut slot, path_raw, force, coverage)? {
            Admission::Covered { job, status } => (job, status, true),
            Admission::Queued { job, analyze_guard } => {
                slot.pending.push_back(PendingAnalyze {
                    job: Arc::clone(&job),
                    analyze_guard,
                    sink: Arc::new(NoopProgressSink),
                });
                (job, "queued", false)
            }
            Admission::Running { job, analyze_guard } => {
                spawn_supervised_job(
                    Arc::clone(&inner),
                    Arc::clone(&job),
                    Arc::new(NoopProgressSink),
                    analyze_guard,
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

fn queued_kickoff_response(job: &AnalyzeJob) -> AsyncKickoffResponse {
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
        job: Arc<AnalyzeJob>,
        /// Snapshot taken under the slot lock and job state lock. It is never
        /// a terminal state disguised as `running`.
        status: &'static str,
    },
    Running {
        job: Arc<AnalyzeJob>,
        analyze_guard: crate::server::AnalyzeGuard,
    },
    Queued {
        job: Arc<AnalyzeJob>,
        analyze_guard: crate::server::AnalyzeGuard,
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
    slot: &mut AnalyzeSlot,
    path_raw: String,
    force: bool,
    coverage: Option<CoverageIdentity>,
) -> Result<Admission, ToolError> {
    // Every request participates in closed admission before it can be
    // coalesced. Covered requests drop this temporary guard below; only newly
    // admitted queued/running jobs retain one through their supervisor.
    let analyze_guard = inner
        .persist
        .begin_analyze()
        .map_err(|message| ToolError(message.to_string()))?;

    if let Some(coverage) = coverage.as_ref() {
        if let Some((job, status)) = covering_job(slot, coverage, force) {
            drop(analyze_guard);
            return Ok(Admission::Covered { job, status });
        }
    }

    if slot_is_occupied(slot) {
        let job = install_new_queued(slot, path_raw, force, coverage);
        Ok(Admission::Queued { job, analyze_guard })
    } else {
        let job = install_new_running(slot, path_raw, force, coverage);
        Ok(Admission::Running { job, analyze_guard })
    }
}

fn coverage_identity(path_raw: &str) -> Option<CoverageIdentity> {
    let path = paths::canonicalize(std::path::Path::new(path_raw)).ok()?;
    if !path.is_dir() {
        return None;
    }
    let (_, project_root) = RootConfig::load(&path).ok()?;
    Some(CoverageIdentity {
        invocation_path: path,
        project_root,
    })
}

/// Find a covering request and snapshot its nonterminal status. The current
/// job is checked first, then every pending job in FIFO order, under one slot
/// write lock. This follows promotion's slot-then-state lock order, while
/// terminal writers take only the state lock, so no lock cycle is possible.
fn covering_job(
    slot: &AnalyzeSlot,
    coverage: &CoverageIdentity,
    force: bool,
) -> Option<(Arc<AnalyzeJob>, &'static str)> {
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

fn slot_is_occupied(slot: &AnalyzeSlot) -> bool {
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
    slot: &mut AnalyzeSlot,
    path: String,
    force: bool,
    coverage: Option<CoverageIdentity>,
) -> Arc<AnalyzeJob> {
    let (job_id, started_at) = issue_job_id(slot);
    let job = AnalyzeJob::new_running_with_coverage(job_id, path, force, started_at, coverage);
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
    slot: &mut AnalyzeSlot,
    path: String,
    force: bool,
    coverage: Option<CoverageIdentity>,
) -> Arc<AnalyzeJob> {
    let (job_id, started_at) = issue_job_id(slot);
    AnalyzeJob::new_queued_with_coverage(job_id, path, force, started_at, coverage)
}

fn issue_job_id(slot: &mut AnalyzeSlot) -> (String, u64) {
    let issued = now_nanos_u64().max(slot.next_job_id);
    slot.next_job_id = issued.saturating_add(1);
    (format!("{issued:020}"), issued)
}

/// Detach a supervised worker. The supervisor, not any MCP handler, owns both
/// terminal recovery and promotion, so handler cancellation cannot orphan the
/// current slot entry.
fn spawn_supervised_job(
    inner: Arc<ServerInner>,
    job: Arc<AnalyzeJob>,
    sink: Arc<dyn ProgressSink>,
    analyze_guard: crate::server::AnalyzeGuard,
) {
    tokio::spawn(async move {
        let worker = tokio::spawn(run_analyze_job(Arc::clone(&inner), Arc::clone(&job), sink));
        if let Err(error) = worker.await {
            finish_failed_if_nonterminal(&job, format!("indexing worker terminated: {error}"));
        }
        #[cfg(test)]
        pause_before_completion_rotation(&inner).await;
        let successor = promote_pending(&inner, &job);
        drop(analyze_guard);
        if let Some(successor) = successor {
            spawn_supervised_job(
                inner,
                successor.job,
                successor.sink,
                successor.analyze_guard,
            );
        }
    });
}

fn promote_pending(inner: &ServerInner, job: &AnalyzeJob) -> Option<PendingAnalyze> {
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

fn archive_terminal(slot: &mut AnalyzeSlot, job: Arc<AnalyzeJob>) {
    debug_assert!(job.state.read().is_terminal());
    slot.terminal_history.push_back(job);
    if slot.terminal_history.len() > TERMINAL_HISTORY_LIMIT {
        let _ = slot.terminal_history.pop_front();
    }
}

fn finish_failed_if_nonterminal(job: &AnalyzeJob, msg: String) {
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

async fn wait_for_terminal(job: &AnalyzeJob) {
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

    fn job(id: &str, path: &std::path::Path, force: bool, queued: bool) -> Arc<AnalyzeJob> {
        let coverage = coverage_identity(&path.display().to_string());
        if queued {
            AnalyzeJob::new_queued_with_coverage(
                id.into(),
                path.display().to_string(),
                force,
                0,
                coverage,
            )
        } else {
            AnalyzeJob::new_running_with_coverage(
                id.into(),
                path.display().to_string(),
                force,
                0,
                coverage,
            )
        }
    }

    fn pending(inner: &ServerInner, job: Arc<AnalyzeJob>) -> PendingAnalyze {
        PendingAnalyze {
            job,
            analyze_guard: inner.persist.begin_analyze().unwrap(),
            sink: Arc::new(NoopProgressSink),
        }
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
            "[response]\nmax_bytes = 0\n",
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
            let blocker_job = AnalyzeJob::new_running(
                "blocker".into(),
                blocker.to_string_lossy().into_owned(),
                false,
                0,
            );
            slot.current = Some(Arc::clone(&blocker_job));
            let admission =
                admit_job(&inner, &mut slot, raw_link, false, Some(admitted_target)).unwrap();
            let Admission::Queued { job, analyze_guard } = admission else {
                panic!("symlink request must queue behind the current job");
            };
            slot.pending.push_back(PendingAnalyze {
                job: Arc::clone(&job),
                analyze_guard,
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
        drop(successor.analyze_guard);

        let state = queued.state.read();
        let JobStatus::Completed(result) = &state.status else {
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
        let coverer = AnalyzeJob::new_running_with_coverage(
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
