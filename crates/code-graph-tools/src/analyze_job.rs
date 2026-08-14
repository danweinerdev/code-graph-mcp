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
    pub(crate) fn set_phase(&self, phase: AnalyzePhase) {
        let mut s = self.state.write();
        s.current_phase = Some(phase);
        s.progress = 0;
        s.progress_message = match phase {
            AnalyzePhase::LoadingCache => {
                // No per-step report fires during rkyv deserialization,
                // so this is the only message the client sees for the
                // duration of the load. Synthetic `(0, 1)` totals
                // communicate "one load task in flight" — matches the
                // Persisting convention for the analogous "single
                // serialized op with no granular progress" case.
                s.progress_total = 1;
                "Loading cache from disk".to_string()
            }
            AnalyzePhase::Discovering => "Discovering source files".to_string(),
            AnalyzePhase::Parsing => "Parsing source files".to_string(),
            AnalyzePhase::Resolving => "Resolving cross-file edges".to_string(),
            AnalyzePhase::Persisting => {
                s.progress_total = 1;
                "Persisting cache to disk".to_string()
            }
            AnalyzePhase::Completed => {
                // Terminal stamp. Set both numerator and denominator
                // to 1 so a progress-bar UI renders 100%; the message
                // names the terminal explicitly so clients reading
                // only `progress_message` (without `status`) still
                // see "done."
                s.progress = 1;
                s.progress_total = 1;
                "Analyze complete".to_string()
            }
        };
    }
}

impl JobMutableState {
    #[allow(dead_code)]
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self.status, JobStatus::Completed(_) | JobStatus::Failed(_))
    }
}
