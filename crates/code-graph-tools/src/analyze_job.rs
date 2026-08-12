//! Shared FIFO slot + job types for long-running server work.
//!
//! - `JobSlot` lives in a `PlRwLock` on `ServerInner` and holds one
//!   current job, a FIFO of admitted queued jobs, and at most one terminal
//!   job from the previous run (`previous_terminal`).
//! - `Job` is immutable in shape after construction; only its
//!   inner `state` (a single `PlRwLock<JobMutableState>`) mutates. All
//!   mutable state lives behind that one lock — no atomics. Held only
//!   via `Arc<Job>`; `pub(crate)` fields + no `Clone` derive
//!   keep the Arc-only invariant compiler-enforced.
//! - `JobStatus` tags the state machine: Queued → Running → Completed(result)
//!   or Failed(msg).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::RwLock as PlRwLock;

use crate::handlers::analyze::AnalyzeResult;
use crate::handlers::DetectCommunitiesResponse;
use crate::indexer::ProgressSink;
use crate::server::JobGuard;

/// The operation a retained job executes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Analyze,
    DetectCommunities,
}

/// Typed, public-safe request retained with a job. Runtime-only settings such
/// as the configured response byte budget deliberately do not appear here.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "request_kind", rename_all = "snake_case")]
pub enum JobRequest {
    Analyze {
        path: String,
        force: bool,
    },
    DetectCommunities {
        granularity: Option<String>,
        max_iterations: Option<u32>,
        members_per_community: Option<u32>,
        limit: Option<u32>,
        offset: Option<u32>,
    },
}

/// Terminal payload retained for generic polling. Untagged serialization keeps
/// each `result` value byte-for-byte the synchronous tool response shape.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(untagged)]
pub enum JobResult {
    Analyze(AnalyzeResult),
    DetectCommunities(DetectCommunitiesResponse),
}

/// Number of displaced terminal jobs retained for job-addressable polling.
pub(crate) const TERMINAL_HISTORY_LIMIT: usize = 32;

// `is_terminal` is the rotation helper retained for callers who want
// the predicate without pattern-matching on `JobStatus` directly —
// kept for future use even though both handlers currently inline the
// `matches!` check at their call sites.
#[derive(Default)]
pub(crate) struct JobSlot {
    pub(crate) current: Option<Arc<Job>>,
    pub(crate) previous_terminal: Option<Arc<Job>>,
    /// Terminal jobs displaced from `current`, oldest first. This retention is
    /// independent of `previous_terminal`'s one-rotation compatibility view.
    pub(crate) terminal_history: VecDeque<Arc<Job>>,
    /// Admitted jobs that have not started, in FIFO admission order. Each
    /// retains its shutdown-drain guard and original progress sink until a
    /// terminal current job promotes it.
    pub(crate) pending: VecDeque<PendingJob>,
    /// Monotonic issuance floor for job IDs. Wall-clock nanoseconds alone can
    /// collide when several requests arrive in one clock tick.
    pub(crate) next_job_id: u64,
    /// True from installing/promoting `current` until its supervisor has
    /// performed the terminal slot transition. It prevents admissions from
    /// racing a terminal-visible worker before that authoritative transition.
    pub(crate) current_completion_pending: bool,
    /// Deterministic test-only pause after a worker has written terminal state
    /// but before its supervisor acquires the slot to rotate/promote.
    #[cfg(test)]
    pub(crate) completion_hook: Option<CompletionHook>,
    /// Deterministic worker-panic injection for the generic community-job
    /// supervisor regression test. Consumed once by the worker.
    #[cfg(test)]
    pub(crate) panic_next_community_job: bool,
}

#[cfg(test)]
pub(crate) struct CompletionHook {
    pub(crate) reached: tokio::sync::oneshot::Sender<()>,
    pub(crate) proceed: tokio::sync::oneshot::Receiver<()>,
}

pub(crate) struct PendingJob {
    pub(crate) job: Arc<Job>,
    pub(crate) job_guard: JobGuard,
    pub(crate) sink: Arc<dyn ProgressSink>,
}

