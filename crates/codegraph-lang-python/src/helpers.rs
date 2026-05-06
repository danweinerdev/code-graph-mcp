//! Helper routines for the Python parser.
//!
//! Phase status: Phase 7.1 ships the small structural helpers
//! ([`find_enclosing_class`], [`extract_module_path`],
//! [`enclosing_function_id`]) used by the upcoming 7.2/7.3/7.4 extractors.
//! `truncate_signature` is re-exported from `codegraph_lang::helpers` (the
//! cross-language consolidation that Phase 7.1 also performed).
//!
//! The module itself is `pub(crate)`; the individual functions are `pub`
//! as a crate-internal convention so callers within `lib.rs` can `use`
//! them freely. The effective visibility cap remains crate-internal.

// Re-export the cross-language `truncate_signature` so call sites within
// this crate import it as `crate::helpers::truncate_signature` — same
// shape as the C++/Rust/Go plugins post-consolidation. Phase 7.2's
// definition extractor will drive this re-export; Phase 7.1 has no
// callers yet.
#[allow(unused_imports)] // wired in Phase 7.2
pub use codegraph_lang::helpers::truncate_signature;

use tree_sitter::Node;

/// Walk `node`'s parent chain and return the first ancestor that is a
/// `class_definition`, or `None` if `node` is not nested inside a class.
///
/// Used by Phase 7.2's definition extractor to decide whether a
/// `function_definition` is a free function or a method, and by 7.3 to
/// build `<path>:<Class>::<method>` symbol IDs for calls inside methods.
///
/// **Decorator transparency:** `@property def foo(self)` parses as
/// `decorated_definition > function_definition`. The `decorated_definition`
/// itself is *not* a `class_definition`, so the walk passes through it
/// transparently. This matches Python's runtime semantics — a decorated
/// method is still a method of its enclosing class.
///
/// **Nested classes:** `class Outer: class Inner: ...` — for any node
/// inside `Inner`, this returns the `Inner` `class_definition` (the
/// nearest enclosing class), not `Outer`. 7.2 uses that to set
/// `Symbol.parent = "Inner"`, and reads the *parent of `Inner`* to
/// populate the parent's namespace if needed.
#[allow(dead_code)] // wired in Phase 7.2
pub fn find_enclosing_class(node: Node<'_>) -> Option<Node<'_>> {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "class_definition" {
            return Some(n);
        }
        current = n.parent();
    }
    None
}

/// Walk a `dotted_name` or `relative_import` node and produce the dotted
/// module path string, e.g. `"foo.bar"` for `import foo.bar` or `".utils"`
/// for `from .utils import x` or `"."` for `from . import x`.
///
/// Parameters:
/// - `import_node` — a `dotted_name` or `relative_import` node captured
///   by [`crate::queries::IMPORT_QUERIES`]. For `dotted_name` we read the
///   node's text directly (it is already in canonical `a.b.c` form). For
///   `relative_import` we walk children and reconstruct, preserving the
///   leading-dot prefix.
/// - `content` — the source-file bytes the AST was parsed from.
///
/// Returns the empty string if the node text cannot be decoded as UTF-8
/// or if the node is an unexpected kind (defensive, matches the C++/Go
/// plugins' posture toward malformed AST).
///
/// **Relative-import preservation rule (7.4 verification field):** for
/// `from . import utils` the recorded path is `"."` (the `relative_import`
/// node carries only dots; the imported names live in the parent
/// statement's `name` field, NOT here). For `from .utils import x` the
/// recorded path is `".utils"` — the leading dots are preserved verbatim
/// so downstream consumers can distinguish relative imports from absolute.
/// The default `resolve_include` correctly returns `None` against these
/// dotted module strings because they are not filesystem paths.
#[allow(dead_code)] // wired in Phase 7.4
pub fn extract_module_path(import_node: Node<'_>, content: &[u8]) -> String {
    match import_node.kind() {
        "dotted_name" => import_node.utf8_text(content).unwrap_or("").to_owned(),
        "relative_import" => {
            // `relative_import` parses as a sequence of `import_prefix`
            // (leading dots) optionally followed by a `dotted_name`. The
            // raw node text already includes both, so reading the whole
            // node's text gives us `.`, `..`, `.utils`, `..pkg.mod`, etc.
            // verbatim with the dot prefix intact.
            import_node.utf8_text(content).unwrap_or("").to_owned()
        }
        _ => String::new(),
    }
}

