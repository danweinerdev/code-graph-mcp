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
//! layer (`code-graph-core::CppConfig`) drains them before this function is
//! called. An empty pattern would loop forever (every byte position matches
//! a zero-length pattern with zero advance). A `debug_assert!` guards the
//! invariant in debug builds.

use std::borrow::Cow;
use std::collections::HashSet;

/// Replace every whole-word occurrence of each macro pattern in `content`
/// with same-length spaces. Empty list short-circuits to `Cow::Borrowed`
/// for zero allocation.
///
/// Whole-word boundary uses ASCII identifier rules (`[A-Za-z0-9_]`); `$`
/// is intentionally excluded (GCC/Clang extension not used in UE).
///
/// # Robustness
///
/// Empty entries in `macros` are skipped silently. `code-graph-core`'s
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

/// Identifier *start* predicate. Stricter than [`is_ident_byte`]: rejects
/// ASCII digits at the lead position so we don't mis-classify a numeric
/// literal as the start of an identifier scan. The "continue" predicate
/// inside an identifier remains [`is_ident_byte`].
#[inline]
fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

/// Recognize a C++ lexical region starting at `content[i]` and return the
/// byte position immediately past its close, or `None` if `content[i]` is an
/// ordinary byte that does not open a region.
///
/// Recognized regions (in dispatch order):
///   1. Line comments `// … \n` — closed by `\n` or EOF.
///   2. Block comments `/* … */` — closed by `*/` or EOF (NOT nestable; C++
///      forbids nested block comments — `/* outer /* inner */ trailing */`
///      closes at the first `*/`).
///   3. Raw strings `R"DELIM(…)DELIM"` — closed by the matching `)DELIM"`.
///      The `R"` opener is rejected if the preceding byte is identifier-
///      continue (so `xR"…"` is NOT a raw string).
///   4. Double-quoted strings `"…"` — closed by `"` not preceded by an odd
///      number of `\` bytes.
///   5. Single-quoted char literals `'…'` — same close rule as `"…"`.
///
/// Comment dispatch precedes string dispatch: `//` followed by `"` does NOT
/// enter string mode (the `"` is inside the line comment).
///
/// EOF-truncated regions return `Some(content.len())` rather than `None`.
/// `None` means "this byte is not a region opener; advance by 1 and try
/// again." `Some(content.len())` means "I recognized this as a region and
/// it extends to EOF; terminate the scan." The callers (`find_balanced_close`
/// and `strip_macros_with_args`) rely on this distinction.
///
/// # Out of scope (treated as ordinary bytes; documented limitations):
///   - Line continuations: `\` at EOL inside a `//` comment does NOT extend
///     the comment.
///   - Trigraphs: `??/` (alias for `\`), `??(`, `??)` are not interpreted.
///   - Digraphs: `<%`, `%>`, `:>`, `<:`, `%:` are not interpreted.
///   - Encoding-prefixed raw strings (`u8R"…"`, `LR"…"`, `uR"…"`, `UR"…"`)
///     — the identifier-prefix check intentionally rejects them; they fall
///     through to ordinary `"…"` mode (incorrect but acceptable per design).
///   - `\u`-escaped delimiter characters in raw strings — raw strings don't
///     process escapes by definition; not an issue.
pub(crate) fn skip_lexical(content: &[u8], i: usize) -> Option<usize> {
    if i >= content.len() {
        return None;
    }
    let b0 = content[i];
    let b1 = if i + 1 < content.len() {
        Some(content[i + 1])
    } else {
        None
    };

    // Comment dispatch precedes string dispatch: a `"` inside a `// …` line
    // comment must NOT open a string. Same for `/* … */`.
    if b0 == b'/' && b1 == Some(b'/') {
        return Some(skip_line_comment(content, i));
    }
    if b0 == b'/' && b1 == Some(b'*') {
        return Some(skip_block_comment(content, i));
    }

    // Raw-string dispatch must precede ordinary double-quote dispatch so a
    // valid `R"DELIM(…)DELIM"` is recognized. The prefix check inside
    // `skip_raw_string` returns `None` when `R"` is part of a larger
    // identifier (e.g. `xR"…"`), letting the dispatch fall through.
    if b0 == b'R' && b1 == Some(b'"') {
        if let Some(end) = skip_raw_string(content, i) {
            return Some(end);
        }
        // `R"` was part of a larger identifier; fall through to ordinary
        // handling (which returns `None` because `R` is not a region opener).
        return None;
    }

    if b0 == b'"' {
        return Some(skip_double_quoted(content, i));
    }
    if b0 == b'\'' {
        return Some(skip_single_quoted(content, i));
    }

    None
}

/// Scan a `//` line comment starting at `content[i]` (caller guaranteed
/// `content[i..i+2] == b"//"`). Returns the byte position immediately past
/// the closing `\n`, or `content.len()` if EOF arrives first.
fn skip_line_comment(content: &[u8], i: usize) -> usize {
    debug_assert!(i + 1 < content.len() && content[i] == b'/' && content[i + 1] == b'/');
    let mut k = i + 2;
    while k < content.len() {
        if content[k] == b'\n' {
            return k + 1;
        }
        k += 1;
    }
    content.len()
}

/// Scan a `/* … */` block comment starting at `content[i]` (caller guaranteed
/// `content[i..i+2] == b"/*"`). C++ does NOT allow nested block comments —
/// the first `*/` always closes, even if an inner `/*` precedes it. Returns
/// the byte position immediately past the closing `*/`, or `content.len()`
/// if EOF arrives before any `*/` (tree-sitter will surface the unterminated
/// block as an ERROR node).
fn skip_block_comment(content: &[u8], i: usize) -> usize {
    debug_assert!(i + 1 < content.len() && content[i] == b'/' && content[i + 1] == b'*');
    let mut k = i + 2;
    while k + 1 < content.len() {
        if content[k] == b'*' && content[k + 1] == b'/' {
            return k + 2;
        }
        k += 1;
    }
    content.len()
}