pub(crate) struct Job {
    pub(crate) job_id: String,
    pub(crate) kind: JobKind,
    pub(crate) request: JobRequest,
    /// The caller-supplied path, retained for status and diagnostics.
    pub(crate) path: String,
    /// Coverage identity captured at admission. Besides coalescing coverage,
    /// its invocation path is the stable execution path: a queued symlink
    /// request must index the target it named when admitted even if the
    /// symlink is retargeted before promotion. `None` means the request did
    /// not name a valid existing project and the worker must validate the raw
    /// path/config to preserve its established error behavior.
    pub(crate) coverage: Option<CoverageIdentity>,
    pub(crate) force: bool,
    /// Response budget captured at community-job admission so queued work has
    /// the same inputs it had at kickoff, even if a later analyze reloads TOML.
    pub(crate) max_bytes: usize,
    pub(crate) started_at: u64,
    pub(crate) state: PlRwLock<JobMutableState>,
    /// Wakes sync callers waiting for this job's terminal state. Callers arm
    /// this before checking state, so a terminal transition cannot be lost.
    pub(crate) terminal_changed: tokio::sync::Notify,
}

/// Stable identity used to decide whether one admitted analyze can reuse
/// another. Both paths are canonical: invocation containment is meaningful
/// only within one discovered project root.
#[derive(Clone, Debug)]
pub(crate) struct CoverageIdentity {
    pub(crate) invocation_path: PathBuf,
    pub(crate) project_root: PathBuf,
}

#[derive(Default)]
pub(crate) struct JobMutableState {
    pub(crate) status: JobStatus,
    pub(crate) finished_at: Option<u64>,
    pub(crate) progress: u32,
    pub(crate) progress_total: u32,
    pub(crate) progress_message: String,
    /// Active indexing phase. `None` until the worker enters its first
    /// phase (post-config-load). Independent of [`JobStatus`] — that
    /// field carries Running/Completed/Failed; this field names which
    /// of the indexing phases the worker was last working in. Both are
    /// projected onto the [`crate::handlers::status::AnalyzeJobView`]
    /// wire shape so clients can distinguish "running, currently
    /// resolving" from "running, currently persisting" without grepping
    /// the human-readable `progress_message` prefix.
    ///
    /// Set explicitly by the worker at each phase boundary in
    /// [`crate::handlers::analyze::run_analyze_job`]. Resets `progress`
    /// / `progress_total` to 0 on every transition so a stale
    /// previous-phase count never bleeds into a current-phase observation
    /// for the moment between `set_phase` and the new phase's first
    /// `ProgressSink::report`. Terminal jobs leave the field at whatever
    /// the last set value was — clients reading `status == "completed"`
    /// (or "failed") should treat `current_phase` as historical.
    pub(crate) current_phase: Option<JobPhase>,
}

#[derive(Default)]
pub(crate) enum JobStatus {
    #[default]
    Running,
    Queued,
    Completed(JobResult),
    Failed(String),
}

/// Indexing-phase tag for [`JobMutableState::current_phase`].
///
/// Wire spelling is snake_case via `Serialize` so the JSON value matches
/// the field name conventions used by `JobStatus` (also snake_case
/// strings on the wire). Variants cover only the *indexing* phases —
/// terminal Running/Completed/Failed live on [`JobStatus`] and are not
/// duplicated here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobPhase {
    /// rkyv cache file is being deserialized from disk before the
    /// real work begins. On UE-scale projects the cache is multi-GB
    /// and this can take minutes; without a distinct phase polling
    /// clients would see `discovering` with `progress: 0/0` and
    /// assume the indexer is hung. Skipped when `force=true` AND the
    /// invocation scope equals the project root — the cache is about
    /// to be discarded so loading it is wasted I/O.
    LoadingCache,
    /// Walking the file tree and assembling the discover-list. Fast on
    /// most projects; may be invisible to a single client poll.
    Discovering,
    /// Per-file tree-sitter parse + symbol extraction (the rayon pool
    /// branch of `index_directory`). Dominant phase on cold runs.
    Parsing,
    /// Cross-file edge resolution: bare-token call/include targets are
    /// promoted to symbol_ids via the freshly-built indexes.
    Resolving,
    /// Cache serialization (rkyv archive + binary write). One-shot at
    /// the end of a successful analyze; no per-file progress.
    Persisting,
    /// Terminal "done" indicator stamped by `finish_completed`
    /// atomically with `JobStatus::Completed`. A polling client
    /// observing `current_phase == "completed"` can treat the
    /// analyze as finished without separately consulting `status` —
    /// removes the ambiguity where `current_phase == "persisting"`
    /// alone couldn't distinguish "still persisting" from "already
    /// done." **Failed jobs intentionally retain their last
    /// in-flight phase** (e.g. `"parsing"` if parsing died) so
    /// `current_phase + error` together tell the agent where the
    /// failure happened; `Completed` is reserved for successful
    /// terminals.
    Completed,
    /// Whole-graph label propagation for `detect_communities_async`.
    DetectingCommunities,
}

