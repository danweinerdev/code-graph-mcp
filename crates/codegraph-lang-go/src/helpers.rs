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
/// Handles both forms:
/// - Pointer receiver: `func (s *Server) M()` → `parameter_declaration.type`
///   is a `pointer_type` whose child is a `type_identifier` → returns
///   `"Server"`.
/// - Value receiver: `func (s Server) M()` → `parameter_declaration.type` is
///   a `type_identifier` directly → returns `"Server"`.
///
/// Returns the empty string if the receiver shape is unexpected (defensive:
/// matches the C++ extractor's posture toward malformed AST).
#[allow(dead_code)] // wired in Phase 6.2
pub fn extract_receiver_type(receiver: Node<'_>, content: &[u8]) -> String {
    // Find the (first) parameter_declaration child of the parameter_list.
    let mut cursor = receiver.walk();
    let param_decl = receiver
        .named_children(&mut cursor)
        .find(|c| c.kind() == "parameter_declaration");
    let Some(param_decl) = param_decl else {
        return String::new();
    };

    // The parameter_declaration's `type` field is either a pointer_type
    // (whose child is a type_identifier) or a type_identifier directly.
    let Some(type_node) = param_decl.child_by_field_name("type") else {
        return String::new();
    };

    match type_node.kind() {
        "pointer_type" => {
            // Descend into the pointer's inner type. The first named child is
            // the pointee type. For `*Server` this is a `type_identifier`.
            let mut cursor = type_node.walk();
            let inner = type_node.named_children(&mut cursor).next();
            match inner {
                Some(n) if n.kind() == "type_identifier" => {
                    n.utf8_text(content).unwrap_or("").to_owned()
                }
                _ => String::new(),
            }
        }
        "type_identifier" => type_node.utf8_text(content).unwrap_or("").to_owned(),
        // Any other shape (e.g. generic type instantiation) — defensive
        // fallback to empty.
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
#[allow(dead_code)] // wired in Phase 6.2
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

/// Truncate a signature at the first `{` or `;`, dropping the body and any
/// trailing whitespace. Falls back to a 200-byte cutoff when neither marker
/// is found, returning `<prefix>...`. Mirrors the C++ plugin's
/// `truncate_signature` byte-for-byte so signatures across languages share
/// the same shape.
///
/// The cutoff is computed via `char_indices`, so the slice boundary is
/// guaranteed to land on a UTF-8 char boundary by construction. Multi-byte
/// content past 200 bytes does not panic.
#[allow(dead_code)] // wired in Phase 6.2
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
    // These three tests are byte-identical to the corresponding tests in
    // `codegraph-lang-rust/src/helpers.rs` (which are in turn byte-identical
    // to the C++ copies). Keeping them in lockstep is the cheap insurance
    // against a drift between language plugins.

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
}
