//! Helper routines for the Rust parser.
//!
//! Phase status: Phase 5.1 ships the small ancestor-walk helpers
//! ([`find_enclosing_impl`], [`resolve_mod_namespace`]) in working form;
//! Phase 5.3 promotes [`split_use_path`] from a stub to a full recursive
//! walker over `use_tree` variants.
//!
//! The module itself is `pub(crate)`; the individual functions are `pub` as
//! a crate-internal convention so callers within `lib.rs` can `use` them
//! freely. The effective visibility cap remains crate-internal.

use tree_sitter::Node;

/// Walk a `use_tree` (the `argument` field of a `use_declaration`, or any
/// node nested inside a `scoped_use_list`/`use_list`) and produce one
/// fully-qualified path string per terminal leaf.
///
/// Behavior (from Phase 5.3's verification):
/// - `use foo;` → `["foo"]`
/// - `use foo::bar;` → `["foo::bar"]`
/// - `use foo::{a, b};` → `["foo::a", "foo::b"]`
/// - `use foo::{a, b::c};` → `["foo::a", "foo::b::c"]`
/// - `use foo::*;` → `["foo::*"]`
/// - `use foo as bar;` → `["foo"]` (alias dropped)
/// - `use std::io::{self, Read};` → `["std::io", "std::io::Read"]`
/// - `use std::{io::{self, Read}, collections::HashMap};` →
///   `["std::io", "std::io::Read", "std::collections::HashMap"]`
///
/// Top-level callers pass the use_declaration's `argument` node and an
/// empty scope. The function recurses inside grouped/nested forms with the
/// running scope joined by `::`.
pub fn split_use_path(use_tree: Node<'_>, content: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    walk_use_tree(use_tree, "", content, &mut out);
    out
}

/// Inner recursive walker. `scope` is the dotted prefix accumulated from
/// enclosing `scoped_use_list` paths (e.g. `"std::io"` when descending into
/// the `{self, Read}` of `use std::io::{self, Read}`).
fn walk_use_tree(node: Node<'_>, scope: &str, content: &str, out: &mut Vec<String>) {
    let bytes = content.as_bytes();
    match node.kind() {
        // Terminal: bare identifier or dotted path. The source text already
        // is the full path (`foo` or `foo::bar::baz`), so we just glue it
        // onto the running scope.
        "identifier" | "scoped_identifier" | "crate" | "super" | "metavariable" => {
            let leaf = node.utf8_text(bytes).unwrap_or("");
            out.push(join_scope(scope, leaf));
        }

        // Bare `self` inside a use_list: emits the parent scope itself as a
        // leaf path. Example: `use std::io::{self, Read};` produces
        // `std::io` from the `self` token (with scope `std::io`).
        // `use self;` at top level (scope empty) is grammatically odd and
        // not in the spec; we skip it via the empty-scope guard.
        "self" if !scope.is_empty() => {
            out.push(scope.to_owned());
        }

        // `foo::{a, b, ...}`: the `path` field is a prefix; the `list`
        // field is a use_list whose children are nested use_trees.
        "scoped_use_list" => {
            let path_text = node
                .child_by_field_name("path")
                .and_then(|n| n.utf8_text(bytes).ok())
                .unwrap_or("");
            let new_scope = join_scope(scope, path_text);
            if let Some(list) = node.child_by_field_name("list") {
                walk_use_tree(list, &new_scope, content, out);
            }
        }

        // `{a, b, c::d}`: walk each named child with the same scope.
        "use_list" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                walk_use_tree(child, scope, content, out);
            }
        }

        // `foo::*`: the named child (if any) is the prefix path; emit one
        // entry of `<scope>::<prefix>::*` (or `<scope>::*` if no prefix).
        "use_wildcard" => {
            let prefix = node
                .named_children(&mut node.walk())
                .next()
                .and_then(|n| n.utf8_text(bytes).ok())
                .unwrap_or("");
            let combined = join_scope(scope, prefix);
            let star = if combined.is_empty() {
                "*".to_owned()
            } else {
                format!("{combined}::*")
            };
            out.push(star);
        }

        // `foo as bar`: alias is dropped, recurse on the `path` field.
        "use_as_clause" => {
            if let Some(path) = node.child_by_field_name("path") {
                walk_use_tree(path, scope, content, out);
            }
        }

        // Defensive: an unexpected node type contributes nothing. Keeping
        // silent rather than panicking matches the C++ extractor's posture
        // toward unrecognized captures.
        _ => {}
    }
}

/// Join a running scope with a leaf segment using `::`. Either side may be
/// empty: empty scope returns the leaf as-is; empty leaf returns the scope.
fn join_scope(scope: &str, leaf: &str) -> String {
    match (scope.is_empty(), leaf.is_empty()) {
        (true, _) => leaf.to_owned(),
        (false, true) => scope.to_owned(),
        (false, false) => format!("{scope}::{leaf}"),
    }
}

