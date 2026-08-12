//! Typed core for `get_status`.
//!
//! `get_status` is UNGATED (Design Decision 8 / plan task 2.2 notes): it
//! must answer before any `analyze_codebase` has run, so unlike
//! `core::watch`, there is no `require_indexed` call here.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::core::{ToolError, ToolOk, ToolResult};
use crate::handlers::status::{format_unix_nanos_rfc3339, AnalyzeJobView, StatusResult};
use crate::server::ServerInner;

/// `get_status` body. Pure read — no locks held across the return.
///
/// Body moved verbatim from `handlers::status::get_status`; the only
/// change is the return type (`ToolResult<StatusResult>` instead of
/// `CallToolResult`) and the terminal `tool_success_json` call, which the
/// adapter now performs.
pub fn get_status(inner: Arc<ServerInner>) -> ToolResult<StatusResult> {
    let binary_version = env!("CODE_GRAPH_GIT_SHA").to_string();
    let package_version = env!("CARGO_PKG_VERSION").to_string();
    let release_build = !cfg!(debug_assertions);

    // Project root + config path: both derive from `root_path` (set by
    // the most recent analyze). If never indexed, both are None.
    let project_root = inner.root_path.read().clone();
    let config_path = project_root.as_ref().and_then(|root| {
        let p = root.join(".code-graph.toml");
        if p.exists() {
            Some(p.to_string_lossy().into_owned())
        } else {
            None
        }
    });
    let indexed_root = project_root.map(|p| p.to_string_lossy().into_owned());

    // Config counts: cheap read of the cached `RootConfig`. The TOML
    // file is NOT re-read here — these are exactly the values that
    // applied during the most recent analyze.
    let (macro_strip_count, macro_strip_with_args_count) = {
        let cfg = inner.config.read();
        (
            cfg.cpp.macro_strip.len(),
            cfg.cpp.macro_strip_with_args.len(),
        )
    };

    let indexed = inner.indexed.load(Ordering::Acquire);
    let stats = inner.graph.read().stats();

    let built_at_nanos = inner.index_built_at.load(Ordering::Acquire);
    let index_built_at = if built_at_nanos == 0 {
        None
    } else {
        Some(format_unix_nanos_rfc3339(built_at_nanos))
    };
    let index_force_built = if indexed {
        Some(inner.index_force_built.load(Ordering::Acquire))
    } else {
        None
    };

    // Snapshot the slot under the read lock — job Arcs plus the FIFO IDs —
    // then drop the guard before walking the job state. Building views
    // outside the slot lock keeps progress writes from contending with
    // polls beyond the constant-time Arc::clone window.
    let (current_job, previous_terminal_job, analyze_job_pending_count, analyze_job_pending_ids) = {
        let slot = inner.analyze_slot.read();
        (
            slot.current.clone(),
            slot.previous_terminal.clone(),
            u32::try_from(slot.pending.len()).unwrap_or(u32::MAX),
            slot.pending
                .iter()
                .map(|pending| pending.job.job_id.clone())
                .collect(),
        )
    };
    let analyze_job = current_job.as_deref().map(AnalyzeJobView::from_job);
    let analyze_job_previous_terminal = previous_terminal_job
        .as_deref()
        .map(AnalyzeJobView::from_job);

    let result = StatusResult {
        binary_version,
        package_version,
        release_build,
        config_path,
        config_macro_strip_count: macro_strip_count,
        config_macro_strip_with_args_count: macro_strip_with_args_count,
        indexed,
        indexed_root,
        index_files: stats.files,
        index_symbols: stats.nodes,
        index_edges: stats.edges,
        index_built_at,
        index_force_built,
        analyze_job,
        analyze_job_previous_terminal,
        analyze_job_pending_count,
        analyze_job_pending_ids,
    };

    Ok(ToolOk::Value(result))
}

