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
//! stubbed `parse_file` that returns an empty `FileGraph`.
//!
//! Phase 5.2 wires `extract_definitions` into `parse_file`. Phases 5.3 and
//! 5.4 fill in:
//!
//! - **Phase 5.3** — use-tree expansion (recursive walk for grouped /
//!   wildcard / aliased imports) and `extern crate`
//! - **Phase 5.4** — call extraction (direct, method, scoped, macro) and
//!   inheritance edges (`impl Trait for Type`)
//!
//! # Known Rust parser limitations
//!
//! These match the documented design and apply to the Rust parser as it is
//! built out. They are intentional, not bugs.
//!
//! 1. **`macro_rules!` definitions are not extracted as symbols.** Only
//!    invocations produce call edges (Phase 5.4). The `DEFINITION_QUERIES`
//!    constant explicitly does not match `macro_definition` (the
//!    tree-sitter-rust 0.24 node type that wraps `macro_rules!` blocks),
//!    and Phase 5.2 ships an anti-regression test
//!    (`macro_rules_definition_produces_zero_symbols`) that asserts a
//!    fixture with `macro_rules! foo { ... }` yields no Symbol records.
//! 2. **Forward declarations excluded.** Trait method declarations like
//!    `fn bar();` are `function_signature_item`, not `function_item`, in
//!    tree-sitter-rust 0.24. The DEFINITION_QUERIES only match
//!    `function_item` (which requires a body), so trait method
//!    declarations without bodies do not produce symbols. Methods inside
//!    `impl Trait for Type { ... }` blocks DO produce symbols (with
//!    parent=Type). Default methods inside trait bodies (with bodies)
//!    also produce symbols.
//! 3. **`#[derive(...)]` and proc-macro attributes** appear as
//!    `attribute_item` (not `macro_invocation`) so they are NOT captured
//!    as call edges (Phase 5.4 limitation).
//! 4. **Call resolution is heuristic** — same-file > same-parent >
//!    same-namespace > global, identical to the C++ plugin's behavior via
//!    the default `LanguagePlugin::resolve_call` impl.
//! 5. **Complex use trees expanded but lifetime/generic constraints not
//!    represented.** Use-edge `to` fields record the dotted path; generic
//!    parameters and lifetime bounds are not part of the edge.

pub(crate) mod helpers;
pub(crate) mod queries;

use std::path::Path;

use codegraph_core::{FileGraph, Language, Symbol, SymbolKind};
use codegraph_lang::{LanguagePlugin, ParseError};
use streaming_iterator::StreamingIterator;
use tree_sitter::{
    Language as TsLanguage, Node, Parser as TsParser, Query, QueryCursor, Tree as TsTree,
};

