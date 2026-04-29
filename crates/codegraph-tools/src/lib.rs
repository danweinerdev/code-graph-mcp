//! MCP tool handlers, discovery walker, indexer, and watcher.
//!
//! Phase 3 entry point: this crate hosts everything that wires the Phase 1
//! C++ parser and Phase 2 graph engine into a running MCP server. Submodule
//! responsibilities:
//!
//! - [`server`] — `CodeGraphServer`, `ServerInner`, the 15-tool dispatch
//!   table, and `require_indexed`. (Phase 3.1)
//! - [`discovery`] — parallel filesystem walker. (Phase 3.2)
//! - [`indexer`] — per-job rayon parsing pool, edge resolution, progress
//!   reporting trait. (Phase 3.3, with [`indexer::ProgressSink`] shipped
//!   here in Phase 3.1 so 3.2 and 3.3 can both depend on it without
//!   circularity.)

pub mod discovery;
pub mod indexer;
pub mod server;

pub use server::{CodeGraphServer, ServerInner, WatchHandle};
