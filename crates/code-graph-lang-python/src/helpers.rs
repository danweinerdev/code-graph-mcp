//! Helper routines for the Python parser.
//!
//! The small structural helpers ([`find_enclosing_class`],
//! [`extract_module_path`], [`enclosing_function_id`]) feed the
//! definition/call/import extractors. `truncate_signature` is
//! re-exported from the shared `code_graph_lang::helpers` module so
//! every language plugin reuses the same logic.
//!
//! The module itself is `pub(crate)`; the individual functions are `pub`
//! as a crate-internal convention so callers within `lib.rs` can `use`
//! them freely. The effective visibility cap remains crate-internal.

// Re-export the cross-language `truncate_signature` so call sites within
// this crate import it as `crate::helpers::truncate_signature` — same
// shape as the C++/Rust/Go plugins post-consolidation. Wired by Phase
// 7.2's definition extractor.
pub use code_graph_lang::helpers::truncate_signature;

use code_graph_core::CallShape;
use tree_sitter::Node;

/// Classify a captured callee identifier's call shape (F2).
///
/// `cap_node` is the `call.name` capture. When it is the `attribute` child
/// of an `attribute` node (attribute call `recv.method()`), the call goes
/// through a receiver value: `self.method()` / `cls.method()` is
/// [`CallShape::SelfReceiver`] (the receiver is the enclosing class) —
/// but ONLY when that name is also the ENCLOSING function's first
/// parameter (phase-12 review, blind-spots F1: `self`/`cls` are ordinary
/// identifiers in Python, so a shadowing parameter or local — e.g.
/// `def m(self, cls): cls.foo()` — must not claim the verified
/// self-receiver shape; it degrades to [`CallShape::Receiver`], the
/// unverified direction). Every other receiver is [`CallShape::Receiver`]
/// — including module-qualified calls (`mod.func()`), which the generic
/// resolver cannot distinguish from instance calls without import
/// analysis, and chained calls. A `self.method()` inside a nested `def`
/// (whose own first parameter is not `self`) also degrades to `Receiver`:
/// conservative, never a false `Resolved`. Direct calls (`foo()`,
/// `MyClass()`, `super()`) are [`CallShape::Free`].
pub fn python_call_shape(cap_node: Node<'_>, content: &[u8]) -> CallShape {
    let Some(parent) = cap_node.parent() else {
        return CallShape::Free;
    };
    if parent.kind() != "attribute" {
        // Direct call (`cb()`): degrade to Receiver when the name is a
        // locally bound VALUE — an enclosing function's parameter or a
        // body re-binding — because the resolver cannot verify which
        // callable that value is (blind-spots cycle-5 F2). Ordinary
        // module/class-level names stay Free. An UNREADABLE callee name
        // (invalid UTF-8 — unreachable in practice, upstream callee-name
        // extraction would already have failed) also degrades to Receiver:
        // every uncertain path takes the conservative direction, never a
        // possible false `Resolved`.
        let verified_free = cap_node
            .utf8_text(content)
            .map(|name| !python_locally_bound_callable(cap_node, content, name))
            .unwrap_or(false);
        return if verified_free {
            CallShape::Free
        } else {
            CallShape::Receiver
        };
    }
    if parent.child_by_field_name("attribute").map(|n| n.id()) != Some(cap_node.id()) {
        return CallShape::Free;
    }
    match parent
        .child_by_field_name("object")
        .and_then(|receiver| receiver.utf8_text(content).ok())
    {
        Some(name @ ("self" | "cls")) if receiver_binds_enclosing_class(parent, content, name) => {
            CallShape::SelfReceiver
        }
        _ => CallShape::Receiver,
    }
}

/// Direct-call shape refinement (phase-12 review, blind-spots cycle-5
/// F2): a bare `cb()` whose name is bound by an enclosing function's
/// PARAMETER (any position, `def callback(self, cb): cb()`) or re-bound
/// in an enclosing function's body (`cb = get_fn(); cb()`) calls a local
/// VALUE — the generic resolver cannot verify which callable that value
/// is, so a sole indexed symbol with that name must not resolve
/// `Resolved/1`. Returns `true` when the name is locally bound at
/// `cap_node`'s position; the caller degrades the shape to
/// [`CallShape::Receiver`]. Walks every enclosing `lambda` and
/// `function_definition` up to module level; module-level direct calls
/// (no enclosing function) are never locally bound.
pub fn python_locally_bound_callable(cap_node: Node<'_>, content: &[u8], name: &str) -> bool {
    let mut current = cap_node.parent();
    while let Some(n) = current {
        match n.kind() {
            "lambda" => {
                if lambda_params_bind(n, content, name) {
                    return true;
                }
            }
            "function_definition" => {
                let params_bind = n
                    .child_by_field_name("parameters")
                    .is_some_and(|p| subtree_names_identifier(p, content, name));
                if params_bind {
                    return true;
                }
                if n.child_by_field_name("body")
                    .is_some_and(|b| subtree_rebinds(b, content, name))
                {
                    return true;
                }
            }
            _ => {}
        }
        current = n.parent();
    }
    false
}

