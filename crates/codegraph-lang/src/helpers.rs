//! Shared helpers used by every per-language plugin crate.
//!
//! The first inhabitant is [`truncate_signature`], which was originally
//! duplicated byte-identical across the C++, Rust, and Go plugin crates. The
//! Phase 6 debrief flagged consolidation as the natural follow-up once a
//! third copy landed; Phase 7.1 promotes it here so the about-to-be-fourth
//! Python plugin can reuse the same logic without spawning yet another copy.
//!
//! Any future helper that needs to be byte-identical across plugins belongs
//! here — keep this module strictly language-agnostic. Anything that touches
//! a tree-sitter grammar (and therefore a specific node-kind vocabulary)
//! should stay in the per-language `helpers.rs`.

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
}
