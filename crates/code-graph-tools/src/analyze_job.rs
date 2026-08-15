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

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
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
    /// Canonical analyze requests waiting behind `current`. Each entry carries
    /// the number of requests compacted into it, so the 32-request capacity
    /// includes synchronous followers and async aliases, not merely scans.
    /// This is deliberately analyze-specific: it is not a general job
    /// scheduler.
    pub(crate) pending: VecDeque<PendingAnalyze>,
    /// Async IDs absorbed by pending compaction. They are intentionally
    /// internal: status continues to expose only `current`. Entries remain
    /// while their target has the same current/previous-terminal retention
    /// as its canonical job.
    pub(crate) aliases: HashMap<String, String>,
}

impl AnalyzeSlot {
    fn resolve_alias_id(&self, job_id: &str) -> Option<String> {
        let mut canonical_id = job_id.to_string();
        let mut remaining = self.aliases.len().saturating_add(1);
        while let Some(next) = self.aliases.get(&canonical_id) {
            if remaining == 0 {
                return None;
            }
            canonical_id = next.clone();
            remaining -= 1;
        }
        Some(canonical_id)
    }

    /// Resolve a canonical or absorbed async handle to its satisfying job.
    ///
    /// Following the map rather than assuming a single hop keeps displaced
    /// followers correct even if compaction has formed an alias chain.
    pub(crate) fn resolve_async_job(&self, job_id: &str) -> Option<Arc<AnalyzeJob>> {
        let canonical_id = self.resolve_alias_id(job_id)?;

        self.current
            .iter()
            .chain(self.previous_terminal.iter())
            .map(Arc::clone)
            .chain(self.pending.iter().map(|pending| Arc::clone(&pending.job)))
            .find(|job| job.job_id == canonical_id)
    }

    /// Number of live requests still waiting behind the running scan. Terminal
    /// alias retention deliberately lives outside `pending`, so the grace
    /// window never occupies a request-capacity slot.
    pub(crate) fn pending_request_count(&self) -> usize {
        self.pending
            .iter()
            .map(|pending| pending.request_count)
            .sum()
    }

    /// Drop aliases only when the canonical terminal falls out of the same
    /// one-job grace window used by `previous_terminal`.
    pub(crate) fn discard_aliases_for(&mut self, job_id: &str) {
        let stale: HashSet<_> = self
            .aliases
            .keys()
            .filter(|alias| {
                self.resolve_alias_id(alias)
                    .is_some_and(|target| target == job_id)
            })
            .cloned()
            .collect();
        self.aliases.retain(|alias, _| !stale.contains(alias));
    }
}

/// One process-wide, collision-safe source for the 20-digit decimal IDs
/// exposed by analyze kickoff. It advances from wall-clock nanoseconds so
/// ordinary IDs retain their timestamp ordering while rapid queued admissions
/// cannot reuse one value.
static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(0);

pub(crate) fn allocate_job_timestamp() -> u64 {
    loop {
        let now = crate::handlers::analyze::now_nanos_u64();
        let previous = NEXT_JOB_ID.load(Ordering::Relaxed);
        let next = now.max(
            previous
                .checked_add(1)
                .expect("analyze job ID space exhausted"),
        );
        if NEXT_JOB_ID
            .compare_exchange_weak(previous, next, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            return next;
        }
    }
}

/// One canonical pending scan. Its guard is acquired at admission, not when
/// promoted, so daemon shutdown accounts for queued work and cannot let it
/// escape the drain.
pub(crate) struct PendingAnalyze {
    pub(crate) job: Arc<AnalyzeJob>,
    pub(crate) guard: crate::server::AnalyzeGuard,
    /// Canonical request plus every synchronous or asynchronous follower
    /// compacted into this pending scan.
    pub(crate) request_count: usize,
}

pub(crate) struct AnalyzeJob {
    pub(crate) job_id: String,
    pub(crate) path: String,
    /// Original request force, retained for the existing status view.
    pub(crate) force: bool,
    pub(crate) started_at: u64,
    pub(crate) state: PlRwLock<JobMutableState>,
    terminal: tokio::sync::Notify,
}

#[derive(Default)]
pub(crate) struct JobMutableState {
    pub(crate) status: JobStatus,
    pub(crate) finished_at: Option<u64>,
    pub(crate) progress: u32,
    pub(crate) progress_total: u32,
    pub(crate) progress_message: String,
    /// Force requested by a follower while this job remained pending. A
    /// running job is never compacted or upgraded.
    pub(crate) forced_by_follower: bool,
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
    /// Set only when a pending canonical request is replaced by a broader
    /// pending request. The old job is never executed; callers follow this
    /// internal link to the scan that satisfies them.
    pub(crate) replacement: Option<Arc<AnalyzeJob>>,
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
            terminal: tokio::sync::Notify::new(),
        })
    }

    pub(crate) fn force(&self) -> bool {
        self.force || self.state.read().forced_by_follower
    }

    /// A compacted scan must retain every request's invalidation intent.
    pub(crate) fn or_force(&self, force: bool) {
        if force {
            self.state.write().forced_by_follower = true;
        }
    }

    pub(crate) fn notify_terminal(&self) {
        self.terminal.notify_waiters();
    }

    pub(crate) async fn wait_for_terminal(mut job: Arc<Self>) -> Arc<Self> {
        loop {
            // Arm before inspecting mutable state. `Notify` does not retain
            // notifications for a future waiter, so checking first could lose
            // a terminal transition in the check-to-await window.
            let next = {
                let observed = Arc::clone(&job);
                let notified = observed.terminal.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let next = {
                    let state = observed.state.read();
                    if state.is_terminal() {
                        return Arc::clone(&job);
                    }
                    state.replacement.clone()
                };
                if next.is_none() {
                    notified.await;
                }
                next
            };
            if let Some(next) = next {
                job = next;
            }
        }
    }

    pub(crate) fn replace_with(&self, replacement: Arc<AnalyzeJob>) {
        self.state.write().replacement = Some(replacement);
        self.notify_terminal();
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
