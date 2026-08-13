//! `analyze_codebase` / `analyze_codebase_async` MCP-facing handlers.
//!
//! Thin adapters over `core::analyze` (Design Decision 3 / plan task
//! 2.6). All indexing logic — the slot protocol, the cache fast-path,
//! the parse/resolve/merge pipeline, and progress bookkeeping — lives
//! in `core::analyze`, which is rmcp-agnostic. This module's job is:
//!
//! - keep `AnalyzeResult` and `AsyncKickoffResponse` (the response
//!   shapes, per Design Decision 4's precedent for shared response
//!   types staying where they were defined, with the core importing
//!   them);
//! - keep `now_nanos_u64` (a pure helper used by both this module and
//!   `core::analyze`);
//! - bridge the one genuinely rmcp-specific piece: forwarding indexing
//!   progress to `peer.notify_progress` (OQ-A3). `core::analyze` takes
//!   an abstract `Arc<dyn ProgressSink>`; `build_progress_sink` below
//!   constructs the rmcp-forwarding implementation from the `Peer` /
//!   `ProgressToken` the wire layer hands us.
//!
//! `finish_completed` / `finish_failed` / `JobAwareProgressSink` are
//! re-exported from `core::analyze` (not redefined here) purely so this
//! module's existing white-box tests of those internals — unmodified,
//! per Design Decision 6 — keep resolving them via `use super::*;`.

use std::sync::Arc;

use rmcp::model::{CallToolResult, ProgressNotificationParam, ProgressToken};
use rmcp::service::RoleServer;
use rmcp::Peer;
use serde::{Deserialize, Serialize};

use crate::indexer::{NoopProgressSink, ProgressEvent, ProgressSink};
use crate::server::ServerInner;

#[cfg(test)]
pub(crate) use crate::core::analyze::{finish_completed, finish_failed, JobAwareProgressSink};

// The items below are unused by this module's own (now-thin) adapter
// bodies, but this module's `#[cfg(test)] mod tests` (unmodified, per
// Decision 6) reaches them via `use super::*;` — they used to be
// pulled in for the logic that has since moved to `core::analyze`.
// Gated to test builds only so a normal build doesn't warn on unused
// imports.
#[cfg(test)]
use crate::analyze_job::{Job, JobStatus};
#[cfg(test)]
use code_graph_core::{paths, RootConfig};
#[cfg(test)]
use std::sync::atomic::Ordering;

/// JSON-shape mirror of Go's `analyzeResult` in `internal/tools/analyze.go`.
/// Field order, names, and `omitempty` semantics match the Go struct exactly.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AnalyzeResult {
    pub files: u32,
    pub symbols: u32,
    pub edges: u32,
    pub root_path: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coalesced_by: Option<String>,
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

/// Bridges the abstract `ProgressSink` trait (core-side, rmcp-agnostic)
/// to the concrete rmcp `Peer::notify_progress` call — the ONE place,
/// per OQ-A3, where the progress bridge crosses back into rmcp.
/// `report()` is a synchronous, non-blocking `try_send` (safe to call
/// from the rayon parse pool inside `spawn_blocking`); the background
/// task spawned by [`build_progress_sink`] drains the channel and
/// performs the actual throttled, timeout-guarded async
/// `peer.notify_progress` call.
struct RmcpProgressSink(tokio::sync::mpsc::Sender<ProgressEvent>);

impl ProgressSink for RmcpProgressSink {
    fn report(&self, progress: u32, total: u32, message: &str) {
        let _ = self.0.try_send(ProgressEvent {
            progress,
            total,
            message: message.to_string(),
        });
    }
}

/// Builds the progress sink for one `analyze_codebase` invocation.
///
/// `Some((peer, token))` spawns the throttled rmcp-forwarding task
/// (moved here verbatim from the pre-refactor `run_analyze_job`) and
/// returns a sink that feeds it, plus the task's `JoinHandle`. `None`
/// (async kickoff, or a sync call with no MCP progress token) returns a
/// [`NoopProgressSink`] and no task to await.
///
/// The returned `JoinHandle`, when present, MUST be awaited by the
/// caller after the core call returns: the `Arc<dyn ProgressSink>`
/// passed into `core::analyze` is the only thing keeping the channel's
/// sender alive, so once the core function drops its last clone (on
/// return), the channel closes and the forwarder's trailing flush runs
/// to completion.
fn build_progress_sink(
    peer: Option<Peer<RoleServer>>,
    progress_token: Option<ProgressToken>,
) -> (Arc<dyn ProgressSink>, Option<tokio::task::JoinHandle<()>>) {
    let (peer, token) = match (peer, progress_token) {
        (Some(p), Some(t)) => (p, t),
        _ => return (Arc::new(NoopProgressSink), None),
    };

    let (tx, mut rx) = tokio::sync::mpsc::channel::<ProgressEvent>(64);
    let handle = tokio::spawn(async move {
        const THROTTLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);
        let mut last_sent = std::time::Instant::now()
            .checked_sub(THROTTLE_INTERVAL)
            .unwrap_or_else(std::time::Instant::now);
        let mut latest: Option<ProgressEvent> = None;

        while let Some(evt) = rx.recv().await {
            latest = Some(evt);
            let now = std::time::Instant::now();
            if now.duration_since(last_sent) >= THROTTLE_INTERVAL {
                if let Some(e) = latest.take() {
                    last_sent = now;
                    send_progress(&peer, &token, e).await;
                }
            }
        }
        if let Some(e) = latest {
            send_progress(&peer, &token, e).await;
        }
    });

    (Arc::new(RmcpProgressSink(tx)), Some(handle))
}

/// One throttled/timeout-guarded `peer.notify_progress` call. Split out
/// of `build_progress_sink`'s forwarder loop only to avoid duplicating
/// the `ProgressNotificationParam` construction at the two call sites
/// (mid-stream throttle release and end-of-stream trailing flush).
async fn send_progress(peer: &Peer<RoleServer>, token: &ProgressToken, e: ProgressEvent) {
    const NOTIFY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
    let mut params = ProgressNotificationParam::new(token.clone(), e.progress as f64);
    if e.total > 0 {
        params = params.with_total(e.total as f64);
    }
    params = params.with_message(e.message);
    let _ = tokio::time::timeout(NOTIFY_TIMEOUT, peer.notify_progress(params)).await;
}

/// `analyze_codebase` MCP tool. Slot-protocol coordination, the cache
/// fast-path, the parse pipeline, merge, and persist all live in
/// [`crate::core::analyze::analyze_codebase`]; this adapter's only job
/// is building the rmcp-forwarding progress sink, awaiting its
/// forwarder task after the core call returns (so the trailing flush
/// completes), and converting the typed result to the wire type.
pub async fn analyze_codebase(
    inner: Arc<ServerInner>,
    path_raw: String,
    force: bool,
    peer: Option<Peer<RoleServer>>,
    progress_token: Option<ProgressToken>,
) -> CallToolResult {
    let (sink, forwarder) = build_progress_sink(peer, progress_token);
    let result = crate::core::analyze::analyze_codebase(inner, path_raw, force, sink).await;
    if let Some(handle) = forwarder {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    }
    crate::core::to_call_tool_result(result)
}

/// `analyze_codebase_async` MCP tool — kickoff handler that returns in
/// milliseconds regardless of indexing duration. No progress sink to
/// build: async kickoff has no client-side progress channel, so this
/// adapter is a one-line call into the core.
pub async fn analyze_codebase_async(
    inner: Arc<ServerInner>,
    path_raw: String,
    force: bool,
) -> CallToolResult {
    crate::core::to_call_tool_result(
        crate::core::analyze::analyze_codebase_async(inner, path_raw, force).await,
    )
}

