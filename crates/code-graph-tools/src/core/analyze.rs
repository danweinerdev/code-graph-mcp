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

use code_graph_core::{paths, ConfigError, Language, RootConfig};
use code_graph_graph::Graph;

use crate::analyze_job::{
    allocate_job_timestamp, AnalyzeAdmission, AnalyzeJob, AnalyzePhase, AnalyzePreparation,
    JobStatus,
};
use crate::core::{ToolError, ToolOk, ToolResult};
use crate::handlers::analyze::now_nanos_u64;
use crate::handlers::status::format_unix_nanos_rfc3339;
use crate::indexer::{
    build_file_index, build_symbol_index, index_directory_with_extra, resolve_edges_with_indexes,
    NoopProgressSink, ProgressSink,
};
use crate::server::ServerInner;

pub use crate::handlers::analyze::{AnalyzeResult, AsyncKickoffResponse};

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

/// Delivers a promoted pending scan's progress to each compacted synchronous
/// request. It is created after `promote_next` releases the analyze-slot
/// lock, so sinks may safely re-enter slot-reading code while reporting.
struct FanoutProgressSink {
    sinks: Vec<Arc<dyn ProgressSink>>,
}

impl ProgressSink for FanoutProgressSink {
    fn report(&self, progress: u32, total: u32, message: &str) {
        for sink in &self.sinks {
            sink.report(progress, total, message);
        }
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
    #[cfg(test)]
    wait_for_worker_start_hook(&inner);
    #[cfg(test)]
    if take_injected_worker_panic(&job.path) {
        panic!("injected analyze worker panic");
    }

    let path_raw = job.path.clone();
    let force = job.force();

    // A bound Linux daemon keeps logical paths for all index work, but must
    // reject the operation before its first filesystem lookup if that logical
    // root has been substituted since startup.
    if let Err(error) = inner.ensure_daemon_root_current() {
        finish_failed(&job, error);
        return;
    }

    let (abs_path, mut cfg, project_root, applied_config_path) =
        if let Some(admission) = job.admission.clone() {
            match admission.preparation {
                Ok(preparation) => (
                    admission.path,
                    preparation.config,
                    preparation.project_root,
                    preparation.applied_config_path,
                ),
                Err(error) => {
                    finish_failed(&job, error);
                    return;
                }
            }
        } else {
            match probe_analyze_admission(&inner, &path_raw) {
                Ok(admission) => {
                    #[cfg(test)]
                    wait_for_config_discovery_hook(&inner);
                    match admission.preparation {
                        Ok(preparation) => (
                            admission.path,
                            preparation.config,
                            preparation.project_root,
                            preparation.applied_config_path,
                        ),
                        Err(error) => {
                            finish_failed(&job, error);
                            return;
                        }
                    }
                }
                Err(ToolError(error)) => {
                    finish_failed(&job, error);
                    return;
                }
            }
        };
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
        let needs_resolver_metadata_upgrade = load_ok
            && !probe
                .files_missing_resolver_metadata(Language::Go)
                .is_empty();
        if load_ok {
            let cached_resolver_metadata = probe.resolver_metadata_snapshot();
            for plugin in inner.registry.plugins() {
                plugin.restore_resolver_metadata(&cached_resolver_metadata);
            }
        }
        if load_ok && !needs_resolver_metadata_upgrade && probe.files_in_scope_count(&abs_path) > 0
        {
            let in_scope_stale: Vec<_> = all_stale
                .iter()
                .filter(|p| p.starts_with(&abs_path))
                .collect();
            // The staleness probe only sees files already IN the cache — a
            // file created since the cache was written, or an invocation
            // scope wider than the one that built the cache, is invisible
            // to it. Walk the scope (discovery only, no parse — seconds on
            // UE-scale trees, vs the minutes of parse the fast path exists
            // to skip) and fall through to the slow path when anything on
            // disk is missing from the cache. Found by dogfooding: a
            // root-scope analyze over a subtree-scoped cache returned the
            // subtree and never indexed the rest of the repo, and a file
            // added after a full index was silently never indexed.
            let scope_has_uncached_files = in_scope_stale.is_empty() && {
                let discovered = crate::discovery::discover(
                    &abs_path,
                    &inner.registry,
                    &cfg,
                    &crate::indexer::NoopProgressSink,
                );
                discovered
                    .files
                    .iter()
                    .any(|file| !probe.has_file(&file.path))
            };
            if in_scope_stale.is_empty() && !scope_has_uncached_files {
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
                    // `get_status` takes the matching read guard. Keep every
                    // status-facing part of a successful analyze publication
                    // inside this write section so it cannot combine this
                    // graph with metadata from a prior index.
                    let _publication = inner.status_publication.write();
                    {
                        let mut g = inner.graph.write();
                        *g = probe;
                    }
                    #[cfg(test)]
                    wait_for_publication_hook(&inner);
                    *inner.root_path.write() = Some(project_root.clone());
                    *inner.cache_root.write() = Some(project_root.clone());
                    *inner.config.write() = cfg.clone();
                    {
                        let mut applied = inner.applied_index.write();
                        applied.root_path = Some(project_root.clone());
                        applied.config_path = applied_config_path.clone();
                        applied.config = cfg;
                    }
                    inner.indexed.store(true, Ordering::Release);
                    inner
                        .index_built_at
                        .store(now_nanos_u64(), Ordering::Release);
                    inner.index_force_built.store(force, Ordering::Release);
                }
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

        // Hydrate plugin-owned resolver state from the same cached snapshot
        // that supplies out-of-scope symbols. This keeps scoped resolution
        // internally consistent after restart instead of combining cached
        // symbols with metadata reparsed from newer on-disk bytes.
        let cached_resolver_metadata = merged_graph.resolver_metadata_snapshot();
        let legacy_go_paths = merged_graph.files_missing_resolver_metadata(Language::Go);
        for plugin in registry.registry.plugins() {
            plugin.restore_resolver_metadata(&cached_resolver_metadata);
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
        // An explicit analyze is a disk-refresh boundary. In particular,
        // `go.mod` may have changed while watch mode was inactive without
        // changing the indexed `.go` path universe, so a path-keyed module
        // cache must not survive into this pass.
        for plugin in registry.registry.plugins() {
            plugin.invalidate_resolution_cache();
        }
        let mut migration_files = Vec::new();
        for path in legacy_go_paths {
            if path.is_file() {
                migration_files.push((path, Language::Go));
            } else {
                merged_graph.remove_file(&path);
            }
        }
        let (mut fresh_graphs, parse_warnings) = match index_directory_with_extra(
            &abs_path_for_pool,
            &migration_files,
            &registry.registry,
            &cfg_for_pool,
            &sink,
        ) {
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
        // Every resolver input sees one graph per path: cached sibling graphs
        // plus fresh graphs, with fresh replacing any stale same-path cache
        // entry. Only fresh graphs are subsequently resolved and merged.
        let fresh_paths: std::collections::HashSet<&str> = fresh_graphs
            .iter()
            .map(|graph| graph.path.as_str())
            .collect();
        let mut resolution_graphs: Vec<_> = cached_snapshot
            .into_iter()
            .filter(|graph| !fresh_paths.contains(graph.path.as_str()))
            .collect();
        resolution_graphs.extend(fresh_graphs.iter().cloned());
        let symbol_index = build_symbol_index(&resolution_graphs);
        let file_index = build_file_index(&resolution_graphs);

        resolve_edges_with_indexes(
            &mut fresh_graphs,
            &resolution_graphs,
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
        // Persist metadata for the complete resolver universe, not only fresh
        // files. This upgrades footerless v13 Go cache entries reconstructed
        // by the compatibility path during prepare_resolution.
        for fg in &resolution_graphs {
            let path = std::path::PathBuf::from(&fg.path);
            if let Some(metadata) = registry
                .registry
                .plugin_for(fg.language)
                .and_then(|plugin| plugin.resolver_metadata_for_path(&path))
            {
                merged_graph.set_resolver_metadata(path, metadata);
            }
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
                    // `get_status` takes the matching read guard. Keep every
                    // status-facing part of a successful analyze publication
                    // inside this write section so it cannot combine this
                    // graph with metadata from a prior index.
                    let _publication = inner.status_publication.write();
                    let stats = {
                        let mut g = inner.graph.write();
                        *g = merged_graph;
                        g.stats()
                    };
                    #[cfg(test)]
                    wait_for_publication_hook(&inner);
                    *inner.root_path.write() = Some(project_root.clone());
                    *inner.cache_root.write() = Some(project_root.clone());
                    *inner.config.write() = cfg.clone();
                    {
                        let mut applied = inner.applied_index.write();
                        applied.root_path = Some(project_root.clone());
                        applied.config_path = applied_config_path.clone();
                        applied.config = cfg;
                    }
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
    drop(s);
    job.notify_terminal();
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
    job.notify_terminal();
}

/// Pause a test worker after graph replacement while it still owns the
/// publication lock. This makes the no-tear status invariant deterministic
/// without adding a production synchronization point.
#[cfg(test)]
fn wait_for_publication_hook(inner: &ServerInner) {
    if let Some(hook) = inner.publication_hook.lock().take() {
        let _ = hook.reached.send(());
        hook.proceed.wait();
    }
}

/// Pause after discovery has selected config provenance. This makes it
/// possible to prove a subsequent config create/remove cannot alter what is
/// eventually published to `get_status`.
#[cfg(test)]
fn wait_for_config_discovery_hook(inner: &ServerInner) {
    if let Some(hook) = inner.config_discovery_hook.lock().take() {
        let _ = hook.reached.send(());
        hook.proceed.wait();
    }
}

/// Pause after an async admission probe has completed its filesystem work.
/// The caller reaches this only from `spawn_blocking`, so tests can prove an
/// unrelated task still runs on a single-worker Tokio runtime.
#[cfg(test)]
fn wait_for_admission_probe_hook(inner: &ServerInner) {
    if let Some(hook) = inner.admission_probe_hook.lock().take() {
        let _ = hook.reached.send(());
        hook.proceed.wait();
    }
}

/// Pause at worker entry so panic-supervision tests can enqueue followers
/// before the deliberately panicking worker reaches its test-only trigger.
#[cfg(test)]
fn wait_for_worker_start_hook(inner: &ServerInner) {
    if let Some(hook) = inner.worker_start_hook.lock().take() {
        let _ = hook.reached.send(());
        hook.proceed.wait();
    }
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
    // Keep synchronous and asynchronous requests on the same admission path.
    // In particular, pending-work compaction must see the request's directory
    // validation and discovered project root before deciding whether it can
    // attach to another request. This probe may read config files, so keep it
    // off the Tokio worker just as the async kickoff does.
    let probe_inner = Arc::clone(&inner);
    let admission =
        tokio::task::spawn_blocking(move || probe_analyze_admission(&probe_inner, &path_raw))
            .await
            .map_err(|error| {
                ToolError(format!("sync analyze admission probe panicked: {error}"))
            })??;
    let admission = admit_sync_with_admission(&inner, admission, force, Arc::clone(&sink))?;

    let job = match admission {
        SyncAdmission::RunNow { job, guard } => {
            spawn_analyze_worker(Arc::clone(&inner), Arc::clone(&job), sink, guard);
            AnalyzeJob::wait_for_terminal(job).await
        }
        SyncAdmission::Follower(job) => AnalyzeJob::wait_for_terminal(job).await,
    };

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
/// `await`ed inline. Arrivals while work is active compact only against
/// pending requests and return immediately.
///
/// No progress sink parameter — async kickoff has no client-side
/// progress channel; agents observe progress by polling `get_analyze_status`.
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

    // Canonicalization, directory validation, daemon-root validation, and
    // config/project discovery all stat or read from the filesystem. Awaiting
    // the probe keeps the admission result synchronous to the caller without
    // parking a Tokio runtime worker on a slow network mount.
    let probe_inner = Arc::clone(&inner);
    let admission =
        tokio::task::spawn_blocking(move || probe_analyze_admission(&probe_inner, &path_raw))
            .await
            .map_err(|error| {
                ToolError(format!("async analyze admission probe panicked: {error}"))
            })??;
    let kickoff = admit_async_with_admission(&inner, admission, force)?;

    match kickoff {
        Kickoff::Pending { job_id, started_at } => Ok(ToolOk::Value(AsyncKickoffResponse {
            job_id,
            status: "running",
            started_at: format_unix_nanos_rfc3339(started_at),
            existing: false,
            note: "analyze queued behind active work — poll get_analyze_status for progress and the terminal result",
        })),
        Kickoff::New(job, analyze_guard) => {
            let response = AsyncKickoffResponse {
                job_id: job.job_id.clone(),
                status: "running",
                started_at: format_unix_nanos_rfc3339(job.started_at),
                existing: false,
                note: "analyze kicked off — poll get_analyze_status for progress and the terminal result",
            };
            // Detach: the JoinHandle is dropped intentionally so the
            // worker outlives this call. Terminal state flows back
            // through `job.state`, observable via get_status.
            spawn_analyze_worker(inner, job, Arc::new(NoopProgressSink), analyze_guard);
            Ok(ToolOk::Value(response))
        }
    }
}

const MAX_PENDING_ANALYZES: usize = 32;

enum SyncAdmission {
    RunNow {
        job: Arc<AnalyzeJob>,
        guard: crate::server::AnalyzeGuard,
    },
    Follower(Arc<AnalyzeJob>),
}

enum Kickoff {
    New(Arc<AnalyzeJob>, crate::server::AnalyzeGuard),
    Pending { job_id: String, started_at: u64 },
}

enum PendingAdmission {
    Canonical(Arc<AnalyzeJob>),
    Attached(Arc<AnalyzeJob>),
}

/// Perform all filesystem-bound checks needed before a request enters the
/// slot. The resulting admission is retained by both synchronous and
/// asynchronous canonical jobs so [`compact_pending`] can require matching
/// successful project discovery before using path coverage.
fn probe_analyze_admission(
    inner: &ServerInner,
    path_raw: &str,
) -> Result<AnalyzeAdmission, ToolError> {
    inner.ensure_daemon_root_current().map_err(ToolError)?;
    let path = paths::canonicalize(std::path::Path::new(path_raw))
        .map_err(|_| ToolError(format!("directory does not exist: {path_raw}")))?;
    let preparation = (|| {
        if !path.is_dir() {
            return Err(ToolError(format!(
                "path is not a directory: {}",
                path.display()
            )));
        }
        if let Some(daemon_root) = inner.daemon_project_root.get() {
            if !path.starts_with(daemon_root) {
                return Err(ToolError(format!(
                    "daemon is bound to project root {}; cannot analyze path {} outside that root",
                    daemon_root.display(),
                    path.display()
                )));
            }
        }

        let (config, project_root, applied_config_path) =
            RootConfig::load_with_provenance(&path).map_err(config_error_to_tool_error)?;
        if let Some(daemon_root) = inner.daemon_project_root.get() {
            if daemon_root != &project_root {
                return Err(ToolError(format!(
                    "daemon is bound to project root {}; cannot analyze project root {}",
                    daemon_root.display(),
                    project_root.display()
                )));
            }
        }
        Ok(AnalyzePreparation {
            config,
            project_root,
            applied_config_path,
        })
    })()
    .map_err(|error: ToolError| error.0);
    #[cfg(test)]
    wait_for_admission_probe_hook(inner);
    Ok(AnalyzeAdmission { path, preparation })
}

fn config_error_to_tool_error(error: ConfigError) -> ToolError {
    match error {
        ConfigError::Toml(error) => ToolError(format!("failed to parse .code-graph.toml: {error}")),
        ConfigError::Io(error) => ToolError(format!("failed to read .code-graph.toml: {error}")),
        error @ ConfigError::ExtensionMissingDot { .. }
        | error @ ConfigError::ExtensionConflict { .. }
        | error @ ConfigError::MacroStripConflict { .. }
        | error @ ConfigError::MacroDefineTypeEmptyName
        | error @ ConfigError::MacroDefineTypeKeyword { .. } => {
            ToolError(format!("invalid .code-graph.toml: {error}"))
        }
    }
}

fn is_ancestor_or_equal(ancestor: &str, descendant: &str) -> bool {
    std::path::Path::new(descendant).starts_with(std::path::Path::new(ancestor))
}

fn next_job(path: String, force: bool, admission: Option<AnalyzeAdmission>) -> Arc<AnalyzeJob> {
    let started_at = allocate_job_timestamp();
    AnalyzeJob::new_running_with_admission(
        format!("{started_at:020}"),
        path,
        force,
        started_at,
        admission,
    )
}

/// Allocate the opaque handle for an async follower without creating another
/// queued scan. The handle is retained in `AnalyzeSlot::aliases` and resolves
/// to the pending canonical job that will satisfy this request.
fn next_async_alias_id() -> (String, u64) {
    let started_at = allocate_job_timestamp();
    (format!("{started_at:020}"), started_at)
}

/// Add a request to pending work. `current` is deliberately absent from this
/// function: compaction never observes or mutates active work.
fn compact_pending(
    slot: &mut crate::analyze_job::AnalyzeSlot,
    path: String,
    force: bool,
    admission: AnalyzeAdmission,
    guard: crate::server::AnalyzeGuard,
    sync_sink: Option<Arc<dyn ProgressSink>>,
) -> Result<PendingAdmission, ToolError> {
    // Capacity is by every live request, before compaction. A covered
    // follower is still a pending request and cannot bypass the 32-request
    // bound merely because it needs no new scan.
    if slot.pending_request_count() == MAX_PENDING_ANALYZES {
        return Err(ToolError(
            "analyze queue is full; retry after a pending analyze completes".to_string(),
        ));
    }

    if let Some(index) = slot.pending.iter().position(|entry| {
        admissions_share_project_root(entry.job.admission.as_ref(), &admission)
            && is_ancestor_or_equal(&entry.job.path, &path)
    }) {
        let entry = &mut slot.pending[index];
        entry.job.or_force(force);
        entry.request_count += 1;
        if let Some(sink) = sync_sink {
            entry.sync_sinks.push(sink);
        }
        return Ok(PendingAdmission::Attached(Arc::clone(&entry.job)));
    }

    let descendants: Vec<usize> = slot
        .pending
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            (admissions_share_project_root(entry.job.admission.as_ref(), &admission)
                && is_ancestor_or_equal(&path, &entry.job.path))
            .then_some(index)
        })
        .collect();
    let job = next_job(path, force, Some(admission));
    if descendants.is_empty() {
        slot.pending.push_back(crate::analyze_job::PendingAnalyze {
            job: Arc::clone(&job),
            guard,
            request_count: 1,
            sync_sinks: sync_sink.into_iter().collect(),
        });
        return Ok(PendingAdmission::Canonical(job));
    }

    let first = descendants[0];
    let mut removed = Vec::with_capacity(descendants.len());
    for index in descendants.into_iter().rev() {
        removed.push(
            slot.pending
                .remove(index)
                .expect("pending index came from queue"),
        );
    }
    let mut request_count = 1;
    let mut sync_sinks: Vec<Arc<dyn ProgressSink>> = sync_sink.into_iter().collect();
    for displaced in removed {
        request_count += displaced.request_count;
        job.or_force(displaced.job.force());
        sync_sinks.extend(displaced.sync_sinks);
        displaced.job.replace_with(Arc::clone(&job));
        slot.aliases
            .insert(displaced.job.job_id.clone(), job.job_id.clone());
        for alias in slot.aliases.values_mut() {
            if alias == &displaced.job.job_id {
                *alias = job.job_id.clone();
            }
        }
        // Dropping the displaced guard is correct: the replacement's guard
        // now accounts for this compacted canonical work.
    }
    slot.pending.insert(
        first,
        crate::analyze_job::PendingAnalyze {
            job: Arc::clone(&job),
            guard,
            request_count,
            sync_sinks,
        },
    );
    Ok(PendingAdmission::Canonical(job))
}

fn admissions_share_project_root(
    pending: Option<&AnalyzeAdmission>,
    incoming: &AnalyzeAdmission,
) -> bool {
    matches!(
        (pending, &incoming.preparation),
        (
            Some(AnalyzeAdmission {
                preparation: Ok(pending),
                ..
            }),
            Ok(incoming),
        ) if pending.project_root == incoming.project_root
    )
}

fn admit_sync_with_admission(
    inner: &Arc<ServerInner>,
    admission: AnalyzeAdmission,
    force: bool,
    sink: Arc<dyn ProgressSink>,
) -> Result<SyncAdmission, ToolError> {
    let path = admission.path.to_string_lossy().into_owned();
    let mut slot = inner.analyze_slot.write();
    if slot
        .current
        .as_ref()
        .is_some_and(|job| matches!(job.state.read().status, JobStatus::Running))
        || !slot.pending.is_empty()
    {
        let guard = inner
            .persist
            .begin_analyze()
            .map_err(|message| ToolError(message.to_string()))?;
        let pending = compact_pending(&mut slot, path, force, admission, guard, Some(sink))?;
        #[cfg(debug_assertions)]
        debug_record_pending_admission();
        let job = match pending {
            PendingAdmission::Canonical(job) | PendingAdmission::Attached(job) => job,
        };
        return Ok(SyncAdmission::Follower(job));
    }
    let guard = inner
        .persist
        .begin_analyze()
        .map_err(|message| ToolError(message.to_string()))?;
    let job = install_new_running(&mut slot, path, force, Some(admission));
    Ok(SyncAdmission::RunNow { job, guard })
}

fn admit_async_with_admission(
    inner: &Arc<ServerInner>,
    admission: AnalyzeAdmission,
    force: bool,
) -> Result<Kickoff, ToolError> {
    let path = admission.path.to_string_lossy().into_owned();
    let mut slot = inner.analyze_slot.write();
    if slot
        .current
        .as_ref()
        .is_some_and(|job| matches!(job.state.read().status, JobStatus::Running))
        || !slot.pending.is_empty()
    {
        let guard = inner
            .persist
            .begin_analyze()
            .map_err(|message| ToolError(message.to_string()))?;
        let pending = compact_pending(&mut slot, path, force, admission, guard, None)?;
        #[cfg(debug_assertions)]
        debug_record_pending_admission();
        return match pending {
            PendingAdmission::Canonical(job) => Ok(Kickoff::Pending {
                job_id: job.job_id.clone(),
                started_at: job.started_at,
            }),
            PendingAdmission::Attached(target) => {
                let (job_id, started_at) = next_async_alias_id();
                slot.aliases.insert(job_id.clone(), target.job_id.clone());
                Ok(Kickoff::Pending { job_id, started_at })
            }
        };
    }
    let guard = inner
        .persist
        .begin_analyze()
        .map_err(|message| ToolError(message.to_string()))?;
    let job = install_new_running(&mut slot, path, force, Some(admission));
    Ok(Kickoff::New(job, guard))
}

/// In-memory queue tests exercise admission directly, without creating a
/// filesystem fixture. Production async requests always use
/// [`admit_async_with_admission`] after the blocking probe.
#[cfg(test)]
fn admit_async(inner: &Arc<ServerInner>, path: String, force: bool) -> Result<Kickoff, ToolError> {
    let path_buf = std::path::PathBuf::from(&path);
    admit_async_with_admission(
        inner,
        AnalyzeAdmission {
            path: path_buf,
            preparation: Ok(AnalyzePreparation {
                config: RootConfig::default(),
                project_root: std::path::PathBuf::from("/"),
                applied_config_path: None,
            }),
        },
        force,
    )
}

fn spawn_analyze_worker(
    inner: Arc<ServerInner>,
    job: Arc<AnalyzeJob>,
    sink: Arc<dyn ProgressSink>,
    guard: crate::server::AnalyzeGuard,
) {
    tokio::spawn(async move {
        // Keep the pipeline in a child task so this detached supervisor gets
        // a JoinError instead of unwinding before it can terminalize the job,
        // release its daemon admission, and promote queued work.
        let worker = tokio::spawn(run_analyze_job(Arc::clone(&inner), Arc::clone(&job), sink));
        if let Err(join_error) = worker.await {
            finish_failed(&job, format!("analyze worker panicked: {join_error}"));
        }
        drop(guard);
        if let Some(next) = promote_next(&inner, &job) {
            spawn_pending_worker(inner, next);
        }
    });
}

/// Arm a one-shot, path-targeted panic at the start of an analyze pipeline.
/// Keeping the hook test-only makes the worker-supervision path deterministic
/// without adding a production failure mode.
#[cfg(test)]
fn inject_worker_panic(path: String) {
    let mut target = injected_worker_panic_target()
        .lock()
        .expect("injected worker-panic mutex must not be poisoned");
    *target = Some(path);
}

#[cfg(test)]
fn take_injected_worker_panic(path: &str) -> bool {
    let mut target = injected_worker_panic_target()
        .lock()
        .expect("injected worker-panic mutex must not be poisoned");
    if target.as_deref() == Some(path) {
        target.take();
        true
    } else {
        false
    }
}

#[cfg(test)]
fn injected_worker_panic_target() -> &'static std::sync::Mutex<Option<String>> {
    static TARGET: std::sync::OnceLock<std::sync::Mutex<Option<String>>> =
        std::sync::OnceLock::new();
    TARGET.get_or_init(|| std::sync::Mutex::new(None))
}

fn promote_next(
    inner: &ServerInner,
    completed: &Arc<AnalyzeJob>,
) -> Option<crate::analyze_job::PendingAnalyze> {
    let mut slot = inner.analyze_slot.write();
    if !slot
        .current
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, completed))
    {
        return None;
    }
    let next = slot.pending.pop_front()?;
    if let Some(expired) = slot.previous_terminal.take() {
        slot.discard_aliases_for(&expired.job_id);
    }
    slot.previous_terminal = slot.current.replace(Arc::clone(&next.job));
    Some(next)
}

fn spawn_pending_worker(inner: Arc<ServerInner>, pending: crate::analyze_job::PendingAnalyze) {
    // `promote_next` returns only after releasing the slot write guard. Build
    // the fan-out here so a blocking or re-entrant sink never reports while
    // that guard is held.
    let sink: Arc<dyn ProgressSink> = Arc::new(FanoutProgressSink {
        sinks: pending.sync_sinks,
    });
    spawn_analyze_worker(inner, pending.job, sink, pending.guard);
}

#[cfg(test)]
mod queue_tests {
    use super::*;
    use crate::analyze_job::AnalyzeSlot;
    use code_graph_lang::LanguageRegistry;

