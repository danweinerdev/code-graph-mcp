//! Wire adapter for the history tools. The typed body lives in
//! [`crate::core::history`]; this layer converts to `CallToolResult` only.

use std::path::PathBuf;

use code_graph_graph::Graph;
use code_graph_vcs::VcsRegistry;
use parking_lot::RwLock;
use rmcp::model::CallToolResult;

/// `blame_symbol` adapter. The server-side `require_indexed` gate has
/// already run; the core body re-checks with its own guard (Decision 8
/// set-equality invariant), so `indexed` is passed as `true` here exactly
/// like the other migrated handlers.
pub async fn blame_symbol(
    graph: &RwLock<Graph>,
    vcs: &VcsRegistry,
    root: Option<PathBuf>,
    symbol: &str,
    at: Option<&str>,
) -> CallToolResult {
    crate::core::to_call_tool_result(
        crate::core::history::blame_symbol(graph, vcs, true, root, symbol, at).await,
    )
}
