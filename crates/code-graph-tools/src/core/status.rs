//! Typed core for `get_status`.
//!
//! `get_status` is UNGATED (Design Decision 8 / plan task 2.2 notes): it
//! must answer before any `analyze_codebase` has run, so unlike
//! `core::watch`, there is no `require_indexed` call here.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::analyze_job::JobKind;
use crate::core::{ToolError, ToolOk, ToolResult};
use crate::handlers::status::{format_unix_nanos_rfc3339, AnalyzeJobView, JobView, StatusResult};
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

    // Root, config provenance, and config counts come from one successful
    // analyze snapshot. Do not probe `.code-graph.toml` here: creation or
    // removal after indexing must not rewrite the provenance of the active
    // graph, and queued/failed analyzes must not replace it either.
    // Lock order is status_publication -> graph/applied-index. Successful
    // analyze and watch mutations take the matching write guard, so this
    // snapshot cannot mix a newly replaced graph with prior provenance.
    let (
        config_path,
        indexed_root,
        macro_strip_count,
        macro_strip_with_args_count,
        indexed,
        stats,
        index_built_at,
        index_force_built,
    ) = {
        let _publication = inner.status_publication.read();
        let applied = inner.applied_index.read();
        let config_path = applied
            .config_path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        let indexed_root = applied
            .root_path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        let macro_strip_count = applied.config.cpp.macro_strip.len();
        let macro_strip_with_args_count = applied.config.cpp.macro_strip_with_args.len();
        let indexed = inner.indexed.load(Ordering::Acquire);
        let stats = inner.graph.read().stats();
        let built_at_nanos = inner.index_built_at.load(Ordering::Acquire);
        let index_built_at =
            (built_at_nanos != 0).then(|| format_unix_nanos_rfc3339(built_at_nanos));
        let index_force_built = indexed.then(|| inner.index_force_built.load(Ordering::Acquire));
        (
            config_path,
            indexed_root,
            macro_strip_count,
            macro_strip_with_args_count,
            indexed,
            stats,
            index_built_at,
            index_force_built,
        )
    };

    // Snapshot the slot under the read lock — job Arcs plus the FIFO IDs —
    // then drop the guard before walking the job state. Building views
    // outside the slot lock keeps progress writes from contending with
    // polls beyond the constant-time Arc::clone window.
    let (current_job, previous_terminal_job, terminal_history, pending) = {
        let slot = inner.analyze_slot.read();
        (
            slot.current.clone(),
            slot.previous_terminal.clone(),
            slot.terminal_history.iter().cloned().collect::<Vec<_>>(),
            slot.pending
                .iter()
                .map(|pending| Arc::clone(&pending.job))
                .collect::<Vec<_>>(),
        )
    };
    let job = current_job.as_deref().map(JobView::from_job);
    let job_previous_terminal = previous_terminal_job.as_deref().map(JobView::from_job);
    let job_pending_count = u32::try_from(pending.len()).unwrap_or(u32::MAX);
    let job_pending_ids = pending.iter().map(|job| job.job_id.clone()).collect();
    let analyze_job = current_job
        .as_deref()
        .filter(|job| job.kind == JobKind::Analyze)
        .map(AnalyzeJobView::from_job);
    // Compatibility projection is intentionally not merely the generic
    // one-rotation terminal: community jobs may intervene between analyses.
    // Retention is oldest-first, so scan newest-first after the immediate
    // generic terminal to locate the latest retained analyze terminal.
    let analyze_job_previous_terminal = previous_terminal_job
        .iter()
        .chain(terminal_history.iter().rev())
        .find(|job| job.kind == JobKind::Analyze)
        .map(|job| AnalyzeJobView::from_job(job));
    let analyze_job_pending_count = u32::try_from(
        pending
            .iter()
            .filter(|job| job.kind == JobKind::Analyze)
            .count(),
    )
    .unwrap_or(u32::MAX);
    let analyze_job_pending_ids = pending
        .iter()
        .filter(|job| job.kind == JobKind::Analyze)
        .map(|job| job.job_id.clone())
        .collect();

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
        job,
        job_previous_terminal,
        job_pending_count,
        job_pending_ids,
        analyze_job,
        analyze_job_previous_terminal,
        analyze_job_pending_count,
        analyze_job_pending_ids,
    };

    Ok(ToolOk::Value(result))
}