    fn server() -> crate::server::CodeGraphServer {
        crate::server::CodeGraphServer::new(LanguageRegistry::new())
    }

    fn pending_paths(slot: &AnalyzeSlot) -> Vec<String> {
        slot.pending
            .iter()
            .map(|pending| pending.job.path.clone())
            .collect()
    }

    fn queue(server: &crate::server::CodeGraphServer, path: &str, force: bool) -> Arc<AnalyzeJob> {
        queue_admission(server, test_admission(path), force)
    }

    fn queue_admission(
        server: &crate::server::CodeGraphServer,
        admission: AnalyzeAdmission,
        force: bool,
    ) -> Arc<AnalyzeJob> {
        let path = admission.path.to_string_lossy().into_owned();
        let guard = server.inner.persist.begin_analyze().unwrap();
        compact_pending(
            &mut server.inner.analyze_slot.write(),
            path,
            force,
            admission,
            guard,
            None,
        )
        .map(|admission| match admission {
            PendingAdmission::Canonical(job) | PendingAdmission::Attached(job) => job,
        })
        .unwrap()
    }

    fn test_admission(path: &str) -> AnalyzeAdmission {
        AnalyzeAdmission {
            path: std::path::PathBuf::from(path),
            preparation: Ok(AnalyzePreparation {
                config: RootConfig::default(),
                project_root: std::path::PathBuf::from("/"),
                applied_config_path: None,
            }),
        }
    }

