//! C# language plugin for code-graph-mcp.
//!
//! Uses tree-sitter (via the `tree-sitter` and `tree-sitter-c-sharp`
//! crates) to extract symbols, calls, import edges, and inheritance edges
//! from `.cs` source files.
//!
//! # Phase status
//!
//! Phase 2.1 shipped the crate scaffold: dependency wiring, empty query
//! string constants in [`queries`] that compile against tree-sitter-c-sharp
//! 0.23.5, the [`CSharpParser`] struct with cached `Query` objects, and
//! the [`LanguagePlugin`] impl.
//!
//! Phase 2.2 wires `extract_definitions` (classes, structs, interfaces,
//! enums, methods, constructors, local functions) and switches
//! `parse_to_filegraph` from the empty-graph stub to a real
//! tree-sitter-driven parse. Partial classes (Decision 3), default
//! interface methods (Decision 11), and extension methods (Decision 5) are
//! covered with inline tests.
//!
//! Phase 2.3 will wire `extract_calls` (direct, member-access, chained,
//! null-conditional, lambda body, LINQ, `new`/constructor).
//!
//! Phase 2.4 will wire `extract_imports` (`using`, `using static`,
//! `using A = X.Y`, `global using`, `using` inside namespace blocks).
//!
//! Phase 2.5 will wire `extract_inheritance` (`base_list` for classes,
//! structs, and interfaces — both class extension and interface
//! implementation produce the same [`EdgeKind::Inherits`] per Decision 2).
//!
//! # Default trait methods
//!
//! `CSharpParser` does NOT override [`LanguagePlugin::resolve_call`] or
//! [`LanguagePlugin::resolve_include`].
//!
//! - `resolve_call`: the default scope-aware heuristic (same file > same
//!   parent > same namespace > global) is the documented contract,
//!   mirroring the four shipped plugins. Extension method calls
//!   (`myString.CountWords()`) resolve through this same path with the
//!   same imperfection class as C++ overloaded-function resolution
//!   (Decision 5).
//! - `resolve_include`: C# imports (`using System.Collections.Generic`)
//!   are dotted namespace paths, not filesystem paths — the default
//!   basename-match resolver returns `None` for them, mirroring the
//!   Python plugin's approach to dotted module-path imports.
//!
//! # C#-specific notes
//!
//! - **Default interface methods** are distinguished from abstract ones
//!   by **presence of a method body**, not by a `default` keyword.
//!   `interface I { void Foo() { ... } }` produces a Symbol;
//!   `interface I { void Bar(); }` does not (forward-declaration rule —
//!   Decision 11). Confirmed against tree-sitter-c-sharp 0.23.5: the
//!   discriminator is the `body:` field on `method_declaration`. The
//!   body kind can be `block` (curly-brace body) or
//!   `arrow_expression_clause` (`int Foo() => 42`); both forms count as
//!   "has body" and yield a Symbol. Abstract methods have no `body:`
//!   field at all and yield no Symbol.
//! - **Partial classes** (Decision 3) emit one Class symbol per
//!   declaration; merging across files is deferred to hierarchy-walk time
//!   via the bare-name `from`-field rule. The `partial` modifier is not
//!   inspected at extraction time.
//! - **Extension methods** are syntactic methods of their enclosing
//!   static class — the `this` modifier on the first parameter does NOT
//!   remap the parent (Decision 5). The extractor never inspects parameter
//!   modifiers.
//! - **Namespace resolution** walks the ancestor chain for
//!   `namespace_declaration` nodes (block form), joining names outermost-
//!   first with `.` to match C#'s dotted namespace syntax. The file-scoped
//!   form (`namespace MyApp;`) is a top-level sibling to subsequent
//!   declarations rather than their ancestor; we look for it at the
//!   compilation_unit level when no `namespace_declaration` ancestor is
//!   found. Dotted namespace names (`namespace A.B.C { ... }`) parse with
//!   the `name:` field as a `qualified_name`; the verbatim text (`A.B.C`)
//!   becomes the namespace string.
//! - **Records** (`record_declaration`) are NOT extracted by Phase 2.2.
//!   Adding `Record` support is deliberately scoped out of this task per
//!   the brief's enumerated declaration list.

pub(crate) mod helpers;
pub(crate) mod queries;

use std::path::Path;

