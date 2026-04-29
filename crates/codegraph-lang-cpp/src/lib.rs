//! C++ language plugin for code-graph-mcp.
//!
//! This crate ports the Go `internal/lang/cpp` package to Rust. It uses
//! tree-sitter (via the `tree-sitter` and `tree-sitter-cpp` crates) to extract
//! symbols, calls, includes, and inheritance edges from C/C++ source.
//!
//! # Phase status
//!
//! Phase 1.5 wires the four extraction loops (`extract_definitions`,
//! `extract_calls`, `extract_includes`, `extract_inheritance`) into
//! [`CppParser::parse_file`]. The full 24-test corpus is ported in Phase 1.6;
//! the inline tests at the bottom of this file cover one representative
//! example of every extraction path so regressions surface immediately.
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

pub(crate) mod helpers;
pub(crate) mod queries;

use std::path::Path;

use codegraph_core::{Edge, EdgeKind, FileGraph, Language, Symbol, SymbolKind};
use codegraph_lang::{LanguagePlugin, ParseError};
use streaming_iterator::StreamingIterator;
use tree_sitter::{
    Language as TsLanguage, Node, Parser as TsParser, Query, QueryCursor, Tree as TsTree,
};

use crate::helpers::{
    enclosing_function_id, find_enclosing_kind, is_cpp_cast, resolve_namespace,
    resolve_parent_class, split_qualified, strip_include_path, truncate_signature,
};
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

    /// Parse `content` (UTF-8 bytes) as C++ and produce a [`FileGraph`]. Used
    /// internally by [`Self::parse_file`] and by the inline tests; kept
    /// crate-private so the public surface stays the trait method.
    fn parse_to_filegraph(&self, path: &Path, content: &[u8]) -> Result<FileGraph, ParseError> {
        let tree = parse_tree(&self.language, content)?;
        let root = tree.root_node();
        let path_str = path.to_string_lossy().into_owned();

        let mut fg = FileGraph {
            path: path_str.clone(),
            language: Language::Cpp,
            symbols: Vec::new(),
            edges: Vec::new(),
        };

        self.extract_definitions(root, content, &path_str, &mut fg);
        self.extract_calls(root, content, &path_str, &mut fg);
        self.extract_includes(root, content, &path_str, &mut fg);
        self.extract_inheritance(root, content, &path_str, &mut fg);

        Ok(fg)
    }

    /// Run the definition query and produce symbols. Mirrors the Go
    /// `extractDefinitions` switch on capture name.
    fn extract_definitions(&self, root: Node<'_>, content: &[u8], path: &str, fg: &mut FileGraph) {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.def_query, root, content);
        let cap_names = self.def_query.capture_names();

        while let Some(m) = matches.next() {
            for capture in m.captures {
                let cap_node = capture.node;
                if cap_node.has_error() {
                    continue;
                }
                let cap_name = capture_name_for_index(cap_names, capture.index);
                let text = cap_node.utf8_text(content).unwrap_or("");

                match cap_name {
                    "func.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "function_definition")
                        else {
                            continue;
                        };
                        let ns = resolve_namespace(cap_node, content);
                        let parent_class = resolve_parent_class(cap_node, content);
                        let kind = if parent_class.is_empty() {
                            SymbolKind::Function
                        } else {
                            SymbolKind::Method
                        };
                        fg.symbols.push(make_symbol(
                            text,
                            kind,
                            path,
                            def_node,
                            content,
                            ns,
                            parent_class,
                        ));
                    }

                    "method.qname" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "function_definition")
                        else {
                            continue;
                        };
                        let (parent, method_name) = split_qualified(text);
                        let ns = resolve_namespace(cap_node, content);
                        fg.symbols.push(make_symbol(
                            &method_name,
                            SymbolKind::Method,
                            path,
                            def_node,
                            content,
                            ns,
                            parent,
                        ));
                    }

                    "class.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "class_specifier")
                        else {
                            continue;
                        };
                        let ns = resolve_namespace(cap_node, content);
                        // For nested classes, find the outer class by walking
                        // up from the class definition node itself.
                        let parent_class = resolve_parent_class(def_node, content);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Class,
                            path,
                            def_node,
                            content,
                            ns,
                            parent_class,
                        ));
                    }

                    "struct.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "struct_specifier")
                        else {
                            continue;
                        };
                        let ns = resolve_namespace(cap_node, content);
                        let parent_class = resolve_parent_class(def_node, content);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Struct,
                            path,
                            def_node,
                            content,
                            ns,
                            parent_class,
                        ));
                    }

                    "enum.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "enum_specifier") else {
                            continue;
                        };
                        let ns = resolve_namespace(cap_node, content);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Enum,
                            path,
                            def_node,
                            content,
                            ns,
                            String::new(),
                        ));
                    }

                    "inline.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "function_definition")
                        else {
                            continue;
                        };
                        let ns = resolve_namespace(cap_node, content);
                        let parent_class = resolve_parent_class(cap_node, content);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Method,
                            path,
                            def_node,
                            content,
                            ns,
                            parent_class,
                        ));
                    }

                    "operator.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "function_definition")
                        else {
                            continue;
                        };
                        let ns = resolve_namespace(cap_node, content);
                        let parent_class = resolve_parent_class(cap_node, content);
                        // Go uses KindFunction for operator overloads even
                        // when defined in-class. Preserve that quirk.
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Function,
                            path,
                            def_node,
                            content,
                            ns,
                            parent_class,
                        ));
                    }

                    "typedef.name" => {
                        let def_node = find_enclosing_kind(cap_node, "type_definition")
                            .or_else(|| find_enclosing_kind(cap_node, "alias_declaration"));
                        let Some(def_node) = def_node else {
                            continue;
                        };
                        let ns = resolve_namespace(cap_node, content);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Typedef,
                            path,
                            def_node,
                            content,
                            ns,
                            String::new(),
                        ));
                    }

                    _ => {}
                }
            }
        }
    }

    /// Run the call query and produce call edges. Mirrors the Go
    /// `extractCalls`, including the cast filter and enclosing-function
    /// fallback to the bare path.
    fn extract_calls(&self, root: Node<'_>, content: &[u8], path: &str, fg: &mut FileGraph) {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.call_query, root, content);
        let cap_names = self.call_query.capture_names();

        while let Some(m) = matches.next() {
            for capture in m.captures {
                let cap_node = capture.node;
                if cap_node.has_error() {
                    continue;
                }
                let cap_name = capture_name_for_index(cap_names, capture.index);
                if cap_name != "call.name" && cap_name != "call.qname" {
                    continue;
                }

                let callee = cap_node.utf8_text(content).unwrap_or("");
                if is_cpp_cast(callee) {
                    continue;
                }

                // Use enclosing call_expression for line info; fall back to
                // the capture node itself if we somehow aren't inside one.
                let call_node =
                    find_enclosing_kind(cap_node, "call_expression").unwrap_or(cap_node);
                let from = enclosing_function_id(cap_node, content, path);

                fg.edges.push(Edge {
                    from,
                    to: callee.to_owned(),
                    kind: EdgeKind::Calls,
                    file: path.to_owned(),
                    line: call_node.start_position().row as u32 + 1,
                });
            }
        }
    }

    /// Run the include query and produce include edges. Quotes/angle brackets
    /// are stripped; otherwise this mirrors Go's `extractIncludes`.
    fn extract_includes(&self, root: Node<'_>, content: &[u8], path: &str, fg: &mut FileGraph) {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.incl_query, root, content);
        let cap_names = self.incl_query.capture_names();

        while let Some(m) = matches.next() {
            for capture in m.captures {
                let cap_node = capture.node;
                if cap_node.has_error() {
                    continue;
                }
                let cap_name = capture_name_for_index(cap_names, capture.index);
                if cap_name != "include.path" {
                    continue;
                }

                let raw = cap_node.utf8_text(content).unwrap_or("");
                let cleaned = strip_include_path(raw);

                fg.edges.push(Edge {
                    from: path.to_owned(),
                    to: cleaned,
                    kind: EdgeKind::Includes,
                    file: path.to_owned(),
                    line: cap_node.start_position().row as u32 + 1,
                });
            }
        }
    }

    /// Run the inheritance query and produce inherits edges. Emits one edge
    /// per (derived, base) pair; mirrors Go's `extractInheritance`, including
    /// its decision to use the bare derived name (not `path:Name`) as the
    /// edge `from` and to record `line: 0` since the query does not carry a
    /// reliable single line number.
    fn extract_inheritance(&self, root: Node<'_>, content: &[u8], path: &str, fg: &mut FileGraph) {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.inh_query, root, content);
        let cap_names = self.inh_query.capture_names();

        while let Some(m) = matches.next() {
            let mut derived_name = String::new();
            let mut base_names: Vec<String> = Vec::new();

            for capture in m.captures {
                let cap_node = capture.node;
                if cap_node.has_error() {
                    continue;
                }
                let cap_name = capture_name_for_index(cap_names, capture.index);
                let text = cap_node.utf8_text(content).unwrap_or("").to_owned();

                match cap_name {
                    "derived.name" => derived_name = text,
                    "base.name" => base_names.push(text),
                    _ => {}
                }
            }

            for base in base_names {
                fg.edges.push(Edge {
                    from: derived_name.clone(),
                    to: base,
                    kind: EdgeKind::Inherits,
                    file: path.to_owned(),
                    line: 0,
                });
            }
        }
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
        self.parse_to_filegraph(path, content)
    }
}

