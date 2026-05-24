//! Slot + job types for the single-flight analyze model.
//!
//! See `Designs/AnalyzeCodebaseAsync/README.md` for the full design.
//! Shape summary:
//! - `AnalyzeSlot` lives in a `PlRwLock` on `ServerInner` and holds at
//!   most one `Running` job (`current`) plus at most one terminal job
//!   from the previous run (`previous_terminal`).
//! - `AnalyzeJob` is immutable in shape after construction; only its
//!   inner `state` (a single `PlRwLock<JobMutableState>`, per Design
//!   Decision 7) mutates. Held only via `Arc<AnalyzeJob>` — no Clone.
//! - `JobStatus` tags the state machine: Running → Completed(result)
//!   or Failed(msg).

use std::sync::Arc;

use parking_lot::RwLock as PlRwLock;

use crate::handlers::analyze::AnalyzeResult;

#[derive(Default)]
pub struct AnalyzeSlot {
    pub current: Option<Arc<AnalyzeJob>>,
    pub previous_terminal: Option<Arc<AnalyzeJob>>,
}

pub struct AnalyzeJob {
    pub job_id: String,
    pub path: String,
    pub force: bool,
    pub started_at: u64,
    pub state: PlRwLock<JobMutableState>,
}

#[derive(Default)]
pub struct JobMutableState {
    pub status: JobStatus,
    pub finished_at: Option<u64>,
    pub progress: u32,
    pub progress_total: u32,
    pub progress_message: String,
}

#[derive(Default)]
pub enum JobStatus {
    #[default]
    Running,
    Completed(AnalyzeResult),
    Failed(String),
}

impl AnalyzeJob {
    pub fn new_running(job_id: String, path: String, force: bool, started_at: u64) -> Arc<Self> {
        Arc::new(Self {
            job_id,
            path,
            force,
            started_at,
            state: PlRwLock::new(JobMutableState::default()),
        })
    }

    pub fn is_terminal_status(state: &JobMutableState) -> bool {
        matches!(state.status, JobStatus::Completed(_) | JobStatus::Failed(_))
    }
}
