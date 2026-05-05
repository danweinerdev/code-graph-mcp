//! Go language plugin for code-graph-mcp.
//!
//! Uses tree-sitter (via the `tree-sitter` and `tree-sitter-go` crates) to
//! extract symbols, calls, and import edges from Go source files.
//!
//! # Phase status
//!
//! Phase 6.1 shipped the crate scaffold: dependency wiring, query strings
//! that compile against tree-sitter-go 0.25, the `GoParser` struct with
//! cached `Query` objects, and the `LanguagePlugin` impl.
//!
//! Phase 6.2 wires `extract_definitions` — function/method/type-spec/
//! type_alias extraction with method-receiver-as-parent (including pointer,
//! value, generic, and anonymous receiver forms) and
//! package-clause-as-namespace. Embedded struct fields produce no
//! `Inherits` edge (anti-regression test in `tests` module).
//!
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

use codegraph_core::{FileGraph, Language, Symbol, SymbolKind};
use codegraph_lang::{LanguagePlugin, ParseError};
use streaming_iterator::StreamingIterator;
use tree_sitter::{
    Language as TsLanguage, Node, Parser as TsParser, Query, QueryCursor, Tree as TsTree,
};

use crate::helpers::{extract_package_name, extract_receiver_type, truncate_signature};
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
    /// instances built inside `parse_file` can attach to it without
    /// rebuilding the `LanguageFn`.
    language: TsLanguage,
    /// Compiled definition query (wired in Phase 6.2).
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

    /// Parse `content` (UTF-8 bytes) as Go and produce a [`FileGraph`].
    /// Used internally by [`Self::parse_file`] (the trait method) and by
    /// the inline tests; kept crate-private so the public surface stays
    /// the trait method.
    fn parse_to_filegraph(&self, path: &Path, content: &[u8]) -> Result<FileGraph, ParseError> {
        let tree = parse_tree(&self.language, content)?;
        let root = tree.root_node();
        let path_str = path.to_string_lossy().into_owned();
        let package_name = extract_package_name(root, content);

        let mut fg = FileGraph {
            path: path_str.clone(),
            language: Language::Go,
            symbols: Vec::new(),
            edges: Vec::new(),
        };

        self.extract_definitions(root, content, &path_str, &package_name, &mut fg);
        // Phases 6.3 and 6.4 will populate calls and imports respectively.

        Ok(fg)
    }

    /// Run the definition query and produce symbols. Mirrors the C++/Rust
    /// plugins' capture-name dispatch: each capture name from
    /// `DEFINITION_QUERIES` maps to a small branch that builds the right
    /// `Symbol`. Every emitted symbol carries
    /// `Symbol.namespace = package_name` (Go packages are flat, single-level).
    ///
    /// Per-capture-name behavior:
    ///
    /// - `func.name` (from `function_declaration`) →
    ///   [`SymbolKind::Function`], no parent. Generic functions
    ///   (`func Map[T any](...)`) come through this branch unchanged —
    ///   `truncate_signature` drops everything from the body opener `{`
    ///   onwards, leaving the `[T any]` type parameters intact. `init()`
    ///   and `main()` are ordinary functions; they receive no special
    ///   treatment.
    /// - `method.name` (from `method_declaration` with a `receiver`
    ///   capture) → [`SymbolKind::Method`], parent = receiver-type name
    ///   resolved by [`extract_receiver_type`] (handles pointer / value /
    ///   generic / anonymous receiver forms).
    /// - `type.name` (from `type_spec`) → kind dispatched on the
    ///   accompanying `@type.body` capture: `struct_type` →
    ///   [`SymbolKind::Struct`], `interface_type` →
    ///   [`SymbolKind::Interface`], anything else →
    ///   [`SymbolKind::Typedef`] (covers `type Handler func(...)`,
    ///   `type Count int`, etc.).
    /// - `alias.name` (from `type_alias`) → [`SymbolKind::Typedef`].
    ///   This is the Go 1.9+ form `type ID = string`, parsed as a
    ///   distinct AST node from `type_spec`.
    /// - `package.name` / `package.def` are intentionally consumed
    ///   without emitting a Symbol — the package name is fetched once
    ///   from the source-file root via [`extract_package_name`] and
    ///   plumbed into every other symbol's `namespace` field.
    ///
    /// Embedded struct fields (`type Foo struct { Bar }`) parse as
    /// `field_declaration` nodes with no name field and are not matched
    /// by any branch above — no symbol and no `Inherits` edge is emitted,
    /// matching the Phase 6.2 design intent (Go's structural composition
    /// is not a syntactic inheritance relationship).
    fn extract_definitions(
        &self,
        root: Node<'_>,
        content: &[u8],
        path: &str,
        package_name: &str,
        fg: &mut FileGraph,
    ) {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.def_query, root, content);
        let cap_names = self.def_query.capture_names();

        while let Some(m) = matches.next() {
            // For type_spec we need the body capture (struct_type vs
            // interface_type vs other) to pick the right SymbolKind, so
            // collect it once per match before dispatching on capture
            // names. type_alias matches don't contribute (always Typedef).
            let mut type_body_kind: Option<&str> = None;
            for capture in m.captures {
                let cap_name = capture_name_for_index(cap_names, capture.index);
                if cap_name == "type.body" {
                    type_body_kind = Some(capture.node.kind());
                    break;
                }
            }

            for capture in m.captures {
                let cap_node = capture.node;
                if cap_node.has_error() {
                    continue;
                }
                let cap_name = capture_name_for_index(cap_names, capture.index);
                let text = cap_node.utf8_text(content).unwrap_or("");

                match cap_name {
                    "func.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "function_declaration")
                        else {
                            continue;
                        };
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Function,
                            path,
                            def_node,
                            content,
                            package_name.to_owned(),
                            String::new(),
                        ));
                    }

                    "method.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "method_declaration")
                        else {
                            continue;
                        };
                        let parent = def_node
                            .child_by_field_name("receiver")
                            .map(|r| extract_receiver_type(r, content))
                            .unwrap_or_default();
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Method,
                            path,
                            def_node,
                            content,
                            package_name.to_owned(),
                            parent,
                        ));
                    }

                    "type.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "type_spec") else {
                            continue;
                        };
                        // Only emit on the `type.name` arm; `type.body`
                        // is consumed without action below. Dispatch on
                        // the body kind we collected during the first
                        // pass over the captures.
                        let kind = match type_body_kind {
                            Some("struct_type") => SymbolKind::Struct,
                            Some("interface_type") => SymbolKind::Interface,
                            // Any other body kind (function_type, slice_type,
                            // map_type, channel_type, type_identifier,
                            // pointer_type, etc.) → defined-type alias.
                            _ => SymbolKind::Typedef,
                        };
                        fg.symbols.push(make_symbol(
                            text,
                            kind,
                            path,
                            def_node,
                            content,
                            package_name.to_owned(),
                            String::new(),
                        ));
                    }

                    "alias.name" => {
                        // `type ID = string` — distinct AST node
                        // (`type_alias`) from `type_spec`. Always Typedef.
                        let Some(def_node) = find_enclosing_kind(cap_node, "type_alias") else {
                            continue;
                        };
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Typedef,
                            path,
                            def_node,
                            content,
                            package_name.to_owned(),
                            String::new(),
                        ));
                    }

                    // Captures consumed without emitting a Symbol:
                    // - `method.receiver`: read above by walking the enclosing
                    //   method_declaration's receiver field.
                    // - `type.body`: dispatched on in the first-pass loop
                    //   above; the body kind picks Struct/Interface/Typedef.
                    // - `alias.body`: type_alias body is always Typedef-like;
                    //   no kind dispatch needed.
                    // - `package.name` / `package.def`: package name is
                    //   fetched once via `extract_package_name` and applied
                    //   to all symbols' namespace field.
                    // - `func.def` / `method.def` / `type.def` / `alias.def`:
                    //   structural anchors used by the queries to bind
                    //   captures to the same definition.
                    _ => {}
                }
            }
        }
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
    /// Phase 6.2 wires definition extraction; calls (6.3) and imports
    /// (6.4) are still stubbed to empty.
    fn parse_file(&self, path: &Path, content: &[u8]) -> Result<FileGraph, ParseError> {
        self.parse_to_filegraph(path, content)
    }

    // resolve_call and resolve_include intentionally NOT overridden — see the
    // crate-level docstring for the rationale (default heuristic matches the
    // C++ and Rust plugins; default basename resolver is a no-op for Go's
    // module-path imports, which is the intended behavior).

    fn close(&self) {}
}

