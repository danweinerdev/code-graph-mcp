//! Helper routines for the C++ parser.
//!
//! These mirror the Go helpers in `internal/lang/cpp/cpp.go` byte-equivalent
//! semantics. Test fixtures match the corresponding Go tests
//! (`TestSplitQualified`, `TestStripIncludePath`, `TestTruncateSignature`,
//! `TestTruncateSignatureByteFallback`, `TestTruncateSignatureUTF8Boundary`).
//!
//! Visibility note: helpers are `pub` (not `pub(crate)`) so the dead-code
//! lint sees them as a public surface during the Phase 1.4 transition where
//! they are unit-tested but not yet wired into `extract_*`. Phase 1.5 plugs
//! them into the extraction pipeline; downgrading to `pub(crate)` at that
//! point is a one-line change.

use tree_sitter::Node;

/// Split a qualified identifier `Scope::Name` into `(scope, name)`. Mirrors
/// `splitQualified` in cpp.go, including its use of last-occurrence so that
/// `a::b::c` splits as `("a::b", "c")`. Returns `("", input)` when no `::`
/// separator is present.
pub fn split_qualified(qualified: &str) -> (String, String) {
    match qualified.rfind("::") {
        Some(idx) => (qualified[..idx].to_owned(), qualified[idx + 2..].to_owned()),
        None => (String::new(), qualified.to_owned()),
    }
}

/// Strip surrounding `"..."` or `<...>` from an `#include` path. Mirrors
/// `stripIncludePath` in cpp.go.
pub fn strip_include_path(raw: &str) -> String {
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'<' && last == b'>') {
            return raw[1..raw.len() - 1].to_owned();
        }
    }
    raw.to_owned()
}

/// Return true if `name` is one of the four C++ cast keywords that
/// tree-sitter parses as a `call_expression`. Mirrors `isCppCast` in cpp.go.
pub fn is_cpp_cast(name: &str) -> bool {
    matches!(
        name,
        "static_cast" | "dynamic_cast" | "const_cast" | "reinterpret_cast"
    )
}

/// Truncate a signature at the first `{` or `;`, dropping the body and any
/// trailing whitespace. Falls back to a 200-byte cutoff when neither marker
/// is found, returning `<prefix>...`. Mirrors `truncateSignature` in cpp.go.
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

/// Walk up `node`'s parent chain, returning the first ancestor (including
/// `node` itself) whose kind matches `kind`. Mirrors `findEnclosingKind`
/// in cpp.go.
pub fn find_enclosing_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut current = Some(node);
    while let Some(n) = current {
        if n.kind() == kind {
            return Some(n);
        }
        current = n.parent();
    }
    None
}

/// Resolve the namespace path of `node` by walking its ancestors, collecting
/// every `namespace_definition`'s `name` field, and joining outermost-first
/// with `::`. Anonymous namespaces (no `name` field) contribute nothing.
/// Mirrors `resolveNamespace` in cpp.go.
pub fn resolve_namespace(node: Node<'_>, content: &[u8]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "namespace_definition" {
            if let Some(name_node) = n.child_by_field_name("name") {
                parts.push(name_node.utf8_text(content).unwrap_or("").to_owned());
            }
        }
        current = n.parent();
    }
    parts.reverse();
    parts.join("::")
}

/// Resolve the immediate enclosing class or struct name for `node`, walking
/// upward through its ancestors. Returns `""` when no class/struct ancestor
/// exists. Mirrors `resolveParentClass` in cpp.go.
pub fn resolve_parent_class(node: Node<'_>, content: &[u8]) -> String {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "class_specifier" || n.kind() == "struct_specifier" {
            if let Some(name_node) = n.child_by_field_name("name") {
                return name_node.utf8_text(content).unwrap_or("").to_owned();
            }
        }
        current = n.parent();
    }
    String::new()
}