impl Job {
    #[cfg(test)]
    pub(crate) fn new_running(
        job_id: String,
        path: String,
        force: bool,
        started_at: u64,
    ) -> Arc<Self> {
        Self::new_running_with_coverage(job_id, path, force, started_at, None)
    }

    pub(crate) fn new_running_with_coverage(
        job_id: String,
        path: String,
        force: bool,
        started_at: u64,
        coverage: Option<CoverageIdentity>,
    ) -> Arc<Self> {
        Arc::new(Self {
            job_id,
            kind: JobKind::Analyze,
            request: JobRequest::Analyze {
                path: path.clone(),
                force,
            },
            path,
            coverage,
            force,
            max_bytes: 0,
            started_at,
            state: PlRwLock::new(JobMutableState::default()),
            terminal_changed: tokio::sync::Notify::new(),
        })
    }

    #[cfg(test)]
    pub(crate) fn new_queued(
        job_id: String,
        path: String,
        force: bool,
        started_at: u64,
    ) -> Arc<Self> {
        Self::new_queued_with_coverage(job_id, path, force, started_at, None)
    }

    pub(crate) fn new_queued_with_coverage(
        job_id: String,
        path: String,
        force: bool,
        started_at: u64,
        coverage: Option<CoverageIdentity>,
    ) -> Arc<Self> {
        Arc::new(Self {
            job_id,
            kind: JobKind::Analyze,
            request: JobRequest::Analyze {
                path: path.clone(),
                force,
            },
            path,
            coverage,
            force,
            max_bytes: 0,
            started_at,
            state: PlRwLock::new(JobMutableState {
                status: JobStatus::Queued,
                ..JobMutableState::default()
            }),
            terminal_changed: tokio::sync::Notify::new(),
        })
    }

    pub(crate) fn new_running_communities(
        job_id: String,
        request: JobRequest,
        started_at: u64,
        max_bytes: usize,
    ) -> Arc<Self> {
        Arc::new(Self {
            job_id,
            kind: JobKind::DetectCommunities,
            request,
            path: String::new(),
            coverage: None,
            force: false,
            max_bytes,
            started_at,
            state: PlRwLock::new(JobMutableState::default()),
            terminal_changed: tokio::sync::Notify::new(),
        })
    }

    pub(crate) fn new_queued_communities(
        job_id: String,
        request: JobRequest,
        started_at: u64,
        max_bytes: usize,
    ) -> Arc<Self> {
        let job = Self::new_running_communities(job_id, request, started_at, max_bytes);
        job.state.write().status = JobStatus::Queued;
        job
    }

