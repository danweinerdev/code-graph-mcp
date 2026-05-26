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
}

#[derive(Default)]
pub(crate) enum JobStatus {
    #[default]
    Running,
    Completed(AnalyzeResult),
    Failed(String),
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
}

impl JobMutableState {
    #[allow(dead_code)]
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self.status, JobStatus::Completed(_) | JobStatus::Failed(_))
    }
}