/// Build a `path:funcName` (or just `path` for top-level) symbol-ID anchor for
/// the function enclosing `node`. Mirrors `enclosingFunctionID` in cpp.go.
pub fn enclosing_function_id(node: Node<'_>, content: &[u8], path: &str) -> String {
    let Some(func_def) = find_enclosing_kind(node, "function_definition") else {
        return path.to_owned();
    };

    let Some(declarator) = func_def.child_by_field_name("declarator") else {
        return path.to_owned();
    };

    if declarator.kind() == "function_declarator" {
        if let Some(name_node) = declarator.child_by_field_name("declarator") {
            let name = name_node.utf8_text(content).unwrap_or("");
            return format!("{path}:{name}");
        }
    }

    path.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_qualified_two_segments() {
        assert_eq!(
            split_qualified("Foo::bar"),
            ("Foo".to_owned(), "bar".to_owned())
        );
    }

    #[test]
    fn split_qualified_uses_last_occurrence() {
        // Mirrors Go's strings.LastIndex behavior — the scope is everything
        // before the final "::".
        assert_eq!(
            split_qualified("a::b::c"),
            ("a::b".to_owned(), "c".to_owned())
        );
    }

    #[test]
    fn split_qualified_no_separator_returns_empty_scope() {
        assert_eq!(
            split_qualified("plain"),
            (String::new(), "plain".to_owned())
        );
    }

    #[test]
    fn split_qualified_class_method_fixture() {
        // Matches TestSplitQualified Go fixture.
        assert_eq!(
            split_qualified("Class::method"),
            ("Class".to_owned(), "method".to_owned())
        );
        assert_eq!(
            split_qualified("ns::Class::method"),
            ("ns::Class".to_owned(), "method".to_owned())
        );
        assert_eq!(
            split_qualified("plainFunc"),
            (String::new(), "plainFunc".to_owned())
        );
    }

    #[test]
    fn strip_include_path_quoted() {
        assert_eq!(strip_include_path("\"foo.h\""), "foo.h");
    }

    #[test]
    fn strip_include_path_system() {
        assert_eq!(strip_include_path("<vector>"), "vector");
    }

    #[test]
    fn strip_include_path_unquoted_passthrough() {
        assert_eq!(strip_include_path("none"), "none");
        assert_eq!(strip_include_path("plain"), "plain");
    }

    #[test]
    fn strip_include_path_engine_fixture() {
        // Matches TestStripIncludePath Go fixture.
        assert_eq!(strip_include_path("\"engine.h\""), "engine.h");
    }

    #[test]
    fn strip_include_path_short_inputs() {
        // Inputs of length < 2 must pass through unchanged.
        assert_eq!(strip_include_path(""), "");
        assert_eq!(strip_include_path("\""), "\"");
    }

    #[test]
    fn is_cpp_cast_recognizes_all_four() {
        for cast in [
            "static_cast",
            "dynamic_cast",
            "const_cast",
            "reinterpret_cast",
        ] {
            assert!(is_cpp_cast(cast), "expected {cast} to be a cast");
        }
    }

    #[test]
    fn is_cpp_cast_rejects_others() {
        assert!(!is_cpp_cast("foo"));
        assert!(!is_cpp_cast(""));
        // Substring near-match must not register.
        assert!(!is_cpp_cast("static_castX"));
        assert!(!is_cpp_cast("Xstatic_cast"));
    }

    #[test]
    fn truncate_signature_stops_at_brace() {
        assert_eq!(truncate_signature("void foo() { return; }"), "void foo()");
    }

    #[test]
    fn truncate_signature_stops_at_semicolon() {
        assert_eq!(truncate_signature("int x;"), "int x");
    }

    #[test]
    fn truncate_signature_no_brace_or_semi_passthrough() {
        // Without a `{` or `;` and under 200 bytes, return as-is.
        assert_eq!(truncate_signature("void foo()"), "void foo()");
    }

    #[test]
    fn truncate_signature_byte_fallback_300_chars() {
        // 300 ASCII characters with no `{` or `;` — must hit the byte fallback.
        let long = "a".repeat(300);
        let got = truncate_signature(&long);
        assert!(got.ends_with("..."), "expected trailing '...', got {got:?}");
        // Prefix must be 200 bytes, plus the 3-byte "...".
        assert_eq!(got.len(), 203);
    }

    #[test]
    fn truncate_signature_utf8_boundary_safe() {
        // Pure multi-byte content (each "あ" is 3 bytes). With no `{` or `;`,
        // the byte-fallback branch kicks in. The cutoff lands on a char
        // boundary by construction (we only ever index at `i` from
        // `char_indices`).
        let long = "あ".repeat(100);
        assert!(long.len() > 200, "fixture must exceed 200 bytes");
        let got = truncate_signature(&long);
        assert!(
            got.is_char_boundary(got.len()),
            "result must be valid UTF-8"
        );
        assert!(got.ends_with("..."));
        // First char_index >= 200 is byte 201 (67 chars of 3 bytes each = 201).
        // Result is the prefix [0..201] plus "...".
        assert_eq!(got.len(), 204);
        // Sanity: the prefix must round-trip back into a String view.
        assert!(std::str::from_utf8(got.as_bytes()).is_ok());
    }

    #[test]
    fn truncate_signature_utf8_mixed_ascii_then_multibyte() {
        // Mirrors Go's TestTruncateSignatureUTF8Boundary: 199 ASCII bytes,
        // then 'é' (2 bytes, byte 199-200), then more ASCII. The first
        // char_index >= 200 lands at byte 201 — we must NOT slice through
        // 'é'.
        let mut input = String::with_capacity(310);
        for _ in 0..199 {
            input.push('a');
        }
        input.push('é'); // 0xC3 0xA9 — bytes 199 and 200
        for _ in 0..100 {
            input.push('b');
        }
        let got = truncate_signature(&input);
        assert!(std::str::from_utf8(got.as_bytes()).is_ok());
        assert!(got.ends_with("..."));
        // The slice ends at byte 201 (first char_index >= 200).
        assert_eq!(got.len(), 204);
    }

    #[test]
    fn truncate_signature_trims_trailing_whitespace_before_brace() {
        assert_eq!(
            truncate_signature("void foo()   \t\n{ return; }"),
            "void foo()"
        );
    }
}
