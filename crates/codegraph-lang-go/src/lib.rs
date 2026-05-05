//! Go language plugin for code-graph-mcp.
//!
//! Uses tree-sitter (via the `tree-sitter` and `tree-sitter-go` crates) to
//! extract symbols, calls, and import edges from Go source files.
//!
//! # Phase status
//!
//! Phase 6.1 ships the crate scaffold: dependency wiring, query strings that
//! compile against tree-sitter-go 0.25, the `GoParser` struct with cached
//! `Query` objects, and the `LanguagePlugin` impl with a stubbed `parse_file`
//! that returns an empty `FileGraph`.
//!
//! Phase 6.2 wires `extract_definitions` (function/method/type-spec/type_alias
//! with method-receiver-as-parent and package-clause-as-namespace).
//! Phase 6.3 wires `extract_calls` (direct and selector_expression calls).
//! Phase 6.4 wires `extract_imports` (single, grouped, aliased, dot, blank).
//! After 6.4, `parse_file` is fully populated and every extractor is live.
//!
//! # Default trait methods
//!
//! `GoParser` does NOT override [`LanguagePlugin::resolve_call`] or
//! [`LanguagePlugin::resolve_include`].
//!
//! - `resolve_call`: the default scope-aware heuristic (same file > same
//!   parent > same namespace > global) is the right baseline for Go and
//!   matches the C++ and Rust plugins.
//! - `resolve_include`: the default basename match against the
//!   [`codegraph_lang::FileIndex`] is **a no-op for Go import paths** because
//!   they are module paths (e.g. `"github.com/sirupsen/logrus"`), not
//!   filesystem paths. The wire format records the full import path
//!   verbatim as the `to` field; leaving it unresolved is the intended
//!   behavior. Module-path resolution (go.mod / vendor) is explicitly out
//!   of scope (see Phase 6.6 limitations).
//!
//! # Known Go parser limitations
//!
//! These match the documented design and apply to the Go parser as it is
//! built out. They are intentional, not bugs.
//!
//! 1. **Structural interface implementation produces no edges.** Go's
//!    interfaces are satisfied structurally — a concrete type implements an
//!    interface by having the right method set, with no syntactic
//!    declaration. There is no `Inherits` edge for Go (Phase 6.2/6.6).
//! 2. **Embedded struct fields produce no `Inherits` edge.** `type T struct
//!    { Bar }` is structural composition (method-set promotion), not
//!    inheritance — no edge is emitted (Phase 6.2 anti-regression test).
//! 3. **Method dispatch is heuristic.** Same as the C++ and Rust plugins —
//!    call edges resolve via scope-aware heuristic matching, which is
//!    syntactic, not semantic. Methods on different receiver types that
//!    share a name may resolve to the wrong candidate.
//! 4. **`go.mod` and vendor directories are not consulted.** Discovery walks
//!    files and respects `.gitignore`; module-path resolution is out of
//!    scope.

pub(crate) mod helpers;
pub(crate) mod queries;

use std::path::Path;

use codegraph_core::{FileGraph, Language};
use codegraph_lang::{LanguagePlugin, ParseError};
use tree_sitter::{Language as TsLanguage, Query};

use crate::queries::{CALL_QUERIES, DEFINITION_QUERIES, IMPORT_QUERIES};

/// File extensions the Go parser claims.
pub const EXTENSIONS: &[&str] = &[".go"];

/// Go source-file parser. Holds the tree-sitter `Language` and the three
/// pre-compiled queries used to drive symbol/edge extraction in Phases
/// 6.2-6.4.
///
/// Construct with [`GoParser::new`]; share across threads (queries are
/// `Send + Sync`).
pub struct GoParser {
    /// Compiled Go grammar. Held so per-call [`tree_sitter::Parser`]
    /// instances built inside `parse_file` (Phase 6.2+) can attach to it
    /// without rebuilding the `LanguageFn`.
    #[allow(dead_code)] // wired in Phase 6.2
    language: TsLanguage,
    /// Compiled definition query.
    #[allow(dead_code)] // wired in Phase 6.2
    def_query: Query,
    /// Compiled call query.
    #[allow(dead_code)] // wired in Phase 6.3
    call_query: Query,
    /// Compiled import query.
    #[allow(dead_code)] // wired in Phase 6.4
    import_query: Query,
}

