//! MCP tool handlers, discovery walker, indexer, and watcher.
//!
//! This crate wires the language parsers and the graph engine into a
//! running MCP server. Submodule responsibilities:
//!
//! - [`server`] — `CodeGraphServer`, `ServerInner`, the 15-tool dispatch
//!   table, and `require_indexed`.
//! - [`discovery`] — parallel filesystem walker.
//! - [`indexer`] — per-job rayon parsing pool, edge resolution, progress
//!   reporting trait, and the tokio bridge sink.

pub mod discovery;
pub mod handlers;
pub mod indexer;
pub mod server;

pub use server::{CodeGraphServer, ServerInner, WatchHandle};
