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
pub mod graph;
mod queries;

pub use algorithms::HierarchyNode;
pub use callgraph::CallChain;
pub use graph::{EdgeEntry, FileEntry, Graph, GraphStats, Node};
pub use queries::{SearchParams, SearchResult};
