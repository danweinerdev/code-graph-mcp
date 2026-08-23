//! Deliberately independent consumer of the public typed core.
//!
//! This manifest has no `rmcp` dependency: importing it here would defeat the
//! compile-boundary check that owns this fixture.

use code_graph_graph::{CallChain, DiagramEdge, Graph};
use code_graph_tools::core::{self, ToolError, ToolOk, ToolResult};
use code_graph_tools::indexer::ProgressSink;
use code_graph_tools::ServerInner;
use code_graph_vcs::VcsRegistry;
use parking_lot::RwLock;
use std::sync::Arc;

#[allow(dead_code)]
async fn analyze_signatures(inner: Arc<ServerInner>, sink: Arc<dyn ProgressSink>) {
    let _: ToolResult<core::analyze::AnalyzeResult> =
        core::analyze::analyze_codebase(inner.clone(), String::new(), false, sink).await;
    let _: ToolResult<core::analyze::AsyncKickoffResponse> =
        core::analyze::analyze_codebase_async(inner, String::new(), false).await;
}

#[allow(dead_code)]
async fn history_signatures(graph: &RwLock<Graph>, vcs: &VcsRegistry, inner: &Arc<ServerInner>) {
    let _: ToolResult<core::history::BlameSymbolResponse> =
        core::history::blame_symbol(graph, vcs, false, None, "", None).await;
    let _: ToolResult<core::history::SymbolHistoryResponse> =
        core::history::symbol_history(inner, false, "", None, None).await;

    let _: Option<core::history::BlameHunkRow> = None;
    let _: Option<core::history::SymbolHistoryEntry> = None;
    let _: Option<core::history::SkippedRevision> = None;
}

#[allow(dead_code)]
fn query_signatures(graph: &RwLock<Graph>) {
    let _: ToolResult<core::query::CallChainResponse> = core::query::callers_or_callees(
        graph,
        false,
        "",
        None,
        core::query::CallDirection::Callers,
        None,
        None,
        0,
        None,
    );
    let _: ToolResult<core::query::Page<CallChain>> =
        core::query::find_overrides(graph, false, "", None, None, 0);
    let _: ToolResult<core::query::Page<core::query::DependencyEntry>> =
        core::query::get_dependencies(graph, false, "", None, None, 0);
    let _: ToolResult<core::query::FindPathResponse> =
        core::query::find_path(graph, false, "", "", None, None);
}

#[allow(dead_code)]
fn status_signatures(inner: Arc<ServerInner>) {
    let _: ToolResult<core::status::StatusResult> = core::status::get_status(inner.clone());
    let _: ToolResult<core::status::AnalyzeJobView> =
        core::status::get_analyze_status(inner, String::new());
}

#[allow(dead_code)]
fn structure_signatures(graph: &RwLock<Graph>) {
    let _: ToolResult<core::structure::Page<core::structure::Cycle>> =
        core::structure::detect_cycles(graph, false, None, None, None, None);
    let _: ToolResult<core::structure::Page<core::structure::SymbolResult>> =
        core::structure::get_orphans(graph, false, None, None, None, None, None, false, None, 0);
    let _: ToolResult<core::structure::ClassHierarchyResponse> =
        core::structure::get_class_hierarchy(graph, false, "", None, None);
    let _: ToolResult<Vec<core::structure::SymbolResult>> =
        core::structure::find_class_candidates(graph, false, "");
    let _: ToolResult<core::structure::CouplingResult> =
        core::structure::get_coupling(graph, false, "", None, None, None, 0);

    let input = core::structure::DiagramInput {
        symbol: None,
        file: None,
        class: None,
        depth: None,
        max_nodes: None,
        format: None,
        styled: false,
        direction: None,
        min_confidence: None,
    };
    let _: ToolResult<Vec<DiagramEdge>> = core::structure::generate_diagram(graph, false, input);
    let _: ToolResult<core::structure::DetectCommunitiesResponse> =
        core::structure::detect_communities(graph, false, None, None, None, None, None, 0);

    let _: Option<core::structure::Community> = None;
    let _: Option<core::structure::CouplingBoth> = None;
    let _: Option<core::structure::CouplingEntry> = None;
    let _: Option<core::structure::DegenerateInfo> = None;
}

#[allow(dead_code)]
fn symbol_signatures(graph: &RwLock<Graph>) {
    let _: ToolResult<core::symbols::Page<core::symbols::SymbolResult>> =
        core::symbols::get_file_symbols(graph, false, "", false, false, None, None, false, 0);
    let _: ToolResult<core::symbols::Page<core::symbols::EnclosingSymbol>> =
        core::symbols::get_symbol_at(graph, false, "", 1, None, None, 0);
    let input = core::symbols::SearchInput {
        query: None,
        kind: None,
        namespace: None,
        language: None,
        subtree: None,
        limit: None,
        offset: None,
        brief: true,
        count_only: false,
        near: false,
        max_distance: None,
    };
    let _: ToolResult<core::symbols::SearchSymbolsResponse> =
        core::symbols::search_symbols(graph, false, input, 0);
    let _: ToolResult<core::symbols::SymbolResult> =
        core::symbols::get_symbol_detail(graph, false, "");
    let _: ToolResult<core::symbols::Page<core::symbols::SummaryRow>> =
        core::symbols::get_symbol_summary(graph, false, None, None, None, false, 0);
}

#[allow(dead_code)]
fn watch_signatures(inner: &Arc<ServerInner>) {
    let _: ToolResult<core::watch::WatchResponse> = core::watch::watch_start(inner);
    let _: ToolResult<core::watch::WatchResponse> = core::watch::watch_stop(inner);
}

fn main() {
    // Shared typed result family remains usable without importing `rmcp`.
    let _: ToolResult<()> = Ok(ToolOk::Value(()));
    let _: ToolError = ToolError(String::new());
}