use code_graph_core::{FileGraph, Language, Symbol, SymbolKind};
use code_graph_lang::helpers::{find_enclosing_kind, truncate_signature};
use code_graph_lang::{LanguagePlugin, ParseError};
use streaming_iterator::StreamingIterator;
use tree_sitter::{
    Language as TsLanguage, Node, Parser as TsParser, Query, QueryCursor, Tree as TsTree,
};

use crate::queries::{CALL_QUERIES, DEFINITION_QUERIES, IMPORT_QUERIES, INHERITANCE_QUERIES};

/// File extensions the C# parser claims. Single extension `.cs` —
/// C# does not have a stub-file analogue (no `.pyi`-equivalent), and
/// `.csx` script files use the same grammar but are out of scope for
/// this plan.
pub const EXTENSIONS: &[&str] = &[".cs"];

/// C# source-file parser. Holds the tree-sitter `Language` and the four
/// pre-compiled queries used to drive symbol/edge extraction in Phases
/// 2.2-2.5.
///
/// Construct with [`CSharpParser::new`]; share across threads (queries
/// are `Send + Sync`).
pub struct CSharpParser {
    /// Compiled C# grammar. Held so per-call [`tree_sitter::Parser`]
    /// instances built inside `parse_file` can attach to it without
    /// rebuilding the `LanguageFn`.
    language: TsLanguage,
    /// Compiled definition query (live in 2.2 — drives
    /// [`Self::extract_definitions`]).
    def_query: Query,
    /// Compiled call query (wired in 2.3).
    #[allow(dead_code)] // wired in Phase 2.3 (extract_calls)
    call_query: Query,
    /// Compiled import query (wired in 2.4).
    #[allow(dead_code)] // wired in Phase 2.4 (extract_imports)
    import_query: Query,
    /// Compiled inheritance query (wired in 2.5).
    #[allow(dead_code)] // wired in Phase 2.5 (extract_inheritance)
    inheritance_query: Query,
}