/// Return one addressable analyze job. The slot is read once so a displacement
/// cannot leave a lookup between `current` and terminal retention.
pub fn get_job_status(inner: Arc<ServerInner>, job_id: String) -> ToolResult<JobView> {
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
    Ok(ToolOk::Value(JobView::from_job(&job)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze_job::{
        Job, JobKind, JobRequest, JobStatus, PendingJob, TERMINAL_HISTORY_LIMIT,
    };
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

    fn completed_job(id: &str) -> std::sync::Arc<Job> {
        let job = Job::new_running(id.into(), "/completed".into(), false, 1);
        let mut state = job.state.write();
        state.status =
            JobStatus::Completed(crate::analyze_job::JobResult::Analyze(AnalyzeResult {
                files: 1,
                symbols: 1,
                edges: 0,
                root_path: "/completed".into(),
                warnings: Vec::new(),
                coalesced_by: None,
            }));
        state.finished_at = Some(2);
        drop(state);
        job
    }

    #[test]
    fn get_job_status_finds_queued_running_and_retained_terminals() {
        let server = CodeGraphServer::new(LanguageRegistry::new());
        let running = Job::new_running("running".into(), "/running".into(), false, 1);
        let queued = Job::new_queued("queued".into(), "/queued".into(), false, 1);
        let completed = completed_job("completed");
        let failed = Job::new_running("failed".into(), "/failed".into(), false, 1);
        failed.state.write().status = JobStatus::Failed("broken".into());
        let guard = server.inner.persist.begin_analyze().unwrap();
        {
            let mut slot = server.inner.analyze_slot.write();
            slot.current = Some(running);
            slot.pending.push_back(PendingJob {
                job: queued,
                job_guard: guard,
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

    fn community_job(id: &str) -> std::sync::Arc<Job> {
        Job::new_running_communities(
            id.into(),
            JobRequest::DetectCommunities {
                granularity: None,
                max_iterations: None,
                members_per_community: None,
                limit: None,
                offset: None,
            },
            1,
            1024,
        )
    }

    #[test]
    fn analyze_previous_terminal_skips_intervening_community_job() {
        let server = CodeGraphServer::new(LanguageRegistry::new());
        let analyze_previous = completed_job("analyze-previous");
        let community_previous = community_job("community-previous");
        community_previous.state.write().status = JobStatus::Failed("community failed".into());
        let analyze_current =
            Job::new_running("analyze-current".into(), "/current".into(), true, 3);
        {
            let mut slot = server.inner.analyze_slot.write();
            slot.current = Some(analyze_current);
            slot.previous_terminal = Some(community_previous);
            slot.terminal_history.push_back(analyze_previous);
        }

        let ToolOk::Value(status) = get_status(server.inner.clone()).unwrap() else {
            panic!("status must return a value")
        };
        assert_eq!(
            status.job_previous_terminal.as_ref().map(|job| job.kind),
            Some(JobKind::DetectCommunities),
            "generic projection remains the immediate generic terminal"
        );
        assert_eq!(
            status
                .analyze_job_previous_terminal
                .as_ref()
                .map(|job| job.job_id.as_str()),
            Some("analyze-previous"),
            "analyze compatibility projection finds the latest retained analyze terminal"
        );
    }

    #[test]
    fn community_current_is_generic_only_but_keeps_analyze_terminal_projection() {
        let server = CodeGraphServer::new(LanguageRegistry::new());
        let analyze_previous = completed_job("analyze-previous");
        let community_current = community_job("community-current");
        {
            let mut slot = server.inner.analyze_slot.write();
            slot.current = Some(community_current);
            slot.previous_terminal = Some(analyze_previous);
        }

        let ToolOk::Value(status) = get_status(server.inner.clone()).unwrap() else {
            panic!("status must return a value")
        };
        assert_eq!(
            status.job.as_ref().map(|job| job.kind),
            Some(JobKind::DetectCommunities)
        );
        assert!(status.analyze_job.is_none());
        assert_eq!(
            status
                .analyze_job_previous_terminal
                .as_ref()
                .map(|job| job.job_id.as_str()),
            Some("analyze-previous")
        );
    }

    #[test]
    fn generic_job_view_preserves_analyze_top_level_field_order() {
        let job = Job::new_running("job".into(), "/path".into(), true, 1);
        let json = serde_json::to_string(&JobView::from_job(&job)).unwrap();
        let expected_prefix = r#"{"job_id":"job","status":"running","path":"/path","force":true,"started_at":"1970-01-01T00:00:00Z","finished_at":null,"progress":0,"progress_total":0,"progress_message":"","error":null,"result":null,"current_phase":null,"kind":"analyze""#;
        assert!(
            json.starts_with(expected_prefix),
            "analyze fields must remain top-level and ordered before additive generic fields: {json}"
        );
    }
}
