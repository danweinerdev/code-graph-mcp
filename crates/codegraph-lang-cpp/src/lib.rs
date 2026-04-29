//! C++ language plugin for code-graph-mcp.
//!
//! This crate ports the Go `internal/lang/cpp` package to Rust. It uses
//! tree-sitter (via the `tree-sitter` and `tree-sitter-cpp` crates) to extract
//! symbols, calls, includes, and inheritance edges from C/C++ source.
//!
//! # Phase status
//!
//! Phase 1.4 (this commit) ships the [`CppParser`] struct, the four
//! tree-sitter queries (definitions, calls, includes, inheritance), and the
//! pure-Rust helpers ported from `cpp.go` (path/qualified/include
//! manipulation, AST walks, signature truncation). [`CppParser::parse_file`]
//! currently returns an empty [`FileGraph`] with the right path/language; the
//! actual extraction loops land in Phase 1.5.
//!
//! # Known C++ parser limitations
//!
//! These were validated against tree-sitter-cpp v0.23.4 and apply to the Go
//! implementation as well; they are intentional, not bugs. Any change to this
//! list MUST be mirrored in `CLAUDE.md`.
//!
//! 1. **Macro-generated definitions** — Macros like `DEFINE_HANDLER(name)`
//!    that expand to function definitions are not visible to tree-sitter (it
//!    sees the macro call, not the expansion). Macro invocations that look
//!    like function calls ARE captured as call edges.
//! 2. **Complex template metaprogramming** — Deeply nested template
//!    specializations may produce incomplete or error-containing AST nodes.
//!    The parser skips error nodes gracefully.
//! 3. **Call resolution is heuristic** — Call edges are resolved via
//!    scope-aware heuristic matching (same file > same class > same
//!    namespace > global). This is syntactic, not semantic; overloaded
//!    functions may resolve to the wrong candidate.
//! 4. **C++ cast expressions** — `static_cast`, `dynamic_cast`, `const_cast`,
//!    and `reinterpret_cast` are filtered out (tree-sitter parses them as
//!    `call_expression`).
//! 5. **Forward declarations excluded** — Only `function_definition` (with
//!    body) produces symbols. Forward declarations (`void foo();`) are
//!    intentionally excluded to avoid duplicates.
//! 6. **Template method calls** — `obj.foo<T>()` via `template_method` node
//!    type is not matched in tree-sitter-cpp v0.23.4. These calls fall
//!    through to the regular `field_expression` pattern when possible.
//! 7. **Function pointer typedefs** — Captured via the alternation pattern
//!    (`type_definition` with a `function_declarator` containing a
//!    `parenthesized_declarator > pointer_declarator > type_identifier`).

pub mod helpers;
pub(crate) mod queries;

use std::path::Path;

use codegraph_core::{FileGraph, Language};
use codegraph_lang::{LanguagePlugin, ParseError};
use tree_sitter::{Language as TsLanguage, Parser as TsParser, Query, QueryCursor};

use crate::queries::{CALL_QUERIES, DEFINITION_QUERIES, INCLUDE_QUERIES, INHERITANCE_QUERIES};

/// File extensions the C++ parser claims. Mirrors the Go
/// `(*CppParser).Extensions()` exactly.
pub const EXTENSIONS: &[&str] = &[".cpp", ".cc", ".cxx", ".c", ".h", ".hpp", ".hxx"];

/// C++ source-file parser. Holds the tree-sitter `Language` and the four
/// pre-compiled queries used to drive symbol/edge extraction.
///
/// Construct with [`CppParser::new`]; share across threads (queries are
/// `Send + Sync`).
pub struct CppParser {
    /// Compiled C++ grammar; held so [`tree_sitter::Parser`] instances built
    /// per `parse_file` call can attach to it without re-building the
    /// `LanguageFn`.
    language: TsLanguage,
    /// Compiled definition query.
    def_query: Query,
    /// Compiled call query.
    call_query: Query,
    /// Compiled include query.
    incl_query: Query,
    /// Compiled inheritance query.
    inh_query: Query,
}

impl CppParser {
    /// Build a new parser, compiling all four tree-sitter queries against the
    /// pinned tree-sitter-cpp grammar. Returns
    /// [`ParseError::Query`](ParseError::Query) carrying the query compiler's
    /// error message if any query fails to compile (this should not happen
    /// against the grammar version we pin in `Cargo.toml`; if it does, the
    /// error tells us which query is at fault).
    pub fn new() -> Result<Self, ParseError> {
        let language: TsLanguage = tree_sitter_cpp::LANGUAGE.into();

        let def_query = Query::new(&language, DEFINITION_QUERIES)
            .map_err(|e| ParseError::Query(format!("definition query: {e}")))?;
        let call_query = Query::new(&language, CALL_QUERIES)
            .map_err(|e| ParseError::Query(format!("call query: {e}")))?;
        let incl_query = Query::new(&language, INCLUDE_QUERIES)
            .map_err(|e| ParseError::Query(format!("include query: {e}")))?;
        let inh_query = Query::new(&language, INHERITANCE_QUERIES)
            .map_err(|e| ParseError::Query(format!("inheritance query: {e}")))?;

        Ok(Self {
            language,
            def_query,
            call_query,
            incl_query,
            inh_query,
        })
    }