use crate::helpers::{find_enclosing_impl, resolve_mod_namespace, truncate_signature};
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
    language: TsLanguage,
    /// Compiled definition query.
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

    /// Parse `content` (UTF-8 bytes) as Rust and produce a [`FileGraph`].
    /// Used internally by [`Self::parse_file`] and by the inline tests;
    /// kept crate-private so the public surface stays the trait method.
    fn parse_to_filegraph(&self, path: &Path, content: &[u8]) -> Result<FileGraph, ParseError> {
        let tree = parse_tree(&self.language, content)?;
        let root = tree.root_node();
        let path_str = path.to_string_lossy().into_owned();

        let mut fg = FileGraph {
            path: path_str.clone(),
            language: Language::Rust,
            symbols: Vec::new(),
            edges: Vec::new(),
        };

        self.extract_definitions(root, content, &path_str, &mut fg);
        // TODO(Phase 5.3): self.extract_uses(root, content, &path_str, &mut fg);
        // TODO(Phase 5.4): self.extract_calls(root, content, &path_str, &mut fg);
        // TODO(Phase 5.4): self.extract_inheritance(root, content, &path_str, &mut fg);

        Ok(fg)
    }

    /// Run the definition query and produce symbols. Mirrors the C++
    /// `extract_definitions`'s capture-name dispatch: each capture name
    /// from `DEFINITION_QUERIES` maps to a small branch that builds the
    /// right `Symbol`.
    ///
    /// Per-node-type behavior:
    ///
    /// - `function_item` whose ancestor walk via [`find_enclosing_impl`]
    ///   returns `Some(impl_node)` → [`SymbolKind::Method`], parent =
    ///   `impl_node.child_by_field_name("type")` text. For
    ///   `impl Trait for Type { fn m() }` the parent is **`Type`, not
    ///   `Trait`** — the trait relationship lives only in the inheritance
    ///   edge (Phase 5.4). The trait-impl-method test
    ///   (`trait_impl_method_parent_is_type_not_trait`) is the
    ///   anti-regression for that rule.
    /// - `function_item` at module level → [`SymbolKind::Function`], no
    ///   parent.
    /// - `struct_item` → [`SymbolKind::Struct`].
    /// - `enum_item` → [`SymbolKind::Enum`].
    /// - `trait_item` → [`SymbolKind::Trait`].
    /// - `type_item` → [`SymbolKind::Typedef`].
    /// - `mod_item` is **not** emitted as a `Symbol` itself — modules act
    ///   as namespace anchors only. `resolve_mod_namespace` walks the
    ///   ancestor chain to populate `Symbol.namespace` (`a::b::c`) on the
    ///   symbols *inside* a `mod a { mod b { mod c { ... } } }` chain.
    fn extract_definitions(&self, root: Node<'_>, content: &[u8], path: &str, fg: &mut FileGraph) {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.def_query, root, content);
        let cap_names = self.def_query.capture_names();
        let content_str = std::str::from_utf8(content).unwrap_or("");

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
                        let Some(def_node) = find_enclosing_kind(cap_node, "function_item") else {
                            continue;
                        };
                        let (kind, parent) = match find_enclosing_impl(cap_node) {
                            Some(impl_node) => {
                                // Trait-impl disambiguation: parent is the
                                // `type` field (the implementing type),
                                // never the `trait` field. For
                                // `impl Trait for Type { fn m() }` parent
                                // = Type. For `impl Type { fn m() }` parent
                                // = Type. For both, the symbol ID becomes
                                // `path:Type::m`.
                                let parent_text = impl_node
                                    .child_by_field_name("type")
                                    .and_then(|n| n.utf8_text(content).ok())
                                    .unwrap_or("")
                                    .to_owned();
                                (SymbolKind::Method, parent_text)
                            }
                            None => (SymbolKind::Function, String::new()),
                        };
                        let ns = resolve_mod_namespace(cap_node, content_str);
                        fg.symbols
                            .push(make_symbol(text, kind, path, def_node, content, ns, parent));
                    }

                    "struct.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "struct_item") else {
                            continue;
                        };
                        let ns = resolve_mod_namespace(cap_node, content_str);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Struct,
                            path,
                            def_node,
                            content,
                            ns,
                            String::new(),
                        ));
                    }

                    "enum.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "enum_item") else {
                            continue;
                        };
                        let ns = resolve_mod_namespace(cap_node, content_str);
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

                    "trait.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "trait_item") else {
                            continue;
                        };
                        let ns = resolve_mod_namespace(cap_node, content_str);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Trait,
                            path,
                            def_node,
                            content,
                            ns,
                            String::new(),
                        ));
                    }

                    "type.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "type_item") else {
                            continue;
                        };
                        let ns = resolve_mod_namespace(cap_node, content_str);
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

                    // mod_item captures are intentionally consumed without
                    // emitting a Symbol — modules are namespace anchors,
                    // not symbols. `resolve_mod_namespace` walks the
                    // ancestor chain on the symbols defined *inside* a
                    // mod block to populate `Symbol.namespace`.
                    "mod.name" => {}

                    _ => {}
                }
            }
        }
    }
}

impl LanguagePlugin for RustParser {
    fn id(&self) -> Language {
        Language::Rust
    }

    fn extensions(&self) -> &'static [&'static str] {
        EXTENSIONS
    }

    fn parse_file(&self, path: &Path, content: &[u8]) -> Result<FileGraph, ParseError> {
        self.parse_to_filegraph(path, content)
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

/// Build a tree-sitter [`TsTree`] for `content` against the Rust grammar.
/// The caller-supplied [`TsLanguage`] is borrowed; the returned tree owns
/// its AST. Returns [`ParseError::Parse`] if `set_language` fails or if
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

/// Look up a capture name by index. Returns `""` (empty) on out-of-range
/// indices, matching the C++ plugin's silent fallback.
fn capture_name_for_index<'a>(cap_names: &[&'a str], index: u32) -> &'a str {
    cap_names.get(index as usize).copied().unwrap_or("")
}

/// Walk up `node`'s parent chain, returning the first ancestor (including
/// `node` itself) whose kind matches `kind`. Local copy of the C++
/// plugin's `find_enclosing_kind` — used to find the `function_item`,
/// `struct_item`, etc. that contains a captured `name` node.
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