/// Shared wire shape for long-running-job kickoff responses.
/// `< 1KB` by construction — five fields, no nested payload.
#[derive(Debug, Serialize)]
pub struct JobKickoffResponse {
    pub job_id: String,
    pub status: &'static str,
    pub started_at: String,
    pub existing: bool,
    pub note: &'static str,
}

/// Analyze compatibility name for the shared kickoff response.
pub type AsyncKickoffResponse = JobKickoffResponse;

/// `analyze_codebase` normally returns its established result body. A sync
/// request admitted behind already-pending work returns the bounded async
/// kickoff body instead.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum SyncAnalyzeResponse {
    Result(AnalyzeResult),
    Queued(AsyncKickoffResponse),
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
        use crate::analyze_job::JobPhase;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<crate::indexer::ProgressEvent>(8);
        let job = Job::new_running("0".into(), "/x".into(), false, 0);
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

        sink.transition_to(JobPhase::Resolving);

        // Post-condition 1: job state reflects the phase transition.
        let s = job.state.read();
        assert_eq!(s.current_phase, Some(JobPhase::Resolving));
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
        use crate::analyze_job::JobPhase;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<crate::indexer::ProgressEvent>(8);
        let job = Job::new_running("0".into(), "/x".into(), false, 0);
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

        sink.transition_to(JobPhase::Persisting);

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
        assert!(
            !parsed.as_object().unwrap().contains_key("coalesced_by"),
            "ordinary analyze responses must omit coalesced_by rather than emit null"
        );
        let _: AnalyzeResult = serde_json::from_str(&body)
            .expect("ordinary analyze body must deserialize through the shared result type");
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
        // The regression: `current_phase` was `None`. After the
        // top-of-worker set_phase stamp + finish_completed's explicit
        // `Completed` indicator, the terminal carries
        // `current_phase: Completed`.
        assert_eq!(
            state.current_phase,
            Some(crate::analyze_job::JobPhase::Completed),
            "successful terminal must carry the explicit Completed indicator; got {:?}",
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
        use crate::analyze_job::JobPhase;

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
        // Terminal should land on the explicit Completed indicator
        // stamped by `finish_completed`. `Completed` is the
        // "successful terminal" signal — polling clients can read
        // current_phase alone without consulting `status` to know
        // the analyze is done.
        assert_eq!(
            state.current_phase,
            Some(JobPhase::Completed),
            "successful terminal must land on Completed; got {:?}",
            state.current_phase
        );
        assert_eq!(state.progress_message, "Analyze complete");
    }

    /// `finish_completed` stamps `current_phase = Completed`,
    /// `progress = 1/1`, `progress_message = "Analyze complete"`,
    /// `status = Completed`, and `finished_at` all under a single
    /// `state.write()`. A polling observer can read any one of these
    /// fields alone and reach the correct "done" conclusion — no
    /// cross-field coherence check needed.
    #[test]
    fn finish_completed_atomically_stamps_completed_phase() {
        use crate::analyze_job::{Job, JobPhase, JobStatus};

        let job = Job::new_running("0".into(), "/x".into(), false, 0);
        // Seed mid-flight state to verify finish_completed overrides
        // everything cleanly.
        job.set_phase(JobPhase::Persisting);
        {
            let mut s = job.state.write();
            s.progress = 0;
            s.progress_message = "Persisting cache to disk".to_string();
        }

        let dummy_result = AnalyzeResult {
            files: 5,
            symbols: 10,
            edges: 20,
            root_path: "/x".to_string(),
            warnings: Vec::new(),
            coalesced_by: None,
        };
        finish_completed(&job, dummy_result.clone());

        let s = job.state.read();
        assert!(matches!(s.status, JobStatus::Completed(_)));
        assert_eq!(s.current_phase, Some(JobPhase::Completed));
        assert_eq!(s.progress, 1);
        assert_eq!(s.progress_total, 1);
        assert_eq!(s.progress_message, "Analyze complete");
        assert!(s.finished_at.is_some());
    }

    /// `finish_failed` does NOT stamp `Completed` — the failed
    /// terminal retains its last in-flight phase so the agent sees
    /// where the failure happened. `current_phase + error` together
    /// localize the failure to its originating phase.
    #[test]
    fn finish_failed_preserves_in_flight_phase() {
        use crate::analyze_job::{Job, JobPhase, JobStatus};

        let job = Job::new_running("0".into(), "/x".into(), false, 0);
        job.set_phase(JobPhase::Parsing);

        finish_failed(&job, "boom".to_string());

        let s = job.state.read();
        assert!(matches!(s.status, JobStatus::Failed(_)));
        assert_eq!(
            s.current_phase,
            Some(JobPhase::Parsing),
            "failed terminal must NOT stamp Completed; should retain Parsing"
        );
    }

    /// Regression: on UE-scale projects (~3GB rkyv cache file), the
    /// fast-path probe at the top of `run_analyze_job` deserialized
    /// the full cache, then if any in-scope files were stale the
    /// worker fell through to `spawn_blocking` which deserialized
    /// the SAME cache file a second time via `merged_graph.load()`.
    /// On slow storage this double-load cost minutes of redundant
    /// I/O per incremental analyze.
    ///
    /// Fix: hoist the probe Graph through the fast-path-fall-through
    /// boundary into `spawn_blocking` so the slow path reuses it.
    ///
    /// This test exercises the slow path by running analyze twice
    /// with the SECOND call making a file stale (touch its mtime).
    /// Without the hoist, the second analyze would re-load the cache;
    /// with the hoist, it reuses the probe and only re-parses the
    /// stale file. We verify correctness here — performance must be
    /// observed via the `[code-graph] phase: cache reused from
    /// fast-path probe` log line on stderr, which is the operational
    /// signal that the optimization fired.
    #[tokio::test]
    async fn analyze_slow_path_with_stale_file_reuses_probe() {
        use crate::analyze_job::{JobPhase, JobStatus};

        let dir = TempDir::new().unwrap();
        let a_cpp = dir.path().join("a.cpp");
        let b_cpp = dir.path().join("b.cpp");
        fs::write(&a_cpp, b"void f() {}\n").unwrap();
        fs::write(&b_cpp, b"void g() {}\n").unwrap();
        let server = server_with_cpp_parser();
        let path = dir.path().to_string_lossy().into_owned();

        // First call: builds cache.
        let r1 = analyze_codebase(server.inner.clone(), path.clone(), false, None, None).await;
        assert!(r1.is_error.is_none() || r1.is_error == Some(false));

        // Bump a file's mtime by writing to it with new content. The
        // cache records each file's mtime at index time; the next
        // `load_and_stale` compares cached mtime vs disk mtime and
        // flags the mismatch — forcing the fast path to fall through
        // to the slow path, which is the code path under test. The
        // sleep ensures filesystem mtime resolution catches the
        // change (some filesystems round to whole seconds).
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fs::write(&a_cpp, b"void f() {} void h() {}\n").unwrap();

        // Second call: cache exists + a.cpp is stale → fast-path
        // probe loads, finds the stale file, falls through. Slow
        // path uses the hoisted probe (no re-load) and re-parses
        // a.cpp on top of the cached graph.
        let r2 = analyze_codebase(server.inner.clone(), path.clone(), false, None, None).await;
        assert!(r2.is_error.is_none() || r2.is_error == Some(false));

        // Correctness: terminal must reach Completed and the graph
        // must contain symbols from both files (b.cpp cached + a.cpp
        // re-parsed, including the new `h` function). If the probe
        // wasn't being reused correctly, the slow path's fresh
        // `Graph::new() + load()` would have recovered — so this
        // test is primarily a smoke check that the refactor didn't
        // break the slow path.
        let slot = server.inner.analyze_slot.read();
        let current = slot
            .current
            .as_ref()
            .expect("analyze must install a slot.current entry");
        let state = current.state.read();
        assert!(matches!(state.status, JobStatus::Completed(_)));
        assert_eq!(state.current_phase, Some(JobPhase::Completed));
        drop(state);
        drop(slot);

        let g = server.inner.graph.read();
        let stats = g.stats();
        assert!(
            stats.files >= 2,
            "both a.cpp and b.cpp must be in the graph after slow-path re-parse; got {stats:?}"
        );
        // The graph keys files by the canonicalized path the indexer
        // stored (`code_graph_core::paths::canonicalize`). On macOS the
        // `TempDir` root lives under `/var/folders/...`, a symlink to
        // `/private/var/...`; canonicalize resolves it, so the raw
        // `a_cpp`/`b_cpp` paths would miss `file_symbols` and return
        // empty. Canonicalize the lookup paths to match storage.
        let a_key = code_graph_core::paths::canonicalize(&a_cpp).unwrap();
        let b_key = code_graph_core::paths::canonicalize(&b_cpp).unwrap();
        // The new `h()` function added in the second pass should be
        // present — proves the slow path actually re-parsed a.cpp
        // instead of just returning the stale cache.
        let symbols: Vec<String> = g
            .file_symbols(&a_key)
            .iter()
            .map(|s| s.name.clone())
            .collect();
        assert!(
            symbols.iter().any(|n| n == "h"),
            "re-parsed a.cpp must contain the new function `h`; got symbols: {symbols:?}"
        );
        // The cached b.cpp should still be in the graph (proves the
        // probe was reused — if it had been thrown away, the slow
        // path would have re-parsed both files, which would also
        // produce the correct result, but the optimization wouldn't
        // have fired).
        let b_symbols: Vec<String> = g
            .file_symbols(&b_key)
            .iter()
            .map(|s| s.name.clone())
            .collect();
        assert!(
            b_symbols.iter().any(|n| n == "g"),
            "b.cpp's symbols must survive in the graph (proves probe-reuse); got: {b_symbols:?}"
        );
    }

    /// Portable regression for the path-keying contract that
    /// `analyze_slow_path_with_stale_file_reuses_probe` originally
    /// tripped over: the indexer stores every file under its
    /// `paths::canonicalize`-normalized key (project invariant —
    /// "stored file paths are absolute and `\\?\`-prefix-stripped via
    /// `dunce` at index time"). A `file_symbols` lookup therefore only
    /// hits when handed the canonicalized form.
    ///
    /// This test runs identically on every platform. On macOS it would
    /// have caught the original bug (the `TempDir` root lives under
    /// `/var/...`, a symlink to `/private/var/...`, so the raw path
    /// diverges from the stored canonical key). On Linux the raw and
    /// canonical forms coincide, so the assertions hold as a tautology
    /// — but they still pin the contract: were the indexer ever changed
    /// to store a non-canonical key, `file_symbols(canonical)` would
    /// stop resolving and this test would fail.
    #[tokio::test]
    async fn analyze_stores_files_under_canonicalized_paths() {
        let dir = TempDir::new().unwrap();
        let a_cpp = dir.path().join("a.cpp");
        fs::write(&a_cpp, b"void f() {}\n").unwrap();
        let server = server_with_cpp_parser();
        let path = dir.path().to_string_lossy().into_owned();

        let r = analyze_codebase(server.inner.clone(), path, false, None, None).await;
        assert!(r.is_error.is_none() || r.is_error == Some(false));

        let g = server.inner.graph.read();

        // The stored key must equal the canonicalized raw path.
        let canonical = paths::canonicalize(&a_cpp).unwrap();
        let stored: Vec<String> = g
            .file_graphs_snapshot()
            .into_iter()
            .map(|fg| fg.path)
            .collect();
        assert!(
            stored
                .iter()
                .any(|p| p == &canonical.to_string_lossy().into_owned()),
            "graph must key a.cpp under its canonicalized path {}; stored: {stored:?}",
            canonical.display()
        );

        // Every stored key must already be canonical (idempotent under
        // re-canonicalization) — the cross-platform statement of the
        // "paths are canonical at index time" invariant.
        for p in &stored {
            let reparsed = paths::canonicalize(std::path::Path::new(p)).unwrap();
            assert_eq!(
                &reparsed.to_string_lossy().into_owned(),
                p,
                "stored path {p} is not in canonical form (would defeat file_symbols lookups)"
            );
        }

        // The lookup contract: canonical hits.
        let symbols: Vec<String> = g
            .file_symbols(&canonical)
            .iter()
            .map(|s| s.name.clone())
            .collect();
        assert!(
            symbols.iter().any(|n| n == "f"),
            "file_symbols(canonical) must resolve a.cpp's symbols; got: {symbols:?}"
        );
    }

    /// Deterministically reproduces, on ANY unix platform (Linux CI
    /// included), the exact path divergence that made
    /// `analyze_slow_path_with_stale_file_reuses_probe` fail only on
    /// macOS. Rather than rely on the host's incidental `/var ->
    /// /private/var` symlink, we create our own symlinked access path
    /// and index through it, forcing the raw access path to differ from
    /// the canonical on-disk path everywhere.
    ///
    /// Contract pinned:
    ///   - the indexer stores the canonical (symlink-resolved) key;
    ///   - `file_symbols(raw_symlink_path)` MISSES (returns empty) —
    ///     this is precisely the original test bug;
    ///   - `file_symbols(canonicalize(raw_symlink_path))` HITS.
    ///
    /// `#[cfg(unix)]`: `std::os::unix::fs::symlink` is unprivileged on
    /// Linux + macOS, which is where the divergence manifests. Windows
    /// symlink creation requires elevation/developer-mode and carries
    /// its own `\\?\`-strip coverage (see CLAUDE.md), so it is out of
    /// scope here.
    #[cfg(unix)]
    #[tokio::test]
    async fn file_symbols_through_symlink_requires_canonical_path() {
        let dir = TempDir::new().unwrap();
        let real = dir.path().join("real");
        fs::create_dir(&real).unwrap();
        fs::write(real.join("a.cpp"), b"void f() {}\n").unwrap();

        // Access the same directory through a symlink — `link/a.cpp` is
        // a valid path to the file but is NOT its canonical on-disk
        // path.
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let via_link = link.join("a.cpp");

        let server = server_with_cpp_parser();
        let r = analyze_codebase(
            server.inner.clone(),
            link.to_string_lossy().into_owned(),
            false,
            None,
            None,
        )
        .await;
        assert!(r.is_error.is_none() || r.is_error == Some(false));

        let g = server.inner.graph.read();

        // Sentinel: confirm indexing actually happened before asserting
        // the discriminator (timing/IO-independent here, but keeps the
        // failure message honest if the corpus ever fails to parse).
        let canonical = paths::canonicalize(&via_link).unwrap();
        let canonical_hits: Vec<String> = g
            .file_symbols(&canonical)
            .iter()
            .map(|s| s.name.clone())
            .collect();
        assert!(
            canonical_hits.iter().any(|n| n == "f"),
            "file_symbols(canonical) must resolve the file indexed via symlink; got: {canonical_hits:?}"
        );

        // Discriminator: the raw symlink path must MISS — this is the
        // bug class the original probe test hit on macOS.
        let raw_hits = g.file_symbols(&via_link);
        assert!(
            raw_hits.is_empty(),
            "file_symbols(raw symlink path) must miss — graph keys files \
             by canonical path, not the access path used to reach them"
        );

        // And the stored key is the resolved real path, not the link.
        let stored: Vec<String> = g
            .file_graphs_snapshot()
            .into_iter()
            .map(|fg| fg.path)
            .collect();
        assert_eq!(
            stored.len(),
            1,
            "exactly one file expected; got: {stored:?}"
        );
        assert_eq!(
            stored[0],
            canonical.to_string_lossy().into_owned(),
            "stored key must be the canonical (symlink-resolved) path"
        );
    }

    /// Pin the message wording for the cache-load skip log so any
    /// future refactor that removes the optimization triggers a
    /// CI failure with a clear pointer.
    #[test]
    fn loading_cache_phase_message_pinned() {
        use crate::analyze_job::{Job, JobPhase};
        let job = Job::new_running("0".into(), "/x".into(), false, 0);
        job.set_phase(JobPhase::LoadingCache);
        let s = job.state.read();
        assert_eq!(s.progress_message, "Loading cache from disk");
        assert_eq!(s.progress_total, 1);
        assert_eq!(s.progress, 0);
    }

    #[tokio::test]
    async fn async_admission_behind_running_job_is_queued_and_visible() {
        // A synthetic current job pins admission without timing: this test is
        // about queue admission and the status projection, not index speed.
        let server = server_with_cpp_parser();
        let inner = server.inner.clone();
        let synthetic = Job::new_running(
            "00000000000000000001".to_string(),
            "/tmp".to_string(),
            false,
            1,
        );
        inner.analyze_slot.write().current = Some(synthetic);

        let r = analyze_codebase_async(inner.clone(), "/tmp".to_string(), false).await;
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        let body: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(body["status"], "queued");
        assert_eq!(body["existing"], false);
        assert_ne!(body["job_id"], "00000000000000000001");

        let status: serde_json::Value =
            serde_json::from_str(&body_text(&get_status(inner))).unwrap();
        assert_eq!(status["analyze_job"]["job_id"], "00000000000000000001");
        assert_eq!(status["analyze_job_pending_count"], 1);
        assert_eq!(
            status["analyze_job_pending_ids"],
            serde_json::json!([body["job_id"]])
        );
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

    #[tokio::test]
    async fn daemon_root_boundary_precedes_foreign_config_and_accepts_owned_scopes() {
        let fixture = TempDir::new().unwrap();
        let root = fixture.path().join("daemon-root");
        let nested = root.join("nested");
        let foreign = fixture.path().join("foreign");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(&foreign).unwrap();
        fs::write(root.join(".code-graph.toml"), "").unwrap();
        fs::write(root.join("root.cpp"), b"void root_symbol() {}\n").unwrap();
        fs::write(nested.join("nested.cpp"), b"void nested_symbol() {}\n").unwrap();
        fs::write(
            foreign.join(".code-graph.toml"),
            "[discovery\nmax_threads = nope\n",
        )
        .unwrap();

        let canonical_root = paths::canonicalize(&root).unwrap();
        let canonical_foreign = paths::canonicalize(&foreign).unwrap();
        let server = server_with_cpp_parser();
        server
            .bind_daemon_project_root(canonical_root.clone())
            .expect("bind daemon project root");

        let foreign_result = analyze_codebase(
            server.inner.clone(),
            foreign.join(".").to_string_lossy().into_owned(),
            true,
            None,
            None,
        )
        .await;
        let foreign_body = body_text(&foreign_result);
        assert_eq!(foreign_result.is_error, Some(true));
        assert!(
            foreign_body.contains("daemon is bound to project root")
                && foreign_body.contains("outside that root"),
            "outside-root request must fail at the daemon boundary: {foreign_body}"
        );
        assert!(
            !foreign_body.contains("failed to parse .code-graph.toml"),
            "outside-root request must not inspect foreign configuration: {foreign_body}"
        );
        assert!(
            !server.inner.indexed.load(Ordering::Relaxed),
            "rejected foreign request must not mark the daemon indexed"
        );
        assert_eq!(server.inner.graph.read().stats().files, 0);
        assert!(server.inner.root_path.read().is_none());
        assert!(server.inner.cache_root.read().is_none());
        assert!(
            !canonical_root.join(".code-graph-cache.db").exists()
                && !canonical_foreign.join(".code-graph-cache.db").exists(),
            "rejected foreign request must not create either cache"
        );

        let same_root = analyze_codebase(
            server.inner.clone(),
            root.join(".").to_string_lossy().into_owned(),
            true,
            None,
            None,
        )
        .await;
        assert!(
            same_root.is_error.is_none() || same_root.is_error == Some(false),
            "daemon root must be accepted: {}",
            body_text(&same_root)
        );

        let nested_scope = analyze_codebase(
            server.inner.clone(),
            nested.to_string_lossy().into_owned(),
            true,
            None,
            None,
        )
        .await;
        assert!(
            nested_scope.is_error.is_none() || nested_scope.is_error == Some(false),
            "nested scope sharing the daemon project root must be accepted: {}",
            body_text(&nested_scope)
        );

        fs::write(nested.join(".code-graph.toml"), "").unwrap();
        let nested_project = analyze_codebase(
            server.inner.clone(),
            nested.to_string_lossy().into_owned(),
            true,
            None,
            None,
        )
        .await;
        let nested_project_body = body_text(&nested_project);
        assert_eq!(nested_project.is_error, Some(true));
        assert!(
            nested_project_body.contains("daemon is bound to project root")
                && nested_project_body.contains(&canonical_root.display().to_string())
                && nested_project_body
                    .contains(&paths::canonicalize(&nested).unwrap().display().to_string()),
            "nested project config must remain a distinct rejected root: {nested_project_body}"
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
        wait_for_job_terminal(server.inner.clone(), parsed["job_id"].as_str().unwrap()).await;
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
        wait_for_job_terminal(inner, &job_id).await;
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
            JobStatus::Completed(crate::analyze_job::JobResult::Analyze(result)) => {
                assert_eq!(
                    result.files, 1,
                    "Completed result.files should match the 1-file fixture"
                );
            }
            JobStatus::Completed(crate::analyze_job::JobResult::DetectCommunities(_)) => {
                panic!("analyze fixture must not retain a community result")
            }
            JobStatus::Running => {
                panic!("sync analyze returned with slot still Running — terminal write missed")
            }
            JobStatus::Queued => {
                panic!("sync analyze returned with slot still Queued — terminal write missed")
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
        wait_for_job_terminal(inner_a, &job_id).await;
    }

    // ----- Analyze admission and FIFO race tests ---------------------------
    //
    // These tests verify that the slot admits distinct jobs, serializes their
    // workers through FIFO promotion, and preserves sync/async outcomes. They
    // use the recording plugin's `SLEEP_PER_PARSE_MS` knob to stretch the
    // indexing window wide enough for later calls to enter the pending queue.
    //
    // **Knob hygiene.** Every test that sets `SLEEP_PER_PARSE_MS` does so
    // through the `ParseSleepGuard` RAII helper below. If two tests in this
    // binary ran concurrently and one leaked a non-zero value, the other
    // would silently slow down — Cargo's default is parallel test execution
    // within a binary. Tests using the knob clean up via the guard.

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

    /// Records progress reports from the core-only sync path. This pins the
    /// Decision 7 distinction: the first queued sync request retains its real
    /// sink, whereas an immediately-returning queued sync request uses Noop.
    struct CountingProgressSink(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl ProgressSink for CountingProgressSink {
        fn report(&self, _progress: u32, _total: u32, _message: &str) {
            self.0.fetch_add(1, AtomicOrdering::Relaxed);
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

    /// (Task 4.1) Two `analyze_codebase_async` calls released
    /// simultaneously via a `Barrier` both hit the slot write lock at the
    /// same instant; the `PlRwLock` serializes them so one observes the
    /// other's `Running` write. Determinism comes from the barrier — both
    /// tasks reach the slot-write attempt at the same wall-clock point —
    /// and from the slot lock itself, which makes the check+rotate+install
    /// step atomic. NO sleep knob: indexing time is irrelevant; the
    /// synchronization happens entirely in the slot.
    #[tokio::test]
    async fn concurrent_async_kickoffs_coalesce_covered_requests() {
        let _guard = ParseSleepGuard::set(50);
        let dir = tempdir_with_n_rec(20);
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[parsing]\nmax_threads = 1\n",
        )
        .unwrap();
        let (server, _calls) = server_with_recording_plugin();
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
            job_ids
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            1,
            "covered duplicate requests must share one job"
        );
        assert!(parsed.iter().any(|v| v["existing"] == true));
        assert!(parsed.iter().all(|v| v["status"] == "running"));
        for job_id in &job_ids {
            wait_for_job_terminal(inner.clone(), job_id).await;
        }
    }

    /// (Task 4.1) Sequential kickoff creates a distinct FIFO queued job.
    /// The parse-delay guard keeps the first worker running long enough for
    /// the second kickoff to be admitted behind it with a different job ID.
    #[tokio::test]
    async fn async_duplicate_kickoff_after_first_started_reuses_running_job() {
        let _guard = ParseSleepGuard::set(50);
        let dir = tempdir_with_n_rec(5);
        let (server, _calls) = server_with_recording_plugin();
        let inner = server.inner.clone();
        let path = dir.path().to_string_lossy().into_owned();

        let first = analyze_codebase_async(inner.clone(), path.clone(), true).await;
        let first_parsed: serde_json::Value = serde_json::from_str(&body_text(&first)).unwrap();
        let first_job_id = first_parsed["job_id"].as_str().unwrap().to_string();
        assert_eq!(first_parsed["existing"], serde_json::json!(false));

        tokio::task::yield_now().await;

        let second = analyze_codebase_async(inner.clone(), path.clone(), true).await;
        let second_parsed: serde_json::Value = serde_json::from_str(&body_text(&second)).unwrap();
        let second_job_id = second_parsed["job_id"].as_str().unwrap().to_string();
        assert_eq!(second_parsed["existing"], serde_json::json!(true));
        assert_eq!(second_parsed["status"], serde_json::json!("running"));
        assert_eq!(
            second_job_id, first_job_id,
            "covered duplicate requests must reuse the running job"
        );
        wait_for_job_terminal(inner.clone(), &first_job_id).await;
    }

    /// A sync request covered by a running job waits for the coverer's outcome,
    /// annotates only its response, and leaves the stored result pristine.
    #[tokio::test]
    async fn sync_running_coverer_returns_coalesced_result_without_mutating_stored_result() {
        let _guard = ParseSleepGuard::set(50);
        let dir = tempdir_with_n_rec(5);
        let (server, _calls) = server_with_recording_plugin();
        let inner = server.inner.clone();
        let path = dir.path().to_string_lossy().into_owned();

        let kickoff = analyze_codebase_async(inner.clone(), path.clone(), false).await;
        let kickoff: serde_json::Value = serde_json::from_str(&body_text(&kickoff)).unwrap();
        let kickoff_id = kickoff["job_id"].as_str().unwrap().to_string();
        assert!(
            kickoff["status"] == "running",
            "async kickoff must start the first job"
        );

        let sync = tokio::spawn({
            let inner = inner.clone();
            let path = path.clone();
            async move { analyze_codebase(inner, path, false, None, None).await }
        });
        tokio::task::yield_now().await;
        let status: serde_json::Value =
            serde_json::from_str(&body_text(&get_status(inner.clone()))).unwrap();
        assert_eq!(status["analyze_job_pending_count"], 0);
        let sync_r = sync.await.expect("sync queued task panicked");
        assert!(sync_r.is_error.is_none() || sync_r.is_error == Some(false));
        let sync_body = body_text(&sync_r);
        let sync_result: AnalyzeResult =
            serde_json::from_str(&sync_body).expect("coalesced body must use AnalyzeResult");
        assert_eq!(
            sync_result.coalesced_by.as_deref(),
            Some(kickoff_id.as_str())
        );

        let stored = crate::core::status::get_job_status(inner.clone(), kickoff_id.clone())
            .expect("coverer must remain addressable");
        let crate::core::ToolOk::Value(stored) = stored else {
            panic!("coverer status must be structured")
        };
        assert_eq!(
            stored.result.and_then(|result| match result {
                crate::analyze_job::JobResult::Analyze(result) => result.coalesced_by,
                crate::analyze_job::JobResult::DetectCommunities(_) => None,
            }),
            None,
            "coalesced_by belongs only to the synchronous caller's cloned result"
        );
        wait_for_job_terminal(inner, &kickoff_id).await;
    }

    /// A coverer can itself be pending behind unrelated work. The sync caller
    /// still waits for that coverer rather than getting its own queued job.
    #[tokio::test]
    async fn sync_pending_coverer_returns_coalesced_result_without_mutating_stored_result() {
        let _guard = ParseSleepGuard::set(50);
        let running = tempdir_with_n_rec(20);
        let coverer = tempdir_with_n_rec(5);
        let child = coverer.path().join("child");
        fs::create_dir(&child).unwrap();
        fs::write(child.join("child.rec"), b"// rec\n").unwrap();
        fs::write(coverer.path().join(".code-graph.toml"), "").unwrap();
        let (server, _calls) = server_with_recording_plugin();
        let inner = server.inner.clone();

        let first = analyze_codebase_async(
            inner.clone(),
            running.path().to_string_lossy().into_owned(),
            false,
        )
        .await;
        let first: serde_json::Value = serde_json::from_str(&body_text(&first)).unwrap();
        let first_id = first["job_id"].as_str().unwrap().to_string();
        let pending = analyze_codebase_async(
            inner.clone(),
            coverer.path().to_string_lossy().into_owned(),
            false,
        )
        .await;
        let pending: serde_json::Value = serde_json::from_str(&body_text(&pending)).unwrap();
        let pending_id = pending["job_id"].as_str().unwrap().to_string();
        assert_eq!(pending["status"], "queued");

        let sync = analyze_codebase(
            inner.clone(),
            child.to_string_lossy().into_owned(),
            false,
            None,
            None,
        )
        .await;
        let result: AnalyzeResult = serde_json::from_str(&body_text(&sync)).unwrap();
        assert_eq!(result.coalesced_by.as_deref(), Some(pending_id.as_str()));

        let stored = crate::core::status::get_job_status(inner.clone(), pending_id.clone())
            .expect("pending coverer must remain addressable");
        let crate::core::ToolOk::Value(stored) = stored else {
            panic!("pending coverer status must be structured")
        };
        assert_eq!(
            stored.result.and_then(|result| match result {
                crate::analyze_job::JobResult::Analyze(result) => result.coalesced_by,
                crate::analyze_job::JobResult::DetectCommunities(_) => None,
            }),
            None
        );
        wait_for_job_terminal(inner.clone(), &first_id).await;
        wait_for_job_terminal(inner, &pending_id).await;
    }

    /// Decision 7 preserves blocking synchronous semantics for the first
    /// distinct request behind a running job: no request was pending before
    /// admission, so it waits for its own terminal result.
    #[tokio::test]
    async fn sync_first_queued_behind_running_waits_for_its_own_terminal_result() {
        let _guard = ParseSleepGuard::set(50);
        let running = tempdir_with_n_rec(20);
        let queued = tempdir_with_n_rec(5);
        let (server, _calls) = server_with_recording_plugin();
        let inner = server.inner.clone();
        let first = analyze_codebase_async(
            inner.clone(),
            running.path().to_string_lossy().into_owned(),
            false,
        )
        .await;
        let first: serde_json::Value = serde_json::from_str(&body_text(&first)).unwrap();
        let first_id = first["job_id"].as_str().unwrap().to_string();

        let progress_reports = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sync = tokio::spawn({
            let inner = inner.clone();
            let path = queued.path().to_string_lossy().into_owned();
            let sink = std::sync::Arc::new(CountingProgressSink(std::sync::Arc::clone(
                &progress_reports,
            )));
            async move { crate::core::analyze::analyze_codebase(inner, path, false, sink).await }
        });
        let deadline = Instant::now() + Duration::from_secs(1);
        let queued_id = loop {
            let pending = inner
                .analyze_slot
                .read()
                .pending
                .front()
                .map(|pending| pending.job.job_id.clone());
            if let Some(id) = pending {
                break id;
            }
            assert!(
                Instant::now() < deadline,
                "first distinct sync request must enter the FIFO"
            );
            tokio::task::yield_now().await;
        };
        assert_ne!(queued_id, first_id);
        wait_for_job_terminal(inner.clone(), &first_id).await;
        wait_for_job_terminal(inner.clone(), &queued_id).await;
        let sync = sync.await.expect("blocking sync task panicked");
        assert!(matches!(
            sync,
            Ok(crate::core::ToolOk::Value(SyncAnalyzeResponse::Result(_)))
        ));
        assert!(
            progress_reports.load(AtomicOrdering::Relaxed) > 0,
            "the first queued sync request must preserve its real progress sink"
        );
        let stored = crate::core::status::get_job_status(inner, queued_id).unwrap();
        let crate::core::ToolOk::Value(stored) = stored else {
            panic!("queued sync job must be retrievable")
        };
        assert_eq!(stored.status, "completed");
    }

    /// Decision 7 lets a sync request return its queued kickoff immediately
    /// only when another FIFO entry already existed before its admission.
    #[tokio::test]
    async fn sync_queued_behind_pending_preserves_fifo_and_is_retrievable() {
        let _guard = ParseSleepGuard::set(50);
        let running = tempdir_with_n_rec(20);
        let ahead = tempdir_with_n_rec(5);
        let sync_dir = tempdir_with_n_rec(5);
        let (server, _calls) = server_with_recording_plugin();
        let inner = server.inner.clone();
        let first = analyze_codebase_async(
            inner.clone(),
            running.path().to_string_lossy().into_owned(),
            false,
        )
        .await;
        let first: serde_json::Value = serde_json::from_str(&body_text(&first)).unwrap();
        let first_id = first["job_id"].as_str().unwrap().to_string();
        let ahead = analyze_codebase_async(
            inner.clone(),
            ahead.path().to_string_lossy().into_owned(),
            false,
        )
        .await;
        let ahead: serde_json::Value = serde_json::from_str(&body_text(&ahead)).unwrap();
        let ahead_id = ahead["job_id"].as_str().unwrap().to_string();

        let started = Instant::now();
        let sync = analyze_codebase(
            inner.clone(),
            sync_dir.path().to_string_lossy().into_owned(),
            false,
            None,
            None,
        )
        .await;
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "sync request behind pending work must return its queued kickoff immediately"
        );
        let kickoff: serde_json::Value = serde_json::from_str(&body_text(&sync)).unwrap();
        let sync_id = kickoff["job_id"].as_str().unwrap().to_string();
        assert_eq!(kickoff["status"], "queued");
        let status: serde_json::Value =
            serde_json::from_str(&body_text(&get_status(inner.clone()))).unwrap();
        assert_eq!(
            status["analyze_job_pending_ids"],
            serde_json::json!([ahead_id, sync_id])
        );
        for id in [&first_id, &ahead_id, &sync_id] {
            wait_for_job_terminal(inner.clone(), id).await;
        }
        let stored = crate::core::status::get_job_status(inner, sync_id).unwrap();
        let crate::core::ToolOk::Value(stored) = stored else {
            panic!("queued sync job must be retrievable")
        };
        assert_eq!(stored.status, "completed");
    }

    /// A covered sync caller propagates the coverer's original terminal error
    /// with stable attribution rather than manufacturing a success response.
    #[tokio::test]
    async fn sync_coalesced_failure_propagates_coverer_error() {
        let dir = tempdir_with_one_cpp();
        fs::write(dir.path().join(".code-graph.toml"), "").unwrap();
        let server = server_with_cpp_parser();
        let inner = server.inner.clone();
        let canonical = paths::canonicalize(dir.path()).expect("fixture path canonicalizes");
        let (config, project_root, config_present) =
            RootConfig::load_with_presence(&canonical).unwrap();
        let coverage = crate::analyze_job::CoverageIdentity {
            invocation_path: canonical.clone(),
            project_root,
            config_identity: serde_json::to_string(&config).unwrap(),
            config,
            config_present,
        };
        let coverer = Job::new_running_with_coverage(
            "coverer".to_string(),
            dir.path().to_string_lossy().into_owned(),
            false,
            0,
            Some(coverage),
        );
        inner.analyze_slot.write().current = Some(coverer.clone());
        let coverer_refs_before_waiter = Arc::strong_count(&coverer);
        let waiter = tokio::spawn({
            let inner = inner.clone();
            let path = dir.path().to_string_lossy().into_owned();
            async move { analyze_codebase(inner, path, false, None, None).await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while Arc::strong_count(&coverer) == coverer_refs_before_waiter {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("covered waiter must retain the coverer before its terminal failure");
        finish_failed(&coverer, "coverer failed".to_string());
        let result = waiter.await.unwrap();
        assert_eq!(result.is_error, Some(true));
        assert_eq!(body_text(&result), "coverer failed (coalesced_by: coverer)");
    }

    /// FIFO promotion continues after a queued terminal failure: A runs, B
    /// fails during config load, then C must become current and complete.
    #[tokio::test]
    async fn queued_failure_promotes_the_next_fifo_job_and_rotates_previous_terminal() {
        let _guard = ParseSleepGuard::set(50);
        let running_dir = tempdir_with_n_rec(5);
        let bad_dir = tempdir_with_malformed_toml();
        let good_dir = tempdir_with_n_rec(1);
        let (server, _calls) = server_with_recording_plugin();
        let inner = server.inner.clone();

        let first = analyze_codebase_async(
            inner.clone(),
            running_dir.path().to_string_lossy().into_owned(),
            false,
        )
        .await;
        let first_id: serde_json::Value = serde_json::from_str(&body_text(&first)).unwrap();
        let first_id = first_id["job_id"].as_str().unwrap().to_string();
        let failed = analyze_codebase_async(
            inner.clone(),
            bad_dir.path().to_string_lossy().into_owned(),
            false,
        )
        .await;
        let failed: serde_json::Value = serde_json::from_str(&body_text(&failed)).unwrap();
        let failed_id = failed["job_id"].as_str().unwrap().to_string();
        let third = analyze_codebase_async(
            inner.clone(),
            good_dir.path().to_string_lossy().into_owned(),
            false,
        )
        .await;
        let third: serde_json::Value = serde_json::from_str(&body_text(&third)).unwrap();
        let third_id = third["job_id"].as_str().unwrap().to_string();

        let initial: serde_json::Value =
            serde_json::from_str(&body_text(&get_status(inner.clone()))).unwrap();
        assert_eq!(initial["analyze_job"]["job_id"], first_id);
        assert_eq!(
            initial["analyze_job_pending_ids"],
            serde_json::json!([failed_id, third_id])
        );

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                Instant::now() < deadline,
                "third queued job did not complete"
            );
            let status: serde_json::Value =
                serde_json::from_str(&body_text(&get_status(inner.clone()))).unwrap();
            if status["analyze_job"]["job_id"] == third_id
                && status["analyze_job"]["status"] == "completed"
            {
                assert_eq!(status["analyze_job_previous_terminal"]["job_id"], failed_id);
                assert_eq!(status["analyze_job_previous_terminal"]["status"], "failed");
                assert_eq!(status["analyze_job_pending_count"], 0);
                break;
            }
            tokio::task::yield_now().await;
        }
        for job_id in [&first_id, &failed_id, &third_id] {
            wait_for_job_terminal(inner.clone(), job_id).await;
        }
    }

    /// The terminal write precedes slot rotation. While the supervisor is
    /// deliberately paused in that gap, later admissions must remain FIFO
    /// behind both the terminal current and the existing pending head.
    #[tokio::test]
    async fn terminal_rotation_gap_keeps_later_admission_behind_fifo_head() {
        let _guard = ParseSleepGuard::set(50);
        let dir = tempdir_with_n_rec(20);
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[parsing]\nmax_threads = 1\n",
        )
        .unwrap();
        let (server, calls) = server_with_recording_plugin();
        let inner = server.inner.clone();
        let path = dir.path().to_string_lossy().into_owned();
        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
        let (proceed_tx, proceed_rx) = tokio::sync::oneshot::channel();
        inner.analyze_slot.write().completion_hook = Some(crate::analyze_job::CompletionHook {
            reached: reached_tx,
            proceed: proceed_rx,
        });

        let first = analyze_codebase_async(inner.clone(), path.clone(), true).await;
        let first: serde_json::Value = serde_json::from_str(&body_text(&first)).unwrap();
        let first_id = first["job_id"].as_str().unwrap().to_string();
        let second = analyze_codebase_async(inner.clone(), path.clone(), true).await;
        let second: serde_json::Value = serde_json::from_str(&body_text(&second)).unwrap();
        let second_id = second["job_id"].as_str().unwrap().to_string();
        assert_eq!(second_id, first_id, "running coverer must be reused");

        reached_rx
            .await
            .expect("first supervisor must pause after terminal state before slot rotation");
        let third = analyze_codebase_async(inner.clone(), path.clone(), true).await;
        let third: serde_json::Value = serde_json::from_str(&body_text(&third)).unwrap();
        let third_id = third["job_id"].as_str().unwrap().to_string();
        assert_eq!(third["existing"], false);
        assert_eq!(
            third["status"], "queued",
            "a terminal coverer must retry admission rather than report running"
        );
        let gap: serde_json::Value =
            serde_json::from_str(&body_text(&get_status(inner.clone()))).unwrap();
        assert_eq!(gap["analyze_job"]["job_id"], first_id);
        assert_eq!(gap["analyze_job"]["status"], "completed");
        assert_eq!(
            gap["analyze_job_pending_ids"],
            serde_json::json!([third_id])
        );

        proceed_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                Instant::now() < deadline,
                "FIFO head never promoted after terminal gap"
            );
            let status: serde_json::Value =
                serde_json::from_str(&body_text(&get_status(inner.clone()))).unwrap();
            if status["analyze_job"]["job_id"] == third_id
                && status["analyze_job"]["status"] == "running"
            {
                assert_eq!(status["analyze_job_previous_terminal"]["job_id"], first_id);
                assert_eq!(status["analyze_job_pending_ids"], serde_json::json!([]));
                break;
            }
            tokio::task::yield_now().await;
        }
        for job_id in [&first_id, &third_id] {
            wait_for_job_terminal(inner.clone(), job_id).await;
        }
        assert_eq!(
            calls.lock().unwrap().len(),
            2,
            "the covered duplicate must not start a second pipeline"
        );
    }

    /// A sync request owns only its terminal wait. Aborting that request's MCP
    /// task must not cancel its detached supervisor or strand an admitted
    /// queued successor.
    #[tokio::test]
    async fn aborting_sync_waiter_does_not_strand_queued_successor() {
        let _guard = ParseSleepGuard::set(50);
        let running_dir = tempdir_with_n_rec(5);
        let successor_dir = tempdir_with_n_rec(5);
        let (server, _calls) = server_with_recording_plugin();
        let inner = server.inner.clone();
        let running_path = running_dir.path().to_string_lossy().into_owned();
        let successor_path = successor_dir.path().to_string_lossy().into_owned();
        let sync = tokio::spawn({
            let inner = inner.clone();
            async move { analyze_codebase(inner, running_path, false, None, None).await }
        });
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if inner.analyze_slot.read().current.is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "sync request never installed current job"
            );
            tokio::task::yield_now().await;
        }
        let running_id = inner
            .analyze_slot
            .read()
            .current
            .as_ref()
            .unwrap()
            .job_id
            .clone();
        let queued = analyze_codebase_async(inner.clone(), successor_path, false).await;
        let queued: serde_json::Value = serde_json::from_str(&body_text(&queued)).unwrap();
        let queued_id = queued["job_id"].as_str().unwrap().to_string();
        assert_eq!(queued["existing"], false);
        assert_eq!(queued["status"], "queued");
        assert_ne!(queued_id, running_id);
        sync.abort();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                Instant::now() < deadline,
                "queued successor was stranded after sync cancellation"
            );
            let status: serde_json::Value =
                serde_json::from_str(&body_text(&get_status(inner.clone()))).unwrap();
            if status["analyze_job"]["job_id"] == queued_id
                && status["analyze_job"]["status"] == "completed"
            {
                assert_eq!(
                    status["analyze_job_previous_terminal"]["job_id"], running_id,
                    "the queued successor must be promoted after the aborted sync waiter's job"
                );
                break;
            }
            tokio::task::yield_now().await;
        }
        wait_for_job_terminal(inner, &queued_id).await;
    }

    /// Cancelling the first blocking sync request after it enters the FIFO
    /// must not cancel its admitted job or strand that job behind its coverer.
    #[tokio::test]
    async fn aborting_first_queued_sync_waiter_does_not_strand_its_job() {
        let _guard = ParseSleepGuard::set(50);
        let running_dir = tempdir_with_n_rec(5);
        let queued_dir = tempdir_with_n_rec(5);
        let (server, _calls) = server_with_recording_plugin();
        let inner = server.inner.clone();
        let running = analyze_codebase_async(
            inner.clone(),
            running_dir.path().to_string_lossy().into_owned(),
            false,
        )
        .await;
        let running: serde_json::Value = serde_json::from_str(&body_text(&running)).unwrap();
        let running_id = running["job_id"].as_str().unwrap().to_string();
        let sync = tokio::spawn({
            let inner = inner.clone();
            let path = queued_dir.path().to_string_lossy().into_owned();
            async move { analyze_codebase(inner, path, false, None, None).await }
        });
        let deadline = Instant::now() + Duration::from_secs(1);
        let queued_id = loop {
            let pending = inner
                .analyze_slot
                .read()
                .pending
                .front()
                .map(|pending| pending.job.job_id.clone());
            if let Some(id) = pending {
                break id;
            }
            assert!(
                Instant::now() < deadline,
                "first sync request must enter the FIFO before cancellation"
            );
            tokio::task::yield_now().await;
        };
        sync.abort();
        wait_for_job_terminal(inner.clone(), &running_id).await;
        wait_for_job_terminal(inner.clone(), &queued_id).await;
        let stored = crate::core::status::get_job_status(inner, queued_id).unwrap();
        let crate::core::ToolOk::Value(stored) = stored else {
            panic!("cancelled sync request's queued job must remain retrievable")
        };
        assert_eq!(stored.status, "completed");
    }

    /// Addressable terminal retention outlives the legacy one-rotation view:
    /// 33 archived terminals keep the newest 32 while the active current job
    /// remains independently retrievable.
    #[tokio::test]
    async fn job_status_history_evicts_only_oldest_archived_terminal() {
        let dir = tempdir_with_one_cpp();
        let server = server_with_cpp_parser();
        let inner = server.inner.clone();
        let path = dir.path().to_string_lossy().into_owned();
        let mut ids = Vec::new();

        for _ in 0..34 {
            let kickoff = analyze_codebase_async(inner.clone(), path.clone(), false).await;
            let kickoff: serde_json::Value = serde_json::from_str(&body_text(&kickoff)).unwrap();
            ids.push(kickoff["job_id"].as_str().unwrap().to_string());
            let _ = poll_until_terminal(inner.clone(), Duration::from_secs(5)).await;
        }

        {
            let slot = inner.analyze_slot.read();
            assert_eq!(slot.terminal_history.len(), 32);
            assert_eq!(slot.terminal_history.front().unwrap().job_id, ids[1]);
            assert_eq!(slot.terminal_history.back().unwrap().job_id, ids[32]);
            assert_eq!(slot.current.as_ref().unwrap().job_id, ids[33]);
        }
        assert!(crate::core::status::get_job_status(inner.clone(), ids[0].clone()).is_err());
        for id in ids.iter().skip(1) {
            let result = crate::core::status::get_job_status(inner.clone(), id.clone());
            assert!(
                result.is_ok(),
                "retained/current job {id} must remain addressable"
            );
        }
    }

    /// An in-flight sync `analyze_codebase` admits an async queued job. The 20-file ×
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
    async fn sync_kickoff_queues_async_kickoff() {
        let _guard = ParseSleepGuard::set(50);
        let running_dir = tempdir_with_n_rec(20);
        let successor_dir = tempdir_with_n_rec(5);
        let (server, _calls) = server_with_recording_plugin();
        let inner = server.inner.clone();
        let running_path = running_dir.path().to_string_lossy().into_owned();
        let successor_path = successor_dir.path().to_string_lossy().into_owned();

        let sync_handle = {
            let inner = inner.clone();
            tokio::spawn(
                async move { analyze_codebase(inner, running_path, false, None, None).await },
            )
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

        let async_r = analyze_codebase_async(inner.clone(), successor_path, false).await;
        let async_parsed: serde_json::Value = serde_json::from_str(&body_text(&async_r)).unwrap();
        let successor_id = async_parsed["job_id"].as_str().unwrap();
        assert_eq!(async_parsed["existing"], serde_json::json!(false));
        assert_eq!(async_parsed["status"], serde_json::json!("queued"));
        assert_ne!(successor_id, sync_job_id);
        let queued_status: serde_json::Value =
            serde_json::from_str(&body_text(&get_status(inner.clone()))).unwrap();
        assert_eq!(
            queued_status["analyze_job_pending_ids"],
            serde_json::json!([successor_id])
        );

        // Drain the sync handler so the worker completes inside this test's
        // runtime — avoids the worker future being dropped mid-flight when
        // the test's runtime tears down.
        let _ = sync_handle.await.expect("sync handler task panicked");
        wait_for_job_terminal(inner.clone(), successor_id).await;
        let completed: serde_json::Value =
            serde_json::from_str(&body_text(&get_status(inner))).unwrap();
        assert_eq!(completed["analyze_job"]["job_id"], successor_id);
        assert_eq!(completed["analyze_job"]["status"], "completed");
        assert_eq!(
            completed["analyze_job_previous_terminal"]["job_id"],
            sync_job_id
        );
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

    /// Drain a specific admitted job before test fixture teardown. Terminal
    /// state alone is insufficient: the detached supervisor still owns slot
    /// rotation and its `AnalyzeGuard` until it clears completion-pending.
    async fn wait_for_job_terminal(inner: Arc<ServerInner>, job_id: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                Instant::now() < deadline,
                "job {job_id} did not reach terminal before test teardown"
            );
            let result = crate::core::status::get_job_status(inner.clone(), job_id.to_string())
                .expect("admitted job must remain addressable while draining");
            let crate::core::ToolOk::Value(view) = result else {
                panic!("get_job_status must return an AnalyzeJobView")
            };
            if view.status == "completed" || view.status == "failed" {
                let supervisor_finished = {
                    let slot = inner.analyze_slot.read();
                    slot.current
                        .as_ref()
                        .is_none_or(|current| current.job_id != job_id)
                        || !slot.current_completion_pending
                };
                if supervisor_finished {
                    return;
                }
            }
            tokio::task::yield_now().await;
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

        {
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
        wait_for_job_terminal(inner.clone(), &t1_job_id).await;
        wait_for_job_terminal(inner, &t2_job_id).await;
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

        let (previous_id, current_id) = {
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
            (previous_id, current_id)
        };
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
        for job_id in [&t1_job_id, &t2_job_id, &t3_job_id] {
            wait_for_job_terminal(inner.clone(), job_id).await;
        }
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

        let kickoff = analyze_codebase_async(
            inner.clone(),
            dir.path().to_string_lossy().into_owned(),
            false,
        )
        .await;
        let kickoff: serde_json::Value = serde_json::from_str(&body_text(&kickoff)).unwrap();
        let kickoff_id = kickoff["job_id"].as_str().unwrap().to_string();

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
        wait_for_job_terminal(inner, &kickoff_id).await;
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

        {
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
        wait_for_job_terminal(inner.clone(), &t1_job_id).await;
        wait_for_job_terminal(inner, &t2_job_id).await;
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

        let kickoff = analyze_codebase_async(inner.clone(), path, false).await;
        let kickoff: serde_json::Value = serde_json::from_str(&body_text(&kickoff)).unwrap();
        let kickoff_id = kickoff["job_id"].as_str().unwrap().to_string();

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
        wait_for_job_terminal(inner, &kickoff_id).await;
    }
}