    fn admit_sync(
        inner: &Arc<ServerInner>,
        path: String,
        force: bool,
        sink: Arc<dyn ProgressSink>,
    ) -> Result<SyncAdmission, ToolError> {
        admit_sync_with_admission(inner, test_admission(&path), force, sink)
    }

    struct ReentrantSink {
        name: &'static str,
        inner: Arc<ServerInner>,
        events: std::sync::mpsc::Sender<(&'static str, bool, String)>,
        block_once: std::sync::Mutex<Option<Arc<std::sync::Barrier>>>,
    }

    impl ProgressSink for ReentrantSink {
        fn report(&self, _progress: u32, _total: u32, message: &str) {
            // A promoted sink must run after `promote_next` drops its slot
            // write guard. Re-entering the slot is the deadlock regression.
            let slot_unlocked = self.inner.analyze_slot.try_read().is_some();
            self.events
                .send((self.name, slot_unlocked, message.to_string()))
                .expect("test event receiver remains live");
            if let Some(barrier) = self.block_once.lock().unwrap().take() {
                barrier.wait();
            }
        }
    }

    #[tokio::test]
    async fn promoted_sync_followers_fan_out_sinks_without_slot_lock_or_async_sink() {
        let server = server();
        let running = AnalyzeJob::new_running("running".into(), "/active".into(), false, 0);
        server.inner.analyze_slot.write().current = Some(Arc::clone(&running));
        let (events_tx, events_rx) = std::sync::mpsc::channel();
        let release_first = Arc::new(std::sync::Barrier::new(2));
        let first_sink: Arc<dyn ProgressSink> = Arc::new(ReentrantSink {
            name: "first",
            inner: Arc::clone(&server.inner),
            events: events_tx.clone(),
            block_once: std::sync::Mutex::new(Some(Arc::clone(&release_first))),
        });
        let second_sink: Arc<dyn ProgressSink> = Arc::new(ReentrantSink {
            name: "second",
            inner: Arc::clone(&server.inner),
            events: events_tx,
            block_once: std::sync::Mutex::new(None),
        });

        let first =
            match admit_sync(&server.inner, "/queued/child".into(), false, first_sink).unwrap() {
                SyncAdmission::Follower(job) => job,
                SyncAdmission::RunNow { .. } => panic!("running work must queue sync follower"),
            };
        let second = match admit_sync(
            &server.inner,
            "/queued/child/deeper".into(),
            false,
            second_sink,
        )
        .unwrap()
        {
            SyncAdmission::Follower(job) => job,
            SyncAdmission::RunNow { .. } => panic!("covered sync work must be a follower"),
        };
        assert!(Arc::ptr_eq(&first, &second));
        let alias = match admit_async(&server.inner, "/queued/child/async".into(), false).unwrap() {
            Kickoff::Pending { job_id, .. } => job_id,
            Kickoff::New(_, _) => panic!("async follower must remain pending"),
        };
        let replacement_id = match admit_async(&server.inner, "/queued".into(), false).unwrap() {
            Kickoff::Pending { job_id, .. } => job_id,
            Kickoff::New(_, _) => panic!("ancestor must replace queued descendant"),
        };

        finish_completed(&running, completed_result("/active"));
        let pending = promote_next(&server.inner, &running).expect("replacement must promote");
        assert_eq!(pending.job.job_id, replacement_id);
        assert_eq!(
            pending.sync_sinks.len(),
            2,
            "only the two synchronous followers contribute promoted sinks"
        );
        let promoted = Arc::clone(&pending.job);
        let sinks = pending.sync_sinks.clone();
        let reporter = std::thread::spawn(move || {
            let sink = JobAwareProgressSink {
                inner: FanoutProgressSink { sinks },
                job: promoted,
            };
            sink.transition_to(AnalyzePhase::Parsing);
            sink.report(1, 1, "Parsing: queued.cpp");
        });
        let first_event = events_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("first synchronous sink receives promoted phase event");
        assert_eq!(first_event.0, "first");
        assert!(
            first_event.1,
            "reporting must not hold the analyze-slot lock"
        );
        release_first.wait();
        let mut remaining = Vec::new();
        for _ in 0..3 {
            remaining.push(
                events_rx
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .expect("both synchronous sinks receive phase and progress events"),
            );
        }
        let all_events = std::iter::once(first_event)
            .chain(remaining)
            .collect::<Vec<_>>();
        for name in ["first", "second"] {
            let sink_events = all_events
                .iter()
                .filter(|event| event.0 == name)
                .collect::<Vec<_>>();
            assert_eq!(sink_events.len(), 2, "{name} must receive both events");
            assert!(
                sink_events.iter().all(|event| event.1),
                "{name} must be called without the analyze-slot lock"
            );
            assert!(
                sink_events
                    .iter()
                    .any(|event| event.2 == "Parsing source files"),
                "{name} must receive the promoted phase boundary"
            );
            assert!(
                sink_events
                    .iter()
                    .any(|event| event.2 == "Parsing: queued.cpp"),
                "{name} must receive promoted file progress"
            );
        }
        reporter.join().expect("promoted reporter must not panic");

        finish_completed(&pending.job, completed_result("/queued"));
        assert!(promote_next(&server.inner, &pending.job).is_none());
        let first_terminal = AnalyzeJob::wait_for_terminal(first).await;
        let second_terminal = AnalyzeJob::wait_for_terminal(second).await;
        assert!(Arc::ptr_eq(&first_terminal, &second_terminal));
        let alias_terminal = server
            .inner
            .analyze_slot
            .read()
            .resolve_async_job(&alias)
            .expect("async alias remains pollable but owns no sink");
        assert!(Arc::ptr_eq(&first_terminal, &alias_terminal));
        assert!(matches!(
            &alias_terminal.state.read().status,
            JobStatus::Completed(AnalyzeResult { root_path, .. }) if root_path == "/queued"
        ));
    }