/// Scan a double-quoted string starting at `content[i]` (caller guaranteed
/// `content[i] == b'"'`). A `"` byte closes the literal iff the number of
/// consecutive `\` bytes immediately preceding it is EVEN (zero counts as
/// even). The walk-back-counting-`\` approach is the canonical C++-standard
/// escape interpretation.
///
/// # Worked examples
///
/// `"\""` — 4 bytes (`"`, `\`, `"`, `"`):
///   - Open at 0. At pos 2 see `"`; walk back: pos 1 is `\` (count=1), pos 0
///     is `"` stop. Count is odd → escaped, continue.
///   - At pos 3 see `"`; walk back: pos 2 is `"` (not `\`, stop). Count=0
///     (even) → closes. Return `Some(4)`.
///
/// `"foo\\"` — 7 bytes (`"`, `f`, `o`, `o`, `\`, `\`, `"`), representing a
/// C++ literal containing the single byte `\`:
///   - At pos 6 see `"`; walk back: pos 5 `\` (1), pos 4 `\` (2), pos 3 `o`
///     stop. Count=2 (even) → closes. Return `Some(7)`.
///
/// Inside a string body, `//` and `/*` are NOT special — only the close-
/// quote rule applies. We do NOT recurse into the dispatch shell.
///
/// EOF before close → `Some(content.len())`.
fn skip_double_quoted(content: &[u8], i: usize) -> usize {
    debug_assert!(content[i] == b'"');
    skip_quoted(content, i, b'"')
}

/// Scan a single-quoted char literal starting at `content[i]` (caller
/// guaranteed `content[i] == b'\''`). Same odd-`\`-count escape rule as
/// `skip_double_quoted`; handles `'\''`, `'\n'`, etc. EOF → `content.len()`.
fn skip_single_quoted(content: &[u8], i: usize) -> usize {
    debug_assert!(content[i] == b'\'');
    skip_quoted(content, i, b'\'')
}

/// Shared close-quote scan for `"…"` and `'…'`. `closer` is the byte that
/// closes the literal. Walk forward from `i+1`; for each candidate `closer`,
/// walk backward counting preceding `\` bytes; even count → closes; odd →
/// escaped, continue. The literal-start `i` bounds the walk-back so a string
/// like `"\""` doesn't underflow past its open quote.
fn skip_quoted(content: &[u8], i: usize, closer: u8) -> usize {
    let mut k = i + 1;
    while k < content.len() {
        if content[k] == closer {
            let mut backslashes = 0usize;
            let mut j = k;
            while j > i + 1 {
                j -= 1;
                if content[j] == b'\\' {
                    backslashes += 1;
                } else {
                    break;
                }
            }
            if backslashes % 2 == 0 {
                return k + 1;
            }
        }
        k += 1;
    }
    content.len()
}

/// Scan a raw string `R"DELIM(…)DELIM"` starting at `content[i]` (caller
/// guaranteed `content[i..i+2] == b"R\""`).
///
/// The prefix check is load-bearing: if `i > 0 && is_ident_byte(content[i-1])`,
/// the `R"` belongs to a larger identifier (e.g. `xR"…"`) and this is NOT a
/// raw string. Return `None` so the dispatch shell falls through.
///
/// Otherwise extract the delimiter tag by scanning from `i+2` for the first
/// `(`. The tag is `content[i+2..paren_pos]`. EOF before `(` →
/// `Some(content.len())`. Once the tag is captured, scan from `paren_pos+1`
/// for the close sequence `)<tag>"`; return `Some(close_pos + tag.len() + 2)`
/// where `close_pos` is the index of the `)`. EOF before close →
/// `Some(content.len())`.
///
/// Raw-string bodies do NOT process escapes — `\)`, `\"`, etc. inside the
/// body are ordinary bytes and only the literal close sequence terminates.
fn skip_raw_string(content: &[u8], i: usize) -> Option<usize> {
    debug_assert!(i + 1 < content.len() && content[i] == b'R' && content[i + 1] == b'"');
    // Identifier-prefix check: `xR"…"` is part of identifier `xR`, not a
    // raw string. Encoding prefixes (`u8R"`, `LR"`, etc.) are rejected here
    // too — they fall through to ordinary `"…"` dispatch by design.
    if i > 0 && is_ident_byte(content[i - 1]) {
        return None;
    }
    // Scan for `(` to capture the delimiter tag.
    let tag_start = i + 2;
    let mut k = tag_start;
    while k < content.len() && content[k] != b'(' {
        k += 1;
    }
    if k >= content.len() {
        // EOF before `(`.
        return Some(content.len());
    }
    let tag_end = k;
    let paren_pos = k;
    let tag_len = tag_end - tag_start;

    // Scan for the close sequence `)<tag>"` starting after the open paren.
    let mut p = paren_pos + 1;
    while p < content.len() {
        if content[p] == b')' && p + 1 + tag_len < content.len() {
            // Match tag bytes.
            let tag_matches = content[p + 1..p + 1 + tag_len] == content[tag_start..tag_end];
            if tag_matches && content[p + 1 + tag_len] == b'"' {
                return Some(p + 1 + tag_len + 1);
            }
        }
        p += 1;
    }
    Some(content.len())
}

