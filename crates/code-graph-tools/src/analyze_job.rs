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

// Narrow `#[allow(dead_code)]` cover the surface area exercised only by
// 1.4 (async handler + get_status view): `previous_terminal` is written
// here but read by `get_status`; `job_id` / `started_at` ride the wire
// in the async kickoff response; `is_terminal` is the rotation helper
// used by both handlers when 1.4 lands. The sync handler (1.3) already
// reads `current`, `state`, `status` (Running / Completed / Failed), and
// calls `new_running` — no allow needed for those.
#[derive(Default)]
pub(crate) struct AnalyzeSlot {
    pub(crate) current: Option<Arc<AnalyzeJob>>,
    #[allow(dead_code)]
    pub(crate) previous_terminal: Option<Arc<AnalyzeJob>>,
}

pub(crate) struct AnalyzeJob {
    #[allow(dead_code)]
    pub(crate) job_id: String,
    pub(crate) path: String,
    pub(crate) force: bool,
    #[allow(dead_code)]
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
