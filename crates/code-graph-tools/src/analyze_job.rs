//! Slot + job types for the single-flight analyze model.
//!
//! - `AnalyzeSlot` lives in a `PlRwLock` on `ServerInner` and holds at
//!   most one `Running` job (`current`) plus at most one terminal job
//!   from the previous run (`previous_terminal`).
//! - `AnalyzeJob` is immutable in shape after construction; only its
//!   inner `state` (a single `PlRwLock<JobMutableState>`) mutates. All
//!   mutable state lives behind that one lock — no atomics. Held only
//!   via `Arc<AnalyzeJob>`; `pub(crate)` fields + no `Clone` derive
//!   keep the Arc-only invariant compiler-enforced.
//! - `JobStatus` tags the state machine: Running → Completed(result)
//!   or Failed(msg).

use std::sync::Arc;

use parking_lot::RwLock as PlRwLock;

use crate::handlers::analyze::AnalyzeResult;

// `is_terminal` is the rotation helper retained for callers who want
// the predicate without pattern-matching on `JobStatus` directly —
// kept for future use even though both handlers currently inline the
// `matches!` check at their call sites.
#[derive(Default)]
pub(crate) struct AnalyzeSlot {
    pub(crate) current: Option<Arc<AnalyzeJob>>,
    pub(crate) previous_terminal: Option<Arc<AnalyzeJob>>,
}

pub(crate) struct AnalyzeJob {
    pub(crate) job_id: String,
    pub(crate) path: String,
    pub(crate) force: bool,
    pub(crate) started_at: u64,
    pub(crate) state: PlRwLock<JobMutableState>,
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
    pub(crate) current_phase: Option<AnalyzePhase>,
}

#[derive(Default)]
pub(crate) enum JobStatus {
    #[default]
    Running,
    Completed(AnalyzeResult),
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
pub enum AnalyzePhase {
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
}

impl AnalyzeJob {
    pub(crate) fn new_running(
        job_id: String,
        path: String,
        force: bool,
        started_at: u64,
    ) -> Arc<Self> {
        Arc::new(Self {
            job_id,
            path,
            force,
            started_at,
            state: PlRwLock::new(JobMutableState::default()),
        })
    }

    /// Transition the job into a new indexing phase. Resets per-phase
    /// counters (`progress` / `progress_total`) so observers polling
    /// between `set_phase` and the new phase's first
    /// `ProgressSink::report` see a clean zeroed page rather than the
    /// previous phase's stale totals. `progress_message` is left alone
    /// — the next sink event will overwrite it; clearing it would create
    /// an empty-string snapshot that's harder to interpret.
    pub(crate) fn set_phase(&self, phase: AnalyzePhase) {
        let mut s = self.state.write();
        s.current_phase = Some(phase);
        s.progress = 0;
        s.progress_total = 0;
    }
}

impl JobMutableState {
    #[allow(dead_code)]
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self.status, JobStatus::Completed(_) | JobStatus::Failed(_))
    }
}
