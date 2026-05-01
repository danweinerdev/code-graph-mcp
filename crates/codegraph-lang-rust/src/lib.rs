//! Rust language plugin for code-graph-mcp.
//!
//! Uses tree-sitter (via the `tree-sitter` and `tree-sitter-rust` crates)
//! to extract symbols, calls, use-declarations, and trait-impl edges from
//! Rust source files.
//!
//! # Phase status
//!
//! Phase 5.1 ships the crate scaffold: dependency wiring, query strings
//! that compile against tree-sitter-rust 0.24.x, the `RustParser` struct
//! with cached `Query` objects, and the `LanguagePlugin` impl with a
//! stubbed `parse_file` that returns an empty `FileGraph`. The four
//! `extract_*` loops are filled in by:
//!
//! - **Phase 5.2** — definition extraction with impl context and
//!   trait-impl disambiguation
//! - **Phase 5.3** — use-tree expansion (recursive walk for grouped /
//!   wildcard / aliased imports) and `extern crate`
//! - **Phase 5.4** — call extraction (direct, method, scoped, macro) and
//!   inheritance edges (`impl Trait for Type`)
//!
//! # Known Rust parser limitations (will apply once Phase 5.2-5.4 land)
//!
//! These match the documented design and apply to the Rust parser as it is
//! built out. They are intentional, not bugs.
//!
//! 1. **`macro_rules!` definitions are not extracted as symbols.** Only
//!    invocations produce call edges. The `DEFINITION_QUERIES` constant
//!    explicitly does not match `macro_rules_definition`.
//! 2. **`#[derive(...)]` and proc-macro attributes** appear as
//!    `attribute_item` (not `macro_invocation`) so they are NOT captured
//!    as call edges.
//! 3. **Call resolution is heuristic** — same-file > same-parent >
//!    same-namespace > global, identical to the C++ plugin's behavior via
//!    the default `LanguagePlugin::resolve_call` impl.
//! 4. **Complex use trees expanded but lifetime/generic constraints not
//!    represented.** Use-edge `to` fields record the dotted path; generic
//!    parameters and lifetime bounds are not part of the edge.

pub(crate) mod helpers;
pub(crate) mod queries;

use std::path::Path;

use codegraph_core::{FileGraph, Language};
use codegraph_lang::{LanguagePlugin, ParseError};
use tree_sitter::{Language as TsLanguage, Query};

use crate::queries::{CALL_QUERIES, DEFINITION_QUERIES, INHERITANCE_QUERIES, USE_QUERIES};

/// File extensions the Rust parser claims.
pub const EXTENSIONS: &[&str] = &[".rs"];

/// Rust source-file parser. Holds the tree-sitter `Language` and the four
/// pre-compiled queries used to drive symbol/edge extraction in Phases
/// 5.2-5.4.
///
/// Construct with [`RustParser::new`]; share across threads (queries are
/// `Send + Sync`).
pub struct RustParser {
    /// Compiled Rust grammar. Held so per-call [`tree_sitter::Parser`]
    /// instances built inside `parse_file` can attach to it without
    /// rebuilding the `LanguageFn`.
    #[allow(dead_code)] // wired in Phase 5.2
    language: TsLanguage,
    /// Compiled definition query.
    #[allow(dead_code)] // wired in Phase 5.2
    def_query: Query,
    /// Compiled call query.
    #[allow(dead_code)] // wired in Phase 5.4
    call_query: Query,
    /// Compiled use-declaration query.
    #[allow(dead_code)] // wired in Phase 5.3
    use_query: Query,
    /// Compiled inheritance / trait-impl query.
    #[allow(dead_code)] // wired in Phase 5.4
    inh_query: Query,
}

impl RustParser {
    /// Build a new parser, compiling all four tree-sitter queries against
    /// the pinned tree-sitter-rust grammar. Returns an
    /// [`anyhow::Error`] (wrapping the query compiler's message) if any
    /// query fails to compile against the pinned grammar version.
    ///
    /// Successful return is the Phase 5.1 acceptance gate that proves
    /// every query string in `queries.rs` parses against
    /// tree-sitter-rust 0.24.x.
    pub fn new() -> anyhow::Result<Self> {
        let language: TsLanguage = tree_sitter_rust::LANGUAGE.into();

        let def_query = Query::new(&language, DEFINITION_QUERIES)
            .map_err(|e| anyhow::anyhow!("definition query: {e}"))?;
        let call_query =
            Query::new(&language, CALL_QUERIES).map_err(|e| anyhow::anyhow!("call query: {e}"))?;
        let use_query =
            Query::new(&language, USE_QUERIES).map_err(|e| anyhow::anyhow!("use query: {e}"))?;
        let inh_query = Query::new(&language, INHERITANCE_QUERIES)
            .map_err(|e| anyhow::anyhow!("inheritance query: {e}"))?;

        Ok(Self {
            language,
            def_query,
            call_query,
            use_query,
            inh_query,
        })
    }

