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
//!
//! # Unsafe code allowance
//!
//! The workspace lint `unsafe_code = "forbid"` is overridden here at
//! crate scope to allow exactly one boundary: `memmap2::Mmap::map` in
//! `persist::packed::mmap`. Mmap is unavoidable for the v7 cache's
//! zero-copy load path — no safe mmap wrapper exists in the Rust
//! ecosystem because the OS can invalidate the mapping under
//! concurrent file modification. The site is isolated to a single
//! function with a documented `// SAFETY:` block covering the
//! atomic-rename write contract that keeps the mapped inode stable
//! for the mmap's lifetime. See `.plans/Designs/PackedCache/README.md`
//! Decision 5 for the full rationale.
#![allow(unsafe_code)]

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