    fn completed_result(root_path: &str) -> AnalyzeResult {
        AnalyzeResult {
            files: 1,
            symbols: 1,
            edges: 0,
            root_path: root_path.to_string(),
            warnings: Vec::new(),
        }
    }

    fn write_config(root: &std::path::Path) {
        std::fs::write(root.join(".code-graph.toml"), "[cpp]\nmacro_strip = []\n")
            .expect("write fixture config");
    }

    fn preparation_error(job: &AnalyzeJob) -> &str {
        job.admission
            .as_ref()
            .and_then(|admission| admission.preparation.as_ref().err())
            .expect("fixture job must retain its failed admission")
    }

    #[test]
    fn pending_file_children_keep_sync_and_async_failure_jobs() {
        let root = tempfile::tempdir().expect("create admission fixture");
        let parent_dir = root.path().join("parent");
        std::fs::create_dir(&parent_dir).expect("create parent directory");
        write_config(&parent_dir);
        let file = parent_dir.join("child.cpp");
        std::fs::write(&file, "void child() {}\n").expect("write child file");
        let server = server();

        let parent = probe_analyze_admission(&server.inner, &parent_dir.to_string_lossy())
            .expect("valid parent admission");
        let child = probe_analyze_admission(&server.inner, &file.to_string_lossy())
            .expect("file path still produces a retained admission");
        assert!(matches!(
            &child.preparation,
            Err(message) if message.contains("path is not a directory")
        ));
        let parent_job = queue_admission(&server, parent, false);

        let sync_job = match admit_sync_with_admission(
            &server.inner,
            child.clone(),
            false,
            Arc::new(NoopProgressSink),
        )
        .expect("file sync request must queue its own failed job")
        {
            SyncAdmission::Follower(job) => job,
            SyncAdmission::RunNow { .. } => {
                panic!("pending work must keep the sync request queued")
            }
        };
        let async_id = match admit_async_with_admission(&server.inner, child, false)
            .expect("file async request must retain kickoff semantics")
        {
            Kickoff::Pending { job_id, .. } => job_id,
            Kickoff::New(_, _) => panic!("pending work must keep the async request queued"),
        };
        let slot = server.inner.analyze_slot.read();
        let async_job = slot
            .resolve_async_job(&async_id)
            .expect("failed async request remains pollable through its canonical ID");
        assert_eq!(slot.pending.len(), 3);
        assert!(!Arc::ptr_eq(&parent_job, &sync_job));
        assert!(!Arc::ptr_eq(&parent_job, &async_job));
        assert!(preparation_error(&sync_job).contains("path is not a directory"));
        assert!(preparation_error(&async_job).contains("path is not a directory"));
    }

