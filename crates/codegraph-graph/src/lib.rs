//! In-memory code graph engine, algorithms, and persistence.
//!
//! Phase 2 ports the graph storage and queries from the Go reference at
//! `internal/graph/graph.go`. This crate is intentionally free of MCP / async
//! / I/O concerns — it is the unit-testable heart of the binary.
//!
//! Phase 2.1 delivers the storage shape (`Graph`, `Node`, `EdgeEntry`,
//! `FileEntry`, `GraphStats`) and the merge / remove / clear mutators.
//! Subsequent tasks add queries, BFS algorithms, Tarjan SCC, the
//! diamond-safe class hierarchy, coupling, and the Mermaid renderer.

mod algorithms;
mod callgraph;
mod diagrams;
pub mod graph;
mod queries;

#[cfg(test)]
mod test_fixtures;

pub use algorithms::HierarchyNode;
pub use callgraph::CallChain;
pub use diagrams::{DiagramEdge, DiagramResult};
pub use graph::{EdgeEntry, FileEntry, Graph, GraphStats, Node};
pub use queries::{SearchParams, SearchResult};

/// Re-export of [`parking_lot::RwLock`] so downstream callers (e.g. the
/// Phase-3 MCP server's `ServerInner`) can write `use codegraph_graph::RwLock`
/// without risking an accidental `std::sync::RwLock` import. The two types
/// have the same surface but different semantics: `parking_lot::RwLock` is
/// faster, doesn't poison on panic, and its `read()` / `write()` return guards
/// directly rather than `LockResult`. Task 2.6 establishes this as the
/// canonical lock type for `Graph`.
pub use parking_lot::RwLock;