/// True when, at `node`'s position, the identifier `name` (`self`/`cls`)
/// can only be the enclosing method's receiver binding: it is the
/// enclosing `function_definition`'s FIRST parameter (the binding
/// Python's method-call protocol supplies), no intervening `lambda`
/// between the call and that function re-binds it, and nothing inside
/// the function's subtree re-binds it (assignments, walrus expressions,
/// `for` targets, `as` patterns, `global`/`nonlocal` declarations, or
/// nested lambda parameters — phase-12 review, blind-spots cycle-2 F1:
/// `self`/`cls` are ordinary identifiers, so any re-binding makes the
/// receiver unverifiable). Every uncertain path returns `false`, which
/// degrades to [`CallShape::Receiver`] — conservative, never a false
/// `Resolved`. The subtree scan is deliberately coarse (a re-binding
/// inside a nested `def` poisons the whole enclosing method): the cost
/// of the false-negative is a `Heuristic/1` tag on a true edge, the
/// cost of the alternative is a confidently-wrong `Resolved/1`.
fn receiver_binds_enclosing_class(node: Node<'_>, content: &[u8], name: &str) -> bool {
    // Walk up to the nearest function_definition; a lambda on the way
    // whose parameters bind `name` shadows the method receiver.
    let mut current = node.parent();
    let func = loop {
        match current {
            Some(n) if n.kind() == "function_definition" => break n,
            Some(n) => {
                if n.kind() == "lambda" && lambda_params_bind(n, content, name) {
                    return false;
                }
                current = n.parent();
            }
            None => return false,
        }
    };
    // A `@staticmethod` receives NO receiver binding: a first parameter
    // that happens to be named `self`/`cls` is an ordinary argument of
    // arbitrary type (phase-12 review, blind-spots cycle-5 F1).
    // Decorators wrap the function as `decorated_definition >
    // function_definition`, and decorator text is compared without the
    // leading `@`.
    if let Some(wrapper) = func.parent() {
        if wrapper.kind() == "decorated_definition" {
            for i in 0..u32::try_from(wrapper.named_child_count()).unwrap_or(0) {
                let Some(child) = wrapper.named_child(i) else {
                    continue;
                };
                if child.kind() == "decorator"
                    && child
                        .utf8_text(content)
                        .is_ok_and(|t| t.trim_start_matches('@').trim() == "staticmethod")
                {
                    return false;
                }
            }
        }
    }
    let Some(params) = func.child_by_field_name("parameters") else {
        return false;
    };
    let Some(first) = params.named_child(0) else {
        return false;
    };
    // A variadic first parameter (`*self` / `**self`) is a tuple/dict of
    // ordinary arguments, never the receiver — reject before the
    // identifier-inside extraction can match its inner name (blind-spots
    // cycle-5 F1).
    if matches!(
        first.kind(),
        "list_splat_pattern" | "dictionary_splat_pattern"
    ) {
        return false;
    }
    let ident = if first.kind() == "identifier" {
        Some(first)
    } else {
        // typed_parameter / default_parameter / typed_default_parameter:
        // the parameter name is the first identifier inside.
        (0..u32::try_from(first.named_child_count()).unwrap_or(0))
            .filter_map(|i| first.named_child(i))
            .find(|c| c.kind() == "identifier")
    };
    let first_matches = ident
        .and_then(|n| n.utf8_text(content).ok())
        .is_some_and(|text| text == name);
    if !first_matches {
        return false;
    }
    let Some(body) = func.child_by_field_name("body") else {
        return false;
    };
    !subtree_rebinds(body, content, name)
}

/// Push every child of `n` (named and anonymous) onto `stack` by index —
/// avoids `TreeCursor` borrow-lifetime tangles in iterative walks.
fn push_children<'a>(n: Node<'a>, stack: &mut Vec<Node<'a>>) {
    for i in 0..n.child_count() {
        if let Some(child) = n.child(u32::try_from(i).unwrap_or(u32::MAX)) {
            stack.push(child);
        }
    }
}