    #[test]
    fn pending_malformed_and_daemon_root_failures_keep_canonical_jobs() {
        let malformed_root = tempfile::tempdir().expect("create malformed-config fixture");
        let parent_dir = malformed_root.path().join("parent");
        let malformed_dir = parent_dir.join("malformed");
        std::fs::create_dir_all(&malformed_dir).expect("create malformed child directory");
        write_config(&parent_dir);
        std::fs::write(malformed_dir.join(".code-graph.toml"), "[cpp\n")
            .expect("write malformed config");
        let malformed_server = server();
        let parent =
            probe_analyze_admission(&malformed_server.inner, &parent_dir.to_string_lossy())
                .expect("valid parent admission");
        let malformed =
            probe_analyze_admission(&malformed_server.inner, &malformed_dir.to_string_lossy())
                .expect("malformed config remains a retained admission");
        let parent_job = queue_admission(&malformed_server, parent, false);
        let malformed_job =
            match admit_async_with_admission(&malformed_server.inner, malformed, false)
                .expect("malformed async request must retain kickoff semantics")
            {
                Kickoff::Pending { job_id, .. } => malformed_server
                    .inner
                    .analyze_slot
                    .read()
                    .resolve_async_job(&job_id)
                    .expect("malformed canonical job remains pending"),
                Kickoff::New(_, _) => panic!("pending parent must keep malformed request queued"),
            };
        assert!(!Arc::ptr_eq(&parent_job, &malformed_job));
        assert!(preparation_error(&malformed_job).contains("failed to parse .code-graph.toml"));

        let daemon_root = tempfile::tempdir().expect("create daemon-root fixture");
        let daemon_child = daemon_root.path().join("nested");
        std::fs::create_dir(&daemon_child).expect("create nested project");
        write_config(daemon_root.path());
        write_config(&daemon_child);
        let daemon_server = server();
        let daemon_project_root =
            paths::canonicalize(daemon_root.path()).expect("canonicalize root");
        daemon_server
            .bind_daemon_project_root(daemon_project_root)
            .expect("bind daemon project root once");
        let parent =
            probe_analyze_admission(&daemon_server.inner, &daemon_root.path().to_string_lossy())
                .expect("daemon parent remains valid");
        let child = probe_analyze_admission(&daemon_server.inner, &daemon_child.to_string_lossy())
            .expect("daemon-root mismatch remains a retained admission");
        let parent_job = queue_admission(&daemon_server, parent, false);
        let daemon_job = match admit_async_with_admission(&daemon_server.inner, child, false)
            .expect("daemon-root-invalid child must retain kickoff semantics")
        {
            Kickoff::Pending { job_id, .. } => daemon_server
                .inner
                .analyze_slot
                .read()
                .resolve_async_job(&job_id)
                .expect("daemon-root-invalid canonical job remains pending"),
            Kickoff::New(_, _) => panic!("pending parent must keep daemon-invalid request queued"),
        };
        assert!(!Arc::ptr_eq(&parent_job, &daemon_job));
        assert!(preparation_error(&daemon_job).contains("cannot analyze project root"));
    }

    #[test]
    fn pending_nested_project_does_not_compact() {
        let root = tempfile::tempdir().expect("create nested-project fixture");
        let child_dir = root.path().join("nested");
        std::fs::create_dir(&child_dir).expect("create nested project");
        write_config(root.path());
        write_config(&child_dir);
        let server = server();
        let parent = probe_analyze_admission(&server.inner, &root.path().to_string_lossy())
            .expect("parent project admission");
        let child = probe_analyze_admission(&server.inner, &child_dir.to_string_lossy())
            .expect("nested project admission");
        let parent_root = match &parent.preparation {
            Ok(preparation) => preparation.project_root.clone(),
            Err(error) => panic!("parent must prepare successfully: {error}"),
        };
        let child_root = match &child.preparation {
            Ok(preparation) => preparation.project_root.clone(),
            Err(error) => panic!("child must prepare successfully: {error}"),
        };
        assert_ne!(parent_root, child_root);
        let parent_job = queue_admission(&server, parent, false);
        let child_job = queue_admission(&server, child, false);
        assert!(!Arc::ptr_eq(&parent_job, &child_job));
        assert_eq!(server.inner.analyze_slot.read().pending.len(), 2);
    }

    #[test]
    fn pending_same_project_descendant_still_compacts() {
        let root = tempfile::tempdir().expect("create same-project fixture");
        let child_dir = root.path().join("child");
        std::fs::create_dir(&child_dir).expect("create child scope");
        write_config(root.path());
        let server = server();
        let parent = probe_analyze_admission(&server.inner, &root.path().to_string_lossy())
            .expect("parent project admission");
        let child = probe_analyze_admission(&server.inner, &child_dir.to_string_lossy())
            .expect("child scope admission");
        let parent_root = match &parent.preparation {
            Ok(preparation) => preparation.project_root.clone(),
            Err(error) => panic!("parent must prepare successfully: {error}"),
        };
        let child_root = match &child.preparation {
            Ok(preparation) => preparation.project_root.clone(),
            Err(error) => panic!("child must prepare successfully: {error}"),
        };
        assert_eq!(parent_root, child_root);
        let parent_job = queue_admission(&server, parent, false);
        let child_job = queue_admission(&server, child, true);
        assert!(Arc::ptr_eq(&parent_job, &child_job));
        assert!(parent_job.force(), "same-project follower force still ORs");
        assert_eq!(server.inner.analyze_slot.read().pending_request_count(), 2);
    }

    #[test]
    fn pending_compaction_absorbs_followers_or_force_and_preserves_disjoint_fifo() {
        let server = server();
        let a = queue(&server, "/queue/a", false);
        let _b = queue(&server, "/queue/b", false);
        let follower = queue(&server, "/queue/a/child", true);

        assert!(Arc::ptr_eq(&a, &follower));
        assert!(
            a.force(),
            "follower force must OR into pending canonical job"
        );
        assert_eq!(
            pending_paths(&server.inner.analyze_slot.read()),
            ["/queue/a", "/queue/b"]
        );
        assert_eq!(
            server.inner.analyze_slot.read().pending_request_count(),
            3,
            "canonical scans and covered followers each occupy a pending request slot"
        );
    }

    #[test]
    fn incoming_ancestor_replaces_descendants_at_earliest_fifo_position() {
        let server = server();
        let before = queue(&server, "/queue/before", false);
        let child = queue(&server, "/queue/root/child", false);
        let grandchild = queue(&server, "/queue/root/other", true);
        let after = queue(&server, "/queue/after", false);
        let replacement = queue(&server, "/queue/root", false);

        assert_eq!(
            pending_paths(&server.inner.analyze_slot.read()),
            ["/queue/before", "/queue/root", "/queue/after"]
        );
        assert!(
            replacement.force(),
            "displaced force must OR into replacement"
        );
        assert!(Arc::ptr_eq(
            child
                .state
                .read()
                .replacement
                .as_ref()
                .expect("displaced child must point at replacement"),
            &replacement
        ));
        assert!(grandchild.state.read().replacement.is_some());
        assert!(Arc::ptr_eq(&before, &queue_job(&server, 0)));
        assert!(Arc::ptr_eq(&after, &queue_job(&server, 2)));
    }

    fn queue_job(server: &crate::server::CodeGraphServer, index: usize) -> Arc<AnalyzeJob> {
        Arc::clone(&server.inner.analyze_slot.read().pending[index].job)
    }

