//! Shared test fixtures for the `codegraph-graph` test modules.
//!
//! Before this module existed, each `#[cfg(test)]` block in `graph.rs`,
//! `queries.rs`, `callgraph.rs`, and `algorithms.rs` carried its own copy
//! of `sym`, `call_edge`, `inherit_edge`, `include_edge`, and `make_fg`,
//! with diverging signatures (e.g. `include_edge` was 3 args in three files
//! and 2 args in `algorithms.rs`). The duplicates drift apart over time —
//! consolidating them here gives every test module the same builder shape
//! and one place to extend defaults.
//!
//! The module is gated behind `#[cfg(test)]` and exposes its helpers as
//! `pub(crate)` so they remain invisible to downstream crates and to the
//! release build.

use codegraph_core::{Edge, EdgeKind, FileGraph, Language, Symbol, SymbolKind};

/// Build a [`Symbol`] with sensible defaults for fields the test does not
/// care about (line/column = 1/0, empty namespace/parent, language = Cpp).
/// Reach for [`sym_full`] when a test needs to pin namespace, parent, or
/// language explicitly.
pub(crate) fn sym(name: &str, kind: SymbolKind, file: &str) -> Symbol {
    sym_full(name, kind, file, "", "", Language::Cpp)
}

/// Build a [`Symbol`] with every field explicit. Use this when the test
/// asserts on `namespace`, `parent`, or `language`; otherwise the simpler
/// [`sym`] is preferred.
pub(crate) fn sym_full(
    name: &str,
    kind: SymbolKind,
    file: &str,
    namespace: &str,
    parent: &str,
    language: Language,
) -> Symbol {
    Symbol {
        name: name.to_string(),
        kind,
        file: file.to_string(),
        line: 1,
        column: 0,
        end_line: 1,
        signature: String::new(),
        namespace: namespace.to_string(),
        parent: parent.to_string(),
        language,
    }
}

/// `Calls` edge from `from` to `to`, attributed to `file` at the given
/// `line`. The four-argument shape is the most general — tests that don't
/// care about the line number conventionally pass `1`.
pub(crate) fn call_edge(from: &str, to: &str, file: &str, line: u32) -> Edge {
    Edge {
        from: from.to_string(),
        to: to.to_string(),
        kind: EdgeKind::Calls,
        file: file.to_string(),
        line,
    }
}

/// `Inherits` edge from `from` to `to`, attributed to `file`. Inherits
/// edges in this codebase use bare derived/base names (Phase 1 quirk
/// preserved); see `EdgeKind::Inherits` and the `merge_routes_inherits_*`
/// tests for context.
pub(crate) fn inherit_edge(from: &str, to: &str, file: &str) -> Edge {
    Edge {
        from: from.to_string(),
        to: to.to_string(),
        kind: EdgeKind::Inherits,
        file: file.to_string(),
        line: 0,
    }
}

/// `Includes` edge from `from` to `to`, attributed to `file`. The Tarjan
/// SCC tests typically pass `from` as the file because the include map is
/// keyed by source file.
pub(crate) fn include_edge(from: &str, to: &str, file: &str) -> Edge {
    Edge {
        from: from.to_string(),
        to: to.to_string(),
        kind: EdgeKind::Includes,
        file: file.to_string(),
        line: 0,
    }
}

/// Construct a [`FileGraph`] with explicit `language`. Tests in the
/// `algorithms.rs` / `callgraph.rs` modules historically defaulted to Cpp
/// and dropped the language argument; the canonical signature here keeps
/// language explicit so language-sensitive query tests (Phase 2.2's
/// `search_language_filter`) work the same way.
pub(crate) fn make_fg(
    path: &str,
    language: Language,
    symbols: Vec<Symbol>,
    edges: Vec<Edge>,
) -> FileGraph {
    FileGraph {
        path: path.to_string(),
        language,
        symbols,
        edges,
    }
}
