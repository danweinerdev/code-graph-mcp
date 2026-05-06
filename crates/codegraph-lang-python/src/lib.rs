//! Python language plugin for code-graph-mcp.
//!
//! Uses tree-sitter (via the `tree-sitter` and `tree-sitter-python` crates)
//! to extract symbols, calls, import edges, and inheritance edges from
//! `.py` and `.pyi` source files.
//!
//! # Phase status
//!
//! Phase 7.1 ships the crate scaffold: dependency wiring, query strings
//! that compile against tree-sitter-python 0.25, the `PythonParser` struct
//! with cached `Query` objects, and the `LanguagePlugin` impl. At this
//! checkpoint `parse_file` returns an empty `FileGraph` (no symbols, no
//! edges) — extraction logic is wired in 7.2 (definitions), 7.3 (calls),
//! 7.4 (imports), and 7.5 (inheritance).
//!
//! # Default trait methods
//!
//! `PythonParser` does NOT override [`LanguagePlugin::resolve_call`] or
//! [`LanguagePlugin::resolve_include`].
//!
//! - `resolve_call`: the default scope-aware heuristic (same file > same
//!   parent > same namespace > global) is the documented contract.
//!   Python's dynamic typing makes any static call resolution inherently
//!   noisy — a method call `obj.foo()` cannot be resolved to a concrete
//!   `foo` without type inference, which is out of scope for a tree-sitter-
//!   based static analyzer. The default heuristic produces the same kind
//!   of best-effort result as for C++/Rust/Go.
//! - `resolve_include`: the default basename match against the
//!   [`codegraph_lang::FileIndex`] is **a no-op for Python module paths**
//!   because they are dotted module strings (e.g. `"foo.bar"`), not
//!   filesystem paths. The wire format records the full module path
//!   verbatim as the `to` field; leaving it unresolved is the intended
//!   behavior — `import x.y.z` does not trivially map to `x/y/z.py`
//!   without consulting `sys.path` and the project layout, both of which
//!   are out of scope.
//!
//! # Python-specific notes
//!
//! - The tree-sitter-python grammar uses the node kind **`call`**, NOT
//!   `call_expression`. This is the most common footgun when porting a
//!   Python plugin from muscle memory of the C++/Rust/Go grammars.
//! - **`async def` parses as `function_definition`** in tree-sitter-python
//!   0.25 — there is no separate `async_function_definition` node. The
//!   single `function_definition` query in `queries.rs` covers both sync
//!   and async forms.
//! - `.py` and `.pyi` files share the same grammar; both extensions
//!   dispatch to the same parser. `.pyi` stub files use the same
//!   `function_definition` / `class_definition` nodes — `def f() -> int:
//!   ...` parses as a `function_definition` whose body is a single
//!   `expression_statement` containing `...`. No separate query path is
//!   needed.

pub(crate) mod helpers;
pub(crate) mod queries;

use std::path::Path;

use codegraph_core::{FileGraph, Language};
use codegraph_lang::{LanguagePlugin, ParseError};
use tree_sitter::{Language as TsLanguage, Query};

use crate::queries::{CALL_QUERIES, DEFINITION_QUERIES, IMPORT_QUERIES, INHERITANCE_QUERIES};

/// File extensions the Python parser claims. Both `.py` (regular sources)
/// and `.pyi` (stub files) dispatch to the same parser — the grammar is
/// identical and the stubs use the same `function_definition` /
/// `class_definition` nodes with `...` bodies.
pub const EXTENSIONS: &[&str] = &[".py", ".pyi"];

/// Python source-file parser. Holds the tree-sitter `Language` and the
/// four pre-compiled queries used to drive symbol/edge extraction in
/// Phases 7.2-7.5.
///
/// Construct with [`PythonParser::new`]; share across threads (queries are
/// `Send + Sync`).
pub struct PythonParser {
    /// Compiled Python grammar. Held so per-call [`tree_sitter::Parser`]
    /// instances built inside `parse_file` can attach to it without
    /// rebuilding the `LanguageFn`. Phase 7.1 does not yet build any
    /// per-call parser instances (parse_file returns an empty FileGraph),
    /// but the field is in place for 7.2-7.5 to consume directly.
    #[allow(dead_code)]
    language: TsLanguage,
    /// Compiled definition query (wired in 7.2).
    #[allow(dead_code)]
    def_query: Query,
    /// Compiled call query (wired in 7.3).
    #[allow(dead_code)]
    call_query: Query,
    /// Compiled import query (wired in 7.4).
    #[allow(dead_code)]
    import_query: Query,
    /// Compiled inheritance query (wired in 7.5).
    #[allow(dead_code)]
    inheritance_query: Query,
}

