//! Helper routines for the Go parser.
//!
//! Phase status: Phase 6.1 ships the small structural helpers
//! ([`extract_receiver_type`], [`extract_package_name`], [`truncate_signature`])
//! used by the Phase 6.2 definition extractor.
//!
//! The module itself is `pub(crate)`; the individual functions are `pub` as a
//! crate-internal convention so callers within `lib.rs` can `use` them
//! freely. The effective visibility cap remains crate-internal.
//!
//! ## `truncate_signature` consolidation
//!
//! This helper is byte-identical to the copy in
//! `codegraph-lang-rust/src/helpers.rs` (which is in turn byte-identical to
//! the copy in `codegraph-lang-cpp/src/helpers.rs`). The Phase 5 debrief
//! flagged consolidation to a shared module as the natural follow-up once a
//! third copy lands. **That consolidation is intentionally NOT done in
//! Phase 6.1** — it is scope creep for the scaffold task. With three copies
//! now in tree, the consolidation is a clean follow-up that can land
//! independently.

use tree_sitter::Node;

/// Extract the receiver-type name from a `method_declaration`'s `receiver`
/// field (a `parameter_list` containing one `parameter_declaration`).
///
/// Handles all receiver forms produced by tree-sitter-go 0.25:
/// - Pointer receiver: `func (s *Server) M()` → `parameter_declaration.type`
///   is a `pointer_type` whose child is a `type_identifier` → returns
///   `"Server"`.
/// - Value receiver: `func (s Server) M()` → `parameter_declaration.type` is
///   a `type_identifier` directly → returns `"Server"`.
/// - Generic pointer receiver: `func (s *Server[T]) M()` →
///   `pointer_type → generic_type → type_identifier` → returns `"Server"`.
///   The generic-type arguments are dropped; only the bare type name is
///   recorded so symbol IDs and call-resolution lookups stay textual.
/// - Generic value receiver: `func (s Server[T]) M()` →
///   `generic_type → type_identifier` → returns `"Server"`.
/// - Anonymous receivers (no parameter name): `func (*Foo) M()` and
///   `func (Foo) M()` — the parameter_declaration's `type` field may be
///   absent in this form; tree-sitter-go parses the bare type as a single
///   nameless `parameter_declaration` whose first named child is the
///   type. The fallback path below handles both.
///
/// Returns the empty string if the receiver shape is unexpected (defensive:
/// matches the C++ extractor's posture toward malformed AST).
pub fn extract_receiver_type(receiver: Node<'_>, content: &[u8]) -> String {
    // Find the (first) parameter_declaration child of the parameter_list.
    let mut cursor = receiver.walk();
    let param_decl = receiver
        .named_children(&mut cursor)
        .find(|c| c.kind() == "parameter_declaration");
    let Some(param_decl) = param_decl else {
        return String::new();
    };

    // Prefer the `type` field when present; fall back to the first named
    // child for anonymous receivers (`func (*Foo) M()` or `func (Foo) M()`)
    // where tree-sitter-go records the type as the parameter_declaration's
    // sole child rather than under a `type` field.
    let type_node = param_decl
        .child_by_field_name("type")
        .or_else(|| param_decl.named_child(0));
    let Some(type_node) = type_node else {
        return String::new();
    };

    receiver_type_name(type_node, content)
}

/// Resolve a receiver-type AST node to its bare type-identifier text.
///
/// Centralises the descent rules used by [`extract_receiver_type`] so that
/// pointer-of-generic and bare-generic forms share the same logic. Returns
/// the empty string on any unexpected shape (matches the parent function's
/// defensive posture).
fn receiver_type_name(type_node: Node<'_>, content: &[u8]) -> String {
    match type_node.kind() {
        "type_identifier" => type_node.utf8_text(content).unwrap_or("").to_owned(),
        "pointer_type" => {
            // Descend into the pointer's inner type. The first named child is
            // the pointee type — either a bare `type_identifier` (`*Server`)
            // or a `generic_type` (`*Server[T]`).
            let mut cursor = type_node.walk();
            let inner = type_node.named_children(&mut cursor).next();
            match inner {
                Some(n) => receiver_type_name(n, content),
                None => String::new(),
            }
        }
        "generic_type" => {
            // `generic_type` wraps a `type_identifier` (the bare type name)
            // followed by `type_arguments`. Drop the arguments and record
            // only the bare type name so receiver lookups stay textual.
            let mut cursor = type_node.walk();
            let ident = type_node
                .named_children(&mut cursor)
                .find(|c| c.kind() == "type_identifier");
            match ident {
                Some(n) => n.utf8_text(content).unwrap_or("").to_owned(),
                None => String::new(),
            }
        }
        _ => String::new(),
    }
}