impl CSharpParser {
    /// Build a new parser, compiling all four tree-sitter queries against
    /// the pinned tree-sitter-c-sharp grammar. Returns an [`anyhow::Error`]
    /// (wrapping the query compiler's message) if any query fails to
    /// compile against the pinned grammar version.
    ///
    /// Successful return is the Phase 2.1 acceptance gate that proves
    /// every query string in [`queries`] parses against tree-sitter-c-
    /// sharp 0.23.5. Phase 2.2 fills [`DEFINITION_QUERIES`]; the other
    /// three remain empty until 2.3/2.4/2.5 land their respective
    /// extractors.
    pub fn new() -> anyhow::Result<Self> {
        let language: TsLanguage = tree_sitter_c_sharp::LANGUAGE.into();

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

    /// Parse `content` (UTF-8 bytes) as C# and produce a [`FileGraph`].
    /// Internal entry point for [`Self::parse_file`] (the trait method);
    /// kept crate-private so the public surface stays the trait method
    /// while each per-extractor method (the upcoming 2.3/2.4/2.5
    /// extractors) can be tested via `parse_file` without exposing them.
    /// Mirrors the Python plugin's `parse_to_filegraph` indirection.
    ///
    /// Phase 2.2 wires `extract_definitions` into the pipeline; the call,
    /// import, and inheritance extractors are added in 2.3, 2.4, and 2.5
    /// respectively. Until those land, `parse_file` produces only Symbol
    /// records — zero edges.
    fn parse_to_filegraph(&self, path: &Path, content: &[u8]) -> Result<FileGraph, ParseError> {
        let tree = parse_tree(&self.language, content)?;
        let root = tree.root_node();
        let path_str = path.to_string_lossy().into_owned();

        let mut fg = FileGraph {
            path: path_str.clone(),
            language: Language::CSharp,
            symbols: Vec::new(),
            edges: Vec::new(),
        };

        self.extract_definitions(root, content, &path_str, &mut fg);

        Ok(fg)
    }

    /// Run the definition query and produce symbols. Mirrors the C++/Rust/
    /// Go/Python plugins' capture-name dispatch: each capture name from
    /// `DEFINITION_QUERIES` maps to a small branch that builds the right
    /// [`Symbol`].
    ///
    /// Per-capture-name behavior:
    ///
    /// - `class.name` from `class_declaration` → [`SymbolKind::Class`].
    ///   Parent is the immediate enclosing class/struct/interface (or
    ///   empty for top-level classes; nested classes record the immediate
    ///   outer class). Partial classes (Decision 3): the `partial`
    ///   modifier is NOT inspected — each declaration produces its own
    ///   Symbol; agents disambiguate via path/line.
    /// - `struct.name` from `struct_declaration` → [`SymbolKind::Struct`].
    /// - `interface.name` from `interface_declaration` →
    ///   [`SymbolKind::Interface`].
    /// - `enum.name` from `enum_declaration` → [`SymbolKind::Enum`]. Enum
    ///   members are not extracted (Decision 12 analog for C#).
    /// - `method.name` from `method_declaration` → branches on enclosing
    ///   scope:
    ///     * Inside `interface_declaration` with a `body:` field present →
    ///       [`SymbolKind::Function`], no parent (Decision 11 — default
    ///       interface methods extract as Function, matching Rust trait
    ///       default methods).
    ///     * Inside `interface_declaration` with no `body:` field →
    ///       skipped (forward-declaration rule, no Symbol record).
    ///     * Inside `class_declaration` / `struct_declaration` →
    ///       [`SymbolKind::Method`] with parent = enclosing type name.
    ///       Extension methods (Decision 5) extract here too — the
    ///       `this` parameter modifier is not inspected; the syntactic
    ///       parent wins.
    ///     * No enclosing class/struct/interface → [`SymbolKind::Function`]
    ///       with no parent (defensive: shouldn't happen in well-formed
    ///       C# but the extractor doesn't assume well-formedness).
    /// - `ctor.name` from `constructor_declaration` →
    ///   [`SymbolKind::Method`] with parent = enclosing class/struct name.
    ///   The captured name *is* the class/struct identifier (C#
    ///   constructor syntax — the constructor's name matches its enclosing
    ///   type's name). When emitted, `Symbol.name` is the constructor
    ///   identifier itself; the parent is filled from the enclosing type.
    /// - `local.name` from `local_function_statement` →
    ///   [`SymbolKind::Function`] with no parent. Local functions are
    ///   nested inside method bodies; treating them as Function (no
    ///   parent) matches the Python/Go conventions for nested
    ///   function-shaped declarations.
    ///
    /// Captures consumed without emitting a Symbol:
    /// - `class.def` / `struct.def` / `interface.def` / `enum.def` /
    ///   `method.def` / `ctor.def` / `local.def`: structural anchors used
    ///   by the queries to bind captures to the same definition. The
    ///   `name`-arm above already resolves the enclosing definition via
    ///   `find_enclosing_kind`.
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
                if text.is_empty() {
                    continue;
                }

                match cap_name {
                    "class.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "class_declaration")
                        else {
                            continue;
                        };
                        let parent = enclosing_type_name(def_node, content);
                        let namespace = enclosing_namespace(def_node, content);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Class,
                            path,
                            def_node,
                            content,
                            parent,
                            namespace,
                        ));
                    }

                    "struct.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "struct_declaration")
                        else {
                            continue;
                        };
                        let parent = enclosing_type_name(def_node, content);
                        let namespace = enclosing_namespace(def_node, content);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Struct,
                            path,
                            def_node,
                            content,
                            parent,
                            namespace,
                        ));
                    }

                    "interface.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "interface_declaration")
                        else {
                            continue;
                        };
                        let parent = enclosing_type_name(def_node, content);
                        let namespace = enclosing_namespace(def_node, content);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Interface,
                            path,
                            def_node,
                            content,
                            parent,
                            namespace,
                        ));
                    }

                    "enum.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "enum_declaration")
                        else {
                            continue;
                        };
                        let parent = enclosing_type_name(def_node, content);
                        let namespace = enclosing_namespace(def_node, content);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Enum,
                            path,
                            def_node,
                            content,
                            parent,
                            namespace,
                        ));
                    }

                    "method.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "method_declaration")
                        else {
                            continue;
                        };

                        // Decision 11: inside an interface_declaration, a
                        // method with a body extracts as Function (no
                        // parent) — matching the Rust trait-default-method
                        // contract. A method without a body is an abstract
                        // declaration and produces no Symbol (forward-
                        // declaration rule, mirroring C++/Rust/Go).
                        let in_interface =
                            find_enclosing_kind(def_node, "interface_declaration").is_some();
                        let has_body = def_node.child_by_field_name("body").is_some();

                        if in_interface {
                            if !has_body {
                                continue;
                            }
                            let namespace = enclosing_namespace(def_node, content);
                            fg.symbols.push(make_symbol(
                                text,
                                SymbolKind::Function,
                                path,
                                def_node,
                                content,
                                String::new(),
                                namespace,
                            ));
                            continue;
                        }

                        // Outside an interface: classify as Method when an
                        // enclosing class/struct exists; otherwise fall
                        // back to Function (defensive — well-formed C#
                        // can't have a method outside a type, but the
                        // extractor stays robust to recovery from syntax
                        // errors).
                        let parent = enclosing_type_name(def_node, content);
                        let namespace = enclosing_namespace(def_node, content);
                        let kind = if parent.is_empty() {
                            SymbolKind::Function
                        } else {
                            SymbolKind::Method
                        };
                        fg.symbols.push(make_symbol(
                            text, kind, path, def_node, content, parent, namespace,
                        ));
                    }

                    "ctor.name" => {
                        let Some(def_node) =
                            find_enclosing_kind(cap_node, "constructor_declaration")
                        else {
                            continue;
                        };
                        let parent = enclosing_type_name(def_node, content);
                        let namespace = enclosing_namespace(def_node, content);
                        let kind = if parent.is_empty() {
                            SymbolKind::Function
                        } else {
                            SymbolKind::Method
                        };
                        fg.symbols.push(make_symbol(
                            text, kind, path, def_node, content, parent, namespace,
                        ));
                    }

                    "local.name" => {
                        let Some(def_node) =
                            find_enclosing_kind(cap_node, "local_function_statement")
                        else {
                            continue;
                        };
                        // Local functions are nested inside method bodies;
                        // they are not members of the enclosing type, so
                        // they extract as Function with no parent (matches
                        // the Python/Go convention for nested function-
                        // shaped declarations).
                        let namespace = enclosing_namespace(def_node, content);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Function,
                            path,
                            def_node,
                            content,
                            String::new(),
                            namespace,
                        ));
                    }

                    // `*.def` captures are structural anchors — the `name`
                    // arms above resolved the enclosing definition node
                    // via `find_enclosing_kind`.
                    _ => {}
                }
            }
        }
    }
}

