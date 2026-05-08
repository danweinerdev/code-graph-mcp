//! Pre-parse byte substitution for C++ source.
//!
//! `strip_macros` removes API-export macros (e.g., Unreal Engine's `CORE_API`,
//! `ENGINE_API`) from C++ source bytes by overwriting each whole-word match
//! with the same number of space characters. Tree-sitter then parses the
//! cleaned bytes; line/column offsets are preserved exactly because spaces
//! have the same byte count as the original macro identifier.
//!
//! This module is the algorithm-only deliverable for Phase 1.2 of the
//! `CppMacroStrip` plan. Phase 2 wires it into `CppParser::preprocess` (a
//! `LanguagePlugin` trait method that does not yet exist); for now the
//! function is callable but uncalled in production code paths.
//!
//! # Whole-word matching
//!
//! A macro is replaced only when bordered by non-identifier characters on
//! both sides. ASCII identifier rules apply (`[A-Za-z0-9_]`). The `$`
//! character is a GCC/Clang extension that does NOT appear in core C++
//! identifiers and is not used in Unreal Engine codebases; `is_ident_byte`
//! intentionally excludes it.
//!
//! # Empty-pattern guard
//!
//! Empty-string entries in `macros` are NOT supported here. The config-load
//! layer (`codegraph-core::CppConfig`) drains them before this function is
//! called. An empty pattern would loop forever (every byte position matches
//! a zero-length pattern with zero advance). A `debug_assert!` guards the
//! invariant in debug builds.

use std::borrow::Cow;

/// Replace every whole-word occurrence of each macro pattern in `content`
/// with same-length spaces. Empty list short-circuits to `Cow::Borrowed`
/// for zero allocation.
///
/// Whole-word boundary uses ASCII identifier rules (`[A-Za-z0-9_]`); `$`
/// is intentionally excluded (GCC/Clang extension not used in UE).
///
/// # Robustness
///
/// Empty entries in `macros` are skipped silently. `codegraph-core`'s
/// `RootConfig::load` already drains empty strings before the substitution
/// is invoked in production, but this function defensively no-ops on them
/// so a misuse from a test, benchmark, or future caller cannot infinite-
/// loop the byte scan in a release build. A `debug_assert!` in debug
/// builds still surfaces the contract violation.
pub fn strip_macros<'a>(content: &'a [u8], macros: &[String]) -> Cow<'a, [u8]> {
    if macros.is_empty() {
        return Cow::Borrowed(content);
    }
    let mut out = content.to_vec();
    for macro_name in macros {
        let pat = macro_name.as_bytes();
        debug_assert!(
            !pat.is_empty(),
            "empty macro pattern reached strip_macros — config-load filter is broken",
        );
        if pat.is_empty() {
            continue;
        }
        let mut i = 0;
        while i + pat.len() <= out.len() {
            if out[i..i + pat.len()] == *pat {
                let preceded = i > 0 && is_ident_byte(out[i - 1]);
                let followed = i + pat.len() < out.len() && is_ident_byte(out[i + pat.len()]);
                if !preceded && !followed {
                    for b in &mut out[i..i + pat.len()] {
                        *b = b' ';
                    }
                }
                i += pat.len();
            } else {
                i += 1;
            }
        }
    }
    Cow::Owned(out)
}