/// Walk the children of `root` (the source file) looking for the
/// `package_clause` and return the package name. Go source files always
/// begin with a `package_clause`, so a tree-walk over the root's direct
/// children is sufficient — no recursion required.
///
/// Returns the empty string if no `package_clause` is found (e.g. a
/// pathological fixture or partial parse).
pub fn extract_package_name(root: Node<'_>, content: &[u8]) -> String {
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        if child.kind() == "package_clause" {
            // package_clause has no fields; the package_identifier is its
            // first (and only) named child.
            let mut inner = child.walk();
            let name_node = child
                .named_children(&mut inner)
                .find(|c| c.kind() == "package_identifier");
            if let Some(name_node) = name_node {
                return name_node.utf8_text(content).unwrap_or("").to_owned();
            }
        }
    }
    String::new()
}

/// Build a `path:fn_name` (free fn) or `path:Parent::fn_name` (method)
/// symbol-ID anchor for the function enclosing `node`. Mirrors the C++/Rust
/// plugins' `enclosing_function_id` and matches the [`codegraph_core::symbol_id`]
/// shape produced by Phase 6.2's definition extractor so call edges' `from`
/// fields line up exactly with definition IDs.
///
/// Behavior:
/// - No enclosing `function_declaration` or `method_declaration` (e.g. a
///   call inside a package-level closure assigned to a global, like
///   `var H = func() { foo() }`) → returns `path` (the bare file path),
///   matching the C++ lambda-at-global-scope rule.
/// - `function_declaration` → returns `<path>:<fn_name>`.
/// - `method_declaration` → returns `<path>:<ReceiverType>::<fn_name>`.
///   Receiver-type extraction goes through [`extract_receiver_type`] so
///   pointer / value / generic / anonymous receiver forms all collapse to
///   the bare type name.
/// - Closures (`func_literal`) are transparent: a call inside a closure
///   walks past the closure node and reports the closure's enclosing
///   `function_declaration` or `method_declaration` as the `from`. The
///   parent-chain walk does not stop at closure boundaries.
pub fn enclosing_function_id(node: Node<'_>, content: &[u8], path: &str) -> String {
    let mut current = Some(node);
    while let Some(n) = current {
        match n.kind() {
            "function_declaration" => {
                let name = n
                    .child_by_field_name("name")
                    .and_then(|nm| nm.utf8_text(content).ok())
                    .unwrap_or("");
                if name.is_empty() {
                    return path.to_owned();
                }
                return format!("{path}:{name}");
            }
            "method_declaration" => {
                let name = n
                    .child_by_field_name("name")
                    .and_then(|nm| nm.utf8_text(content).ok())
                    .unwrap_or("");
                let parent = n
                    .child_by_field_name("receiver")
                    .map(|r| extract_receiver_type(r, content))
                    .unwrap_or_default();
                if name.is_empty() {
                    return path.to_owned();
                }
                if parent.is_empty() {
                    return format!("{path}:{name}");
                }
                return format!("{path}:{parent}::{name}");
            }
            _ => {}
        }
        current = n.parent();
    }
    // Fallback: package-level closure (e.g. `var H = func() { foo() }`) —
    // no enclosing function/method declaration. Mirrors the C++ lambda-at-
    // global-scope behavior: report the bare file path.
    path.to_owned()
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

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser as TsParser;

    /// Parse a snippet of Go source against tree-sitter-go. Used by the
    /// helper tests to build a real AST without going through `GoParser`.
    fn parse(src: &str) -> tree_sitter::Tree {
        let mut parser = TsParser::new();
        let language: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
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

    // ---- extract_receiver_type --------------------------------------------

    #[test]
    fn extract_receiver_type_pointer_form() {
        // `func (s *Server) M() {}` — receiver is *Server; expect "Server".
        let src = "package main\nfunc (s *Server) M() {}\n";
        let tree = parse(src);
        let method =
            find_first(tree.root_node(), "method_declaration").expect("method_declaration");
        let receiver = method
            .child_by_field_name("receiver")
            .expect("receiver field");
        assert_eq!(extract_receiver_type(receiver, src.as_bytes()), "Server");
    }

    #[test]
    fn extract_receiver_type_value_form() {
        // `func (s Server) M() {}` — value receiver; expect "Server".
        let src = "package main\nfunc (s Server) M() {}\n";
        let tree = parse(src);
        let method =
            find_first(tree.root_node(), "method_declaration").expect("method_declaration");
        let receiver = method
            .child_by_field_name("receiver")
            .expect("receiver field");
        assert_eq!(extract_receiver_type(receiver, src.as_bytes()), "Server");
    }

    #[test]
    fn extract_receiver_type_anonymous_receiver_pointer() {
        // Receiver name is omitted but pointer type is intact: `func (*Foo) M() {}`.
        // Still extracts "Foo".
        let src = "package main\nfunc (*Foo) M() {}\n";
        let tree = parse(src);
        let method =
            find_first(tree.root_node(), "method_declaration").expect("method_declaration");
        let receiver = method
            .child_by_field_name("receiver")
            .expect("receiver field");
        assert_eq!(extract_receiver_type(receiver, src.as_bytes()), "Foo");
    }

    #[test]
    fn extract_receiver_type_anonymous_receiver_value() {
        // Anonymous value receiver: `func (Foo) M() {}` — no parameter name,
        // bare type_identifier as receiver. Mirrors the pointer-anonymous
        // test for the value form. The helper's `named_child(0)` fallback
        // path catches the receiver type when no `type` field is set.
        let src = "package main\nfunc (Foo) M() {}\n";
        let tree = parse(src);
        let method =
            find_first(tree.root_node(), "method_declaration").expect("method_declaration");
        let receiver = method
            .child_by_field_name("receiver")
            .expect("receiver field");
        assert_eq!(extract_receiver_type(receiver, src.as_bytes()), "Foo");
    }

    #[test]
    fn extract_receiver_type_generic_pointer_form() {
        // Generic pointer receiver: `func (s *Server[T]) M() {}`.
        // tree-sitter-go parses this as pointer_type → generic_type →
        // type_identifier. The helper drops the generic arguments and
        // records the bare type name "Server".
        let src = "package main\nfunc (s *Server[T]) M() {}\n";
        let tree = parse(src);
        let method =
            find_first(tree.root_node(), "method_declaration").expect("method_declaration");
        let receiver = method
            .child_by_field_name("receiver")
            .expect("receiver field");
        assert_eq!(extract_receiver_type(receiver, src.as_bytes()), "Server");
    }

    #[test]
    fn extract_receiver_type_generic_value_form() {
        // Generic value receiver: `func (s Server[T]) M() {}` —
        // generic_type → type_identifier. Same bare-name extraction rule.
        let src = "package main\nfunc (s Server[T]) M() {}\n";
        let tree = parse(src);
        let method =
            find_first(tree.root_node(), "method_declaration").expect("method_declaration");
        let receiver = method
            .child_by_field_name("receiver")
            .expect("receiver field");
        assert_eq!(extract_receiver_type(receiver, src.as_bytes()), "Server");
    }

    // ---- extract_package_name ---------------------------------------------

    #[test]
    fn extract_package_name_main() {
        let src = "package main\n";
        let tree = parse(src);
        assert_eq!(
            extract_package_name(tree.root_node(), src.as_bytes()),
            "main"
        );
    }

    #[test]
    fn extract_package_name_with_declarations_after() {
        let src = "package server\n\nfunc Run() {}\n";
        let tree = parse(src);
        assert_eq!(
            extract_package_name(tree.root_node(), src.as_bytes()),
            "server"
        );
    }

    #[test]
    fn extract_package_name_unicode_identifier() {
        // Go permits Unicode in identifiers; ensure UTF-8 text extraction works.
        let src = "package π\n";
        let tree = parse(src);
        assert_eq!(extract_package_name(tree.root_node(), src.as_bytes()), "π");
    }

    // ---- truncate_signature -----------------------------------------------
    //
    // The function itself is language-agnostic and is byte-identical to the
    // C++ and Rust copies. The test inputs here are Go-idiomatic so the
    // coverage matches the language this crate parses; the underlying
    // truncation logic is exercised the same way regardless of input
    // language.

    #[test]
    fn truncate_signature_stops_at_brace() {
        // Body opener strips for func signatures.
        assert_eq!(truncate_signature("func Foo() { return }"), "func Foo()");
    }

    #[test]
    fn truncate_signature_stops_at_semicolon() {
        // Go uses semicolons rarely (gofmt strips them), but the function
        // is language-agnostic — keep coverage of the `;` branch with a
        // valid Go statement. Single-line `for` clauses contain `;`s but
        // truncate_signature stops at the FIRST one, so a top-level
        // semicolon-terminated declaration is the cleanest fixture.
        assert_eq!(truncate_signature("var x int;"), "var x int");
    }

    #[test]
    fn truncate_signature_trims_trailing_whitespace_before_brace() {
        assert_eq!(
            truncate_signature("func Foo()   \t\n{ return }"),
            "func Foo()"
        );
    }

    #[test]
    fn truncate_signature_byte_fallback_300_chars() {
        // 300 ASCII characters with no `{` or `;` — must hit the 200-byte
        // fallback. Mirrors the C++ test at
        // `crates/codegraph-lang-cpp/src/helpers.rs` so the Go copy carries
        // the same regression coverage of the byte-cutoff branch.
        let long = "a".repeat(300);
        let got = truncate_signature(&long);
        assert!(got.ends_with("..."), "expected trailing '...', got {got:?}");
        // Prefix must be 200 bytes, plus the 3-byte "...".
        assert_eq!(got.len(), 203);
    }
}
