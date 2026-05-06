//! Helper routines for the C++ parser.
//!
//! These mirror the Go helpers in `internal/lang/cpp/cpp.go` byte-equivalent
//! semantics. Test fixtures match the corresponding Go tests
//! (`TestSplitQualified`, `TestStripIncludePath`, `TestTruncateSignature`,
//! `TestTruncateSignatureByteFallback`, `TestTruncateSignatureUTF8Boundary`).
//!
//! The module itself is `pub(crate)`; the individual functions are `pub` as
//! a crate-internal convention so callers within `lib.rs` can `use` them
//! freely. The effective visibility cap remains crate-internal.
//!
//! `truncate_signature` is re-exported from `codegraph_lang::helpers` (the
//! shared cross-language module). Phase 7.1 consolidated the previously
//! byte-identical C++/Rust/Go copies into one canonical implementation; this
//! `pub use` keeps the historical `crate::helpers::truncate_signature` import
//! path working from `lib.rs`.

pub use codegraph_lang::helpers::truncate_signature;

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

    // truncate_signature behavior is exhaustively tested at the
    // codegraph_lang::helpers layer where the function now lives. The
    // `pub use` re-export above keeps callers (in lib.rs via
    // `crate::helpers::truncate_signature`) working unchanged.
}