/// Build a [`Symbol`] from a definition node. Centralizes the row/column/
/// signature math so each branch in `extract_definitions` stays small.
/// Mirrors the C++ plugin's `make_symbol`.
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
        language: Language::Rust,
    }
}

#[cfg(test)]
mod tests {
    //! Phase 5.1 structural smoke tests + Phase 5.2 definition extraction
    //! coverage.
    //!
    //! Behavioral coverage for uses (5.3), calls (5.4), and inheritance
    //! (5.4) lands alongside the corresponding `extract_*` loops.
    use super::*;
    use codegraph_core::symbol_id;

    // ----------------------------------------------------------------
    // Phase 5.1 — structural smoke tests
    // ----------------------------------------------------------------

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

    // ----------------------------------------------------------------
    // Phase 5.2 — definition extraction
    // ----------------------------------------------------------------

    /// Parse `src` against `RustParser` and return the resulting
    /// FileGraph at a synthetic absolute path. Used by every Phase 5.2
    /// behavioral test below.
    fn parse(src: &str) -> FileGraph {
        let p = RustParser::new().unwrap();
        p.parse_file(Path::new("/tmp/test.rs"), src.as_bytes())
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
        // Phase 5.1's empty-graph stub assertion is now obsolete — 5.2
        // populates symbols. Keep the path/language assertion which
        // still belongs at this layer.
        let fg = parse("fn foo() {}");
        assert_eq!(fg.path, "/tmp/test.rs");
        assert_eq!(fg.language, Language::Rust);
    }

    #[test]
    fn free_function_produces_function_kind_no_parent() {
        let fg = parse("fn foo() {}");
        let s = sym(&fg, "foo");
        assert_eq!(s.kind, SymbolKind::Function);
        assert!(s.parent.is_empty(), "free fn must have empty parent");
        assert!(s.namespace.is_empty(), "top-level fn has no namespace");
        assert_eq!(s.language, Language::Rust);
    }

    #[test]
    fn inherent_impl_method_has_parent_equals_type() {
        let fg = parse("struct Foo; impl Foo { fn bar() {} }");
        let bar = sym(&fg, "bar");
        assert_eq!(bar.kind, SymbolKind::Method);
        assert_eq!(bar.parent, "Foo");
        assert_eq!(symbol_id(bar), "/tmp/test.rs:Foo::bar");
    }

    /// CRITICAL anti-regression: for `impl Trait for Type { fn m() }` the
    /// method's parent MUST be `Type`, never `Trait`. The trait
    /// relationship lives only in the inheritance edge (Phase 5.4).
    #[test]
    fn trait_impl_method_parent_is_type_not_trait() {
        let src = "trait Trait {} struct Foo; impl Trait for Foo { fn bar() {} }";
        let fg = parse(src);
        let bar = sym(&fg, "bar");
        assert_eq!(bar.kind, SymbolKind::Method);
        assert_eq!(
            bar.parent, "Foo",
            "trait-impl method parent must be the implementing type, not the trait"
        );
        assert_ne!(bar.parent, "Trait", "must NOT use trait name as parent");
        assert_eq!(symbol_id(bar), "/tmp/test.rs:Foo::bar");
    }

    #[test]
    fn struct_item_produces_struct_kind() {
        let fg = parse("struct Foo { x: i32 }");
        let s = sym(&fg, "Foo");
        assert_eq!(s.kind, SymbolKind::Struct);
        assert!(s.parent.is_empty());
    }

    #[test]
    fn enum_item_produces_enum_kind() {
        let fg = parse("enum Color { Red, Green, Blue }");
        let s = sym(&fg, "Color");
        assert_eq!(s.kind, SymbolKind::Enum);
    }

    #[test]
    fn trait_item_produces_trait_kind() {
        let fg = parse("trait Speak { fn hello(&self); }");
        let s = sym(&fg, "Speak");
        assert_eq!(s.kind, SymbolKind::Trait);
        // Trait method declaration `fn hello(&self);` is a
        // function_signature_item (no body) and is intentionally NOT
        // emitted as a Symbol — see crate-level limitations docstring.
        assert!(
            !fg.symbols.iter().any(|s| s.name == "hello"),
            "trait method declarations without bodies must not produce symbols"
        );
    }

    #[test]
    fn type_item_produces_typedef_kind() {
        let fg = parse("type MyInt = i32;");
        let s = sym(&fg, "MyInt");
        assert_eq!(s.kind, SymbolKind::Typedef);
    }