/// Walk `node`'s parent chain and return the first ancestor that is an
/// `impl_item`, or `None` if `node` is not inside an impl block.
///
/// Used by Phase 5.2's definition extractor to decide whether a
/// `function_item` is a free function or a method, and to look up the
/// impl block's `type` field for the parent.
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

/// Truncate a signature at the first `{` or `;`, dropping the body and any
/// trailing whitespace. Falls back to a 200-byte cutoff when neither marker
/// is found, returning `<prefix>...`. Mirrors the C++ plugin's
/// `truncate_signature` byte-for-byte so signatures across languages share
/// the same shape.
///
/// The cutoff is computed via `char_indices`, so the slice boundary is
/// guaranteed to land on a UTF-8 char boundary by construction. Multi-byte
/// content past 200 bytes does not panic.
pub fn truncate_signature(s: &str) -> String {
    for (i, c) in s.char_indices() {
        if c == '{' || c == ';' {
            return s[..i].trim_end_matches([' ', '\t', '\n', '\r']).to_owned();
        }
        if i >= 200 {
            return format!("{}...", &s[..i]);
        }
    }
    s.to_owned()
}

/// Walk `node`'s parent chain, collecting the names of every enclosing
/// `mod_item`, and join them outermost-first with `::`.
///
/// `mod a { mod b { fn x() {} } }` → for the `fn x` node, returns `"a::b"`.
/// A node with no enclosing `mod_item` returns the empty string.
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

/// Build a `path:fn_name` (free fn) or `path:Type::fn_name` (impl method)
/// symbol-ID anchor for the function enclosing `node`. Mirrors the C++
/// plugin's `enclosing_function_id` and matches the `symbol_id()` shape
/// produced by Phase 5.2's definition extractor so call edges' `from`
/// fields line up exactly with definition IDs.
///
/// Behavior:
/// - No enclosing `function_item` (e.g. a call at module-level inside a
///   `static` initializer) → returns `path` (the bare file path), matching
///   the C++ top-level-call rule.
/// - `function_item` with no enclosing `impl_item` → returns
///   `<path>:<fn_name>`.
/// - `function_item` inside an `impl_item` → returns
///   `<path>:<Type>::<fn_name>` where `Type` is the impl's `type` field
///   text. For `impl Trait for Type { fn m() }` the prefix is `Type`,
///   never `Trait` — matches Phase 5.2's trait-impl disambiguation.
/// - Closures (`closure_expression`) are transparent: a call inside a
///   closure walks past the closure and reports the closure's enclosing
///   `function_item` as the `from`.
pub fn enclosing_function_id(node: Node<'_>, content: &[u8], path: &str) -> String {
    let Some(func) = find_enclosing_kind(node, "function_item") else {
        return path.to_owned();
    };
    let Some(name_node) = func.child_by_field_name("name") else {
        return path.to_owned();
    };
    let fn_name = name_node.utf8_text(content).unwrap_or("");

    match find_enclosing_impl(func) {
        Some(impl_node) => {
            let parent_type = impl_node
                .child_by_field_name("type")
                .and_then(|n| n.utf8_text(content).ok())
                .unwrap_or("");
            if parent_type.is_empty() {
                format!("{path}:{fn_name}")
            } else {
                format!("{path}:{parent_type}::{fn_name}")
            }
        }
        None => format!("{path}:{fn_name}"),
    }
}

