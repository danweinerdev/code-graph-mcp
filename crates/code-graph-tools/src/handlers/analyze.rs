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

use crate::analyze_job::{AnalyzeJob, JobStatus};
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
        Some(tokio::spawn(async move {
            while rx.recv().await.is_some() {}
        }))
    };

    let registry = Arc::clone(&inner);
    let cfg_for_pool = cfg.clone();
    let abs_path_for_pool = abs_path.clone();
    let project_root_for_pool = project_root.clone();
    let scope_is_project_root = abs_path == project_root;
    let job_for_pool = Arc::clone(&job);
    let blocking_handle = tokio::task::spawn_blocking(move || {
        let sink = JobAwareProgressSink {
            inner: ChannelProgressSink(tx),
            job: job_for_pool,
        };
        let mut blocking_warnings: Vec<String> = Vec::new();

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

        eprintln!("[code-graph] phase: resolving edges");
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

    if let Some(handle) = forwarder {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    }

    let (merged_graph, blocking_warnings) = match blocking_result {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            finish_failed(&job, format!("indexing failed: {e}"));
            return;
        }
        Err(join_err) => {
            finish_failed(&job, format!("indexing task panicked: {join_err}"));
            return;
        }
    };

    warnings.extend(blocking_warnings);

    if merged_graph.stats().files == 0 {
        finish_failed(
            &job,
            format!("no supported source files found in {}", abs_path.display()),
        );
        return;
    }

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
    finish_completed(&job, result);
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

    /// RAII guard that sets the recording plugin's per-`parse_file` sleep
    /// knob on construction and resets it to `0` on drop. The Drop reset is
    /// load-bearing: tests in this binary run concurrently by default, so a
    /// leaked non-zero value would silently stretch every concurrent test's
    /// indexing wall time and turn deterministic synchronization into
    /// timing-dependent flake.
    struct ParseSleepGuard;
    impl ParseSleepGuard {
        fn set(ms: u64) -> Self {
            SLEEP_PER_PARSE_MS.store(ms, AtomicOrdering::Relaxed);
            Self
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
}