    /// File extensions handled by this plugin. Mirrors the Go method of the
    /// same name. Exposed as an associated function so the trait
    /// implementation and external callers (e.g. CLI argument parsing) share
    /// the single source of truth.
    pub fn extensions() -> &'static [&'static str] {
        EXTENSIONS
    }
}

impl LanguagePlugin for CppParser {
    fn id(&self) -> Language {
        Language::Cpp
    }

    fn extensions(&self) -> &'static [&'static str] {
        EXTENSIONS
    }

    fn parse_file(&self, path: &Path, content: &[u8]) -> Result<FileGraph, ParseError> {
        // Phase 1.4 stub: parse the file end-to-end so the grammar + queries
        // are exercised on every call (catching grammar/version skew at
        // parse time rather than discovery time), but skip extraction. The
        // tree, cursor, and four queries are all touched here so that
        // adding extraction in 1.5 is purely a matter of populating
        // `symbols`/`edges` from the existing matches loops.
        // TODO(1.5): replace this with extract_definitions / extract_calls /
        // extract_includes / extract_inheritance.
        let mut parser = TsParser::new();
        parser
            .set_language(&self.language)
            .map_err(|e| ParseError::Parse(format!("set_language: {e}")))?;
        let tree = parser
            .parse(content, None)
            .ok_or_else(|| ParseError::Parse("tree-sitter returned no tree".to_owned()))?;
        let root = tree.root_node();
        let mut cursor = QueryCursor::new();

        // Touch each query so Phase 1.5 can drop in the matches loop without
        // re-plumbing field access. The cursor is dropped at end of scope.
        for query in [
            &self.def_query,
            &self.call_query,
            &self.incl_query,
            &self.inh_query,
        ] {
            let _ = cursor.matches(query, root, content);
        }

        Ok(FileGraph {
            path: path.to_string_lossy().into_owned(),
            language: Language::Cpp,
            symbols: Vec::new(),
            edges: Vec::new(),
        })
    }
}

// Compile-time interface check. Mirrors the Go
// `var _ parser.Parser = (*CppParser)(nil)` line at the top of cpp.go.
const _: fn() = || {
    fn assert_plugin<T: LanguagePlugin + ?Sized>() {}
    assert_plugin::<CppParser>();
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_compiles_all_four_queries() {
        // The whole point of Phase 1.4: every query string parses against
        // tree-sitter-cpp 0.23.4. Failure here means a query needs updating.
        let p = CppParser::new().expect("CppParser::new must succeed");
        // Touch each field so the test fails loudly if `new` ever stops
        // populating them.
        let _ = (
            &p.language,
            &p.def_query,
            &p.call_query,
            &p.incl_query,
            &p.inh_query,
        );
    }

    #[test]
    fn extensions_match_go_list() {
        assert_eq!(
            CppParser::extensions(),
            &[".cpp", ".cc", ".cxx", ".c", ".h", ".hpp", ".hxx"]
        );
        // Trait-side and assoc-fn-side must agree.
        let p = CppParser::new().unwrap();
        assert_eq!(LanguagePlugin::extensions(&p), CppParser::extensions());
    }

    #[test]
    fn id_is_cpp() {
        let p = CppParser::new().unwrap();
        assert_eq!(p.id(), Language::Cpp);
    }

    #[test]
    fn parse_file_phase14_returns_empty_filegraph_with_correct_path_and_language() {
        // Phase 1.4 stub: extraction lands in 1.5. The returned FileGraph
        // must still carry the right path and language tag.
        let p = CppParser::new().unwrap();
        let path = Path::new("/tmp/test.cpp");
        let fg = p.parse_file(path, b"void foo() {}").unwrap();
        assert_eq!(fg.path, "/tmp/test.cpp");
        assert_eq!(fg.language, Language::Cpp);
        assert!(fg.symbols.is_empty());
        assert!(fg.edges.is_empty());
    }

    #[test]
    fn cpp_parser_is_object_safe_via_box_dyn() {
        // The registry stores Box<dyn LanguagePlugin>; this confirms
        // CppParser meets that bound.
        let p: Box<dyn LanguagePlugin> = Box::new(CppParser::new().unwrap());
        assert_eq!(p.id(), Language::Cpp);
    }
}