/// Find the byte position of the `)` that balances the `(` at `open_paren`.
///
/// Lexical regions (string literals, char literals, raw strings, line/block
/// comments) are skipped wholesale via [`skip_lexical`] so parentheses that
/// live inside string or comment bodies do NOT affect the depth count. The
/// caller passes the index of the opener — a `debug_assert!` enforces that
/// `content[open_paren] == b'('` in debug builds.
///
/// Returns `Some(close_pos)` where `content[close_pos] == b')'` and the
/// parens balance, or `None` if EOF is reached with unbalanced depth (the
/// caller's contract: silently bail and advance past the identifier so we
/// don't infinite-loop or destructively rewrite a half-open construct).
pub(crate) fn find_balanced_close(content: &[u8], open_paren: usize) -> Option<usize> {
    debug_assert!(open_paren < content.len() && content[open_paren] == b'(');
    let mut depth: u32 = 1;
    let mut i = open_paren + 1;
    while i < content.len() {
        // Skip lexical regions so parens inside strings/comments/raw-strings
        // don't affect depth.
        if let Some(end) = skip_lexical(content, i) {
            i = end;
            continue;
        }
        match content[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Whole-word identifier-with-arguments scanner. For every occurrence of an
/// identifier listed in `tokens` that is immediately followed (modulo ASCII
/// whitespace) by a `(`, overwrite the byte span `[ident_start..=close]`
/// with spaces, where `close` is the matching `)`. Lexical regions (strings,
/// chars, raw strings, line/block comments) are skipped wholesale via
/// [`skip_lexical`] so identifier-shaped matches inside literal bodies do
/// NOT trigger substitution.
///
/// Returns the number of substitutions performed (one per balanced
/// identifier+arglist span replaced).
///
/// # Whole-word & paren rules
///
/// * A token only matches at an *identifier-start* boundary
///   ([`is_ident_start`] rejects ASCII digits at the lead position).
/// * The identifier span is `[ident_start..i_after]` where `i_after` is the
///   first byte that fails [`is_ident_byte`] — this preserves the existing
///   whole-word semantic from `strip_macros`.
/// * After the identifier, ASCII whitespace is skipped; if the next non-
///   whitespace byte is not `(`, the identifier is left alone (a bare
///   `UCLASS` with no arglist is invisible to this scanner — only
///   `strip_macros` would touch it, and even then only if listed in
///   `macro_strip`).
/// * Unbalanced `(` (EOF before matching `)`) → silent bail; the scan
///   advances past the identifier so we don't re-scan it and don't
///   destructively rewrite a half-open construct.
///
/// # Newline preservation in the fill
///
/// `\n` bytes inside the matched span are preserved (not replaced with
/// spaces), so line numbers after the macro stay aligned with the original
/// source. All other bytes (including `\r`, `\t`, identifier chars, and the
/// parens themselves) become space. Without this carve-out, a multi-line
/// `DECLARE_DELEGATE_TwoParams(\n  Foo,\n  Bar)` would collapse onto a
/// single logical line of spaces and every symbol position reported AFTER
/// the macro would drift by the number of newlines the macro spanned. The
/// whole-word `strip_macros` doesn't need this carve-out because identifier
/// patterns cannot contain `\n`; parameterized arg lists can and do.
///
/// # Borrow-checker note
///
/// The implementation copies the candidate identifier into a scratch
/// `Vec<u8>` before consulting `tokens` and before calling
/// [`find_balanced_close`]. This is required: a borrowed slice of
/// `content` cannot be held across the subsequent byte-by-byte mutable
/// fill of `content[ident_start..=close]` or the immutable borrow that
/// `find_balanced_close(content, j)` takes. The scratch is reused across
/// the loop (single allocation up front).
pub(crate) fn strip_macros_with_args(content: &mut [u8], tokens: &HashSet<Vec<u8>>) -> usize {
    let mut replacements = 0usize;
    let mut id_buf: Vec<u8> = Vec::with_capacity(64);
    let mut i = 0;
    while i < content.len() {
        // (1) Skip lexical regions wholesale.
        if let Some(end) = skip_lexical(content, i) {
            i = end;
            continue;
        }
        // (2) Non-ident-start byte: advance.
        if !is_ident_start(content[i]) {
            i += 1;
            continue;
        }
        // (3) Walk identifier span [ident_start..i_after].
        let ident_start = i;
        let mut i_after = i + 1;
        while i_after < content.len() && is_ident_byte(content[i_after]) {
            i_after += 1;
        }
        // (4) Copy identifier into scratch (borrow-checker: we'll mutate
        // `content` below and call `find_balanced_close` which takes
        // `&[u8]`; we cannot hold a slice borrow of `content` across
        // those operations).
        id_buf.clear();
        id_buf.extend_from_slice(&content[ident_start..i_after]);
        if !tokens.contains(&id_buf) {
            i = i_after;
            continue;
        }
        // (5) Skip whitespace AND lexical regions (comments) forward,
        // require `(`. Consulting `skip_lexical` here handles tool-
        // generated or hand-written annotations like
        // `UCLASS /* clang-format off */ (BlueprintType)` or
        // `UCLASS // category annotation\n(BlueprintType)` where the
        // comment sits between the identifier and its arg list. Without
        // this, the scanner would see `/` after the whitespace skip,
        // fail the `b'('` check, and silently leave the macro un-
        // stripped — the macro then defeats tree-sitter's class parse
        // with no diagnostic.
        let mut j = i_after;
        loop {
            if j >= content.len() {
                break;
            }
            if content[j].is_ascii_whitespace() {
                j += 1;
                continue;
            }
            if let Some(end) = skip_lexical(content, j) {
                j = end;
                continue;
            }
            break;
        }
        if j >= content.len() || content[j] != b'(' {
            // Bare ident — leave alone.
            i = i_after;
            continue;
        }
        // (6) Find balanced ')'. Silent bail on unbalanced.
        match find_balanced_close(content, j) {
            Some(close) => {
                // Preserve `\n` bytes in the replaced span so multi-line
                // arg lists (e.g. `DECLARE_DELEGATE_TwoParams(\n  Foo,\n
                // Bar)`) don't collapse the file's line numbering. Every
                // other byte becomes space, keeping column offsets stable.
                for b in &mut content[ident_start..=close] {
                    if *b != b'\n' {
                        *b = b' ';
                    }
                }
                replacements += 1;
                i = close + 1;
            }
            None => {
                // Bail; advance past the identifier so we don't re-scan it.
                i = i_after;
            }
        }
    }
    replacements
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
    use std::collections::HashSet;
    use std::path::Path;

    use code_graph_core::{EdgeKind, FileGraph, Symbol, SymbolKind};
    use code_graph_lang::LanguagePlugin;
    use pretty_assertions::assert_eq;

    use super::{find_balanced_close, skip_lexical, strip_macros, strip_macros_with_args};
    use crate::CppParser;

    /// Build a single-token `HashSet<Vec<u8>>` from a string literal — the
    /// canonical shape `strip_macros_with_args` expects.
    fn tokens(names: &[&str]) -> HashSet<Vec<u8>> {
        names.iter().map(|n| n.as_bytes().to_vec()).collect()
    }

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

    // ---------------------------------------------------------------------
    // skip_lexical unit tests
    //
    // Each test asserts the exact `Option<usize>` returned by `skip_lexical`
    // at a given start offset. `skip_lexical` is the lexer the
    // `find_balanced_close` / `strip_macros_with_args` pair consults to
    // avoid descending into string/comment/raw-string regions while
    // counting parens.
    // ---------------------------------------------------------------------

    #[test]
    fn skip_lexical_ordinary_byte_returns_none() {
        let s = b"abc";
        assert_eq!(skip_lexical(s, 0), None);
        assert_eq!(skip_lexical(s, 1), None);
        assert_eq!(skip_lexical(s, 2), None);
        // Out-of-bounds index is None as well.
        assert_eq!(skip_lexical(s, 3), None);
    }

    #[test]
    fn skip_lexical_line_comment_normal_close() {
        // `// hi\nrest` — the line comment runs from index 0 to the byte
        // after `\n` at index 5; resume position = 6.
        let s = b"// hi\nrest";
        assert_eq!(skip_lexical(s, 0), Some(6));
    }

    #[test]
    fn skip_lexical_line_comment_eof_truncated() {
        // No trailing newline. End == content.len().
        let s = b"// hi";
        assert_eq!(skip_lexical(s, 0), Some(s.len()));
    }

    #[test]
    fn skip_lexical_block_comment_normal_close() {
        // `/* hi */rest` — close at index 6 (`*`), resume at 8.
        let s = b"/* hi */rest";
        assert_eq!(skip_lexical(s, 0), Some(8));
    }

    #[test]
    fn skip_lexical_block_comment_eof_truncated() {
        let s = b"/* hi";
        assert_eq!(skip_lexical(s, 0), Some(s.len()));
    }

    #[test]
    fn skip_lexical_block_comment_no_nesting() {
        // `/* outer /* inner */ trailing */`
        // The first `*/` (at byte index 18) closes the comment; the
        // trailing `*/` is ordinary bytes.
        let s = b"/* outer /* inner */ trailing */";
        // Locate the first `*/` to compute the expected resume offset.
        let first_close = s.windows(2).position(|w| w == b"*/").unwrap();
        assert_eq!(skip_lexical(s, 0), Some(first_close + 2));
    }

    #[test]
    fn skip_lexical_comment_before_string_precedence() {
        // `// "unterminated\nrest` — the `"` is inside the line comment
        // and must NOT open a string. Resume position = byte past `\n`.
        let s = b"// \"unterminated\nrest";
        let newline_pos = s.iter().position(|&b| b == b'\n').unwrap();
        assert_eq!(skip_lexical(s, 0), Some(newline_pos + 1));
    }

    #[test]
    fn skip_lexical_double_quoted_simple() {
        // `"abc"`
        let s = b"\"abc\"";
        assert_eq!(skip_lexical(s, 0), Some(5));
    }

    #[test]
    fn skip_lexical_double_quoted_escaped_quote() {
        // Source bytes: " f o o \ " b a r "  → 10 bytes.
        // The `\"` at index 5 is escaped; the `"` at index 9 closes.
        let s = b"\"foo\\\"bar\"";
        assert_eq!(s.len(), 10, "fixture sanity");
        assert_eq!(skip_lexical(s, 0), Some(10));
    }

    #[test]
    fn skip_lexical_double_quoted_escaped_backslash_before_close() {
        // Source bytes: " f o o \ \ "  → 7 bytes.
        // Two consecutive `\` BEFORE the `"` is an even count (the pair is
        // one escape sequence representing a single backslash). The trailing
        // `"` closes the literal at index 6; resume at 7.
        let s = b"\"foo\\\\\"";
        assert_eq!(s.len(), 7, "fixture sanity");
        assert_eq!(skip_lexical(s, 0), Some(7));
    }

    #[test]
    fn skip_lexical_double_quoted_eof_truncated() {
        let s = b"\"abc";
        assert_eq!(skip_lexical(s, 0), Some(s.len()));
    }

    #[test]
    fn skip_lexical_single_quoted_simple() {
        // `'a'`
        let s = b"'a'";
        assert_eq!(skip_lexical(s, 0), Some(3));
    }

    #[test]
    fn skip_lexical_single_quoted_escaped_quote() {
        // Source bytes: ' \ ' '  → 4 bytes; the escaped `'` doesn't close,
        // the trailing `'` does.
        let s = b"'\\''";
        assert_eq!(s.len(), 4, "fixture sanity");
        assert_eq!(skip_lexical(s, 0), Some(4));
    }

    #[test]
    fn skip_lexical_single_quoted_newline_escape() {
        // Source bytes: ' \ n '  → 4 bytes.
        let s = b"'\\n'";
        assert_eq!(s.len(), 4, "fixture sanity");
        assert_eq!(skip_lexical(s, 0), Some(4));
    }

    #[test]
    fn skip_lexical_single_quoted_eof_truncated() {
        // Unterminated char literal: no closing `'`. Returns
        // `Some(content.len())` so the caller advances past EOF and
        // terminates. tree-sitter surfaces the unterminated literal as
        // ERROR; we just propagate the EOF position cleanly. Mirrors
        // `skip_lexical_double_quoted_eof_truncated`.
        let s = b"'abc";
        assert_eq!(skip_lexical(s, 0), Some(4));
    }

    #[test]
    fn skip_lexical_string_body_ignores_comments() {
        // `"// not a comment"` — string mode dominates, the `//` inside is
        // ordinary bytes. Close is the trailing `"`.
        let s = b"\"// not a comment\"";
        assert_eq!(skip_lexical(s, 0), Some(s.len()));
    }

    #[test]
    fn skip_lexical_raw_string_empty_tag() {
        // `R"(simple)"`
        let s = b"R\"(simple)\"";
        assert_eq!(skip_lexical(s, 0), Some(s.len()));
    }

    #[test]
    fn skip_lexical_raw_string_complex_tag() {
        // `R"DELIM(hello)DELIM"`
        let s = b"R\"DELIM(hello)DELIM\"";
        assert_eq!(skip_lexical(s, 0), Some(s.len()));
    }

    #[test]
    fn skip_lexical_raw_string_body_with_escape_lookalikes() {
        // `R"(with \)\n\" inside)"` — none of `\)`, `\n`, `\"` are
        // interpreted inside a raw string; only `)<empty-tag>"` closes.
        let s = b"R\"(with \\)\\n\\\" inside)\"";
        assert_eq!(skip_lexical(s, 0), Some(s.len()));
    }

    #[test]
    fn skip_lexical_raw_string_eof_no_close() {
        // `R"TAG(body without close...`
        let s = b"R\"TAG(body without close";
        assert_eq!(skip_lexical(s, 0), Some(s.len()));
    }

    #[test]
    fn skip_lexical_raw_string_prefix_rejection() {
        // `xR"(...)"` — `R` at index 1 is preceded by `x` (identifier byte),
        // so `R"` is part of identifier `xR`, NOT a raw-string opener.
        // `skip_lexical(s, 1)` must return None.
        let s = b"xR\"(payload)\"";
        assert_eq!(skip_lexical(s, 1), None);
    }

    #[test]
    fn skip_lexical_consecutive_regions() {
        // `"foo"/*bar*/` — call at index 0 consumes `"foo"`, then call at
        // the returned index consumes `/*bar*/`. End position == len.
        let s = b"\"foo\"/*bar*/";
        let after_string = skip_lexical(s, 0).expect("string region");
        assert_eq!(after_string, 5, "\"foo\" is 5 bytes");
        let after_block = skip_lexical(s, after_string).expect("block region");
        assert_eq!(after_block, s.len(), "block comment runs to EOF");
    }

    // ---------------------------------------------------------------------
    // find_balanced_close unit tests
    //
    // Each test asserts the exact `Option<usize>` returned by
    // `find_balanced_close(content, open_paren)` for a carefully-counted
    // fixture. Indices are spelled out in comments so a future reader can
    // sanity-check without re-deriving the offsets.
    // ---------------------------------------------------------------------

    #[test]
    fn find_balanced_close_empty_parens() {
        // `()` — bytes: `(`=0, `)`=1. Length 2.
        let s = b"()";
        assert_eq!(find_balanced_close(s, 0), Some(1));
    }

    #[test]
    fn find_balanced_close_nested() {
        // `(a(b)c)` — bytes: `(`=0, `a`=1, `(`=2, `b`=3, `)`=4, `c`=5, `)`=6.
        // Length 7. Outer close at index 6.
        let s = b"(a(b)c)";
        assert_eq!(find_balanced_close(s, 0), Some(6));
    }

    #[test]
    fn find_balanced_close_paren_inside_double_string() {
        // `("a)b")` — bytes: `(`=0, `"`=1, `a`=2, `)`=3, `b`=4, `"`=5,
        // `)`=6. Length 7. The `)` at index 3 lives inside the string
        // literal `"a)b"`; `skip_lexical(s, 1)` returns `Some(6)` (one past
        // the closing `"` at index 5), so the walker never sees the inner
        // `)`. The outer close is at index 6.
        let s = b"(\"a)b\")";
        assert_eq!(find_balanced_close(s, 0), Some(6));
    }

    #[test]
    fn find_balanced_close_paren_inside_block_comment() {
        // `(/* ) */)` — bytes: `(`=0, `/`=1, `*`=2, ` `=3, `)`=4, ` `=5,
        // `*`=6, `/`=7, `)`=8. Length 9. The `)` at index 4 lives inside
        // the block comment `/* ) */` (which closes at `*/` ending at byte
        // 8 exclusive); `skip_lexical(s, 1)` returns `Some(8)`. The outer
        // close is at index 8.
        let s = b"(/* ) */)";
        assert_eq!(find_balanced_close(s, 0), Some(8));
    }

    #[test]
    fn find_balanced_close_paren_inside_raw_string() {
        // `(R"X())X")` — bytes: `(`=0, `R`=1, `"`=2, `X`=3, `(`=4,
        // `)`=5, `)`=6, `X`=7, `"`=8, `)`=9. Length 10. The raw string
        // opens at index 1 with delimiter tag `X`; its body is the
        // single byte `)` at index 5 (between the `(` at index 4 and
        // the close sequence `)X"` at indices 6-7-8). The raw-string
        // close is at byte 8 (inclusive); `skip_lexical(s, 1)` returns
        // `Some(9)`. The outer close is at index 9.
        let s = b"(R\"X())X\")";
        assert_eq!(find_balanced_close(s, 0), Some(9));
    }

    #[test]
    fn find_balanced_close_paren_inside_char_literal() {
        // `('(')` — bytes: `(`=0, `'`=1, `(`=2, `'`=3, `)`=4. Length 5.
        // The `(` at index 2 lives inside the char literal `'('`;
        // `skip_lexical(s, 1)` returns `Some(4)` (one past the closing
        // `'` at index 3), so the walker never sees the inner `(`. The
        // outer close is at index 4.
        let s = b"('(')";
        assert_eq!(find_balanced_close(s, 0), Some(4));
    }

    #[test]
    fn find_balanced_close_eof_unbalanced_returns_none() {
        // `(unclosed` — bytes: `(`=0, `u`=1, …, `d`=8. Length 9. No
        // matching `)` before EOF; the walker exits the loop with
        // `depth==1` and returns `None`. This is the contract the caller
        // (`strip_macros_with_args`) relies on to silently bail rather
        // than destructively rewrite a half-open construct.
        let s = b"(unclosed";
        assert_eq!(find_balanced_close(s, 0), None);
    }

    // ---------------------------------------------------------------------
    // strip_macros_with_args unit tests
    //
    // The scanner replaces `IDENT(...)` spans with spaces (preserving `\n`
    // bytes so line offsets after the macro stay aligned). The harness for
    // these tests is intentionally low-ceremony: build a token set, run
    // `strip_macros_with_args` over a `Vec<u8>` copy of the input, and
    // assert on `String::from_utf8(buf).unwrap()`.
    // ---------------------------------------------------------------------

    /// Case (a): `GENERATED_BODY()` — the canonical Unreal use site. The
    /// 16-byte span (identifier + `()`) collapses to 16 spaces. Length
    /// equality is asserted explicitly because the byte-preserving
    /// contract is what makes downstream symbol positions correct.
    #[test]
    fn strip_macros_with_args_empty_args() {
        let input = b"GENERATED_BODY()";
        let mut buf = input.to_vec();
        let n = strip_macros_with_args(&mut buf, &tokens(&["GENERATED_BODY"]));
        assert_eq!(n, 1, "exactly one replacement");
        assert_eq!(buf.len(), input.len(), "byte length preserved");
        assert_eq!(String::from_utf8(buf).unwrap(), "                ");
    }

    /// Case (b): a real-world UE-style attribute list with nested parens
    /// and commas. The whole span — identifier through outer `)` — is
    /// blanked; the inner `(` and `)` participate in the balanced-paren
    /// scan but do not affect the output (they too become spaces).
    #[test]
    fn strip_macros_with_args_complex_args() {
        let input = b"UCLASS(BlueprintType, meta=(BlueprintSpawnableComponent))";
        let mut buf = input.to_vec();
        let n = strip_macros_with_args(&mut buf, &tokens(&["UCLASS"]));
        assert_eq!(n, 1);
        assert_eq!(buf.len(), input.len());
        assert!(
            buf.iter().all(|&b| b == b' '),
            "every byte must be space; got {:?}",
            String::from_utf8_lossy(&buf),
        );
    }

    /// Case (c): a `)` would appear unbalanced if the scanner naively
    /// counted parens; instead `find_balanced_close` consults
    /// `skip_lexical` and walks past the string literal `"X, Y"`
    /// wholesale. (This particular fixture has no `)` inside the string
    /// but does have a `,` which would corrupt naive comma-split args;
    /// the structural property under test is "the string region is
    /// skipped, not parsed".)
    #[test]
    fn strip_macros_with_args_string_literal_with_commas() {
        let input = br#"UFUNCTION(BlueprintCallable, meta=(DisplayName="X, Y"))"#;
        let mut buf = input.to_vec();
        let n = strip_macros_with_args(&mut buf, &tokens(&["UFUNCTION"]));
        assert_eq!(n, 1);
        assert_eq!(buf.len(), input.len());
        assert!(
            buf.iter().all(|&b| b == b' '),
            "every byte must be space; got {:?}",
            String::from_utf8_lossy(&buf),
        );
    }

    /// Case (d): a multi-line `DECLARE_DELEGATE_TwoParams(\n ... \n ...)`.
    /// The matched span includes two `\n` bytes; the newline-preservation
    /// carve-out keeps both `\n`s in place so line numbers after the
    /// macro do NOT drift. Every non-`\n` byte in the span (including
    /// leading/trailing spaces, identifier chars, commas, and parens)
    /// becomes a literal space.
    #[test]
    fn strip_macros_with_args_multiline_preserves_newlines() {
        let input = b"DECLARE_DELEGATE_TwoParams(\n  FOnHit,\n  AActor*, OtherActor)";
        let mut buf = input.to_vec();
        let n = strip_macros_with_args(&mut buf, &tokens(&["DECLARE_DELEGATE_TwoParams"]));
        assert_eq!(n, 1);
        assert_eq!(buf.len(), input.len(), "byte length preserved");
        // Build the expected output: same length, every byte is space
        // EXCEPT positions where the input had `\n`, which stay `\n`.
        let expected: Vec<u8> = input
            .iter()
            .map(|&b| if b == b'\n' { b'\n' } else { b' ' })
            .collect();
        assert_eq!(buf, expected);
        // Concretely: the two newlines from the input survive at the same
        // byte offsets, so any symbol position reported after this macro
        // sees the same line/column it would have on the original source.
        assert_eq!(
            buf.iter().filter(|&&b| b == b'\n').count(),
            2,
            "both newlines preserved",
        );
    }

    /// Case (e): the candidate `UCLASS(Foo)` lives inside a string
    /// literal. `skip_lexical` advances past the whole `"..."` region at
    /// the top of the scanner loop, so the identifier inside is never
    /// considered for matching. Input is returned unchanged.
    #[test]
    fn strip_macros_with_args_inside_string_no_strip() {
        let input = br#"const char* s = "UCLASS(Foo)";"#;
        let mut buf = input.to_vec();
        let n = strip_macros_with_args(&mut buf, &tokens(&["UCLASS"]));
        assert_eq!(n, 0, "no replacements — UCLASS is inside the string");
        assert_eq!(buf, input);
    }

    /// Case (f): same property as (e) but for line and block comments.
    /// Both comment forms are recognized by `skip_lexical` and skipped
    /// wholesale; the identifier inside is never considered. Bundled as
    /// one test because the assertion shape is identical.
    #[test]
    fn strip_macros_with_args_inside_comment_no_strip() {
        // Line comment.
        let line = b"// UCLASS(Foo)\nreal code";
        let mut buf = line.to_vec();
        let n = strip_macros_with_args(&mut buf, &tokens(&["UCLASS"]));
        assert_eq!(n, 0, "line comment contents are opaque");
        assert_eq!(buf, line);

        // Block comment.
        let block = b"/* UCLASS(Foo) */ real code";
        let mut buf = block.to_vec();
        let n = strip_macros_with_args(&mut buf, &tokens(&["UCLASS"]));
        assert_eq!(n, 0, "block comment contents are opaque");
        assert_eq!(buf, block);
    }

    /// Case (g): unbalanced `(` (EOF before the matching `)`). The
    /// scanner must silently bail — no rewrite, no log — so a
    /// half-open construct from a broken `#if 0 UCLASS(unclosed` region
    /// doesn't get destructively rewritten. Pins Decision 2 in the
    /// UeMacroSupport design.
    #[test]
    fn strip_macros_with_args_unbalanced_paren_bails() {
        let input = b"UCLASS(unclosed";
        let mut buf = input.to_vec();
        let n = strip_macros_with_args(&mut buf, &tokens(&["UCLASS"]));
        assert_eq!(n, 0, "unbalanced parens produce no replacement");
        assert_eq!(buf, input, "bytes left untouched after silent bail");
    }

    /// Case (h): whole-word boundary on the *suffix* side. The
    /// identifier walk continues past `UCLASS` through `_HELPER` because
    /// `_` is an identifier-continue byte, so the matched candidate is
    /// `MY_UCLASS_HELPER` — not in the token set. No strip.
    #[test]
    fn strip_macros_with_args_whole_word_boundary_prefix() {
        let input = b"MY_UCLASS_HELPER(x)";
        let mut buf = input.to_vec();
        let n = strip_macros_with_args(&mut buf, &tokens(&["UCLASS"]));
        assert_eq!(n, 0, "substring match must not fire");
        assert_eq!(buf, input);
    }

    /// Case (i): whole-word boundary on the *prefix* side. The
    /// identifier walk starts at `X` (ident-start) and continues through
    /// `UCLASS`, yielding `XUCLASS` — not in the token set. No strip.
    #[test]
    fn strip_macros_with_args_whole_word_boundary_suffix() {
        let input = b"XUCLASS(x)";
        let mut buf = input.to_vec();
        let n = strip_macros_with_args(&mut buf, &tokens(&["UCLASS"]));
        assert_eq!(n, 0, "substring match must not fire");
        assert_eq!(buf, input);
    }

    /// Case (i'): an inline `/* … */` comment between the macro identifier
    /// and its `(` must NOT defeat the strip. Code-formatting tools and
    /// generated UE headers occasionally insert annotations here; without
    /// the lexical-region skip in step (5), the scanner would see `/` after
    /// the whitespace skip, fail the `b'('` check, and silently bail —
    /// leaving the macro unstripped and the surrounding class invisible
    /// to tree-sitter.
    #[test]
    fn strip_macros_with_args_block_comment_between_ident_and_paren() {
        let input = b"UCLASS /* clang-format off */ (BlueprintType)";
        let mut buf = input.to_vec();
        let n = strip_macros_with_args(&mut buf, &tokens(&["UCLASS"]));
        assert_eq!(
            n, 1,
            "macro with inline comment between ident and `(` must strip"
        );
        assert_eq!(buf.len(), input.len(), "byte length preserved");
        let out = std::str::from_utf8(&buf).unwrap();
        assert!(
            !out.contains("UCLASS"),
            "UCLASS must be stripped; got: {out:?}"
        );
        assert!(
            !out.contains("BlueprintType"),
            "BlueprintType (inside the arg list) must be stripped; got: {out:?}",
        );
    }

    /// Case (i''): same but with a `//` line comment between the identifier
    /// and the `(` on the following line. The `\n` inside the line comment
    /// is consumed by `skip_lexical` (and then preserved verbatim in the
    /// fill because it's inside the stripped span).
    #[test]
    fn strip_macros_with_args_line_comment_between_ident_and_paren() {
        let input = b"UCLASS // some category annotation\n(BlueprintType)";
        let mut buf = input.to_vec();
        let n = strip_macros_with_args(&mut buf, &tokens(&["UCLASS"]));
        assert_eq!(
            n, 1,
            "macro with line comment between ident and `(` must strip"
        );
        assert_eq!(buf.len(), input.len(), "byte length preserved");
        let out = std::str::from_utf8(&buf).unwrap();
        assert!(
            !out.contains("UCLASS"),
            "UCLASS must be stripped; got: {out:?}"
        );
        assert!(
            !out.contains("BlueprintType"),
            "arg list must be stripped; got: {out:?}"
        );
        // The newline inside the line comment is INSIDE the stripped span,
        // so it survives per the `\n`-preservation invariant.
        assert!(
            out.contains('\n'),
            "newline preserved in the fill; got: {out:?}"
        );
    }

    /// Case (j): the two passes fire in sequence on disjoint-list input —
    /// `macro_strip = ["ENGINE_API"]` (whole-word) PLUS `macro_strip_with_args
    /// = ["UCLASS"]` (parameterized) applied to a fixture that exercises
    /// both shapes on the same line. Mirrors the pipeline that
    /// `CppParser::preprocess` runs at production time (pass 1 → pass 2).
    /// Asserts byte-identical length, neither token surviving as a
    /// substring, and the class/parent identifiers untouched.
    #[test]
    fn strip_macros_with_args_disjoint_lists() {
        let input = b"UCLASS(X) class ENGINE_API AActor : public UObject {}";
        let mut buf = input.to_vec();

        // Pass 1: whole-word strip of `ENGINE_API`.
        let cow = strip_macros(&buf, &["ENGINE_API".to_string()]);
        buf = cow.into_owned();
        assert_eq!(buf.len(), input.len(), "pass 1 must preserve byte length");
        assert!(
            !std::str::from_utf8(&buf).unwrap().contains("ENGINE_API"),
            "pass 1 must strip `ENGINE_API`; got: {:?}",
            std::str::from_utf8(&buf).unwrap(),
        );
        // `UCLASS(X)` still present after pass 1 — pass 1 only handles
        // bare-token identifiers.
        assert!(
            std::str::from_utf8(&buf).unwrap().contains("UCLASS(X)"),
            "pass 1 must NOT touch parameterized macros; got: {:?}",
            std::str::from_utf8(&buf).unwrap(),
        );

        // Pass 2: parameterized strip of `UCLASS(...)`.
        let n = strip_macros_with_args(&mut buf, &tokens(&["UCLASS"]));
        assert_eq!(n, 1, "pass 2 must replace exactly one occurrence");
        assert_eq!(buf.len(), input.len(), "pass 2 must preserve byte length");
        let out = std::str::from_utf8(&buf).unwrap();
        assert!(
            !out.contains("UCLASS"),
            "pass 2 must strip `UCLASS(...)`; got: {out:?}",
        );
        assert!(
            !out.contains("ENGINE_API"),
            "ENGINE_API stripping from pass 1 must survive pass 2",
        );

        // Surviving identifiers (class name + parent) must be intact —
        // both passes preserve byte offsets, so the trailing
        // `AActor : public UObject {}` sits at the same column it did
        // in the input.
        assert!(
            out.contains("AActor : public UObject {}"),
            "class declaration must survive both passes intact; got: {out:?}",
        );
    }

    /// Case (k): ASCII whitespace between the identifier and the `(` is
    /// tolerated (UE codebases sometimes write `UCLASS (BlueprintType)`).
    /// The whole span — identifier through outer `)` — is blanked,
    /// including the whitespace in between.
    #[test]
    fn strip_macros_with_args_whitespace_before_paren() {
        let input = b"UCLASS (BlueprintType)";
        let mut buf = input.to_vec();
        let n = strip_macros_with_args(&mut buf, &tokens(&["UCLASS"]));
        assert_eq!(n, 1);
        assert_eq!(buf.len(), input.len());
        assert!(
            buf.iter().all(|&b| b == b' '),
            "every byte must be space; got {:?}",
            String::from_utf8_lossy(&buf),
        );
    }

    /// Case (l): "user error → understandable failure" pin. If a user
    /// lists a macro name in `macro_strip_with_args` that collides with a
    /// real function definition in their codebase, that function's
    /// signature is blanked and the function disappears from the symbol
    /// graph. This is by design — users should choose macro names that
    /// don't collide. The test exists to prevent silent regression of
    /// this property (i.e. to catch us if we ever start trying to
    /// "protect" function definitions heuristically).
    #[test]
    fn strip_macros_with_args_user_function_named_like_macro() {
        let input = b"void UCLASS(int x) { return; }";
        let mut buf = input.to_vec();
        let n = strip_macros_with_args(&mut buf, &tokens(&["UCLASS"]));
        assert_eq!(n, 1, "the function signature is stripped");
        assert_eq!(buf.len(), input.len(), "byte length preserved");

        // Construct the expected output: bytes 5..=17 (UCLASS(int x)) are
        // blanked; everything else is verbatim. The slice math is spelled
        // out so a reader can verify by hand.
        //   v o i d   U C L A S S  (  i  n  t     x  )     {  ...
        //   0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 ...
        let mut expected = input.to_vec();
        for b in &mut expected[5..=17] {
            *b = b' ';
        }
        assert_eq!(buf, expected);

        // Sanity: the prefix `void ` and the suffix ` { return; }`
        // survive verbatim.
        assert!(buf.starts_with(b"void "));
        assert!(buf.ends_with(b" { return; }"));
    }

    /// Case (m): mixed `GENERATED_BODY()` (parameterized) and bare
    /// `GENERATED_BODY` (no parens) in the same file. The parameterized
    /// form strips; the bare form survives because the scanner requires
    /// a `(` after the identifier (modulo whitespace) before substituting.
    ///
    /// If this test ever fails against a real UE fixture — i.e. if bare
    /// `GENERATED_BODY` shows up in a position where leaving it
    /// untouched breaks parsing — Decision 4 in the UeMacroSupport
    /// design must revisit (allow the same token in BOTH `macro_strip`
    /// and `macro_strip_with_args`). Until then the test pins the
    /// "bare form survives benignly" assumption.
    #[test]
    fn strip_macros_with_args_generated_body_bare_and_parens() {
        let input = b"GENERATED_BODY()\nint x;\nGENERATED_BODY\nint y;";
        let mut buf = input.to_vec();
        let n = strip_macros_with_args(&mut buf, &tokens(&["GENERATED_BODY"]));
        assert_eq!(n, 1, "only the parameterized form is stripped");
        assert_eq!(buf.len(), input.len(), "byte length preserved");

        // Expected: first 16 bytes (GENERATED_BODY()) become spaces; the
        // rest of the input is verbatim, including the bare second
        // occurrence.
        let mut expected = input.to_vec();
        for b in &mut expected[0..16] {
            *b = b' ';
        }
        assert_eq!(buf, expected);

        // Concretely: the bare second `GENERATED_BODY` literal survives.
        let cleaned = String::from_utf8(buf).unwrap();
        assert!(
            cleaned.contains("GENERATED_BODY\nint y;"),
            "bare GENERATED_BODY must survive; got {:?}",
            cleaned,
        );
    }
}
