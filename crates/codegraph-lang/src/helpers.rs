//! Shared helpers used by every per-language plugin crate.
//!
//! The first inhabitant is [`truncate_signature`], which was originally
//! duplicated byte-identical across the C++, Rust, and Go plugin crates. The
//! Phase 6 debrief flagged consolidation as the natural follow-up once a
//! third copy landed; Phase 7.1 promotes it here so the about-to-be-fourth
//! Python plugin can reuse the same logic without spawning yet another copy.
//!
//! Phase 7.7 added [`find_enclosing_kind`] — the second cross-plugin helper
//! that was duplicated five times (C++ helpers, Rust helpers, Rust lib, Go
//! lib, Python lib). All copies were functionally identical
//! (`Node<'a>, &str -> Option<Node<'a>>`, walk-up-parent-chain semantics);
//! consolidation here keeps every plugin's call-site shape unchanged.
//!
//! Any future helper that needs to be byte-identical across plugins belongs
//! here — keep this module strictly language-agnostic. Helpers that *touch*
//! the tree-sitter API surface (like `find_enclosing_kind`) live here too,
//! as long as the function body stays grammar-agnostic. Anything that
//! depends on a specific node-kind vocabulary should stay in the
//! per-language `helpers.rs`.

use tree_sitter::Node;

/// Walk up `node`'s parent chain, returning the first ancestor (including
/// `node` itself) whose kind matches `kind`. Returns `None` if no such
/// ancestor exists.
///
/// Phase 7.7 consolidated five byte-identical copies (C++ `helpers.rs`,
/// Rust `helpers.rs`, Rust `lib.rs`, Go `lib.rs`, Python `lib.rs`) into
/// this canonical implementation. Every plugin's call sites now route
/// through this function — call sites stay unchanged; only the import
/// path for the helper differs.
///
/// **Inclusive of `node` itself:** if `node.kind() == kind`, the function
/// returns `Some(node)` immediately without descending into the parent
/// chain. This is the documented contract every plugin's local copy
/// already shipped — preserving it during consolidation keeps existing
/// extractor logic working without per-plugin tweaks.
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

/// Truncate a signature at the first `{` or `;`, dropping the body and any
/// trailing whitespace. Falls back to a 200-byte cutoff when neither marker
/// is found, returning `<prefix>...`. Mirrors the Go reference's
/// `truncateSignature` byte-for-byte so signatures across languages share
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
    //! Canonical test set for [`truncate_signature`]. Pulled from the C++
    //! and Go copies (both carried 8 tests, byte-identical bodies) at
    //! consolidation time. The fixtures mix language-flavored snippets so
    //! the cross-language braces/semicolons/whitespace/UTF-8 paths all stay
    //! exercised at this layer.
    //!
    //! Phase 7.7 added the [`find_enclosing_kind`] tests below, exercising
    //! the parent-walk semantics that all four plugins relied on. Tests use
    //! the Rust grammar (already a dev-dep) but the helper itself is
    //! grammar-agnostic — the kind strings happen to be Rust node kinds.
    use super::*;

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

    #[test]
    fn truncate_signature_empty_input_returns_empty() {
        // Belt-and-suspenders: empty input falls through both the marker
        // and byte-fallback branches and round-trips.
        assert_eq!(truncate_signature(""), "");
    }

    // ---- find_enclosing_kind ------------------------------------------
    //
    // The parent-walk semantics belonged to five byte-identical copies
    // pre-7.7. Tests below pin the contract that all plugins relied on:
    //   - inclusive of the start node
    //   - returns None when no ancestor matches
    //   - returns the *innermost* matching ancestor for nested matches
    //
    // The fixtures use the Rust grammar (already a dev-dep). The helper
    // is grammar-agnostic; choosing Rust here is the cheapest way to
    // build a real `Node` to walk.

    use tree_sitter::Parser as TsParser;

    /// Parse a snippet of Rust source and return the resulting tree.
    fn parse(src: &str) -> tree_sitter::Tree {
        let mut parser = TsParser::new();
        let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&language).expect("set_language");
        parser.parse(src, None).expect("parse")
    }

    /// Find the first descendant whose `kind() == kind`. Test helper —
    /// not the function under test.
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
    fn find_enclosing_kind_returns_node_itself_when_kind_matches() {
        // Inclusive-of-self contract: if the start node already has the
        // target kind, return it without walking up.
        let src = "fn foo() {}";
        let tree = parse(src);
        let func = find_first(tree.root_node(), "function_item").expect("function_item");
        let got = find_enclosing_kind(func, "function_item").expect("must return Some");
        assert_eq!(got.kind(), "function_item");
        assert_eq!(
            got.id(),
            func.id(),
            "must return the same node, not its parent"
        );
    }

    #[test]
    fn find_enclosing_kind_walks_up_for_nested_node() {
        // Identifier `bar` lives inside the function `foo`. Walking up
        // from the identifier must surface the function_item.
        let src = "fn foo() { bar(); }";
        let tree = parse(src);
        let ident = find_first(tree.root_node(), "identifier").expect("identifier");
        let func = find_enclosing_kind(ident, "function_item").expect("must find ancestor");
        assert_eq!(func.kind(), "function_item");
    }

    #[test]
    fn find_enclosing_kind_returns_none_when_no_ancestor_matches() {
        // No `impl_item` exists in this source — walking up from any node
        // must return None.
        let src = "fn foo() {}";
        let tree = parse(src);
        let func = find_first(tree.root_node(), "function_item").expect("function_item");
        assert!(find_enclosing_kind(func, "impl_item").is_none());
    }

    #[test]
    fn find_enclosing_kind_returns_innermost_for_nested_matches() {
        // Nested mod_item — mod outer { mod inner { fn x() {} } }.
        // Walking from function_item must return inner, not outer.
        let src = "mod outer { mod inner { fn x() {} } }";
        let tree = parse(src);
        let func = find_first(tree.root_node(), "function_item").expect("function_item");
        let mod_node = find_enclosing_kind(func, "mod_item").expect("must find inner mod");
        let name = mod_node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(src.as_bytes()).ok())
            .unwrap_or("");
        assert_eq!(name, "inner", "must return the innermost matching ancestor");
    }

    #[test]
    fn find_enclosing_kind_root_node_with_unknown_kind_returns_none() {
        // Walking from the root for a kind no one in the tree has.
        let src = "fn foo() {}";
        let tree = parse(src);
        let root = tree.root_node();
        assert!(find_enclosing_kind(root, "nonexistent_kind").is_none());
    }
}