#[inline]
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    //! Algorithm-correctness suite for `strip_macros`. Cases mirror the
    //! verification field of Plan task 1.2 (cases (a)–(k)).
    //!
    //! Cases (a)–(e) feed the cleaned bytes through `CppParser::parse_file`
    //! and assert on the resulting `FileGraph`. Cases (f) and (h) also drive
    //! parsing because the documented contract is "the parse result is
    //! correct after substitution". Cases (g), (i), (j), (k) are pure
    //! substitution-property assertions.
    use std::borrow::Cow;
    use std::path::Path;

    use codegraph_core::{EdgeKind, FileGraph, Symbol, SymbolKind};
    use codegraph_lang::LanguagePlugin;
    use pretty_assertions::assert_eq;

    use super::strip_macros;
    use crate::CppParser;

    /// Run the C++ parser against bytes that have already been preprocessed
    /// by `strip_macros`. Mirrors how Phase 2 will wire the call.
    fn parse_cleaned(src: &str, macros: &[String]) -> FileGraph {
        let cleaned = strip_macros(src.as_bytes(), macros);
        let p = CppParser::new().expect("CppParser::new must succeed");
        p.parse_file(Path::new("/test.cpp"), &cleaned)
            .expect("parse_file must succeed")
    }

    fn find_symbol<'a>(fg: &'a FileGraph, name: &str) -> Option<&'a Symbol> {
        fg.symbols.iter().find(|s| s.name == name)
    }

    /// Case (a): single API macro between `class` and the class name. The
    /// canonical UE bug — without substitution this produces zero symbols.
    #[test]
    fn case_a_class_with_single_api_macro_extracts_correctly() {
        let src = "class CORE_API MyClass : public UObject {};";
        let fg = parse_cleaned(src, &["CORE_API".to_owned()]);

        let s = find_symbol(&fg, "MyClass").expect("expected MyClass symbol");
        assert_eq!(s.kind, SymbolKind::Class);

        let inherits = fg
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Inherits && e.from == "MyClass" && e.to == "UObject")
            .expect("expected MyClass -> UObject inherits edge");
        // Inheritance edges record line: 0 by parser convention; just confirm
        // the edge shape.
        assert_eq!(inherits.kind, EdgeKind::Inherits);
    }

    /// Case (b): two stacked macros between `class` and the class name. Both
    /// are stripped on independent passes.
    #[test]
    fn case_b_class_with_two_api_macros_extracts_correctly() {
        let src = "class FOO_API BAR_EXTRA MyClass : public Base {};";
        let fg = parse_cleaned(src, &["FOO_API".to_owned(), "BAR_EXTRA".to_owned()]);

        let s = find_symbol(&fg, "MyClass").expect("expected MyClass symbol");
        assert_eq!(s.kind, SymbolKind::Class);

        fg.edges
            .iter()
            .find(|e| e.kind == EdgeKind::Inherits && e.from == "MyClass" && e.to == "Base")
            .expect("expected MyClass -> Base inherits edge");
    }

    /// Case (c): empty `macro_strip` preserves the buggy opt-in semantics.
    /// Users who haven't configured `macro_strip` see the same zero-symbol
    /// behavior they get today.
    #[test]
    fn case_c_class_with_unlisted_macro_still_broken() {
        let src = "class CORE_API MyClass {};";
        let fg = parse_cleaned(src, &[]);

        assert!(
            find_symbol(&fg, "MyClass").is_none(),
            "with empty macro_strip, MyClass must NOT extract — preserves opt-in semantics",
        );
    }

    /// Case (d): `UCLASS()` macro call edge above a macro-prefixed class
    /// definition. Both should appear: the call edge to `UCLASS` and the
    /// `MyClass` symbol with its `UObject` inheritance.
    #[test]
    fn case_d_uclass_above_macro_class_extracts_correctly() {
        let src = "UCLASS()\nclass CORE_API MyClass : public UObject {};";
        let fg = parse_cleaned(src, &["CORE_API".to_owned()]);

        let s = find_symbol(&fg, "MyClass").expect("expected MyClass symbol");
        assert_eq!(s.kind, SymbolKind::Class);

        fg.edges
            .iter()
            .find(|e| e.kind == EdgeKind::Calls && e.to == "UCLASS")
            .expect("expected UCLASS() call edge");

        fg.edges
            .iter()
            .find(|e| e.kind == EdgeKind::Inherits && e.from == "MyClass" && e.to == "UObject")
            .expect("expected MyClass -> UObject inherits edge");
    }

    /// Case (e): function-line API macro is stripped as a free side effect.
    /// The forward declaration alone produces no symbol per existing parser
    /// limitations, so we include a definition with body.
    #[test]
    fn case_e_function_with_inline_api_macro() {
        let src = "void CORE_API DoThing();\nvoid CORE_API DoThing() {}";
        let fg = parse_cleaned(src, &["CORE_API".to_owned()]);

        let s = find_symbol(&fg, "DoThing").expect("expected DoThing symbol");
        assert_eq!(s.kind, SymbolKind::Function);
    }

    /// Case (f): substitution inside an ordinary string literal is harmless.
    /// `"CORE_API is great"` becomes `"          is great"` (still a valid
    /// string) and the surrounding parse is unaffected.
    #[test]
    fn case_f_api_macro_inside_string_literal_unaffected() {
        let src = "const char* msg = \"CORE_API is great\";\nvoid DoThing() {}";
        let fg = parse_cleaned(src, &["CORE_API".to_owned()]);

        let s = find_symbol(&fg, "DoThing").expect("expected DoThing symbol");
        assert_eq!(s.kind, SymbolKind::Function);
    }

    /// Case (g): substitution inside a raw-string-literal body is harmless
    /// (the content is opaque to symbol extraction). This is the *positive*
    /// case for raw strings — the unsafe case where the macro matches the
    /// raw-string TAG is documented as a limitation, not a passing test.
    #[test]
    fn case_g_api_macro_inside_raw_string_literal_unaffected() {
        let src = "const char* s = R\"(CORE_API in raw)\";\nvoid DoThing() {}";
        let fg = parse_cleaned(src, &["CORE_API".to_owned()]);

        let s = find_symbol(&fg, "DoThing").expect("expected DoThing symbol");
        assert_eq!(s.kind, SymbolKind::Function);
    }

    /// Case (h): identifier whose prefix is the macro must NOT be stripped.
    /// `CORE_API_helper` contains `CORE_API` as a substring but the trailing
    /// `_` is an identifier byte, so the whole-word check rejects the match.
    #[test]
    fn case_h_identifier_containing_macro_substring_unchanged() {
        let src = "void CORE_API_helper() {}";
        let fg = parse_cleaned(src, &["CORE_API".to_owned()]);

        let s = find_symbol(&fg, "CORE_API_helper")
            .expect("CORE_API_helper must extract — whole-word boundary check");
        assert_eq!(s.kind, SymbolKind::Function);

        // Belt-and-braces: confirm `helper` did NOT extract as a separate
        // symbol (which it would if `CORE_API` were stripped).
        assert!(
            find_symbol(&fg, "helper").is_none(),
            "no spurious 'helper' symbol — substring match must not fire",
        );
    }

    /// Case (i): prefix-overlap order safety. `["FOO", "FOO_BAR"]` and
    /// `["FOO_BAR", "FOO"]` must produce the same result. This locks the
    /// worked-example claim from the design: whole-word matching makes
    /// ordering safe.
    #[test]
    fn case_i_prefix_overlap_macros_order_safe() {
        let src = "class FOO MyClass {}; class FOO_BAR OtherClass : public Base {};";

        for ordering in [
            vec!["FOO".to_owned(), "FOO_BAR".to_owned()],
            vec!["FOO_BAR".to_owned(), "FOO".to_owned()],
        ] {
            let fg = parse_cleaned(src, &ordering);

            assert!(
                find_symbol(&fg, "MyClass").is_some(),
                "MyClass must extract regardless of macro ordering ({:?})",
                ordering,
            );
            assert!(
                find_symbol(&fg, "OtherClass").is_some(),
                "OtherClass must extract regardless of macro ordering ({:?})",
                ordering,
            );
        }
    }

    /// Case (j): empty list short-circuits to `Cow::Borrowed`. Asserts via
    /// `matches!` on the `Cow` discriminant; this is the zero-allocation
    /// fast path for every non-UE user.
    #[test]
    fn case_j_empty_macro_list_short_circuits_to_borrowed() {
        let result = strip_macros(b"anything", &[]);
        assert!(
            matches!(result, Cow::Borrowed(_)),
            "empty macro list must return Cow::Borrowed (zero allocation)",
        );
    }

    /// Case (k): byte-offset preservation. `MyClass` sits at a known column
    /// in the original source; after stripping the macro prefix, the symbol's
    /// reported `line` and `column` must match those positions exactly.
    /// Replacement-with-spaces (not deletion) is what makes this work.
    #[test]
    fn case_k_byte_offset_preservation() {
        // Layout (zero-indexed columns):
        //   col:  0         1         2
        //         0123456789012345678901234567
        //   line 1: class CORE_API MyClass {};
        // 'class' starts at column 0, 'CORE_API' at column 6, 'MyClass' at
        // column 15.
        let src = "class CORE_API MyClass {};";
        let original_my_class_col = src.find("MyClass").expect("MyClass in source") as u32;
        assert_eq!(original_my_class_col, 15, "test fixture sanity check");

        let fg = parse_cleaned(src, &["CORE_API".to_owned()]);
        let s = find_symbol(&fg, "MyClass").expect("expected MyClass symbol");

        // The Symbol's line/column points to the *enclosing* class_specifier
        // node, which begins at `class` (column 0). Substitution must not
        // shift any byte; we assert byte-offset preservation by checking the
        // class_specifier start position is the original `class` start, AND
        // (more pointedly) that the class_specifier ends at the original
        // semicolon position. If substitution had shifted bytes, end_line or
        // column would drift.
        assert_eq!(s.line, 1, "class_specifier starts on line 1");
        assert_eq!(
            s.column, 0,
            "class_specifier starts at column 0 (the 'class' keyword)",
        );
        assert_eq!(
            s.end_line, 1,
            "class_specifier ends on line 1 — substitution preserves line breaks",
        );

        // Signature is verbatim text of the class_specifier from the
        // CLEANED bytes. It should contain "MyClass" but NOT "CORE_API"
        // (the macro was replaced by spaces). The negative assertion is
        // what discriminates "bytes were actually substituted" from "the
        // pre-substitution source slipped through."
        assert!(
            s.signature.contains("MyClass"),
            "signature should contain MyClass; got {:?}",
            s.signature,
        );
        assert!(
            !s.signature.contains("CORE_API"),
            "signature must not contain stripped macro; got {:?}",
            s.signature,
        );

        // The original source has 'MyClass' at byte/column 15. The cleaned
        // bytes preserve that position because `CORE_API` (8 bytes) became
        // 8 spaces. Verify the offset is unchanged in the cleaned output —
        // this is what the preserves-byte-offsets contract actually means.
        let cleaned = strip_macros(src.as_bytes(), &["CORE_API".to_owned()]);
        let cleaned_my_class_pos = cleaned
            .windows(7)
            .position(|w| w == b"MyClass")
            .expect("MyClass survives substitution") as u32;
        assert_eq!(
            cleaned_my_class_pos, original_my_class_col,
            "MyClass position must not shift after stripping CORE_API",
        );
    }
}
