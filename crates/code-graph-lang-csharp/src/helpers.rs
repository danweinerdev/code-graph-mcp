//! Helper routines for the C# parser.
//!
//! Cross-language helpers ([`code_graph_lang::helpers::truncate_signature`],
//! [`code_graph_lang::helpers::find_enclosing_kind`]) are imported
//! directly from `code-graph-lang` at their use sites in `lib.rs` rather
//! than re-exported through this module. C#-specific helpers live here.

use code_graph_lang::helpers::find_enclosing_kind;
use tree_sitter::Node;

/// Build a `path:fn_name` (free fn / local function) or
/// `path:Parent::fn_name` (method/constructor) symbol-ID anchor for the
/// function enclosing `node`. Mirrors the C++/Rust/Go/Python plugins'
/// `enclosing_function_id` and matches the [`code_graph_core::symbol_id`]
/// shape produced by Phase 2.2's definition extractor so call edges'
/// `from` fields line up exactly with definition IDs.
///
/// Behavior:
/// - **`method_declaration`** with an enclosing `class_declaration` /
///   `struct_declaration` / `record_declaration` → returns
///   `<path>:<TypeName>::<method>`. Uses the *innermost* enclosing type
///   (matches the parent-resolution rule in `extract_definitions`).
/// - **`method_declaration`** inside an `interface_declaration` with a
///   body → returns `<path>:<method>` (no parent). Matches Decision 11's
///   default-interface-method rule: the symbol kind is `Function`, and
///   the symbol ID has no parent prefix. Methods without bodies in an
///   interface yield no Symbol record (forward-declaration rule), so
///   any call lexically inside such a node is unreachable in practice
///   — the walk still reports the `<path>:<method>` form for
///   robustness.
/// - **`constructor_declaration`** → returns `<path>:<TypeName>::<ctor>`.
///   The constructor's name matches its enclosing type's name, but we
///   use `find_enclosing_kind` to walk up to the type rather than
///   relying on the captured name (defensive against malformed input).
/// - **`local_function_statement`** → returns `<path>:<fn_name>` (no
///   parent). Local functions are nested inside method bodies but are
///   not members of the enclosing type, matching the parent-empty rule
///   in `extract_definitions`.
/// - **No enclosing function-shaped declaration** (call at file scope,
///   in a field initializer, in a property accessor, etc.) → returns
///   `path` (the bare file path). Matches the C++/Rust/Go/Python
///   top-level-call rule.
///
/// **Lambda transparency:** `lambda_expression` is NOT a function-shaped
/// declaration in this walk — calls inside `() => Foo()` walk past the
/// lambda and report the *enclosing* method/constructor/local-function
/// as the `from`. This mirrors the Python `lambda` and Go `func_literal`
/// rules.
///
/// **LINQ transparency:** `query_expression` and its children
/// (`from_clause`, `select_clause`, etc.) are not function-shaped
/// declarations either — calls inside `select Foo(x)` walk through the
/// query expression and report the enclosing method as the `from`.
///
/// **Property/field-initializer fallback:** a call inside a property
/// accessor (`get { return Compute(); }`), an arrow-bodied property
/// (`int X => Compute()`), or a static field initializer
/// (`static int x = Compute()`) has no enclosing
/// `method_declaration`/`constructor_declaration`/`local_function_statement`,
/// so the walk falls through to the bare-path branch. The recorded edge
/// has `from = path`, matching the file-level fallback for top-level
/// calls in the other plugins.
pub fn enclosing_function_id(node: Node<'_>, content: &[u8], path: &str) -> String {
    let mut current = Some(node);
    while let Some(n) = current {
        match n.kind() {
            "method_declaration" => {
                let name = n
                    .child_by_field_name("name")
                    .and_then(|nm| nm.utf8_text(content).ok())
                    .unwrap_or("");
                if name.is_empty() {
                    return path.to_owned();
                }
                // A method directly inside an interface with a body is a
                // default interface method — extract_definitions records
                // it as Function with no parent (Decision 11), so the
                // call's `from` must omit the parent too.
                if find_enclosing_kind(n, "interface_declaration").is_some() {
                    return format!("{path}:{name}");
                }
                // Otherwise, prefer the nearest enclosing
                // class/struct/record as the parent. Falls back to the
                // bare `<path>:<name>` form if no enclosing type is
                // found (defensive — shouldn't happen in well-formed
                // C#).
                let parent = nearest_type_name(n, content);
                if parent.is_empty() {
                    return format!("{path}:{name}");
                }
                return format!("{path}:{parent}::{name}");
            }
            "constructor_declaration" => {
                let name = n
                    .child_by_field_name("name")
                    .and_then(|nm| nm.utf8_text(content).ok())
                    .unwrap_or("");
                if name.is_empty() {
                    return path.to_owned();
                }
                let parent = nearest_type_name(n, content);
                if parent.is_empty() {
                    return format!("{path}:{name}");
                }
                return format!("{path}:{parent}::{name}");
            }
            "local_function_statement" => {
                let name = n
                    .child_by_field_name("name")
                    .and_then(|nm| nm.utf8_text(content).ok())
                    .unwrap_or("");
                if name.is_empty() {
                    return path.to_owned();
                }
                return format!("{path}:{name}");
            }
            _ => {}
        }
        current = n.parent();
    }
    path.to_owned()
}