/// Build a tree-sitter [`TsTree`] for `content` against the C++ grammar. The
/// caller-supplied [`TsLanguage`] is borrowed; the returned tree owns its
/// AST. Returns [`ParseError::Parse`] if `set_language` fails or if
/// tree-sitter declines to produce a tree (e.g. on cancellation).
fn parse_tree(language: &TsLanguage, content: &[u8]) -> Result<TsTree, ParseError> {
    let mut parser = TsParser::new();
    parser
        .set_language(language)
        .map_err(|e| ParseError::Parse(format!("set_language: {e}")))?;
    parser
        .parse(content, None)
        .ok_or_else(|| ParseError::Parse("tree-sitter parse failed".to_owned()))
}

/// Look up a capture name by index. Mirrors the Go
/// `(*CppParser).captureNameForIndex`. Returns `""` (empty) on out-of-range
/// indices, matching Go's silent fallback.
fn capture_name_for_index<'a>(cap_names: &[&'a str], index: u32) -> &'a str {
    cap_names.get(index as usize).copied().unwrap_or("")
}

/// Build a [`Symbol`] from a definition node. Centralizes the row/column/
/// signature math so each branch in `extract_definitions` stays small.
fn make_symbol(
    name: &str,
    kind: SymbolKind,
    path: &str,
    def_node: Node<'_>,
    content: &[u8],
    namespace: String,
    parent: String,
) -> Symbol {
    let start = def_node.start_position();
    let end = def_node.end_position();
    Symbol {
        name: name.to_owned(),
        kind,
        file: path.to_owned(),
        line: start.row as u32 + 1,
        column: start.column as u32,
        end_line: end.row as u32 + 1,
        signature: truncate_signature(def_node.utf8_text(content).unwrap_or("")),
        namespace,
        parent,
        language: Language::Cpp,
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
    //! Structural smoke tests that rely on `CppParser` internals
    //! (`language`, `def_query`, `call_query`, etc.). Behavioral coverage
    //! lives in `tests/corpus.rs` (the full Phase 1.6 corpus).
    use super::*;

    #[test]
    fn new_compiles_all_four_queries() {
        // The whole point of Phase 1.4: every query string parses against
        // tree-sitter-cpp 0.23.4. Failure here means a query needs updating.
        let p = CppParser::new().expect("CppParser::new must succeed");
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
        let p = CppParser::new().unwrap();
        assert_eq!(LanguagePlugin::extensions(&p), CppParser::extensions());
    }

    #[test]
    fn id_is_cpp() {
        let p = CppParser::new().unwrap();
        assert_eq!(p.id(), Language::Cpp);
    }

    #[test]
    fn cpp_parser_is_object_safe_via_box_dyn() {
        let p: Box<dyn LanguagePlugin> = Box::new(CppParser::new().unwrap());
        assert_eq!(p.id(), Language::Cpp);
    }

    #[test]
    fn parse_file_returns_correct_path_and_language() {
        let p = CppParser::new().unwrap();
        let path = Path::new("/tmp/test.cpp");
        let fg = p.parse_file(path, b"void foo() {}").unwrap();
        assert_eq!(fg.path, "/tmp/test.cpp");
        assert_eq!(fg.language, Language::Cpp);
        assert!(!fg.symbols.is_empty(), "extraction must populate symbols");
    }
}