    #[tokio::test]
    async fn followers_receive_terminal_success_and_failure() {
        let success = AnalyzeJob::new_running("success".into(), "/queue/success".into(), false, 0);
        let success_follower = Arc::clone(&success);
        finish_completed(
            &success,
            AnalyzeResult {
                files: 1,
                symbols: 2,
                edges: 3,
                root_path: "/queue/success".into(),
                warnings: Vec::new(),
            },
        );
        assert!(matches!(
            AnalyzeJob::wait_for_terminal(success_follower)
                .await
                .state
                .read()
                .status,
            JobStatus::Completed(_)
        ));

        let failed = AnalyzeJob::new_running("failed".into(), "/queue/failed".into(), false, 0);
        let failed_follower = Arc::clone(&failed);
        finish_failed(&failed, "expected failure".into());
        let failed_terminal = AnalyzeJob::wait_for_terminal(failed_follower).await;
        assert!(matches!(
            &failed_terminal.state.read().status,
            JobStatus::Failed(message) if message == "expected failure"
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn worker_panic_fails_active_promotes_successor_and_releases_drain() {
        use code_graph_lang_cpp::CppParser;

        let root = tempfile::tempdir().expect("create worker-panic fixture root");
        let active_dir = root.path().join("active");
        let successor_dir = root.path().join("successor");
        std::fs::create_dir_all(&active_dir).expect("create active fixture directory");
        std::fs::create_dir_all(successor_dir.join("async-follower"))
            .expect("create async follower fixture directory");
        std::fs::create_dir_all(successor_dir.join("sync-follower"))
            .expect("create sync follower fixture directory");
        std::fs::write(successor_dir.join("main.cpp"), b"void recovered() {}\n")
            .expect("write successor source");
        std::fs::write(
            successor_dir.join(".code-graph.toml"),
            "[cpp]\nmacro_strip = []\n",
        )
        .expect("write successor project config");

        let mut registry = code_graph_lang::LanguageRegistry::new();
        registry
            .register(Box::new(CppParser::new().expect("create C++ parser")))
            .expect("register C++ parser");
        let server = crate::server::CodeGraphServer::new(registry);
        let (worker_started_tx, worker_started_rx) = std::sync::mpsc::channel();
        let worker_proceed = Arc::new(std::sync::Barrier::new(2));
        *server.inner.worker_start_hook.lock() = Some(crate::server::WorkerStartHook {
            reached: worker_started_tx,
            proceed: Arc::clone(&worker_proceed),
        });
        let active_path = paths::canonicalize(&active_dir)
            .expect("canonicalize active fixture directory")
            .to_string_lossy()
            .into_owned();
        let successor_path = paths::canonicalize(&successor_dir)
            .expect("canonicalize successor fixture directory")
            .to_string_lossy()
            .into_owned();
        let async_follower_path = paths::canonicalize(&successor_dir.join("async-follower"))
            .expect("canonicalize async follower fixture directory")
            .to_string_lossy()
            .into_owned();
        let sync_follower_path = paths::canonicalize(&successor_dir.join("sync-follower"))
            .expect("canonicalize sync follower fixture directory")
            .to_string_lossy()
            .into_owned();

        inject_worker_panic(active_path.clone());
        let active_job_id =
            match analyze_codebase_async(Arc::clone(&server.inner), active_path, true)
                .await
                .expect("admit active async analyze")
            {
                ToolOk::Value(response) => response.job_id,
                ToolOk::Text(_) => panic!("async kickoff must return its structured response"),
            };
        worker_started_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("active worker must reach the panic-test handoff");
        let successor_job_id =
            match analyze_codebase_async(Arc::clone(&server.inner), successor_path, true)
                .await
                .expect("queue successor async analyze")
            {
                ToolOk::Value(response) => response.job_id,
                ToolOk::Text(_) => panic!("async kickoff must return its structured response"),
            };
        let async_follower_id =
            match analyze_codebase_async(Arc::clone(&server.inner), async_follower_path, false)
                .await
                .expect("attach async follower to queued successor")
            {
                ToolOk::Value(response) => response.job_id,
                ToolOk::Text(_) => panic!("async kickoff must return its structured response"),
            };
        assert_ne!(async_follower_id, successor_job_id);

        let sync_follower = tokio::spawn(analyze_codebase(
            Arc::clone(&server.inner),
            sync_follower_path,
            false,
            Arc::new(NoopProgressSink),
        ));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while server.inner.analyze_slot.read().pending_request_count() != 3 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("both followers must queue before the active panic");
        worker_proceed.wait();

        let sync_result = tokio::time::timeout(std::time::Duration::from_secs(10), sync_follower)
            .await
            .expect("synchronous follower must not remain blocked after active worker panic")
            .expect("synchronous follower task must not panic");
        assert!(matches!(sync_result, Ok(ToolOk::Value(_))));

        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            server.inner.persist.close_analyze_and_wait(),
        )
        .await
        .expect("daemon analyze drain must release every worker admission");

        let slot = server.inner.analyze_slot.read();
        let current = Arc::clone(slot.current.as_ref().expect("successor remains current"));
        assert_eq!(current.job_id, successor_job_id);
        assert!(matches!(
            current.state.read().status,
            JobStatus::Completed(_)
        ));
        let failed_active = slot
            .previous_terminal
            .as_ref()
            .expect("panicked active job is retained as the previous terminal");
        assert_eq!(failed_active.job_id, active_job_id);
        assert!(matches!(
            &failed_active.state.read().status,
            JobStatus::Failed(message) if message.contains("injected analyze worker panic")
        ));
        let async_follower = slot
            .resolve_async_job(&async_follower_id)
            .expect("async alias must resolve after the active worker panic");
        assert!(Arc::ptr_eq(&async_follower, &current));
        assert!(matches!(
            async_follower.state.read().status,
            JobStatus::Completed(_)
        ));
    }

    #[test]
    fn pending_request_cap_rejects_covered_and_sync_followers_at_thirty_third_request() {
        let server = server();
        for index in 0..MAX_PENDING_ANALYZES {
            queue(&server, &format!("/queue/{index}"), false);
        }
        for path in ["/queue/0/nested", "/queue/1/nested"] {
            let guard = server.inner.persist.begin_analyze().unwrap();
            let error = match compact_pending(
                &mut server.inner.analyze_slot.write(),
                path.into(),
                true,
                test_admission(path),
                guard,
                None,
            ) {
                Ok(_) => panic!("a covered 33rd request must be rejected before compaction"),
                Err(error) => error,
            };
            assert!(error.0.contains("queue is full"));
        }
        assert_eq!(
            server.inner.analyze_slot.read().pending_request_count(),
            MAX_PENDING_ANALYZES
        );
        assert_eq!(
            server.inner.analyze_slot.read().pending.len(),
            MAX_PENDING_ANALYZES
        );
        assert!(
            !queue_job(&server, 0).force(),
            "rejected covered followers must not OR force into their target"
        );
    }

    #[test]
    fn terminal_alias_grace_window_does_not_consume_pending_request_capacity() {
        let server = server();
        let terminal =
            AnalyzeJob::new_running("terminal".into(), "/queue/terminal".into(), false, 0);
        {
            let mut slot = server.inner.analyze_slot.write();
            slot.previous_terminal = Some(Arc::clone(&terminal));
            for index in 0..64 {
                slot.aliases
                    .insert(format!("retained-{index}"), terminal.job_id.clone());
            }
        }
        for index in 0..MAX_PENDING_ANALYZES {
            queue(&server, &format!("/queue/{index}"), false);
        }
        assert_eq!(server.inner.analyze_slot.read().pending_request_count(), 32);
        let guard = server.inner.persist.begin_analyze().unwrap();
        assert!(compact_pending(
            &mut server.inner.analyze_slot.write(),
            "/queue/0/covered".into(),
            false,
            test_admission("/queue/0/covered"),
            guard,
            None,
        )
        .is_err());
    }

    #[test]
    fn running_job_is_not_considered_by_pending_compaction() {
        let server = server();
        let running = AnalyzeJob::new_running("running".into(), "/queue/root".into(), false, 0);
        server.inner.analyze_slot.write().current = Some(Arc::clone(&running));

        let queued = queue(&server, "/queue/root/child", true);
        assert_eq!(running.path, "/queue/root");
        assert!(!running.force());
        assert_eq!(queued.path, "/queue/root/child");
        assert!(queued.force());
    }

    #[test]
    fn terminal_current_does_not_allow_new_work_to_overtake_pending_fifo() {
        let server = server();
        let terminal =
            AnalyzeJob::new_running("terminal".into(), "/queue/current".into(), false, 0);
        finish_completed(
            &terminal,
            AnalyzeResult {
                files: 1,
                symbols: 1,
                edges: 0,
                root_path: "/queue/current".into(),
                warnings: Vec::new(),
            },
        );
        server.inner.analyze_slot.write().current = Some(Arc::clone(&terminal));
        let first = queue(&server, "/queue/first", false);
        let second = queue(&server, "/queue/second", false);

        let third = match admit_sync(
            &server.inner,
            "/queue/third".into(),
            false,
            Arc::new(NoopProgressSink),
        )
        .unwrap()
        {
            SyncAdmission::Follower(job) => job,
            SyncAdmission::RunNow { .. } => {
                panic!("terminal current must not overtake pending FIFO")
            }
        };
        assert_eq!(
            pending_paths(&server.inner.analyze_slot.read()),
            ["/queue/first", "/queue/second", "/queue/third"]
        );
        let promoted = promote_next(&server.inner, &terminal).expect("promote FIFO head");
        assert!(Arc::ptr_eq(&promoted.job, &first));
        assert!(Arc::ptr_eq(
            server
                .inner
                .analyze_slot
                .read()
                .current
                .as_ref()
                .expect("promoted job is current"),
            &first
        ));
        assert!(Arc::ptr_eq(&queue_job(&server, 0), &second));
        assert!(Arc::ptr_eq(&queue_job(&server, 1), &third));
    }

    #[test]
    fn async_follower_aliases_resolve_replaced_terminal_success() {
        let server = server();
        let running = AnalyzeJob::new_running("running".into(), "/queue/current".into(), false, 0);
        server.inner.analyze_slot.write().current = Some(Arc::clone(&running));

        let child = match admit_async(&server.inner, "/queue/root/child".into(), false).unwrap() {
            Kickoff::Pending { job_id, .. } => job_id,
            Kickoff::New(_, _) => panic!("running work must queue incoming work"),
        };
        let follower = match admit_async(&server.inner, "/queue/root/child/grandchild".into(), true)
            .unwrap()
        {
            Kickoff::Pending { job_id, .. } => job_id,
            Kickoff::New(_, _) => panic!("covered work must remain pending"),
        };
        let replacement = match admit_async(&server.inner, "/queue/root".into(), false).unwrap() {
            Kickoff::Pending { job_id, .. } => job_id,
            Kickoff::New(_, _) => panic!("ancestor must replace pending descendants"),
        };
        {
            let slot = server.inner.analyze_slot.read();
            assert_ne!(follower, child, "absorbed async work needs its own handle");
            assert_ne!(child, replacement, "replacement needs its own handle");
            assert_ne!(
                follower, replacement,
                "follower must retain its original handle"
            );
            assert_eq!(slot.aliases.get(&child), Some(&replacement));
            assert_eq!(slot.aliases.get(&follower), Some(&replacement));
            assert_eq!(
                slot.aliases.len(),
                2,
                "both async identities must be retained"
            );
            assert_eq!(
                slot.resolve_async_job(&follower)
                    .expect("follower alias must resolve while pending")
                    .job_id,
                replacement
            );
        }

        finish_completed(
            &running,
            AnalyzeResult {
                files: 1,
                symbols: 1,
                edges: 0,
                root_path: "/queue/current".into(),
                warnings: Vec::new(),
            },
        );
        let pending = promote_next(&server.inner, &running).expect("replacement promotes");
        assert_eq!(pending.job.job_id, replacement);
        finish_completed(
            &pending.job,
            AnalyzeResult {
                files: 1,
                symbols: 1,
                edges: 0,
                root_path: "/queue/root".into(),
                warnings: Vec::new(),
            },
        );
        assert!(promote_next(&server.inner, &pending.job).is_none());
        let slot = server.inner.analyze_slot.read();
        let terminal = slot
            .resolve_async_job(&follower)
            .expect("follower alias must retain the canonical terminal");
        assert!(matches!(
            terminal.state.read().status,
            JobStatus::Completed(AnalyzeResult { files: 1, .. })
        ));
        assert!(slot.aliases.contains_key(&child));
        assert!(slot.aliases.contains_key(&follower));
        drop(slot);

        // The canonical terminal stays addressable through the same one-job
        // grace window as `previous_terminal`, then its aliases expire with
        // it on the following rotation.
        let next = {
            let mut slot = server.inner.analyze_slot.write();
            install_new_running(&mut slot, "/queue/next".into(), false, None)
        };
        finish_completed(
            &next,
            AnalyzeResult {
                files: 1,
                symbols: 1,
                edges: 0,
                root_path: "/queue/next".into(),
                warnings: Vec::new(),
            },
        );
        {
            let mut slot = server.inner.analyze_slot.write();
            let _ = install_new_running(&mut slot, "/queue/newer".into(), false, None);
        }
        let slot = server.inner.analyze_slot.read();
        assert!(slot.aliases.is_empty());
        assert!(slot.resolve_async_job(&follower).is_none());
    }

    #[test]
    fn async_follower_aliases_resolve_replaced_terminal_error() {
        let server = server();
        let running = AnalyzeJob::new_running("running".into(), "/queue/current".into(), false, 0);
        server.inner.analyze_slot.write().current = Some(Arc::clone(&running));

        let child = match admit_async(&server.inner, "/queue/root/child".into(), false).unwrap() {
            Kickoff::Pending { job_id, .. } => job_id,
            Kickoff::New(_, _) => panic!("running work must queue incoming work"),
        };
        let follower =
            match admit_async(&server.inner, "/queue/root/child/grandchild".into(), false).unwrap()
            {
                Kickoff::Pending { job_id, .. } => job_id,
                Kickoff::New(_, _) => panic!("covered work must remain pending"),
            };
        let replacement = match admit_async(&server.inner, "/queue/root".into(), false).unwrap() {
            Kickoff::Pending { job_id, .. } => job_id,
            Kickoff::New(_, _) => panic!("ancestor must replace pending descendants"),
        };
        assert_ne!(child, follower, "absorbed async work needs its own handle");

        finish_completed(
            &running,
            AnalyzeResult {
                files: 1,
                symbols: 1,
                edges: 0,
                root_path: "/queue/current".into(),
                warnings: Vec::new(),
            },
        );
        let pending = promote_next(&server.inner, &running).expect("replacement promotes");
        assert_eq!(pending.job.job_id, replacement);
        finish_failed(&pending.job, "expected replacement failure".into());
        assert!(promote_next(&server.inner, &pending.job).is_none());

        let slot = server.inner.analyze_slot.read();
        let terminal = slot
            .resolve_async_job(&follower)
            .expect("follower alias must retain the canonical terminal");
        assert!(matches!(
            &terminal.state.read().status,
            JobStatus::Failed(message) if message == "expected replacement failure"
        ));
        assert!(Arc::ptr_eq(
            &terminal,
            &slot
                .resolve_async_job(&child)
                .expect("displaced async handle must resolve to the same terminal")
        ));
    }
}

#[cfg(test)]
mod admission_probe {
    use super::*;
    use std::sync::{mpsc, Barrier};
    use std::time::Duration;

    /// A held admission probe models slow canonicalization/config discovery.
    /// With one runtime worker, an unrelated task must still run before the
    /// probe is released; otherwise filesystem I/O has leaked onto Tokio.
    #[test]
    fn blocking_probe_keeps_single_worker_runtime_responsive() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("build single-worker Tokio runtime");
        let root = tempfile::tempdir().expect("create admission fixture");
        let server = crate::server::CodeGraphServer::new(code_graph_lang::LanguageRegistry::new());
        let (reached_tx, reached_rx) = mpsc::channel();
        let proceed = Arc::new(Barrier::new(2));
        *server.inner.admission_probe_hook.lock() = Some(crate::server::AdmissionProbeHook {
            reached: reached_tx,
            proceed: Arc::clone(&proceed),
        });

        let inner = Arc::clone(&server.inner);
        let path = root.path().to_string_lossy().into_owned();
        let kickoff =
            runtime.spawn(async move { analyze_codebase_async(inner, path, false).await });
        reached_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("blocking admission probe must reach its test handoff");

        let (unrelated_tx, unrelated_rx) = mpsc::channel();
        runtime.spawn(async move {
            let _ = unrelated_tx.send(());
        });
        let responsive = unrelated_rx
            .recv_timeout(Duration::from_millis(250))
            .is_ok();

        // Always release the blocking-pool task before asserting so a broken
        // implementation cannot strand the runtime during test cleanup.
        proceed.wait();
        let result = runtime
            .block_on(kickoff)
            .expect("async kickoff task must not panic");
        assert!(result.is_ok(), "admission probe fixture must be accepted");
        assert!(
            responsive,
            "a held admission probe blocked the sole Tokio runtime worker"
        );
    }
}

#[cfg(test)]
mod status_publication_tests {
    use super::*;
    use std::sync::{mpsc, Barrier};
    use std::time::Duration;