impl LanguagePlugin for CSharpParser {
    fn id(&self) -> Language {
        Language::CSharp
    }

    fn extensions(&self) -> &'static [&'static str] {
        EXTENSIONS
    }

    /// Parse `content` (UTF-8 bytes) as C# and produce a [`FileGraph`].
    ///
    /// Phase 2.2 wires the definition extractor; Phases 2.3/2.4/2.5 wire
    /// the call, import, and inheritance extractors.
    fn parse_file(&self, path: &Path, content: &[u8]) -> Result<FileGraph, ParseError> {
        self.parse_to_filegraph(path, content)
    }

    // resolve_call and resolve_include intentionally NOT overridden — see
    // the crate-level docstring for the rationale (default heuristic
    // matches the C++/Rust/Go/Python plugins; default basename resolver
    // is a no-op for C#'s dotted namespace `using` paths, which is the
    // intended behavior).
}

/// Build a tree-sitter [`TsTree`] for `content` against the C# grammar.
/// The caller-supplied [`TsLanguage`] is borrowed; the returned tree owns
/// its AST. Returns [`ParseError::Parse`] if `set_language` fails or if
/// tree-sitter declines to produce a tree (e.g. on cancellation). Mirrors
/// `parse_tree` in the C++/Rust/Go/Python plugins byte-for-byte modulo the
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
/// indices, matching the C++/Rust/Go/Python plugins' silent fallback.
fn capture_name_for_index<'a>(cap_names: &[&'a str], index: u32) -> &'a str {
    cap_names.get(index as usize).copied().unwrap_or("")
}

/// Return the immediate enclosing class/struct/interface name for a
/// declaration node, walking ancestors. Returns `""` when the declaration
/// is top-level (or only nested inside namespaces). Used to populate the
/// `parent` field for both nested types and methods/constructors.
///
/// Walks past `cap_node.parent()` so a class at a top-level position
/// returns `""` (not its own name); a class nested inside another class
/// records the outer class as parent.
fn enclosing_type_name(def_node: Node<'_>, content: &[u8]) -> String {
    let mut current = def_node.parent();
    while let Some(n) = current {
        match n.kind() {
            "class_declaration" | "struct_declaration" | "interface_declaration" => {
                if let Some(name_node) = n.child_by_field_name("name") {
                    return name_node.utf8_text(content).unwrap_or("").to_owned();
                }
                return String::new();
            }
            _ => {}
        }
        current = n.parent();
    }
    String::new()
}

