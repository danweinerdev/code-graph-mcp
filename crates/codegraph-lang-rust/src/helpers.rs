//! Helper routines for the Rust parser.
//!
//! Phase status: Phase 5.1 ships the small ancestor-walk helpers
//! ([`find_enclosing_impl`], [`resolve_mod_namespace`]) in working form,
//! plus [`split_use_path`] as a stub returning `vec![]`. Phase 5.3 fills the
//! recursive use-tree walker.
//!
//! The module itself is `pub(crate)`; the individual functions are `pub` as
//! a crate-internal convention so callers within `lib.rs` can `use` them
//! freely. The effective visibility cap remains crate-internal.

use tree_sitter::Node;

/// Walk a `use_tree` (the `argument` field of a `use_declaration`) and
/// produce one fully-qualified path string per terminal leaf.
///
/// Phase 5.3 implements this. Expected behavior (from the phase-doc verification):
/// - `use foo` → `["foo"]`
/// - `use foo::bar` → `["foo::bar"]`
/// - `use foo::{a, b}` → `["foo::a", "foo::b"]`
/// - `use foo::*` → `["foo::*"]`
/// - `use foo as bar` → `["foo"]` (alias dropped; the path is what we record)
/// - `use std::{io::{self, Read}, collections::HashMap}` →
///   `["std::io", "std::io::Read", "std::collections::HashMap"]`
///
/// Until Phase 5.3 lands, this returns an empty `Vec` so the surrounding
/// extractor compiles and the queries-compile gate (`RustParser::new()` →
/// `Ok`) is the only behavior under test in 5.1.
// TODO(5.3): recursive walk over use_tree variants (identifier,
// scoped_identifier, scoped_use_list, use_list, use_wildcard,
// use_as_clause, self).
#[allow(dead_code)] // wired in Phase 5.3
pub fn split_use_path(_use_tree: Node<'_>, _content: &str) -> Vec<String> {
    Vec::new()
}

/// Walk `node`'s parent chain and return the first ancestor that is an
/// `impl_item`, or `None` if `node` is not inside an impl block.
///
/// Used by Phase 5.2's definition extractor to decide whether a
/// `function_item` is a free function or a method, and to look up the
/// impl block's `type` field for the parent.
#[allow(dead_code)] // wired in Phase 5.2
pub fn find_enclosing_impl(node: Node<'_>) -> Option<Node<'_>> {
    let mut current = Some(node);
    while let Some(n) = current {
        if n.kind() == "impl_item" {
            return Some(n);
        }
        current = n.parent();
    }
    None
}

/// Walk `node`'s parent chain, collecting the names of every enclosing
/// `mod_item`, and join them outermost-first with `::`.
///
/// `mod a { mod b { fn x() {} } }` → for the `fn x` node, returns `"a::b"`.
/// A node with no enclosing `mod_item` returns the empty string.
#[allow(dead_code)] // wired in Phase 5.2
pub fn resolve_mod_namespace(node: Node<'_>, content: &str) -> String {
    let bytes = content.as_bytes();
    let mut parts: Vec<String> = Vec::new();
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "mod_item" {
            if let Some(name_node) = n.child_by_field_name("name") {
                parts.push(name_node.utf8_text(bytes).unwrap_or("").to_owned());
            }
        }
        current = n.parent();
    }
    parts.reverse();
    parts.join("::")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser as TsParser;

    /// Parse a snippet of Rust source against tree-sitter-rust. Used by the
    /// helper tests to build a real AST without going through `RustParser`.
    fn parse(src: &str) -> tree_sitter::Tree {
        let mut parser = TsParser::new();
        let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&language).expect("set_language");
        parser.parse(src, None).expect("parse")
    }

    /// Find the first descendant whose `kind() == kind`.
    fn find_first<'a>(node: tree_sitter::Node<'a>, kind: &str) -> Option<tree_sitter::Node<'a>> {
        if node.kind() == kind {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(n) = find_first(child, kind) {
                return Some(n);
            }
        }
        None
    }

    #[test]
    fn split_use_path_stub_returns_empty() {
        // Phase 5.1 stub: walker returns no paths until 5.3 fills it in.
        let src = "use foo;";
        let tree = parse(src);
        let use_tree =
            find_first(tree.root_node(), "identifier").expect("identifier inside use_declaration");
        assert!(split_use_path(use_tree, src).is_empty());
    }

    #[test]
    fn find_enclosing_impl_returns_some_for_impl_method() {
        let src = "impl Foo { fn bar() {} }";
        let tree = parse(src);
        // The `fn bar` is a function_item nested inside impl_item.
        let func = find_first(tree.root_node(), "function_item").expect("function_item");
        let impl_node = find_enclosing_impl(func).expect("must find impl_item ancestor");
        assert_eq!(impl_node.kind(), "impl_item");
    }

    #[test]
    fn find_enclosing_impl_returns_none_for_free_function() {
        let src = "fn foo() {}";
        let tree = parse(src);
        let func = find_first(tree.root_node(), "function_item").expect("function_item");
        assert!(find_enclosing_impl(func).is_none());
    }

    #[test]
    fn resolve_mod_namespace_empty_at_top_level() {
        let src = "fn top() {}";
        let tree = parse(src);
        let func = find_first(tree.root_node(), "function_item").expect("function_item");
        assert_eq!(resolve_mod_namespace(func, src), "");
    }

    #[test]
    fn resolve_mod_namespace_single_mod() {
        let src = "mod a { fn x() {} }";
        let tree = parse(src);
        let func = find_first(tree.root_node(), "function_item").expect("function_item");
        assert_eq!(resolve_mod_namespace(func, src), "a");
    }

    #[test]
    fn resolve_mod_namespace_nested_mods_join_with_double_colon() {
        let src = "mod a { mod b { mod c { fn x() {} } } }";
        let tree = parse(src);
        let func = find_first(tree.root_node(), "function_item").expect("function_item");
        assert_eq!(resolve_mod_namespace(func, src), "a::b::c");
    }
}