/// Walk ancestors of `node` and return the bare name of the nearest
/// enclosing class/struct/record. Returns `""` when no such ancestor
/// exists (e.g. a top-level method in a malformed file). Used by
/// [`enclosing_function_id`] to populate the parent segment of method
/// and constructor symbol IDs.
///
/// `interface_declaration` is intentionally NOT a candidate here — the
/// interface case is handled separately in `enclosing_function_id` to
/// implement Decision 11's no-parent rule for default interface methods.
fn nearest_type_name(node: Node<'_>, content: &[u8]) -> String {
    let mut current = node.parent();
    while let Some(n) = current {
        match n.kind() {
            "class_declaration" | "struct_declaration" | "record_declaration" => {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser as TsParser;

    /// Parse a snippet of C# source against tree-sitter-c-sharp. Used by
    /// the helper tests to build a real AST without going through
    /// `CSharpParser`.
    fn parse(src: &str) -> tree_sitter::Tree {
        let mut parser = TsParser::new();
        let language: tree_sitter::Language = tree_sitter_c_sharp::LANGUAGE.into();
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
    fn enclosing_function_id_for_call_in_method() {
        let src = "class Foo { void Bar() { Baz(); } }";
        let tree = parse(src);
        let inv = find_first(tree.root_node(), "invocation_expression").unwrap();
        assert_eq!(
            enclosing_function_id(inv, src.as_bytes(), "/p/x.cs"),
            "/p/x.cs:Foo::Bar"
        );
    }

    #[test]
    fn enclosing_function_id_for_call_in_constructor() {
        let src = "class Foo { public Foo() { Init(); } }";
        let tree = parse(src);
        let inv = find_first(tree.root_node(), "invocation_expression").unwrap();
        assert_eq!(
            enclosing_function_id(inv, src.as_bytes(), "/p/x.cs"),
            "/p/x.cs:Foo::Foo"
        );
    }

    #[test]
    fn enclosing_function_id_for_call_in_default_interface_method_omits_parent() {
        // Decision 11: default interface methods extract as Function with
        // no parent. Calls inside such a method must report `<path>:Foo`,
        // not `<path>:I::Foo`.
        let src = "interface I { void Foo() { Helper(); } }";
        let tree = parse(src);
        let inv = find_first(tree.root_node(), "invocation_expression").unwrap();
        assert_eq!(
            enclosing_function_id(inv, src.as_bytes(), "/p/x.cs"),
            "/p/x.cs:Foo"
        );
    }

    #[test]
    fn enclosing_function_id_for_call_in_local_function_no_parent() {
        // Local functions extract as Function with no parent. The call
        // inside the local function reports `<path>:Helper`, NOT
        // `<path>:C::Helper` (the local function is the immediate
        // enclosing function-shaped node).
        let src = "class C { void M() { void Helper() { Inner(); } } }";
        let tree = parse(src);
        let inv = find_first(tree.root_node(), "invocation_expression").unwrap();
        assert_eq!(
            enclosing_function_id(inv, src.as_bytes(), "/p/x.cs"),
            "/p/x.cs:Helper"
        );
    }

    #[test]
    fn enclosing_function_id_for_call_inside_lambda_walks_past_lambda() {
        // Lambdas are transparent: the call inside `() => Foo()` reports
        // the enclosing method as the `from`, not the lambda.
        let src = "class C { void M() { System.Action a = () => Foo(); } }";
        let tree = parse(src);
        let inv = find_first(tree.root_node(), "invocation_expression").unwrap();
        assert_eq!(
            enclosing_function_id(inv, src.as_bytes(), "/p/x.cs"),
            "/p/x.cs:C::M"
        );
    }

    #[test]
    fn enclosing_function_id_for_call_inside_linq_walks_past_query() {
        // LINQ query expressions are transparent: a call inside
        // `select Foo(x)` reports the enclosing method.
        let src = "class C { void M() { var r = from x in xs select Foo(x); } }";
        let tree = parse(src);
        let inv = find_first(tree.root_node(), "invocation_expression").unwrap();
        assert_eq!(
            enclosing_function_id(inv, src.as_bytes(), "/p/x.cs"),
            "/p/x.cs:C::M"
        );
    }

    #[test]
    fn enclosing_function_id_for_method_in_struct_uses_struct_name() {
        let src = "struct Pt { public int Sum() { return Compute(); } }";
        let tree = parse(src);
        let inv = find_first(tree.root_node(), "invocation_expression").unwrap();
        assert_eq!(
            enclosing_function_id(inv, src.as_bytes(), "/p/x.cs"),
            "/p/x.cs:Pt::Sum"
        );
    }

    #[test]
    fn enclosing_function_id_for_method_in_record_uses_record_name() {
        let src = "record User(string n) { public bool Check() { return Helper(); } }";
        let tree = parse(src);
        let inv = find_first(tree.root_node(), "invocation_expression").unwrap();
        assert_eq!(
            enclosing_function_id(inv, src.as_bytes(), "/p/x.cs"),
            "/p/x.cs:User::Check"
        );
    }

    #[test]
    fn enclosing_function_id_for_call_in_field_initializer_returns_bare_path() {
        // Static field initializer: no enclosing
        // method/constructor/local-function. Falls back to the bare path.
        let src = "class C { static int x = Compute(); }";
        let tree = parse(src);
        let inv = find_first(tree.root_node(), "invocation_expression").unwrap();
        assert_eq!(
            enclosing_function_id(inv, src.as_bytes(), "/p/x.cs"),
            "/p/x.cs"
        );
    }

    #[test]
    fn enclosing_function_id_for_nested_class_uses_innermost_type() {
        // Method inside a nested class records the *immediate* enclosing
        // class as parent — matches the parent-resolution rule in
        // `extract_definitions`.
        let src = "class Outer { class Inner { void M() { Helper(); } } }";
        let tree = parse(src);
        let inv = find_first(tree.root_node(), "invocation_expression").unwrap();
        assert_eq!(
            enclosing_function_id(inv, src.as_bytes(), "/p/x.cs"),
            "/p/x.cs:Inner::M"
        );
    }
}