/// Return the dotted namespace path for a declaration node by walking
/// `namespace_declaration` ancestors and joining their names outermost-
/// first with `.`. Falls back to the file-scoped form
/// (`file_scoped_namespace_declaration`) at the compilation_unit level
/// when no block-form namespace ancestor is found.
///
/// Examples:
/// - `namespace A { class X { void M() { } } }` → `M`'s namespace = `A`
/// - `namespace A { namespace B { class X { } } }` → `X`'s namespace =
///   `A.B`
/// - `namespace A.B.C { class X { } }` → `X`'s namespace = `A.B.C`
///   (the qualified_name's verbatim text)
/// - `namespace MyApp; class X { }` → `X`'s namespace = `MyApp` (the
///   file-scoped form is a sibling, not ancestor)
fn enclosing_namespace(def_node: Node<'_>, content: &[u8]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut current = def_node.parent();
    while let Some(n) = current {
        if n.kind() == "namespace_declaration" {
            if let Some(name_node) = n.child_by_field_name("name") {
                let text = name_node.utf8_text(content).unwrap_or("").to_owned();
                if !text.is_empty() {
                    parts.push(text);
                }
            }
        }
        current = n.parent();
    }
    parts.reverse();

    if !parts.is_empty() {
        return parts.join(".");
    }

    // No block-form namespace ancestor — check for a file-scoped
    // namespace declaration at the compilation_unit level. The
    // file-scoped form is a sibling of subsequent declarations, not their
    // ancestor.
    if let Some(comp_unit) = find_enclosing_kind(def_node, "compilation_unit") {
        let mut cursor = comp_unit.walk();
        for child in comp_unit.children(&mut cursor) {
            if child.kind() == "file_scoped_namespace_declaration" {
                if let Some(name_node) = child.child_by_field_name("name") {
                    return name_node.utf8_text(content).unwrap_or("").to_owned();
                }
            }
        }
    }

    String::new()
}

/// Build a [`Symbol`] from a definition node. Centralises the row/column/
/// signature math so each branch in `extract_definitions` stays small.
/// Mirrors the C++/Rust/Go/Python plugins' `make_symbol`.
#[allow(clippy::too_many_arguments)]
fn make_symbol(
    name: &str,
    kind: SymbolKind,
    path: &str,
    def_node: Node<'_>,
    content: &[u8],
    parent: String,
    namespace: String,
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
        language: Language::CSharp,
    }
}

#[cfg(test)]
mod tests {
    //! Phase 2.1 structural smoke tests + Phase 2.2 definition-extraction
    //! coverage. Behavioral coverage of call / import / inheritance
    //! extraction lands in 2.3-2.5.
    use super::*;
    use code_graph_core::symbol_id;
    use code_graph_lang::LanguagePlugin;

    // ----------------------------------------------------------------
    // Phase 2.1 — structural smoke tests
    // ----------------------------------------------------------------

    #[test]
    fn parser_is_object_safe_and_id_returns_csharp() {
        let p: Box<dyn LanguagePlugin> = Box::new(CSharpParser::new().unwrap());
        assert_eq!(p.id(), Language::CSharp);
    }

    // ----------------------------------------------------------------
    // Phase 2.2 — definition extraction
    // ----------------------------------------------------------------

    /// Parse `src` against `CSharpParser` at a synthetic absolute path.
    /// Used by every Phase 2.2 behavioral test below.
    fn parse(src: &str) -> FileGraph {
        parse_at(src, "/tmp/test.cs")
    }

    /// Parse `src` against `CSharpParser` at a caller-chosen path. Lets
    /// the partial-class anti-regression exercise two distinct paths.
    fn parse_at(src: &str, path: &str) -> FileGraph {
        let p = CSharpParser::new().unwrap();
        p.parse_file(Path::new(path), src.as_bytes())
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
        let fg = parse("");
        assert_eq!(fg.path, "/tmp/test.cs");
        assert_eq!(fg.language, Language::CSharp);
    }

    #[test]
    fn empty_file_produces_no_symbols() {
        let fg = parse("");
        assert!(fg.symbols.is_empty(), "got: {:?}", fg.symbols);
        assert!(fg.edges.is_empty(), "got: {:?}", fg.edges);
    }

