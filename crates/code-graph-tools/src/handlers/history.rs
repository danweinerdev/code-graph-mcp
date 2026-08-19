//! Wire adapter for the history tools. The typed body lives in
//! [`crate::core::history`]; this layer converts to `CallToolResult` only.

use std::path::PathBuf;
use std::sync::Arc;

use code_graph_graph::Graph;
use code_graph_vcs::VcsRegistry;
use parking_lot::RwLock;
use rmcp::model::CallToolResult;

use crate::server::ServerInner;

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

/// `symbol_history` adapter. Takes the whole `ServerInner` (unlike the
/// other history adapter) because the transition walk needs the language
/// registry inside its `spawn_blocking` closure, which requires an owned
/// `Arc` — the same shape `core::analyze` already uses.
pub async fn symbol_history(
    inner: &Arc<ServerInner>,
    symbol: &str,
    mode: Option<&str>,
    window: Option<u32>,
) -> CallToolResult {
    crate::core::to_call_tool_result(
        crate::core::history::symbol_history(inner, true, symbol, mode, window).await,
    )
}