/// Walk `node`'s parent chain, returning the first ancestor (including
/// `node` itself) whose kind matches `kind`. Crate-internal helper used
/// by [`enclosing_function_id`] (mirrors the same-named helper in `lib.rs`,
/// kept here so the helper module is self-contained for testing).
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

    /// Locate the `argument` node of the (first) `use_declaration` in
    /// `tree`. Phase 5.3 helper tests drive `split_use_path` against this
    /// node directly to verify per-form behavior in isolation from
    /// `extract_uses`.
    fn use_argument<'a>(tree: &'a tree_sitter::Tree) -> tree_sitter::Node<'a> {
        let decl = find_first(tree.root_node(), "use_declaration")
            .expect("source must contain a use_declaration");
        decl.child_by_field_name("argument")
            .expect("use_declaration must expose an `argument` field")
    }

    #[test]
    fn split_use_path_simple_identifier() {
        let src = "use foo;";
        let tree = parse(src);
        assert_eq!(split_use_path(use_argument(&tree), src), vec!["foo"]);
    }

    #[test]
    fn split_use_path_scoped_identifier() {
        let src = "use foo::bar;";
        let tree = parse(src);
        assert_eq!(split_use_path(use_argument(&tree), src), vec!["foo::bar"]);
    }

    #[test]
    fn split_use_path_use_list_flat() {
        let src = "use foo::{a, b};";
        let tree = parse(src);
        assert_eq!(
            split_use_path(use_argument(&tree), src),
            vec!["foo::a", "foo::b"]
        );
    }

    #[test]
    fn split_use_path_use_list_nested_path() {
        let src = "use foo::{a, b::c};";
        let tree = parse(src);
        assert_eq!(
            split_use_path(use_argument(&tree), src),
            vec!["foo::a", "foo::b::c"]
        );
    }

    #[test]
    fn split_use_path_wildcard() {
        let src = "use foo::*;";
        let tree = parse(src);
        assert_eq!(split_use_path(use_argument(&tree), src), vec!["foo::*"]);
    }

    #[test]
    fn split_use_path_as_clause_drops_alias() {
        let src = "use foo as bar;";
        let tree = parse(src);
        assert_eq!(split_use_path(use_argument(&tree), src), vec!["foo"]);
    }

    #[test]
    fn split_use_path_self_in_list_emits_parent_scope() {
        let src = "use std::io::{self, Read};";
        let tree = parse(src);
        // `self` becomes `std::io`; `Read` becomes `std::io::Read`.
        assert_eq!(
            split_use_path(use_argument(&tree), src),
            vec!["std::io", "std::io::Read"]
        );
    }

    #[test]
    fn split_use_path_deeply_nested() {
        let src = "use std::{io::{self, Read}, collections::HashMap};";
        let tree = parse(src);
        assert_eq!(
            split_use_path(use_argument(&tree), src),
            vec!["std::io", "std::io::Read", "std::collections::HashMap"]
        );
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

    #[test]
    fn truncate_signature_stops_at_brace() {
        // Body opener strips for fn signatures.
        assert_eq!(truncate_signature("fn foo() { return; }"), "fn foo()");
    }

    #[test]
    fn truncate_signature_stops_at_semicolon() {
        // Trait method declarations end in `;` (function_signature_item).
        assert_eq!(truncate_signature("fn foo();"), "fn foo()");
    }

    #[test]
    fn truncate_signature_trims_trailing_whitespace_before_brace() {
        assert_eq!(truncate_signature("fn foo()   \t\n{ return; }"), "fn foo()");
    }

    // ---- enclosing_function_id ---------------------------------------

    #[test]
    fn enclosing_function_id_for_call_in_free_fn() {
        let src = "fn outer() { foo(); }";
        let tree = parse(src);
        // The `foo` identifier inside the call is what real callers pass.
        let call = find_first(tree.root_node(), "call_expression").expect("call_expression");
        let id = enclosing_function_id(call, src.as_bytes(), "/tmp/test.rs");
        assert_eq!(id, "/tmp/test.rs:outer");
    }

    #[test]
    fn enclosing_function_id_for_call_at_module_level_returns_bare_path() {
        // `static X: i32 = compute();` — call lives outside any function_item.
        // Expected `from` = the file path itself (matches the C++ top-level
        // call rule).
        let src = "static X: i32 = compute();";
        let tree = parse(src);
        let call = find_first(tree.root_node(), "call_expression").expect("call_expression");
        let id = enclosing_function_id(call, src.as_bytes(), "/tmp/test.rs");
        assert_eq!(id, "/tmp/test.rs");
    }

    #[test]
    fn enclosing_function_id_for_call_in_impl_method_uses_type_prefix() {
        let src = "struct Foo; impl Foo { fn bar(&self) { baz(); } }";
        let tree = parse(src);
        let call = find_first(tree.root_node(), "call_expression").expect("call_expression");
        let id = enclosing_function_id(call, src.as_bytes(), "/tmp/test.rs");
        assert_eq!(id, "/tmp/test.rs:Foo::bar");
    }

    /// Anti-regression: for `impl Trait for Type { fn m() { ... } }` the
    /// `from` of any inner call must be `Type::m`, never `Trait::m`.
    #[test]
    fn enclosing_function_id_for_call_in_trait_impl_uses_type_not_trait() {
        let src = "trait Trait {} struct Foo; impl Trait for Foo { fn bar(&self) { baz(); } }";
        let tree = parse(src);
        let call = find_first(tree.root_node(), "call_expression").expect("call_expression");
        let id = enclosing_function_id(call, src.as_bytes(), "/tmp/test.rs");
        assert_eq!(id, "/tmp/test.rs:Foo::bar");
        assert_ne!(id, "/tmp/test.rs:Trait::bar");
    }

    #[test]
    fn enclosing_function_id_for_call_inside_closure_walks_past_closure() {
        // `outer()` contains a closure that calls `foo()`. The closure has
        // no name; the call's `from` must be `outer`, not the path.
        let src = "fn outer() { let _ = || foo(); }";
        let tree = parse(src);
        let call = find_first(tree.root_node(), "call_expression").expect("call_expression");
        let id = enclosing_function_id(call, src.as_bytes(), "/tmp/test.rs");
        assert_eq!(id, "/tmp/test.rs:outer");
    }
}