/// Return one addressable analyze job. The slot is read once so a displacement
/// cannot leave a lookup between `current` and terminal retention.
pub fn get_job_status(inner: Arc<ServerInner>, job_id: String) -> ToolResult<AnalyzeJobView> {
    if job_id.is_empty() {
        return Err(ToolError("'job_id' is required".to_string()));
    }
    let job = {
        let slot = inner.analyze_slot.read();
        slot.current
            .as_ref()
            .filter(|job| job.job_id == job_id)
            .cloned()
            .or_else(|| {
                slot.pending
                    .iter()
                    .map(|pending| &pending.job)
                    .find(|job| job.job_id == job_id)
                    .cloned()
            })
            .or_else(|| {
                slot.previous_terminal
                    .as_ref()
                    .filter(|job| job.job_id == job_id)
                    .cloned()
            })
            .or_else(|| {
                slot.terminal_history
                    .iter()
                    .find(|job| job.job_id == job_id)
                    .cloned()
            })
    };
    let Some(job) = job else {
        return Err(ToolError(format!("job not found or expired: {job_id:?}")));
    };
    Ok(ToolOk::Value(AnalyzeJobView::from_job(&job)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze_job::{AnalyzeJob, JobStatus, PendingAnalyze, TERMINAL_HISTORY_LIMIT};
    use crate::handlers::analyze::AnalyzeResult;
    use crate::server::CodeGraphServer;
    use code_graph_lang::LanguageRegistry;

    /// AC-28-adjacent sanity: `get_status` is ungated by design, so it
    /// must succeed with `indexed: false` before any `analyze_codebase`
    /// call rather than returning `Err(ToolError)`.
    #[test]
    fn status_before_any_analyze_is_ok_and_reports_unindexed() {
        let server = CodeGraphServer::new(LanguageRegistry::new());
        let result = get_status(server.inner.clone()).expect("get_status is ungated");
        match result {
            ToolOk::Value(status) => {
                assert!(!status.indexed);
                assert!(status.indexed_root.is_none());
                assert!(status.config_path.is_none());
                assert!(status.index_built_at.is_none());
                assert!(status.index_force_built.is_none());
                assert_eq!(status.index_files, 0);
                assert_eq!(status.index_symbols, 0);
                assert_eq!(status.index_edges, 0);
                assert_eq!(status.config_macro_strip_count, 0);
                assert_eq!(status.config_macro_strip_with_args_count, 0);
                assert!(status.analyze_job.is_none());
                assert!(status.analyze_job_previous_terminal.is_none());
                assert_eq!(status.analyze_job_pending_count, 0);
                assert!(status.analyze_job_pending_ids.is_empty());
            }
            ToolOk::Text(_) => panic!("expected Value(StatusResult)"),
        }
    }

    fn completed_job(id: &str) -> std::sync::Arc<AnalyzeJob> {
        let job = AnalyzeJob::new_running(id.into(), "/completed".into(), false, 1);
        let mut state = job.state.write();
        state.status = JobStatus::Completed(AnalyzeResult {
            files: 1,
            symbols: 1,
            edges: 0,
            root_path: "/completed".into(),
            warnings: Vec::new(),
            coalesced_by: None,
        });
        state.finished_at = Some(2);
        drop(state);
        job
    }

    #[test]
    fn get_job_status_finds_queued_running_and_retained_terminals() {
        let server = CodeGraphServer::new(LanguageRegistry::new());
        let running = AnalyzeJob::new_running("running".into(), "/running".into(), false, 1);
        let queued = AnalyzeJob::new_queued("queued".into(), "/queued".into(), false, 1);
        let completed = completed_job("completed");
        let failed = AnalyzeJob::new_running("failed".into(), "/failed".into(), false, 1);
        failed.state.write().status = JobStatus::Failed("broken".into());
        let guard = server.inner.persist.begin_analyze().unwrap();
        {
            let mut slot = server.inner.analyze_slot.write();
            slot.current = Some(running);
            slot.pending.push_back(PendingAnalyze {
                job: queued,
                analyze_guard: guard,
                sink: std::sync::Arc::new(crate::indexer::NoopProgressSink),
            });
            slot.previous_terminal = Some(failed);
            slot.terminal_history.push_back(completed);
        }
        for (id, expected) in [
            ("running", "running"),
            ("queued", "queued"),
            ("completed", "completed"),
            ("failed", "failed"),
        ] {
            let ToolOk::Value(view) = get_job_status(server.inner.clone(), id.into()).unwrap()
            else {
                panic!("get_job_status must return a view")
            };
            assert_eq!(view.status, expected);
        }
    }

    #[test]
    fn get_job_status_reports_empty_and_unknown_ids_exactly() {
        let server = CodeGraphServer::new(LanguageRegistry::new());
        assert!(matches!(
            get_job_status(server.inner.clone(), String::new()),
            Err(ToolError(message)) if message == "'job_id' is required"
        ));
        assert!(matches!(
            get_job_status(server.inner.clone(), "missing".into()),
            Err(ToolError(message)) if message == "job not found or expired: \"missing\""
        ));
    }

    #[test]
    fn terminal_history_evicts_only_the_oldest_terminal() {
        let server = CodeGraphServer::new(LanguageRegistry::new());
        {
            let mut slot = server.inner.analyze_slot.write();
            for n in 0..=TERMINAL_HISTORY_LIMIT {
                slot.terminal_history
                    .push_back(completed_job(&format!("terminal-{n}")));
            }
            while slot.terminal_history.len() > TERMINAL_HISTORY_LIMIT {
                slot.terminal_history.pop_front();
            }
        }
        assert!(get_job_status(server.inner.clone(), "terminal-0".into()).is_err());
        for n in 1..=TERMINAL_HISTORY_LIMIT {
            assert!(
                get_job_status(server.inner.clone(), format!("terminal-{n}")).is_ok(),
                "retained terminal {n} must remain addressable"
            );
        }
    }

    #[test]
    fn lookup_has_no_transient_gap_when_current_is_archived() {
        let server = CodeGraphServer::new(LanguageRegistry::new());
        let terminal = completed_job("atomic");
        {
            let mut slot = server.inner.analyze_slot.write();
            // Model the production displacement update under one slot write:
            // history receives the Arc before current is cleared.
            slot.current = Some(terminal.clone());
            slot.terminal_history.push_back(terminal);
            slot.current = None;
        }
        assert!(get_job_status(server.inner.clone(), "atomic".into()).is_ok());
    }
}