    use code_graph_lang_cpp::CppParser;

    fn server() -> crate::server::CodeGraphServer {
        let mut registry = code_graph_lang::LanguageRegistry::new();
        registry
            .register(Box::new(CppParser::new().expect("create C++ parser")))
            .expect("register C++ parser");
        crate::server::CodeGraphServer::new(registry)
    }

    async fn apply_analyze(server: &crate::server::CodeGraphServer, root: &std::path::Path) {
        let job = AnalyzeJob::new_running(
            "status-publication".into(),
            root.to_string_lossy().into_owned(),
            false,
            0,
        );
        run_analyze_job(
            Arc::clone(&server.inner),
            Arc::clone(&job),
            Arc::new(NoopProgressSink),
        )
        .await;
        assert!(
            matches!(job.state.read().status, JobStatus::Completed(_)),
            "fixture analyze must succeed"
        );
    }

    fn status(server: &crate::server::CodeGraphServer) -> crate::handlers::status::StatusResult {
        let ToolOk::Value(status) = crate::core::status::get_status(Arc::clone(&server.inner))
            .expect("status must succeed")
        else {
            panic!("status must return a value")
        };
        status
    }

    #[tokio::test]
    async fn status_publication_preserves_config_provenance_after_create_and_remove() {
        let created = tempfile::TempDir::new().expect("create config-created fixture");
        let created_root =
            paths::canonicalize(created.path()).expect("canonicalize created fixture");
        std::fs::write(created_root.join("subject.cpp"), "void subject() {}\n")
            .expect("write created fixture source");
        let created_server = server();

        apply_analyze(&created_server, &created_root).await;
        assert!(status(&created_server).config_path.is_none());
        std::fs::write(
            created_root.join(".code-graph.toml"),
            "[cpp]\nmacro_strip = []\n",
        )
        .expect("create config after analyze");
        assert!(
            status(&created_server).config_path.is_none(),
            "a config created after indexing must not claim to have governed the active graph"
        );

        let removed = tempfile::TempDir::new().expect("create config-removed fixture");
        let removed_root =
            paths::canonicalize(removed.path()).expect("canonicalize removed fixture");
        let config_path = removed_root.join(".code-graph.toml");
        std::fs::write(removed_root.join("subject.cpp"), "void subject() {}\n")
            .expect("write removed fixture source");
        std::fs::write(&config_path, "[cpp]\nmacro_strip = []\n").expect("write applied config");
        let removed_server = server();

        apply_analyze(&removed_server, &removed_root).await;
        let expected = config_path.to_string_lossy().into_owned();
        assert_eq!(status(&removed_server).config_path, Some(expected.clone()));
        std::fs::remove_file(&config_path).expect("remove applied config");
        assert_eq!(
            status(&removed_server).config_path,
            Some(expected),
            "removing the applied config must not erase active-index provenance"
        );
    }