/// True when any `identifier` (or identifier-aliased leaf) inside `n`'s
/// subtree has text equal to `name`. Deliberately coarse: used only for
/// statement-level binding constructs (imports, `del`, type aliases)
/// where over-matching a non-binding position merely degrades the shape
/// to `Receiver` — the conservative direction.
fn subtree_names_identifier(n: Node<'_>, content: &[u8], name: &str) -> bool {
    let mut stack = vec![n];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier" && node.utf8_text(content).ok() == Some(name) {
            return true;
        }
        push_children(node, &mut stack);
    }
    false
}

/// True when a `lambda` node's parameters bind `name`.
fn lambda_params_bind(lambda: Node<'_>, content: &[u8], name: &str) -> bool {
    let Some(params) = lambda.child_by_field_name("parameters") else {
        return false;
    };
    let mut stack = vec![params];
    while let Some(n) = stack.pop() {
        if n.kind() == "identifier" && n.utf8_text(content).ok() == Some(name) {
            return true;
        }
        push_children(n, &mut stack);
    }
    false
}

/// Conservative re-binding scan: does any node inside `body` re-bind
/// `name`? Binding positions checked: assignment / augmented-assignment
/// left sides, walrus (`named_expression`) names, `for` and
/// comprehension targets, `as`-pattern aliases, `global` / `nonlocal`
/// declarations, and nested lambda parameters. Attribute/call positions
/// (`self.x = 1` re-binds `x`, not `self`) do not match because the
/// binding-position child there is an `attribute`, not a bare
/// `identifier`.
fn subtree_rebinds(body: Node<'_>, content: &[u8], name: &str) -> bool {
    fn is_name(n: Node<'_>, content: &[u8], name: &str) -> bool {
        n.kind() == "identifier" && n.utf8_text(content).ok() == Some(name)
    }
    // A binding position may be a bare identifier or a tuple/list pattern
    // containing one. An `attribute`/`subscript` target (`self.x = 1`,
    // `d[k] = v`) binds the member/element, NOT the receiver name, so the
    // walk descends ONLY through pattern-shaped containers.
    fn is_pattern_container(kind: &str) -> bool {
        matches!(
            kind,
            "pattern_list" | "tuple_pattern" | "list_pattern" | "parenthesized_expression"
        )
    }
    fn pattern_binds(n: Node<'_>, content: &[u8], name: &str) -> bool {
        if is_name(n, content, name) {
            return true;
        }
        if !is_pattern_container(n.kind()) {
            return false;
        }
        let mut stack: Vec<Node<'_>> = Vec::new();
        push_children(n, &mut stack);
        while let Some(child) = stack.pop() {
            if is_name(child, content, name) {
                return true;
            }
            if is_pattern_container(child.kind()) {
                push_children(child, &mut stack);
            }
        }
        false
    }

    fn declares_name(n: Node<'_>, content: &[u8], name: &str) -> bool {
        let mut found = false;
        for i in 0..n.child_count() {
            if let Some(child) = n.child(u32::try_from(i).unwrap_or(u32::MAX)) {
                if is_name(child, content, name) {
                    found = true;
                    break;
                }
            }
        }
        found
    }

    let mut stack = vec![body];
    while let Some(n) = stack.pop() {
        let binds = match n.kind() {
            "assignment" | "augmented_assignment" => n
                .child_by_field_name("left")
                .is_some_and(|l| pattern_binds(l, content, name)),
            "named_expression" => n
                .child_by_field_name("name")
                .is_some_and(|l| is_name(l, content, name)),
            "for_statement" | "for_in_clause" => n
                .child_by_field_name("left")
                .is_some_and(|l| pattern_binds(l, content, name)),
            // `except Exception as self:` / `with open(f) as self:` — in
            // tree-sitter-python the alias node is a grammar ALIAS of
            // `expression` whose KIND is `as_pattern_target`, so it is a
            // leaf compared by its own TEXT, never by an inner
            // `identifier` kind (phase-12 review, quality cycle-3 F1: the
            // previous is_name/pattern_binds arm was dead code here).
            // Tuple aliases descend through pattern containers.
            "as_pattern_target" => {
                n.utf8_text(content).ok() == Some(name) || pattern_binds(n, content, name)
            }
            // `match`/`case` capture and as-patterns (`case self:`,
            // `case [1] as self:`): any identifier in a case pattern is a
            // potential capture binding. Over-firing on value patterns
            // (`case CONST:`) merely degrades to Receiver — conservative.
            "case_pattern" => {
                let mut found = false;
                let mut inner = vec![n];
                while let Some(p) = inner.pop() {
                    if is_name(p, content, name) || p.utf8_text(content).ok() == Some(name) {
                        found = true;
                        break;
                    }
                    push_children(p, &mut inner);
                }
                found
            }
            // `import os as self` re-binds the name.
            "aliased_import" => n
                .child_by_field_name("alias")
                .is_some_and(|a| is_name(a, content, name)),
            // `import self` / `from os import self` / `del self` /
            // `type self = int` (phase-12 review, quality cycle-4 F6):
            // statement-level binding constructs with no other arm. The
            // whole-subtree identifier scan over-fires on non-binding
            // positions (`from self import x`, `del self.x`) — the
            // over-fire direction is a Receiver downgrade, conservative.
            "import_statement" | "import_from_statement" | "delete_statement" => {
                subtree_names_identifier(n, content, name)
            }
            "type_alias_statement" => n
                .child_by_field_name("left")
                .is_some_and(|l| subtree_names_identifier(l, content, name)),
            // A nested `def self():` / `class self:` re-binds the name.
            "function_definition" | "class_definition" => n
                .child_by_field_name("name")
                .is_some_and(|d| is_name(d, content, name)),
            "global_statement" | "nonlocal_statement" => declares_name(n, content, name),
            "lambda" => lambda_params_bind(n, content, name),
            _ => false,
        };
        if binds {
            return true;
        }
        push_children(n, &mut stack);
    }
    false
}