    #[test]
    fn top_level_class_produces_class_symbol_no_parent() {
        let fg = parse("class Foo { }");
        assert_eq!(fg.symbols.len(), 1, "got: {:?}", fg.symbols);
        let s = sym(&fg, "Foo");
        assert_eq!(s.kind, SymbolKind::Class);
        assert!(s.parent.is_empty(), "top-level class must have no parent");
        assert!(
            s.namespace.is_empty(),
            "top-level class outside any namespace must have empty namespace"
        );
    }

    #[test]
    fn struct_produces_struct_kind() {
        let fg = parse("struct Pt { public int X; }");
        let s = sym(&fg, "Pt");
        assert_eq!(s.kind, SymbolKind::Struct);
    }

    #[test]
    fn interface_produces_interface_kind() {
        let fg = parse("interface IFoo { }");
        let s = sym(&fg, "IFoo");
        assert_eq!(s.kind, SymbolKind::Interface);
    }

    #[test]
    fn enum_produces_enum_kind_and_members_are_not_extracted() {
        let fg = parse("enum Status { Active, Inactive, Pending }");
        // Exactly one Symbol — the enum type. The enum members
        // (Active/Inactive/Pending) are NOT extracted as symbols
        // (Decision 12 analog for C#).
        assert_eq!(
            fg.symbols.len(),
            1,
            "enum members must not produce symbols: got {:?}",
            fg.symbols
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
        );
        let s = sym(&fg, "Status");
        assert_eq!(s.kind, SymbolKind::Enum);
    }

