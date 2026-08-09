//! Typed core for `get_status`.
//!
//! `get_status` is UNGATED (Design Decision 8 / plan task 2.2 notes): it
//! must answer before any `analyze_codebase` has run, so unlike
//! `core::watch`, there is no `require_indexed` call here.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::core::{ToolOk, ToolResult};
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