    /// Transition the job into a new indexing phase atomically with
    /// progress reset and a phase-specific message.
    ///
    /// `progress` resets to 0 (each phase starts counting fresh).
    /// `progress_total` is intentionally NOT reset for `Discovering` /
    /// `Parsing` / `Resolving`: the value carried over from the
    /// previous phase is a sensible denominator (Parsing→Resolving
    /// both walk the same file set, so file_count is the right total
    /// for both), and the new phase's first
    /// [`ProgressSink::report`](crate::indexer::ProgressSink::report)
    /// will overwrite it anyway. This prevents the polling client
    /// from observing a `(progress=0, progress_total=0)` snapshot
    /// during the moment between `set_phase` and the new phase's
    /// first report fire.
    ///
    /// `Persisting` is special-cased: there's no per-step sink.report
    /// during cache serialization, so this is the ONLY message the
    /// client sees for the duration of the persist write. Synthetic
    /// `(0, 1)` totals communicate "one persist task in flight".
    /// Without this special-case, the client would see the stale
    /// `"Resolving edges: <last file>"` message and the resolving
    /// counter for the entire persist window.
    pub(crate) fn set_phase(&self, phase: JobPhase) {
        let mut s = self.state.write();
        s.current_phase = Some(phase);
        s.progress = 0;
        s.progress_message = match phase {
            JobPhase::LoadingCache => {
                // No per-step report fires during rkyv deserialization,
                // so this is the only message the client sees for the
                // duration of the load. Synthetic `(0, 1)` totals
                // communicate "one load task in flight" — matches the
                // Persisting convention for the analogous "single
                // serialized op with no granular progress" case.
                s.progress_total = 1;
                "Loading cache from disk".to_string()
            }
            JobPhase::Discovering => "Discovering source files".to_string(),
            JobPhase::Parsing => "Parsing source files".to_string(),
            JobPhase::Resolving => "Resolving cross-file edges".to_string(),
            JobPhase::Persisting => {
                s.progress_total = 1;
                "Persisting cache to disk".to_string()
            }
            JobPhase::Completed => {
                // Terminal stamp. Set both numerator and denominator
                // to 1 so a progress-bar UI renders 100%; the message
                // names the terminal explicitly so clients reading
                // only `progress_message` (without `status`) still
                // see "done."
                s.progress = 1;
                s.progress_total = 1;
                "Analyze complete".to_string()
            }
            JobPhase::DetectingCommunities => {
                s.progress_total = 1;
                "Detecting file communities".to_string()
            }
        };
    }

    pub(crate) fn mark_running(&self) {
        let mut s = self.state.write();
        debug_assert!(matches!(s.status, JobStatus::Queued));
        s.status = JobStatus::Running;
    }
}

/// Whether an already-admitted request covers a later request.
///
/// Project roots must match before invocation containment is considered.
/// `Path::starts_with` compares components rather than bytes, so `/repo/a`
/// covers `/repo/a/file` but not `/repo/ab`. A forced request can cover either
/// kind of request; a non-forcing request must never absorb a forced request.
pub(crate) fn covers(
    coverer: (&CoverageIdentity, bool),
    covered: (&CoverageIdentity, bool),
) -> bool {
    coverer.0.project_root == covered.0.project_root
        && covered
            .0
            .invocation_path
            .starts_with(&coverer.0.invocation_path)
        && (coverer.1 || !covered.1)
}

impl JobMutableState {
    #[allow(dead_code)]
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self.status, JobStatus::Completed(_) | JobStatus::Failed(_))
    }
}

#[cfg(test)]
mod coalesce {
    use super::{covers, CoverageIdentity};
    use std::path::PathBuf;

    fn identity(path: &str, project_root: &str) -> CoverageIdentity {
        CoverageIdentity {
            invocation_path: PathBuf::from(path),
            project_root: PathBuf::from(project_root),
        }
    }

    #[test]
    fn pure_component_aware_coverage_force_matrix() {
        let root = identity("/repo/a", "/repo");
        let child = identity("/repo/a/child", "/repo");
        let sibling_prefix = identity("/repo/ab", "/repo");
        let shadowing_child = identity("/repo/a/child", "/repo/a/child");

        // Same-scope force matrix.
        assert!(covers((&root, false), (&root, false)));
        assert!(!covers((&root, false), (&root, true)));
        assert!(covers((&root, true), (&root, false)));
        assert!(covers((&root, true), (&root, true)));

        // Nested requests retain the same force asymmetry.
        assert!(covers((&root, false), (&child, false)));
        assert!(covers((&root, true), (&child, false)));
        assert!(covers((&root, true), (&child, true)));
        assert!(
            !covers((&root, false), (&child, true)),
            "a non-force parent must not absorb a force child"
        );
        assert!(!covers((&child, false), (&root, false)));
        assert!(!covers((&root, false), (&sibling_prefix, false)));
        assert!(
            !covers((&root, true), (&shadowing_child, false)),
            "a nested config creates a distinct project boundary"
        );
    }
}