    #[test]
    fn method_in_class_produces_method_kind_with_class_parent() {
        let fg = parse(
            r#"
class Foo {
    public void Bar() { }
}
"#,
        );
        let m = sym(&fg, "Bar");
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.parent, "Foo");
    }

    #[test]
    fn method_in_struct_produces_method_kind_with_struct_parent() {
        let fg = parse(
            r#"
struct Pt {
    public int Sum() { return 0; }
}
"#,
        );
        let m = sym(&fg, "Sum");
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.parent, "Pt");
    }

    #[test]
    fn constructor_records_class_as_parent() {
        let fg = parse(
            r#"
class Foo {
    public Foo() { }
}
"#,
        );
        // Class + constructor (named `Foo` too).
        let ctor = fg
            .symbols
            .iter()
            .find(|s| s.name == "Foo" && s.kind == SymbolKind::Method)
            .unwrap_or_else(|| {
                panic!(
                    "expected a Method named Foo (constructor); got {:?}",
                    fg.symbols
                )
            });
        assert_eq!(ctor.parent, "Foo");
    }

    #[test]
    fn nested_class_records_outer_class_as_parent() {
        let fg = parse(
            r#"
class Outer {
    class Inner { }
}
"#,
        );
        let outer = sym(&fg, "Outer");
        assert!(outer.parent.is_empty(), "outer class must have no parent");

        let inner = sym(&fg, "Inner");
        assert_eq!(inner.kind, SymbolKind::Class);
        assert_eq!(inner.parent, "Outer", "nested class must record outer");
    }

    // ---- Decision 11: default interface methods --------------------

    #[test]
    fn default_interface_method_extracts_as_function_no_parent() {
        // `void Foo() { ... }` inside an interface (Decision 11) —
        // method body present → extracts as Function, NOT Method;
        // parent is empty (matches Rust trait-default-method rule).
        let fg = parse(
            r#"
interface I {
    void Foo() { return; }
}
"#,
        );
        let s = sym(&fg, "Foo");
        assert_eq!(
            s.kind,
            SymbolKind::Function,
            "default interface method must extract as Function (not Method)"
        );
        assert!(
            s.parent.is_empty(),
            "default interface method must have empty parent"
        );
    }

    #[test]
    fn expression_bodied_default_interface_method_extracts_as_function() {
        // `int Foo() => 42` inside an interface — the body field is an
        // arrow_expression_clause, not a block, but it counts as
        // "has body" for the default-interface-method rule.
        let fg = parse(
            r#"
interface I {
    int Foo() => 42;
}
"#,
        );
        let s = sym(&fg, "Foo");
        assert_eq!(
            s.kind,
            SymbolKind::Function,
            "expression-bodied default interface method must extract as Function"
        );
        assert!(s.parent.is_empty());
    }

    #[test]
    fn abstract_interface_method_produces_no_symbol() {
        // `void Bar();` inside an interface (no body) — forward
        // declaration; produces no Symbol record (mirroring C++/Rust/Go).
        let fg = parse(
            r#"
interface I {
    void Bar();
}
"#,
        );
        // Only the interface type itself surfaces.
        assert_eq!(
            fg.symbols.len(),
            1,
            "abstract interface methods must not produce a Symbol; got: {:?}",
            fg.symbols
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
        );
        let i = sym(&fg, "I");
        assert_eq!(i.kind, SymbolKind::Interface);
    }

    #[test]
    fn interface_with_mixed_methods_extracts_only_the_body_having_one() {
        // Discriminator: same interface holds an abstract method and a
        // default method. Only the default method produces a Symbol;
        // the abstract one is dropped. This is the load-bearing
        // anti-regression for Decision 11.
        let fg = parse(
            r#"
interface I {
    void HasBody() { return; }
    void NoBody();
}
"#,
        );
        // Interface + HasBody method = 2 symbols total; NoBody is
        // filtered.
        assert_eq!(
            fg.symbols.len(),
            2,
            "expected interface + HasBody; got: {:?}",
            fg.symbols
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
        );
        let s = sym(&fg, "HasBody");
        assert_eq!(s.kind, SymbolKind::Function);
        assert!(fg.symbols.iter().all(|sym| sym.name != "NoBody"));
    }

    // ---- Decision 5: extension methods -----------------------------

    #[test]
    fn extension_method_records_static_class_as_parent_not_extended_type() {
        // `this string s` parameter modifier marks `Count` as an
        // extension method on `string` (semantically). Decision 5:
        // the extractor uses the *syntactic* enclosing parent (`Ext`),
        // not the semantic extended type (`string`). The `this`
        // modifier is not inspected.
        let fg = parse(
            r#"
static class Ext {
    public static int Count(this string s) { return s.Length; }
}
"#,
        );
        let m = sym(&fg, "Count");
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(
            m.parent, "Ext",
            "extension method must record syntactic parent (Ext), not extended type (string)"
        );
    }

    // ---- Decision 3: partial classes -------------------------------

    #[test]
    fn two_partial_class_declarations_in_same_file_yield_two_class_symbols() {
        // Single-file form of the partial-class case: two `partial
        // class Foo {}` declarations side-by-side in one file. Each
        // produces its own Class symbol with the bare name `Foo`;
        // merging across declarations is deferred to hierarchy-walk
        // time.
        let fg = parse(
            r#"
public partial class Foo { void A() { } }
public partial class Foo { void B() { } }
"#,
        );
        let foos: Vec<&Symbol> = fg
            .symbols
            .iter()
            .filter(|s| s.name == "Foo" && s.kind == SymbolKind::Class)
            .collect();
        assert_eq!(
            foos.len(),
            2,
            "partial classes in the same file must produce two Class symbols; got: {:?}",
            foos
        );
    }

    #[test]
    fn two_partial_class_declarations_across_files_yield_two_class_symbols() {
        // Anti-regression for Decision 3 (verification field): two
        // `partial class Foo {}` declarations in *different files*
        // produce exactly two Class symbols, distinguishable by
        // `Symbol.file` + `Symbol.line`. The extractor does NOT merge
        // across files at extraction time.
        let fg_a = parse_at(
            "public partial class Foo { void A() { } }\n",
            "/tmp/partial_a.cs",
        );
        let fg_b = parse_at(
            "public partial class Foo { void B() { } }\n",
            "/tmp/partial_b.cs",
        );

        let a_class = fg_a
            .symbols
            .iter()
            .find(|s| s.name == "Foo" && s.kind == SymbolKind::Class)
            .expect("file A must have a Class Foo");
        let b_class = fg_b
            .symbols
            .iter()
            .find(|s| s.name == "Foo" && s.kind == SymbolKind::Class)
            .expect("file B must have a Class Foo");

        // Same name → same key into `(Language, name)` SymbolIndex,
        // matching the partial-class merge-by-bare-name rule.
        assert_eq!(a_class.name, b_class.name);
        // Different file paths → two distinct Symbol records.
        assert_ne!(
            a_class.file, b_class.file,
            "partial-class symbols must carry distinct file paths"
        );
        // Each file emits exactly one Foo Class symbol — extraction
        // does not merge across declarations at this layer.
        assert_eq!(
            fg_a.symbols
                .iter()
                .filter(|s| s.name == "Foo" && s.kind == SymbolKind::Class)
                .count(),
            1
        );
        assert_eq!(
            fg_b.symbols
                .iter()
                .filter(|s| s.name == "Foo" && s.kind == SymbolKind::Class)
                .count(),
            1
        );
    }

    // ---- Local functions ------------------------------------------

    #[test]
    fn local_function_inside_method_produces_function_no_parent() {
        // `void Helper() { }` declared inside a method body extracts as
        // Function with no parent (matching Python/Go conventions for
        // nested function-shaped declarations).
        let fg = parse(
            r#"
class C {
    public void Foo() {
        void Helper() { }
        Helper();
    }
}
"#,
        );
        let h = sym(&fg, "Helper");
        assert_eq!(h.kind, SymbolKind::Function);
        assert!(
            h.parent.is_empty(),
            "local function must have empty parent; got {:?}",
            h.parent
        );
    }

    // ---- Namespace handling ---------------------------------------

    #[test]
    fn class_inside_namespace_records_namespace_field() {
        let fg = parse(
            r#"
namespace MyApp {
    class Foo { void M() { } }
}
"#,
        );
        let foo = sym(&fg, "Foo");
        assert_eq!(foo.namespace, "MyApp");
        let m = sym(&fg, "M");
        assert_eq!(m.namespace, "MyApp", "method inherits namespace too");
    }

    #[test]
    fn nested_namespaces_join_with_dot() {
        let fg = parse(
            r#"
namespace Outer {
    namespace Inner {
        class X { }
    }
}
"#,
        );
        let x = sym(&fg, "X");
        assert_eq!(
            x.namespace, "Outer.Inner",
            "nested namespaces must join with '.'"
        );
    }

    #[test]
    fn dotted_namespace_preserves_text() {
        let fg = parse(
            r#"
namespace A.B.C {
    class X { }
}
"#,
        );
        let x = sym(&fg, "X");
        assert_eq!(x.namespace, "A.B.C");
    }

    #[test]
    fn file_scoped_namespace_populates_namespace_field() {
        // C# 10+ file-scoped namespace: `namespace MyApp;` is a sibling
        // of subsequent declarations, not their ancestor. The extractor
        // looks for it at the compilation_unit level when no block-form
        // ancestor is found.
        let fg = parse(
            r#"
namespace MyApp;

class Foo { void M() { } }
"#,
        );
        let foo = sym(&fg, "Foo");
        assert_eq!(foo.namespace, "MyApp");
        let m = sym(&fg, "M");
        assert_eq!(m.namespace, "MyApp");
    }

    // ---- Symbol shape sanity --------------------------------------

    #[test]
    fn line_and_end_line_are_one_indexed_and_populated() {
        let fg = parse(
            r#"
class Foo {
    public void Bar() {
        return;
    }
}
"#,
        );
        let foo = sym(&fg, "Foo");
        assert!(foo.line >= 1, "line is 1-indexed");
        assert!(foo.end_line >= foo.line);

        let bar = sym(&fg, "Bar");
        assert!(bar.line >= 1);
        assert!(bar.end_line >= bar.line);
    }

    #[test]
    fn signature_truncates_at_method_body() {
        let fg = parse(
            r#"
class Foo {
    public int Bar() {
        return 42;
    }
}
"#,
        );
        let bar = sym(&fg, "Bar");
        // truncate_signature drops the body — `{` is a hard cutoff.
        assert!(
            !bar.signature.contains('{'),
            "signature should drop body: got {:?}",
            bar.signature
        );
        // Whatever survives must still mention the method name.
        assert!(
            bar.signature.contains("Bar"),
            "signature should preserve method name; got {:?}",
            bar.signature
        );
    }

    #[test]
    fn symbol_id_for_method_uses_parent_form() {
        // Sanity that the extracted method's parent flows through into
        // `symbol_id` correctly — `path:Class::method` shape.
        let fg = parse_at(
            r#"
class Foo {
    public void Bar() { }
}
"#,
            "/abs/foo.cs",
        );
        let bar = sym(&fg, "Bar");
        assert_eq!(symbol_id(bar), "/abs/foo.cs:Foo::Bar");
    }
}