    /// File extensions handled by this plugin. Exposed as an associated
    /// function so the trait implementation and external callers (e.g.
    /// CLI argument parsing) share the single source of truth.
    pub fn extensions() -> &'static [&'static str] {
        EXTENSIONS
    }
}

impl LanguagePlugin for RustParser {
    fn id(&self) -> Language {
        Language::Rust
    }

    fn extensions(&self) -> &'static [&'static str] {
        EXTENSIONS
    }

    /// Parse a Rust source file.
    ///
    /// Phase 5.1 returns an empty [`FileGraph`] populated only with `path`
    /// and `language`. Phases 5.2/5.3/5.4 fill in symbols and edges via
    /// the cached `def_query` / `use_query` / `call_query` / `inh_query`
    /// drivers.
    fn parse_file(&self, path: &Path, _content: &[u8]) -> Result<FileGraph, ParseError> {
        // TODO(Phase 5.2/5.3/5.4): extract definitions / uses / calls /
        // inheritance via the cached queries.
        Ok(FileGraph {
            path: path.to_string_lossy().into_owned(),
            language: Language::Rust,
            symbols: Vec::new(),
            edges: Vec::new(),
        })
    }

    // resolve_call and resolve_include intentionally NOT overridden:
    // - default resolve_call is the scope-aware heuristic used by the C++
    //   plugin and is the right baseline for Rust.
    // - default resolve_include is a basename match against the FileIndex,
    //   which is a no-op for Rust `use` paths because they are dotted
    //   module paths, not filesystem paths. The wire format records the
    //   full `use` path as the edge's `to` field; leaving it unresolved is
    //   the intended behavior.

    fn close(&self) {}
}

#[cfg(test)]
mod tests {
    //! Structural smoke tests for Phase 5.1.
    //!
    //! Behavioral coverage (definitions / uses / calls / inheritance) lands
    //! in Phases 5.2-5.4 alongside the corresponding `extract_*` loops.
    use super::*;

    #[test]
    fn new_compiles_all_four_queries() {
        // The whole point of Phase 5.1: every query string parses against
        // the pinned tree-sitter-rust. Failure here means a query needs
        // updating.
        let p = RustParser::new().expect("RustParser::new must succeed");
        let _ = (
            &p.language,
            &p.def_query,
            &p.call_query,
            &p.use_query,
            &p.inh_query,
        );
    }

    #[test]
    fn extensions_match_expected_list() {
        assert_eq!(RustParser::extensions(), &[".rs"]);
        let p = RustParser::new().unwrap();
        assert_eq!(LanguagePlugin::extensions(&p), RustParser::extensions());
    }

    #[test]
    fn id_is_rust() {
        let p = RustParser::new().unwrap();
        assert_eq!(p.id(), Language::Rust);
    }

    #[test]
    fn rust_parser_is_object_safe_via_box_dyn() {
        let p: Box<dyn LanguagePlugin> = Box::new(RustParser::new().unwrap());
        assert_eq!(p.id(), Language::Rust);
    }

    #[test]
    fn parse_file_returns_correct_path_and_language_with_empty_graph() {
        // Phase 5.1 stub: parse_file returns an empty FileGraph. Phases
        // 5.2/5.3/5.4 will populate symbols and edges.
        let p = RustParser::new().unwrap();
        let path = Path::new("/tmp/test.rs");
        let fg = p.parse_file(path, b"fn foo() {}").unwrap();
        assert_eq!(fg.path, "/tmp/test.rs");
        assert_eq!(fg.language, Language::Rust);
        assert!(
            fg.symbols.is_empty(),
            "Phase 5.1 stub: symbols populated in 5.2"
        );
        assert!(
            fg.edges.is_empty(),
            "Phase 5.1 stub: edges populated in 5.3/5.4"
        );
    }
}