impl GoParser {
    /// Build a new parser, compiling all three tree-sitter queries against
    /// the pinned tree-sitter-go grammar. Returns an [`anyhow::Error`]
    /// (wrapping the query compiler's message) if any query fails to compile
    /// against the pinned grammar version.
    ///
    /// Successful return is the Phase 6.1 acceptance gate that proves every
    /// query string in `queries.rs` parses against tree-sitter-go 0.25.x.
    pub fn new() -> anyhow::Result<Self> {
        let language: TsLanguage = tree_sitter_go::LANGUAGE.into();

        let def_query = Query::new(&language, DEFINITION_QUERIES)
            .map_err(|e| anyhow::anyhow!("definition query: {e}"))?;
        let call_query =
            Query::new(&language, CALL_QUERIES).map_err(|e| anyhow::anyhow!("call query: {e}"))?;
        let import_query = Query::new(&language, IMPORT_QUERIES)
            .map_err(|e| anyhow::anyhow!("import query: {e}"))?;

        Ok(Self {
            language,
            def_query,
            call_query,
            import_query,
        })
    }

    /// File extensions handled by this plugin. Exposed as an associated
    /// function so the trait implementation and external callers (e.g. CLI
    /// argument parsing) share the single source of truth.
    pub fn extensions() -> &'static [&'static str] {
        EXTENSIONS
    }
}

impl LanguagePlugin for GoParser {
    fn id(&self) -> Language {
        Language::Go
    }

    fn extensions(&self) -> &'static [&'static str] {
        EXTENSIONS
    }

    /// Parse `content` (UTF-8 bytes) as Go and produce a [`FileGraph`].
    ///
    /// Phase 6.1 stub: returns an empty FileGraph (no symbols, no edges).
    /// Phases 6.2/6.3/6.4 will populate the extractors. Until then this
    /// stub is enough to satisfy the trait contract and let the parser be
    /// registered (or instantiated for the object-safety test) without
    /// emitting noise.
    fn parse_file(&self, path: &Path, _content: &[u8]) -> Result<FileGraph, ParseError> {
        Ok(FileGraph {
            path: path.to_string_lossy().into_owned(),
            language: Language::Go,
            symbols: Vec::new(),
            edges: Vec::new(),
        })
    }

    // resolve_call and resolve_include intentionally NOT overridden — see the
    // crate-level docstring for the rationale (default heuristic matches the
    // C++ and Rust plugins; default basename resolver is a no-op for Go's
    // module-path imports, which is the intended behavior).

    fn close(&self) {}
}

#[cfg(test)]
mod tests {
    //! Phase 6.1 structural smoke tests. Behavioral coverage for definitions
    //! (6.2), calls (6.3), and imports (6.4) lands alongside the
    //! corresponding `extract_*` loops.
    use super::*;

    #[test]
    fn new_compiles_all_three_queries() {
        // The whole point of Phase 6.1: every query string parses against
        // the pinned tree-sitter-go. Failure here means a query needs
        // updating.
        let p = GoParser::new().expect("GoParser::new must succeed");
        let _ = (&p.language, &p.def_query, &p.call_query, &p.import_query);
    }

    #[test]
    fn extensions_match_expected_list() {
        assert_eq!(GoParser::extensions(), &[".go"]);
        let p = GoParser::new().unwrap();
        assert_eq!(LanguagePlugin::extensions(&p), GoParser::extensions());
    }

    #[test]
    fn id_is_go() {
        let p = GoParser::new().unwrap();
        assert_eq!(p.id(), Language::Go);
    }

    /// Canonical compile-time-interface check + `id() -> Language::Go`
    /// assertion. Mirrors the C++ test at
    /// `crates/codegraph-lang-cpp/src/lib.rs:542-545` exactly.
    #[test]
    fn go_parser_is_object_safe_via_box_dyn() {
        let p: Box<dyn LanguagePlugin> = Box::new(GoParser::new().unwrap());
        assert_eq!(p.id(), Language::Go);
    }

    #[test]
    fn parse_file_returns_correct_path_and_language() {
        // Phase 6.1 stub: empty FileGraph. Phase 6.2 will populate symbols.
        let p = GoParser::new().unwrap();
        let path = Path::new("/tmp/test.go");
        let fg = p.parse_file(path, b"package main").unwrap();
        assert_eq!(fg.path, "/tmp/test.go");
        assert_eq!(fg.language, Language::Go);
        assert!(
            fg.symbols.is_empty(),
            "Phase 6.1 stub must return zero symbols (extraction lands in 6.2)"
        );
        assert!(
            fg.edges.is_empty(),
            "Phase 6.1 stub must return zero edges (extraction lands in 6.3/6.4)"
        );
    }
}
