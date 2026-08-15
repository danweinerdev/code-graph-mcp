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

    // Snapshot the slot under the read lock — just two Arc::clones —
    // then drop the guard before walking the job state. Building views
    // outside the slot lock keeps progress writes from contending with
    // polls beyond the constant-time Arc::clone window.
    let (current_job, previous_terminal_job) = {
        let slot = inner.analyze_slot.read();
        (slot.current.clone(), slot.previous_terminal.clone())
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
    };

    Ok(ToolOk::Value(result))
}

/// Return the canonical status satisfying one async analyze request. This is
/// deliberately analyze-specific rather than a generic job lookup: aliases
/// exist only for absorbed or displaced analyze requests.
pub fn get_analyze_status(inner: Arc<ServerInner>, job_id: String) -> ToolResult<AnalyzeJobView> {
    let job = {
        let slot = inner.analyze_slot.read();
        slot.resolve_async_job(&job_id)
    }
    .ok_or_else(|| ToolError(format!("analyze job not found or expired: {job_id:?}")))?;
    Ok(ToolOk::Value(AnalyzeJobView::from_job(&job)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze_job::{AnalyzeJob, JobStatus};
    use crate::core::analyze::{finish_completed, finish_failed};
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
            }
            ToolOk::Text(_) => panic!("expected Value(StatusResult)"),
        }
    }

    #[test]
    fn analyze_status_polls_canonical_and_alias_then_expires_with_grace_window() {
        let server = CodeGraphServer::new(LanguageRegistry::new());
        let canonical = AnalyzeJob::new_running("canonical".into(), "/queue/root".into(), false, 0);
        let alias = "follower".to_string();
        {
            let mut slot = server.inner.analyze_slot.write();
            slot.current = Some(Arc::clone(&canonical));
            slot.aliases.insert(alias.clone(), canonical.job_id.clone());
        }

        for handle in [canonical.job_id.clone(), alias.clone()] {
            let ToolOk::Value(view) = get_analyze_status(Arc::clone(&server.inner), handle)
                .expect("canonical and alias handles must poll")
            else {
                panic!("analyze status must be structured")
            };
            assert_eq!(view.job_id, canonical.job_id);
            assert_eq!(view.status, "running");
        }

        finish_completed(
            &canonical,
            AnalyzeResult {
                files: 1,
                symbols: 1,
                edges: 0,
                root_path: "/queue/root".into(),
                warnings: Vec::new(),
            },
        );
        let ToolOk::Value(terminal) = get_analyze_status(Arc::clone(&server.inner), alias.clone())
            .expect("terminal alias stays pollable during the grace window")
        else {
            panic!("analyze status must be structured")
        };
        assert_eq!(terminal.job_id, canonical.job_id);
        assert_eq!(terminal.status, "completed");
        assert!(terminal.result.is_some());

        let next = AnalyzeJob::new_running("next".into(), "/queue/next".into(), false, 1);
        let newer = AnalyzeJob::new_running("newer".into(), "/queue/newer".into(), false, 2);
        {
            let mut slot = server.inner.analyze_slot.write();
            slot.previous_terminal = slot.current.replace(next);
            let prior_current = slot.current.replace(newer).unwrap();
            let expired = slot.previous_terminal.replace(prior_current);
            slot.discard_aliases_for(&expired.expect("canonical is previous terminal").job_id);
        }
        let error = match get_analyze_status(server.inner.clone(), alias) {
            Ok(_) => panic!("alias expires when its canonical terminal leaves grace"),
            Err(error) => error,
        };
        assert!(error.0.contains("not found or expired"));
    }

    #[test]
    fn analyze_status_alias_preserves_terminal_failure_attribution() {
        let server = CodeGraphServer::new(LanguageRegistry::new());
        let canonical = AnalyzeJob::new_running("failed".into(), "/queue/fail".into(), false, 0);
        {
            let mut slot = server.inner.analyze_slot.write();
            slot.current = Some(Arc::clone(&canonical));
            slot.aliases
                .insert("alias".into(), canonical.job_id.clone());
        }
        finish_failed(&canonical, "expected failure".into());

        let ToolOk::Value(view) = get_analyze_status(server.inner.clone(), "alias".into())
            .expect("failed canonical remains addressable through alias")
        else {
            panic!("analyze status must be structured")
        };
        assert_eq!(view.job_id, canonical.job_id);
        assert_eq!(view.status, "failed");
        assert_eq!(view.error.as_deref(), Some("expected failure"));
        assert!(view.result.is_none());
        assert!(matches!(
            canonical.state.read().status,
            JobStatus::Failed(_)
        ));
    }
}