    #[test]
    fn generic_function_with_type_bound() {
        // `fn foo<T: Display>(x: T) {}` — must parse without crashing
        // and the signature must be truncated at `{`.
        let fg = parse("use std::fmt::Display; fn foo<T: Display>(x: T) {}");
        let s = sym(&fg, "foo");
        assert_eq!(s.kind, SymbolKind::Function);
        assert!(
            !s.signature.contains('{'),
            "signature must be truncated at the body opener, got: {:?}",
            s.signature
        );
        assert!(
            s.signature.contains("foo<T: Display>"),
            "type bound must survive truncation, got: {:?}",
            s.signature
        );
    }

    #[test]
    fn generic_function_with_where_clause() {
        let fg = parse("use std::fmt::Display; fn foo<T>(x: T) where T: Display { let _ = x; }");
        let s = sym(&fg, "foo");
        assert_eq!(s.kind, SymbolKind::Function);
        assert!(
            s.signature.contains("where T: Display"),
            "where clause must survive truncation, got: {:?}",
            s.signature
        );
        assert!(!s.signature.contains('{'));
    }

    #[test]
    fn lifetime_parameters() {
        let fg = parse("fn longest<'a>(x: &'a str) -> &'a str { x }");
        let s = sym(&fg, "longest");
        assert_eq!(s.kind, SymbolKind::Function);
        assert!(
            s.signature.contains("longest<'a>"),
            "lifetime param must survive, got: {:?}",
            s.signature
        );
        assert!(s.signature.contains("-> &'a str"));
    }

    #[test]
    fn async_const_unsafe_fn() {
        // All three modifier forms produce Function (or Method inside
        // an impl). Body content irrelevant — we only check kind.
        let fg = parse("async fn a_fn() {} const fn c_fn() -> i32 { 0 } unsafe fn u_fn() {}");
        for name in ["a_fn", "c_fn", "u_fn"] {
            let s = sym(&fg, name);
            assert_eq!(
                s.kind,
                SymbolKind::Function,
                "async/const/unsafe fn must extract as Function, got {:?} for {name}",
                s.kind
            );
        }
    }

    #[test]
    fn async_fn_inside_impl_is_method() {
        // Same modifier handling, but inside an impl → Method.
        let fg = parse("struct Foo; impl Foo { async fn run(&self) {} }");
        let s = sym(&fg, "run");
        assert_eq!(s.kind, SymbolKind::Method);
        assert_eq!(s.parent, "Foo");
    }

    #[test]
    fn nested_mods_produce_namespace_a_b_c() {
        let fg = parse("mod a { mod b { mod c { fn x() {} } } }");
        let x = sym(&fg, "x");
        assert_eq!(
            x.namespace, "a::b::c",
            "nested mods must produce namespace joined with `::`"
        );
        // mod_items themselves do NOT produce Symbols (they're namespace
        // anchors). The only symbol in this fixture is `x`.
        assert!(
            !fg.symbols.iter().any(|s| s.name == "a"),
            "mod_item must not emit a Symbol named after the module"
        );
        assert!(!fg.symbols.iter().any(|s| s.name == "b"));
        assert!(!fg.symbols.iter().any(|s| s.name == "c"));
        assert_eq!(
            fg.symbols.len(),
            1,
            "exactly one symbol expected (the inner fn x)"
        );
    }

    /// CRITICAL anti-regression: `macro_rules! foo { ... }` parses as a
    /// `macro_definition` node (tree-sitter-rust 0.24 names the wrapping
    /// node `macro_definition`, not `macro_rules_definition`). The
    /// DEFINITION_QUERIES intentionally do not match it, so this fixture
    /// must yield zero symbols. If the queries ever drift to capture
    /// macro definitions, this test catches it.
    #[test]
    fn macro_rules_definition_produces_zero_symbols() {
        let fg = parse("macro_rules! foo { () => {} }");
        assert!(
            fg.symbols.is_empty(),
            "macro_rules! definitions must produce zero symbols; got: {:?}",
            fg.symbols.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn signature_is_truncated_at_body_opener() {
        // Belt-and-suspenders: the signature for `fn foo() { ... }`
        // must drop the body. Verifies truncate_signature is wired.
        let fg = parse("fn foo() { let _ = 42; let _ = \"abc\"; }");
        let s = sym(&fg, "foo");
        assert_eq!(s.signature, "fn foo()");
    }
}