/// Build a tree-sitter [`TsTree`] for `content` against the Go grammar.
/// The caller-supplied [`TsLanguage`] is borrowed; the returned tree owns
/// its AST. Returns [`ParseError::Parse`] if `set_language` fails or if
/// tree-sitter declines to produce a tree (e.g. on cancellation). Mirrors
/// `parse_tree` in the C++ and Rust plugins byte-for-byte modulo the
/// language identity.
fn parse_tree(language: &TsLanguage, content: &[u8]) -> Result<TsTree, ParseError> {
    let mut parser = TsParser::new();
    parser
        .set_language(language)
        .map_err(|e| ParseError::Parse(format!("set_language: {e}")))?;
    parser
        .parse(content, None)
        .ok_or_else(|| ParseError::Parse("tree-sitter parse failed".to_owned()))
}

/// Look up a capture name by index. Returns `""` (empty) on out-of-range
/// indices, matching the C++/Rust plugins' silent fallback.
fn capture_name_for_index<'a>(cap_names: &[&'a str], index: u32) -> &'a str {
    cap_names.get(index as usize).copied().unwrap_or("")
}

/// Walk up `node`'s parent chain, returning the first ancestor (including
/// `node` itself) whose kind matches `kind`. Local copy of the C++/Rust
/// plugins' `find_enclosing_kind`.
fn find_enclosing_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut current = Some(node);
    while let Some(n) = current {
        if n.kind() == kind {
            return Some(n);
        }
        current = n.parent();
    }
    None
}