impl PythonParser {
    /// Build a new parser, compiling all four tree-sitter queries against
    /// the pinned tree-sitter-python grammar. Returns an [`anyhow::Error`]
    /// (wrapping the query compiler's message) if any query fails to
    /// compile against the pinned grammar version.
    ///
    /// Successful return is the Phase 7.1 acceptance gate that proves
    /// every query string in `queries.rs` parses against tree-sitter-
    /// python 0.25.
    pub fn new() -> anyhow::Result<Self> {
        let language: TsLanguage = tree_sitter_python::LANGUAGE.into();

        let def_query = Query::new(&language, DEFINITION_QUERIES)
            .map_err(|e| anyhow::anyhow!("definition query: {e}"))?;
        let call_query =
            Query::new(&language, CALL_QUERIES).map_err(|e| anyhow::anyhow!("call query: {e}"))?;
        let import_query = Query::new(&language, IMPORT_QUERIES)
            .map_err(|e| anyhow::anyhow!("import query: {e}"))?;
        let inheritance_query = Query::new(&language, INHERITANCE_QUERIES)
            .map_err(|e| anyhow::anyhow!("inheritance query: {e}"))?;

        Ok(Self {
            language,
            def_query,
            call_query,
            import_query,
            inheritance_query,
        })
    }

    /// File extensions handled by this plugin. Exposed as an associated
    /// function so the trait implementation and external callers (e.g.
    /// CLI argument parsing) share the single source of truth.
    pub fn extensions() -> &'static [&'static str] {
        EXTENSIONS
    }
}

impl LanguagePlugin for PythonParser {
    fn id(&self) -> Language {
        Language::Python
    }

    fn extensions(&self) -> &'static [&'static str] {
        EXTENSIONS
    }

    /// Parse `content` (UTF-8 bytes) as Python and produce a [`FileGraph`].
    ///
    /// At Phase 7.1 this returns an empty FileGraph (no symbols, no
    /// edges). Phases 7.2-7.5 wire definition, call, import, and
    /// inheritance extraction onto this surface.
    fn parse_file(&self, path: &Path, _content: &[u8]) -> Result<FileGraph, ParseError> {
        Ok(FileGraph {
            path: path.to_string_lossy().into_owned(),
            language: Language::Python,
            symbols: Vec::new(),
            edges: Vec::new(),
        })
    }

    // resolve_call and resolve_include intentionally NOT overridden — see
    // the crate-level docstring for the rationale (default heuristic
    // matches the C++/Rust/Go plugins; default basename resolver is a
    // no-op for Python's dotted module-path imports, which is the
    // intended behavior).

    fn close(&self) {}
}

#[cfg(test)]
mod tests {
    //! Phase 7.1 structural smoke tests. Behavioral coverage of definition
    //! / call / import / inheritance extraction lands in 7.2-7.5; at this
    //! checkpoint `parse_file` is a placeholder that returns an empty
    //! FileGraph.
    use super::*;
    use codegraph_lang::LanguagePlugin;

    #[test]
    fn new_compiles_all_four_queries() {
        // The whole point of Phase 7.1: every query string parses against
        // the pinned tree-sitter-python. Failure here means a query needs
        // updating.
        let p = PythonParser::new().expect("PythonParser::new must succeed");
        let _ = (
            &p.language,
            &p.def_query,
            &p.call_query,
            &p.import_query,
            &p.inheritance_query,
        );
    }

    #[test]
    fn extensions_match_expected_list() {
        assert_eq!(PythonParser::extensions(), &[".py", ".pyi"]);
        let p = PythonParser::new().unwrap();
        assert_eq!(LanguagePlugin::extensions(&p), PythonParser::extensions());
    }

    #[test]
    fn id_is_python() {
        let p = PythonParser::new().unwrap();
        assert_eq!(p.id(), Language::Python);
    }

    /// Canonical compile-time-interface check + `id() -> Language::Python`
    /// assertion. Mirrors the C++ test at
    /// `crates/codegraph-lang-cpp/src/lib.rs:542-545` exactly. This is the
    /// Phase 7.1 verification field's named test
    /// (`python_parser_is_object_safe_via_box_dyn`).
    #[test]
    fn python_parser_is_object_safe_via_box_dyn() {
        let p: Box<dyn LanguagePlugin> = Box::new(PythonParser::new().unwrap());
        assert_eq!(p.id(), Language::Python);
    }

    #[test]
    fn parse_file_returns_empty_filegraph_with_correct_path_and_language() {
        // Phase 7.1 stub: parse_file returns an empty FileGraph with the
        // path and language fields populated. 7.2 will replace the empty
        // vectors with extracted symbols.
        let p = PythonParser::new().unwrap();
        let fg = p
            .parse_file(Path::new("/tmp/test.py"), b"")
            .expect("parse_file must succeed");
        assert_eq!(fg.path, "/tmp/test.py");
        assert_eq!(fg.language, Language::Python);
        assert!(fg.symbols.is_empty());
        assert!(fg.edges.is_empty());
    }

    #[test]
    fn parse_file_accepts_pyi_path_extension() {
        // `.pyi` stub files dispatch to the same parser. At 7.1 with the
        // empty-graph stub this only checks the language tag survives;
        // 7.6's testdata corpus exercises real `.pyi` extraction.
        let p = PythonParser::new().unwrap();
        let fg = p
            .parse_file(Path::new("/tmp/stubs.pyi"), b"")
            .expect("parse_file must accept .pyi");
        assert_eq!(fg.path, "/tmp/stubs.pyi");
        assert_eq!(fg.language, Language::Python);
    }
}