/// Walk `node`'s parent chain and return the first ancestor that is a
/// `class_definition`, or `None` if `node` is not nested inside a class.
///
/// Used by the definition extractor to decide whether a
/// `function_definition` is a free function or a method, and by the call
/// extractor to build `<path>:<Class>::<method>` symbol IDs for calls
/// inside methods.
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
///   `relative_import` we read the whole node's text directly — it already
///   includes the leading dots verbatim (e.g. `.utils`, `..pkg`).
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
/// [`code_graph_core::symbol_id`] shape produced by the definition
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

    // ---- python_call_shape: unreadable callee name -------------------------

    /// Phase-12 quality-lane note, fixed: an UNREADABLE direct-callee name
    /// (invalid UTF-8 at the node's span) must degrade to `Receiver` — every
    /// uncertain path takes the conservative direction. Triggered directly:
    /// parse a valid direct call, then classify against a content buffer
    /// whose bytes at the callee's span are invalid UTF-8, so
    /// `utf8_text` genuinely fails. Before the fix this classified `Free`,
    /// leaving a constructible false `Resolved/1`.
    #[test]
    fn unreadable_direct_callee_degrades_to_receiver() {
        let src = "def m():\n    cb()\n";
        let tree = parse(src);
        let call = find_first(tree.root_node(), "call").expect("call node");
        let callee = call.child_by_field_name("function").expect("callee");
        assert_eq!(callee.kind(), "identifier");

        // Sanity: with the true content this is an unbound bare call — Free.
        assert_eq!(
            python_call_shape(callee, src.as_bytes()),
            code_graph_core::CallShape::Free,
            "control: readable unbound bare call keeps the Free shape"
        );

        // Corrupt exactly the callee's span with invalid UTF-8 (0xFF is
        // never valid in UTF-8), keeping the buffer length identical so the
        // node's byte range stays in bounds.
        let mut corrupted = src.as_bytes().to_vec();
        corrupted[callee.start_byte()..callee.end_byte()].fill(0xFF);
        assert_eq!(
            python_call_shape(callee, &corrupted),
            code_graph_core::CallShape::Receiver,
            "an unreadable callee name is unverifiable and must degrade"
        );
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
    fn extract_module_path_handles_multi_name_import_statement() {
        // `import a, b` — tree-sitter-python parses this as one
        // `import_statement` whose `name` field holds both `dotted_name`
        // children (`a` and `b`). The IMPORT_QUERIES doc-string claims
        // multi-name imports are supported; this pins the behavior at
        // the helper layer (each `dotted_name` round-trips to its own
        // path string) before 7.4 wires the import extractor.
        let src = "import a, b\n";
        let tree = parse(src);
        // Walk the entire tree collecting every `dotted_name` node so we
        // exercise both children, not just the first.
        fn collect_kind<'a>(
            node: tree_sitter::Node<'a>,
            kind: &str,
            out: &mut Vec<tree_sitter::Node<'a>>,
        ) {
            if node.kind() == kind {
                out.push(node);
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                collect_kind(child, kind, out);
            }
        }
        let mut dotted = Vec::new();
        collect_kind(tree.root_node(), "dotted_name", &mut dotted);
        assert_eq!(
            dotted.len(),
            2,
            "expected two dotted_name nodes for `import a, b`, got {dotted:?}"
        );
        let paths: Vec<String> = dotted
            .iter()
            .map(|n| extract_module_path(*n, src.as_bytes()))
            .collect();
        assert!(
            paths.iter().any(|p| p == "a"),
            "expected a path `a`, got {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p == "b"),
            "expected a path `b`, got {paths:?}"
        );
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