/// Build a `path:fn_name` (free function) or `path:Class::fn_name` (method)
/// symbol-ID anchor for the function enclosing `node`. Mirrors the C++/
/// Rust/Go plugins' `enclosing_function_id` and matches the
/// [`codegraph_core::symbol_id`] shape produced by Phase 7.2's definition
/// extractor so call edges' `from` fields line up exactly with definition
/// IDs.
///
/// Behavior:
/// - No enclosing `function_definition` (e.g. a call at module top-level
///   like `print("hello")`) → returns `path` (the bare file path),
///   matching the C++ top-level-call rule.
/// - `function_definition` with no enclosing `class_definition` → returns
///   `<path>:<fn_name>`.
/// - `function_definition` inside a `class_definition` → returns
///   `<path>:<Class>::<fn_name>`. Nested classes use the *innermost*
///   enclosing class — `class Outer: class Inner: def m(self): foo()`
///   produces `<path>:Inner::m`, not `<path>:Outer::Inner::m`. (7.2 makes
///   the same choice for the `Symbol.parent` field; 7.3's `from` matches.)
/// - **Lambdas (`lambda` expressions) are transparent**: a call inside a
///   lambda walks past the lambda and reports the lambda's enclosing
///   `function_definition`. The walk does not stop at `lambda` nodes.
/// - **List/set/dict comprehensions are transparent** for the same reason:
///   they are not `function_definition` nodes, so the walk passes through.
///   A call inside `[foo(x) for x in xs]` inside method `bar` reports
///   `<path>:Class::bar` as the `from`.
/// - **Decorator transparency (7.2 rule)**: `@property def foo(self): ...`
///   wraps `function_definition` inside `decorated_definition`. The walk
///   finds the `function_definition` first (the inner node), then the
///   `class_definition` ancestor — the `decorated_definition` is passed
///   through silently.
#[allow(dead_code)] // wired in Phase 7.3
pub fn enclosing_function_id(node: Node<'_>, content: &[u8], path: &str) -> String {
    let mut current = node.parent();
    let mut func: Option<Node<'_>> = None;
    while let Some(n) = current {
        if n.kind() == "function_definition" {
            func = Some(n);
            break;
        }
        current = n.parent();
    }
    let Some(func) = func else {
        return path.to_owned();
    };
    let fn_name = func
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(content).ok())
        .unwrap_or("");
    if fn_name.is_empty() {
        return path.to_owned();
    }
    match find_enclosing_class(func) {
        Some(cls) => {
            let class_name = cls
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(content).ok())
                .unwrap_or("");
            if class_name.is_empty() {
                format!("{path}:{fn_name}")
            } else {
                format!("{path}:{class_name}::{fn_name}")
            }
        }
        None => format!("{path}:{fn_name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser as TsParser;

    /// Parse a snippet of Python source against tree-sitter-python. Used by
    /// the helper tests to build a real AST without going through
    /// `PythonParser`.
    fn parse(src: &str) -> tree_sitter::Tree {
        let mut parser = TsParser::new();
        let language: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
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

    // ---- find_enclosing_class --------------------------------------------

    #[test]
    fn find_enclosing_class_returns_some_for_method() {
        let src = "class Foo:\n    def bar(self):\n        pass\n";
        let tree = parse(src);
        let func =
            find_first(tree.root_node(), "function_definition").expect("function_definition");
        let cls = find_enclosing_class(func).expect("must find class_definition ancestor");
        assert_eq!(cls.kind(), "class_definition");
    }

    #[test]
    fn find_enclosing_class_returns_none_for_free_function() {
        let src = "def foo():\n    pass\n";
        let tree = parse(src);
        let func =
            find_first(tree.root_node(), "function_definition").expect("function_definition");
        assert!(find_enclosing_class(func).is_none());
    }

    #[test]
    fn find_enclosing_class_walks_through_decorated_definition() {
        // `@property def x(self):` parses as decorated_definition >
        // function_definition. The class ancestor is reachable through
        // the decorated_definition wrapper.
        let src = "class Foo:\n    @property\n    def x(self):\n        return 1\n";
        let tree = parse(src);
        let func =
            find_first(tree.root_node(), "function_definition").expect("function_definition");
        let cls = find_enclosing_class(func).expect("must find class_definition through wrapper");
        assert_eq!(cls.kind(), "class_definition");
    }

    #[test]
    fn find_enclosing_class_picks_innermost_for_nested_classes() {
        // `class Outer: class Inner: def m(self): pass` — for a node
        // inside Inner.m, the nearest enclosing class is Inner, not Outer.
        let src = "class Outer:\n    class Inner:\n        def m(self):\n            pass\n";
        let tree = parse(src);
        let func =
            find_first(tree.root_node(), "function_definition").expect("function_definition");
        let cls = find_enclosing_class(func).expect("must find inner class");
        let name = cls
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(src.as_bytes()).ok())
            .unwrap_or("");
        assert_eq!(name, "Inner", "nearest enclosing class must be Inner");
    }

    // ---- extract_module_path --------------------------------------------

    #[test]
    fn extract_module_path_from_dotted_name_simple() {
        // `import foo` — the dotted_name's text is `foo`.
        let src = "import foo\n";
        let tree = parse(src);
        let dotted = find_first(tree.root_node(), "dotted_name").expect("dotted_name");
        assert_eq!(extract_module_path(dotted, src.as_bytes()), "foo");
    }

    #[test]
    fn extract_module_path_from_dotted_name_multi_segment() {
        // `import foo.bar` — dotted_name text is `foo.bar`.
        let src = "import foo.bar\n";
        let tree = parse(src);
        let dotted = find_first(tree.root_node(), "dotted_name").expect("dotted_name");
        assert_eq!(extract_module_path(dotted, src.as_bytes()), "foo.bar");
    }

    #[test]
    fn extract_module_path_from_relative_import_dot_only() {
        // `from . import utils` — relative_import text is `.`.
        let src = "from . import utils\n";
        let tree = parse(src);
        let rel = find_first(tree.root_node(), "relative_import").expect("relative_import");
        assert_eq!(extract_module_path(rel, src.as_bytes()), ".");
    }

    #[test]
    fn extract_module_path_from_relative_import_with_module() {
        // `from .utils import x` — relative_import text is `.utils`
        // (leading dot preserved verbatim).
        let src = "from .utils import x\n";
        let tree = parse(src);
        let rel = find_first(tree.root_node(), "relative_import").expect("relative_import");
        assert_eq!(extract_module_path(rel, src.as_bytes()), ".utils");
    }

    #[test]
    fn extract_module_path_from_relative_import_double_dot() {
        // `from ..pkg import x` — relative_import text preserves both dots.
        let src = "from ..pkg import x\n";
        let tree = parse(src);
        let rel = find_first(tree.root_node(), "relative_import").expect("relative_import");
        assert_eq!(extract_module_path(rel, src.as_bytes()), "..pkg");
    }

    #[test]
    fn extract_module_path_unknown_node_returns_empty() {
        // Defensive: passing a node of an unexpected kind returns empty.
        let src = "x = 1\n";
        let tree = parse(src);
        let assignment = find_first(tree.root_node(), "assignment").expect("assignment");
        assert_eq!(extract_module_path(assignment, src.as_bytes()), "");
    }

    // ---- enclosing_function_id ---------------------------------------

    #[test]
    fn enclosing_function_id_for_call_in_free_function() {
        // `def f(): foo()` — call's from must be `<path>:f`.
        let src = "def f():\n    foo()\n";
        let tree = parse(src);
        let call = find_first(tree.root_node(), "call").expect("call");
        let id = enclosing_function_id(call, src.as_bytes(), "/tmp/test.py");
        assert_eq!(id, "/tmp/test.py:f");
    }

    #[test]
    fn enclosing_function_id_for_call_in_method_uses_class_prefix() {
        // `class Foo: def bar(self): baz()` — call's from must be
        // `<path>:Foo::bar`.
        let src = "class Foo:\n    def bar(self):\n        baz()\n";
        let tree = parse(src);
        let call = find_first(tree.root_node(), "call").expect("call");
        let id = enclosing_function_id(call, src.as_bytes(), "/tmp/test.py");
        assert_eq!(id, "/tmp/test.py:Foo::bar");
    }

    #[test]
    fn enclosing_function_id_for_top_level_call_returns_bare_path() {
        // `print("hello")` at module scope — no enclosing function_definition,
        // so the from is the bare file path (matches the C++ top-level-call
        // rule).
        let src = "print(\"hello\")\n";
        let tree = parse(src);
        let call = find_first(tree.root_node(), "call").expect("call");
        let id = enclosing_function_id(call, src.as_bytes(), "/tmp/test.py");
        assert_eq!(id, "/tmp/test.py");
    }

    #[test]
    fn enclosing_function_id_for_call_in_decorated_method_uses_class_prefix() {
        // `class Foo: @property def x(self): foo()` — decorator-wrapped
        // method. The walk finds function_definition first (skipping
        // decorated_definition), then the class. From = `<path>:Foo::x`.
        let src = "class Foo:\n    @property\n    def x(self):\n        return foo()\n";
        let tree = parse(src);
        let call = find_first(tree.root_node(), "call").expect("call");
        let id = enclosing_function_id(call, src.as_bytes(), "/tmp/test.py");
        assert_eq!(id, "/tmp/test.py:Foo::x");
    }

    #[test]
    fn enclosing_function_id_for_call_inside_lambda_walks_past_lambda() {
        // `def outer(): f = lambda: inner()` — call to `inner` lives
        // inside a `lambda`, which is NOT a function_definition. The walk
        // skips past it and reports `<path>:outer`.
        let src = "def outer():\n    f = lambda: inner()\n";
        let tree = parse(src);
        let call = find_first(tree.root_node(), "call").expect("call");
        let id = enclosing_function_id(call, src.as_bytes(), "/tmp/test.py");
        assert_eq!(id, "/tmp/test.py:outer");
    }

    #[test]
    fn enclosing_function_id_for_call_inside_list_comprehension_walks_past_comprehension() {
        // `def outer(): xs = [foo(x) for x in items]` — call to `foo`
        // lives inside a list_comprehension, which is not a
        // function_definition. The walk passes through and reports
        // `<path>:outer`.
        let src = "def outer():\n    xs = [foo(x) for x in items]\n";
        let tree = parse(src);
        let call = find_first(tree.root_node(), "call").expect("call");
        let id = enclosing_function_id(call, src.as_bytes(), "/tmp/test.py");
        assert_eq!(id, "/tmp/test.py:outer");
    }

    #[test]
    fn enclosing_function_id_for_call_in_nested_class_method_uses_inner_class() {
        // `class Outer: class Inner: def m(self): foo()` — nearest enclosing
        // class is Inner. From = `<path>:Inner::m`.
        let src = "class Outer:\n    class Inner:\n        def m(self):\n            foo()\n";
        let tree = parse(src);
        let call = find_first(tree.root_node(), "call").expect("call");
        let id = enclosing_function_id(call, src.as_bytes(), "/tmp/test.py");
        assert_eq!(id, "/tmp/test.py:Inner::m");
    }
}
