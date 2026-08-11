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

use crate::analyze_job::{AnalyzeJob, AnalyzePhase, JobStatus};
use crate::core::{ToolError, ToolOk, ToolResult};
use crate::handlers::analyze::{now_nanos_u64, AnalyzeResult, AsyncKickoffResponse};
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
}

/// `analyze_codebase` body.
///
/// Slot-protocol coordination only — the heavy lifting (cache fast-path,
/// parse pipeline, merge, persist) lives in [`run_analyze_job`]. The slot
/// is the single-flight gate (Design Decision 1); `index_lock` moves
/// into the worker.
///
/// Ungated by design (Decision 8 / plan task 2.2 notes): this is what
/// creates the index, so there is no core `require_indexed` call here.
pub async fn analyze_codebase(
    inner: Arc<ServerInner>,
    path_raw: String,
    force: bool,
    sink: Arc<dyn ProgressSink>,
) -> ToolResult<AnalyzeResult> {
    if path_raw.is_empty() {
        return Err(ToolError("'path' is required".to_string()));
    }
    let _analyze_guard = inner
        .persist
        .begin_analyze()
        .map_err(|message| ToolError(message.to_string()))?;

    let job = {
        let mut slot = inner.analyze_slot.write();
        if let Some(cur) = &slot.current {
            if matches!(cur.state.read().status, JobStatus::Running) {
                drop(slot);
                return Err(ToolError("indexing already in progress".to_string()));
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

    run_analyze_job(Arc::clone(&inner), Arc::clone(&job), sink).await;

    let state = job.state.read();
    match &state.status {
        JobStatus::Completed(result) => Ok(ToolOk::Value(result.clone())),
        JobStatus::Failed(msg) => Err(ToolError(msg.clone())),
        JobStatus::Running => {
            unreachable!("run_analyze_job must write a terminal JobStatus before returning")
        }
    }
}

/// `analyze_codebase_async` body — kickoff that returns in milliseconds
/// regardless of indexing duration.
///
/// Identical slot protocol to [`analyze_codebase`] (Design Decision 1)
/// except the worker is `tokio::spawn`ed and detached instead of
/// `await`ed inline, and a duplicate kickoff against a `Running` slot is
/// a SUCCESS (not an error — Design Decision 3) carrying the in-flight
/// job's `job_id` with `existing: true`.
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

    enum Kickoff {
        Existing { job_id: String, started_at: u64 },
        New(Arc<AnalyzeJob>, crate::server::AnalyzeGuard),
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
                let guard = inner
                    .persist
                    .begin_analyze()
                    .map_err(|message| ToolError(message.to_string()))?;
                let job = install_new_running(&mut slot, path_raw.clone(), force);
                Kickoff::New(job, guard)
            }
        } else {
            let guard = inner
                .persist
                .begin_analyze()
                .map_err(|message| ToolError(message.to_string()))?;
            let job = install_new_running(&mut slot, path_raw.clone(), force);
            Kickoff::New(job, guard)
        }
    };

    match kickoff {
        Kickoff::Existing { job_id, started_at } => Ok(ToolOk::Value(AsyncKickoffResponse {
            job_id,
            status: "running",
            started_at: format_unix_nanos_rfc3339(started_at),
            existing: true,
            note: "analyze already in progress — args ignored; poll get_status for progress",
        })),
        Kickoff::New(job, analyze_guard) => {
            let response = AsyncKickoffResponse {
                job_id: job.job_id.clone(),
                status: "running",
                started_at: format_unix_nanos_rfc3339(job.started_at),
                existing: false,
                note: "analyze kicked off — poll get_status for progress and the terminal result",
            };
            // Detach: the JoinHandle is dropped intentionally so the
            // worker outlives this call. Terminal state flows back
            // through `job.state`, observable via get_status.
            tokio::spawn(async move {
                let _analyze_guard = analyze_guard;
                run_analyze_job(
                    Arc::clone(&inner),
                    Arc::clone(&job),
                    Arc::new(NoopProgressSink),
                )
                .await;
            });
            Ok(ToolOk::Value(response))
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

/// Save the graph to `<dir>/.code-graph-cache.db`. Lifted to a helper so
/// the lock is held for the minimum span needed to serialize the cache —
/// a long save under the write lock would block all queries.
pub(crate) fn save_cache(inner: &ServerInner, dir: &std::path::Path) -> Result<(), String> {
    let _persist = inner.persist.begin_persist().map_err(str::to_owned)?;
    #[cfg(debug_assertions)]
    debug_delay_persist(dir);
    let g = inner.graph.read();
    let result = g.save(dir).map_err(|error| error.to_string());
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
fn debug_write_persist_completion_marker() {
    if let Ok(marker) = std::env::var("CODE_GRAPH_TEST_PERSIST_MARKER") {
        let _ = std::fs::write(marker, b"persist complete\n");
    }
}
