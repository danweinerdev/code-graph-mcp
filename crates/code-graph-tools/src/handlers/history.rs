//! Wire adapter for the history tools. The typed body lives in
//! [`crate::core::history`]; this layer converts to `CallToolResult` only.

use std::path::PathBuf;
use std::sync::Arc;

use code_graph_graph::Graph;
use code_graph_vcs::VcsRegistry;
use parking_lot::RwLock;
use rmcp::model::CallToolResult;

use crate::server::ServerInner;

/// Thin MCP adapter for [`crate::core::history::blame_symbol`].
pub async fn blame_symbol(
    graph: &RwLock<Graph>,
    indexed: bool,
    vcs: &VcsRegistry,
    root: Option<PathBuf>,
    symbol: &str,
    at: Option<&str>,
) -> CallToolResult {
    crate::core::to_call_tool_result(
        crate::core::history::blame_symbol(graph, vcs, indexed, root, symbol, at).await,
    )
}

/// Thin MCP adapter for [`crate::core::history::symbol_history`].
pub async fn symbol_history(
    inner: &Arc<ServerInner>,
    symbol: &str,
    mode: Option<&str>,
    window: Option<u32>,
) -> CallToolResult {
    let indexed = inner.indexed.load(std::sync::atomic::Ordering::Acquire);
    crate::core::to_call_tool_result(
        crate::core::history::symbol_history(inner, indexed, symbol, mode, window).await,
    )
}
