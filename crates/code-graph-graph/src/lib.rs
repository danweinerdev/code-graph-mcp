//! In-memory code graph engine, algorithms, and persistence.
//!
//! Ports the graph storage and queries from the Go reference at
//! `internal/graph/graph.go`. This crate is intentionally free of MCP / async
//! / I/O concerns — it is the unit-testable heart of the binary.
//!
//! It provides the storage shape (`Graph`, `Node`, `EdgeEntry`,
//! `FileEntry`, `GraphStats`) and the merge / remove / clear mutators,
//! plus queries, BFS algorithms, Tarjan SCC, the diamond-safe class
//! hierarchy, coupling, and the Mermaid renderer.

mod algorithms;
mod callgraph;
mod diagrams;
pub mod graph;
pub mod persist;
mod queries;

#[cfg(test)]
mod test_fixtures;

pub use algorithms::HierarchyNode;
pub use callgraph::CallChain;
pub use diagrams::{DiagramDirection, DiagramEdge, DiagramResult, EdgeDirection};
pub use graph::{EdgeEntry, FileEntry, Graph, GraphStats, IncludeEntry, Node};
pub use persist::{cache_path, stale_paths, PersistError, SWEEP_INTERVAL_NANOS};
pub use queries::{SearchParams, SearchResult};

/// Re-export of [`parking_lot::RwLock`] so downstream callers (e.g. the
/// MCP server's `ServerInner`) can write `use code_graph_graph::RwLock`
/// without risking an accidental `std::sync::RwLock` import. The two types
/// have the same surface but different semantics: `parking_lot::RwLock` is
/// faster, doesn't poison on panic, and its `read()` / `write()` return guards
/// directly rather than `LockResult`. This is the canonical lock type for
/// the server-side `Graph`.
pub use parking_lot::RwLock;