/// Build a [`Symbol`] from a definition node. Centralises the row/column/
/// signature math so each branch in `extract_definitions` stays small.
/// Mirrors the C++/Rust plugins' `make_symbol`.
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
        language: Language::Go,
    }
}

#[cfg(test)]
mod tests {
    //! Phase 6.1 structural smoke tests + Phase 6.2 definition extraction
    //! coverage. Behavioral coverage for calls (6.3) and imports (6.4)
    //! lands alongside the corresponding `extract_*` loops.
    use super::*;
    use codegraph_core::{symbol_id, EdgeKind};

    // ----------------------------------------------------------------
    // Phase 6.1 — structural smoke tests
    // ----------------------------------------------------------------

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

    // ----------------------------------------------------------------
    // Phase 6.2 — definition extraction
    // ----------------------------------------------------------------

    /// Parse `src` against `GoParser` and return the resulting FileGraph
    /// at a synthetic absolute path. Used by every Phase 6.2 behavioral
    /// test below.
    fn parse(src: &str) -> FileGraph {
        let p = GoParser::new().unwrap();
        p.parse_file(Path::new("/tmp/test.go"), src.as_bytes())
            .expect("parse_file must succeed")
    }

    /// Find the (first) symbol with `name`, panicking with a helpful
    /// message if absent. Tests use this when they expect exactly one.
    fn sym<'a>(fg: &'a FileGraph, name: &str) -> &'a Symbol {
        fg.symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| {
                panic!(
                    "expected symbol named {name:?}; got: {:?}",
                    fg.symbols
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                )
            })
    }

    #[test]
    fn parse_file_returns_correct_path_and_language() {
        // Phase 6.1's empty-graph stub assertion is now obsolete — 6.2
        // populates symbols. Keep the path/language assertion which still
        // belongs at this layer.
        let fg = parse("package main\n");
        assert_eq!(fg.path, "/tmp/test.go");
        assert_eq!(fg.language, Language::Go);
    }

    #[test]
    fn empty_package_only_file_produces_no_symbols() {
        // A bare `package main` file (no decls) yields zero symbols and
        // zero edges. Sanity check that the package_clause capture is
        // consumed without emitting a Symbol.
        let fg = parse("package main\n");
        assert!(fg.symbols.is_empty(), "got: {:?}", fg.symbols);
        assert!(fg.edges.is_empty(), "got: {:?}", fg.edges);
    }

    #[test]
    fn free_function_produces_function_kind_no_parent() {
        let fg = parse("package main\nfunc foo() {}\n");
        let s = sym(&fg, "foo");
        assert_eq!(s.kind, SymbolKind::Function);
        assert!(s.parent.is_empty(), "free func must have empty parent");
        assert_eq!(s.namespace, "main", "package name populates namespace");
        assert_eq!(s.language, Language::Go);
        assert_eq!(symbol_id(s), "/tmp/test.go:foo");
        assert!(
            !s.signature.contains('{'),
            "signature must be truncated at body opener, got: {:?}",
            s.signature
        );
        assert!(s.signature.contains("func foo()"));
    }

    #[test]
    fn package_name_populates_namespace_for_all_symbols() {
        // A non-main package — every emitted symbol carries
        // namespace = package name (Go packages are flat, single-level).
        let src = r#"package server
type Server struct{}
func (s *Server) Run() {}
func Helper() {}
"#;
        let fg = parse(src);
        for s in &fg.symbols {
            assert_eq!(
                s.namespace, "server",
                "every symbol must carry namespace=server, got {:?} for {}",
                s.namespace, s.name
            );
        }
    }

    #[test]
    fn method_with_pointer_receiver_has_parent_server() {
        let fg = parse("package main\nfunc (s *Server) M() {}\n");
        let m = sym(&fg, "M");
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.parent, "Server");
        assert_eq!(symbol_id(m), "/tmp/test.go:Server::M");
    }

    #[test]
    fn method_with_value_receiver_has_parent_server() {
        let fg = parse("package main\nfunc (s Server) M() {}\n");
        let m = sym(&fg, "M");
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.parent, "Server");
        assert_eq!(symbol_id(m), "/tmp/test.go:Server::M");
    }

    #[test]
    fn method_with_generic_pointer_receiver_has_parent_server() {
        // `func (s *Server[T]) M()` — pointer_type → generic_type →
        // type_identifier. The helper drops the generic arguments and
        // records bare "Server".
        let src = "package main\nfunc (s *Server[T]) M() {}\n";
        let fg = parse(src);
        let m = sym(&fg, "M");
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(
            m.parent, "Server",
            "generic pointer receiver must record the bare type name"
        );
        assert_eq!(symbol_id(m), "/tmp/test.go:Server::M");
    }

    #[test]
    fn method_with_generic_value_receiver_has_parent_server() {
        // `func (s Server[T]) M()` — generic_type → type_identifier.
        let src = "package main\nfunc (s Server[T]) M() {}\n";
        let fg = parse(src);
        let m = sym(&fg, "M");
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.parent, "Server");
    }

    #[test]
    fn struct_type_spec_produces_struct_kind() {
        let fg = parse("package main\ntype Foo struct { X int }\n");
        let s = sym(&fg, "Foo");
        assert_eq!(s.kind, SymbolKind::Struct);
        assert!(s.parent.is_empty());
    }

    #[test]
    fn interface_type_spec_produces_interface_kind() {
        let fg = parse("package main\ntype Reader interface { Read() error }\n");
        let s = sym(&fg, "Reader");
        assert_eq!(s.kind, SymbolKind::Interface);
        assert!(s.parent.is_empty());
        // The interface body's `method_elem` (`Read()`) must NOT produce
        // a Symbol — the definition queries don't match method_elem, only
        // method_declaration with a receiver.
        assert!(
            !fg.symbols.iter().any(|s| s.name == "Read"),
            "interface method elements must not produce symbols"
        );
    }

    #[test]
    fn type_alias_with_equals_produces_typedef_kind() {
        // `type ID = string` — Go 1.9+ alias form, parsed as `type_alias`
        // (distinct AST node from `type_spec`).
        let fg = parse("package main\ntype ID = string\n");
        let s = sym(&fg, "ID");
        assert_eq!(s.kind, SymbolKind::Typedef);
        assert!(s.parent.is_empty());
    }

    #[test]
    fn type_spec_with_func_body_produces_typedef_kind() {
        // `type Handler func(int) error` — type_spec with function_type
        // body. Anything other than struct_type / interface_type maps to
        // Typedef.
        let fg = parse("package main\ntype Handler func(int) error\n");
        let s = sym(&fg, "Handler");
        assert_eq!(s.kind, SymbolKind::Typedef);
    }

    #[test]
    fn type_spec_with_named_type_body_produces_typedef_kind() {
        // `type Count int` — type_spec with type_identifier body.
        let fg = parse("package main\ntype Count int\n");
        let s = sym(&fg, "Count");
        assert_eq!(s.kind, SymbolKind::Typedef);
    }

    #[test]
    fn generic_function_extracted_with_correct_name_and_signature() {
        // `func Map[T any](s []T) []T {}` — Go 1.18+ generic. tree-sitter
        // records `name: identifier` and `type_parameters` as a sibling.
        // Our extractor reads the name as-is and truncate_signature drops
        // the body, leaving the type-parameter list intact in the
        // signature text.
        let fg = parse("package main\nfunc Map[T any](s []T) []T { return s }\n");
        let m = sym(&fg, "Map");
        assert_eq!(m.kind, SymbolKind::Function);
        assert!(m.parent.is_empty());
        assert!(
            m.signature.contains("Map[T any]"),
            "type parameter list must survive truncation, got: {:?}",
            m.signature
        );
        assert!(
            !m.signature.contains('{'),
            "signature must drop the body opener, got: {:?}",
            m.signature
        );
    }

    #[test]
    fn init_function_is_ordinary_function() {
        // `func init()` and `func main()` are ordinary functions in our
        // extractor — no special-casing.
        let fg = parse("package main\nfunc init() {}\nfunc main() {}\n");
        let init = sym(&fg, "init");
        let main = sym(&fg, "main");
        assert_eq!(init.kind, SymbolKind::Function);
        assert_eq!(main.kind, SymbolKind::Function);
        assert!(init.parent.is_empty());
        assert!(main.parent.is_empty());
        assert_eq!(init.namespace, "main");
    }

    #[test]
    fn signature_is_truncated_at_body_opener() {
        // Belt-and-suspenders: the signature for `func foo() { ... }`
        // must drop the body. Verifies truncate_signature is wired.
        let fg = parse("package main\nfunc foo() { x := 1; _ = x }\n");
        let s = sym(&fg, "foo");
        assert_eq!(s.signature, "func foo()");
    }

    /// CRITICAL anti-regression: `type Foo struct { Bar }` is structural
    /// composition (method-set promotion at runtime), NOT inheritance —
    /// no `Inherits` edge is emitted. Phase 6.2 establishes this design
    /// intent before call/import extraction wires up edges in 6.3/6.4.
    /// (At Phase 6.2 the edges list is unconditionally empty, but the
    /// assertion still pins the rule against a future regression.)
    #[test]
    fn embedded_struct_field_produces_no_inherits_edge() {
        let fg = parse("package main\ntype Foo struct { Bar }\n");
        // Foo must be present as a Struct.
        let foo = sym(&fg, "Foo");
        assert_eq!(foo.kind, SymbolKind::Struct);
        // Embedded field `Bar` must NOT produce a symbol — the
        // definition queries don't match field_declaration nodes.
        assert!(
            !fg.symbols.iter().any(|s| s.name == "Bar"),
            "embedded field must not produce a symbol; got: {:?}",
            fg.symbols.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
        // No Inherits edge under any circumstance.
        let inh: Vec<_> = fg
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Inherits)
            .collect();
        assert!(
            inh.is_empty(),
            "embedded field must not produce Inherits edges; got: {inh:?}"
        );
    }

    #[test]
    fn line_and_end_line_are_one_indexed_and_populated() {
        // Sanity check that line/end_line track the function declaration
        // span (1-indexed). The function below starts at row 1 (line 2,
        // 1-indexed) and ends on the same line.
        let fg = parse("package main\nfunc foo() {}\n");
        let s = sym(&fg, "foo");
        assert_eq!(s.line, 2, "func foo on line 2");
        assert_eq!(s.end_line, 2, "single-line func ends on the same line");
        assert_eq!(s.column, 0, "func declaration starts at column 0");
    }

    #[test]
    fn exported_and_unexported_names_both_extracted() {
        // Go's exportedness is a name convention (uppercase = exported).
        // The extractor must produce symbols for both regardless.
        let fg = parse("package server\nfunc Public() {}\nfunc private() {}\n");
        assert_eq!(sym(&fg, "Public").kind, SymbolKind::Function);
        assert_eq!(sym(&fg, "private").kind, SymbolKind::Function);
    }
}