    /// Discovery returns the path whose bytes it read. A config create/remove
    /// after that point must not turn a default-config analyze into a
    /// configured one, or erase the provenance of a configuration that was
    /// already parsed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn status_publication_uses_discovery_provenance_without_second_probe() {
        let created = tempfile::TempDir::new().expect("create config-created fixture");
        let created_root =
            paths::canonicalize(created.path()).expect("canonicalize created fixture");
        std::fs::write(created_root.join("subject.cpp"), "void subject() {}\n")
            .expect("write created fixture source");
        let created_server = server();
        let (created_reached_tx, created_reached_rx) = tokio::sync::oneshot::channel();
        let created_proceed = Arc::new(Barrier::new(2));
        *created_server.inner.config_discovery_hook.lock() =
            Some(crate::server::ConfigDiscoveryHook {
                reached: created_reached_tx,
                proceed: Arc::clone(&created_proceed),
            });
        let created_job = AnalyzeJob::new_running(
            "config-created-during-analyze".into(),
            created_root.to_string_lossy().into_owned(),
            false,
            0,
        );
        let created_worker = tokio::spawn(run_analyze_job(
            Arc::clone(&created_server.inner),
            Arc::clone(&created_job),
            Arc::new(NoopProgressSink),
        ));
        created_reached_rx
            .await
            .expect("discovery must select default provenance first");
        std::fs::write(
            created_root.join(".code-graph.toml"),
            "[cpp]\nmacro_strip = []\n",
        )
        .expect("create config after discovery");
        created_proceed.wait();
        created_worker
            .await
            .expect("created-config worker must not panic");
        assert!(matches!(
            created_job.state.read().status,
            JobStatus::Completed(_)
        ));
        assert!(
            status(&created_server).config_path.is_none(),
            "a config created after discovery must not become applied provenance"
        );

        let removed = tempfile::TempDir::new().expect("create config-removed fixture");
        let removed_root =
            paths::canonicalize(removed.path()).expect("canonicalize removed fixture");
        let removed_config = removed_root.join(".code-graph.toml");
        std::fs::write(removed_root.join("subject.cpp"), "void subject() {}\n")
            .expect("write removed fixture source");
        std::fs::write(&removed_config, "[cpp]\nmacro_strip = []\n")
            .expect("write config before discovery");
        let removed_server = server();
        let (removed_reached_tx, removed_reached_rx) = tokio::sync::oneshot::channel();
        let removed_proceed = Arc::new(Barrier::new(2));
        *removed_server.inner.config_discovery_hook.lock() =
            Some(crate::server::ConfigDiscoveryHook {
                reached: removed_reached_tx,
                proceed: Arc::clone(&removed_proceed),
            });
        let removed_job = AnalyzeJob::new_running(
            "config-removed-during-analyze".into(),
            removed_root.to_string_lossy().into_owned(),
            false,
            0,
        );
        let removed_worker = tokio::spawn(run_analyze_job(
            Arc::clone(&removed_server.inner),
            Arc::clone(&removed_job),
            Arc::new(NoopProgressSink),
        ));
        removed_reached_rx
            .await
            .expect("discovery must select config provenance first");
        std::fs::remove_file(&removed_config).expect("remove config after discovery");
        removed_proceed.wait();
        removed_worker
            .await
            .expect("removed-config worker must not panic");
        assert!(matches!(
            removed_job.state.read().status,
            JobStatus::Completed(_)
        ));
        assert_eq!(
            status(&removed_server).config_path,
            Some(removed_config.to_string_lossy().into_owned()),
            "a config removed after discovery must remain applied provenance"
        );
    }

    /// The cache fast-path's publication guard covers the gap between
    /// replacing the graph and writing matching status metadata. A concurrent
    /// status read remains blocked until release, then observes the complete
    /// new snapshot rather than new graph statistics plus old provenance.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn status_publication_blocks_polling_until_complete_snapshot() {
        let temp = tempfile::TempDir::new().expect("create publication fixture");
        let root = paths::canonicalize(temp.path()).expect("canonicalize publication fixture");
        std::fs::write(root.join("subject.cpp"), "void old_symbol() {}\n")
            .expect("write publication fixture source");
        let server = server();
        apply_analyze(&server, &root).await;

        let config_path = root.join(".code-graph.toml");
        std::fs::write(&config_path, "[cpp]\nmacro_strip = []\n")
            .expect("write replacement config");
        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
        let proceed = Arc::new(Barrier::new(2));
        *server.inner.publication_hook.lock() = Some(crate::server::PublicationHook {
            reached: reached_tx,
            proceed: Arc::clone(&proceed),
        });

        let job = AnalyzeJob::new_running(
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
        let reader = std::thread::spawn(move || {
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
        let ToolOk::Value(status) = status_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("get_status must complete after publication")
            .expect("get_status must succeed")
        else {
            panic!("status must return a value")
        };
        reader.join().expect("status reader must not panic");
        assert!(status.indexed);
        assert_eq!(status.index_symbols, 1);
        assert_eq!(
            status.config_path,
            Some(config_path.to_string_lossy().into_owned()),
            "status must pair the new graph with its applied config provenance"
        );
        assert!(matches!(job.state.read().status, JobStatus::Completed(_)));
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
    admission: Option<AnalyzeAdmission>,
) -> Arc<AnalyzeJob> {
    let job = next_job(path, force, admission);
    if let Some(prev) = slot.current.take() {
        if let Some(expired) = slot.previous_terminal.take() {
            slot.discard_aliases_for(&expired.job_id);
        }
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
    debug_write_persist_admitted_marker();
    #[cfg(debug_assertions)]
    debug_delay_persist(dir);
    let io_dir = inner.cache_io_root.read().clone();
    // A daemon without the retained procfd capability must not perform a
    // pathname-based save: validating the pathname and opening the cache are
    // separate operations, so root replacement could redirect the latter.
    // Direct servers are unbound and retain their existing logical-path save.
    // SEAM(phase10-macos): this unanchored-save refusal is Linux-only
    // because only Linux retains a procfd cache-I/O alias. On macOS a
    // daemon-bound save always goes through the pathname; phase 10
    // exercises root replacement mid-persist and either accepts the
    // pathname write or adds a macOS anchoring equivalent.
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
        debug_wait_for_persist_release_marker();
        if let Ok(delay_millis) = delay_millis.parse::<u64>() {
            std::thread::sleep(std::time::Duration::from_millis(delay_millis));
        }
    }
}

/// Optional test-only gate for process-level daemon tests. The persistent
/// marker makes the first held save observable without imposing a gate on
/// production or ordinary debug executions; later saves pass once the test
/// releases that same marker.
#[cfg(debug_assertions)]
fn debug_wait_for_persist_release_marker() {
    let Ok(marker) = std::env::var("CODE_GRAPH_TEST_PERSIST_RELEASE_MARKER") else {
        return;
    };
    let marker = std::path::Path::new(&marker);
    while !marker.exists() {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Append one line for every successful pending admission while the slot lock
/// is still held. This is test-only transport coordination; no production
/// behavior or MCP response depends on the marker.
#[cfg(debug_assertions)]
fn debug_record_pending_admission() {
    use std::io::Write;

    let Ok(marker) = std::env::var("CODE_GRAPH_TEST_PENDING_ADMISSION_MARKER") else {
        return;
    };
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(marker)
        .and_then(|mut file| file.write_all(b"pending admission\n"));
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
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

    #[tokio::test]
    async fn async_admission_rejects_replaced_daemon_root_before_canonicalization() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "code-graph-async-analyze-replaced-root-{}-{nonce}",
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

        let result = analyze_codebase_async(
            Arc::clone(&server.inner),
            root.to_string_lossy().into_owned(),
            false,
        )
        .await;
        assert!(
            matches!(&result, Err(ToolError(message)) if message.contains("project root was replaced")),
            "async admission must reject before canonicalizing the replacement root"
        );
        assert!(server.inner.analyze_slot.read().current.is_none());

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
