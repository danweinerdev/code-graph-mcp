//! Byte-level scanner for `[cpp].macro_define_function` matches.
//!
//! For each invocation `MACRO(arg0, arg1, …)` of a configured macro
//! in the C++ source, this scanner parses out the comma-separated
//! arguments and calls a user-supplied closure with `(args, line)`.
//! Used by [`CppParser::synthesize_macro_define_function_symbols`]
//! to produce synthetic `Function` symbols for the names hidden
//! behind token-pasting macros like
//! `IMPLEMENT_RELEASE_FN(MyType) → MyType_Release`.
//!
//! Scope:
//! - Whole-word match on the macro name (same convention as
//!   `macro_strip`): the byte before/after the name must NOT be an
//!   identifier-continuation character.
//! - Followed by an open-paren — the next non-whitespace byte after
//!   the macro name must be `(`. Macros that take no args (`MACRO()`)
//!   produce an empty args list and are reported with `args=[""]`.
//! - Argument parsing splits at depth-1 commas (commas inside
//!   nested `()`, `[]`, `{}`, string/char literals, or comments are
//!   not splits).
//!
//! NON-Goals:
//! - The scanner does NOT distinguish file scope from class-body /
//!   function-body scope. A macro invocation inside a method body
//!   that happens to match a configured macro name will still
//!   produce a synthetic Symbol. Users who care must avoid choosing
//!   macro names that appear inside class/function bodies for other
//!   purposes — the same caveat applies to `macro_strip`.

use crate::preprocess::{find_balanced_close, skip_lexical};

/// Call `emit(arg_text_slices, line)` for every invocation of
/// `macro_name` in `content`. `arg_text_slices` is the per-arg
/// trimmed text vec (depth-1 split on commas); `line` is the
/// 1-based source line of the macro identifier.
pub(crate) fn scan_macro_invocations<F>(content: &[u8], macro_name: &str, mut emit: F)
where
    F: FnMut(&[&str], u32),
{
    let macro_bytes = macro_name.as_bytes();
    if macro_bytes.is_empty() {
        return;
    }

    let mut i: usize = 0;
    while i < content.len() {
        // Skip lexical regions WHOLESALE before testing for a macro name.
        // A configured macro name appearing inside a line/block comment, a
        // string or character literal, or a raw string (`// MAKE_FN(x)`,
        // `/* MAKE_FN(x) */`, `"...MAKE_FN(x)..."`, `R"(MAKE_FN(x))"`) is
        // NOT a real invocation and must not synthesize a phantom symbol.
        // This reuses the same lexer `macro_strip_with_args` walks, so the
        // synthesis pass and the strip pass agree on what is "code" vs
        // "literal/comment".
        if let Some(end) = skip_lexical(content, i) {
            // `skip_lexical` returns the byte just past the region. The
            // `.max(i + 1)` is a belt-and-suspenders guard against a
            // zero-width advance (it never returns `Some(i)` for a real
            // opener) so the loop can never spin.
            i = end.max(i + 1);
            continue;
        }

        // Whole-word match of the macro name starting at `i`. We only reach
        // here on a "code" byte (outside any literal/comment), so a match
        // is a genuine source-level token.
        let end = i + macro_bytes.len();
        if end <= content.len() && &content[i..end] == macro_bytes {
            // Whole-word boundary on the LEFT.
            let left_ok = i == 0 || !is_identifier_continue(content[i - 1]);
            // Whole-word boundary on the RIGHT.
            let right_ok = end >= content.len() || !is_identifier_continue(content[end]);
            if left_ok && right_ok {
                // Skip a configured macro name that appears on its own
                // `#define` directive line. There the "invocation" is the
                // macro's DEFINITION (`#define MACRO(name) struct name {…}`),
                // and the args are the macro's formal parameters — matching
                // it would synthesize a junk symbol named after the
                // parameter (`name`), never the real type. Real invocations
                // live on later, non-`#define` lines and still match.
                if is_on_define_line(content, i) {
                    i = end;
                    continue;
                }
                // Scan forward past whitespace looking for `(`.
                if let Some(open) = skip_ws_to_paren(content, end) {
                    if let Some(close) = find_balanced_close(content, open) {
                        let args_slice = &content[open + 1..close];
                        let line = count_lines_before(content, i) + 1;
                        // A non-UTF8 args region is skipped (no emit) rather
                        // than emitting garbage; either way we advance past
                        // the close-paren below.
                        if let Ok(args_text) = std::str::from_utf8(args_slice) {
                            let parts = split_args_depth1(args_text);
                            let parts_refs: Vec<&str> = parts.iter().map(|s| s.as_str()).collect();
                            emit(&parts_refs, line);
                        }
                        // Advance past the close-paren.
                        i = close + 1;
                        continue;
                    }
                }
                // Whole-word name but not followed by `(...)` — skip past
                // the matched name.
                i = end;
                continue;
            }
            // Not a whole-word match (e.g. `MACROname` or `xMACRO`).
            // Fall through to the single-byte advance below.
        }
        i += 1;
    }
}

/// Whether `byte` is part of an identifier continuation — letter,
/// digit, or `_`. Used for the whole-word match guard.
///
/// `pub(crate)` so the byte-span scanner in
/// [`crate::macro_expand`] reuses the same boundary rule.
pub(crate) fn is_identifier_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Whether the byte at `match_pos` sits on a `#define` directive's
/// logical line — i.e. the first non-whitespace bytes of the line
/// (after honoring backslash line-continuation upward) are `#` then
/// optional whitespace then `define`.
///
/// Walks backward from `match_pos` to the start of the LOGICAL line.
/// A physical line whose preceding line ends with a backslash
/// (`\` immediately before its `\n`, modulo a trailing `\r`) is a
/// continuation, so the walk keeps going up. Once the logical-line
/// start is found, it checks for the `#`…`define` opener.
///
/// `pub(crate)` so the byte-span scanner in [`crate::macro_expand`]
/// shares the exact same define-line guard.
pub(crate) fn is_on_define_line(content: &[u8], match_pos: usize) -> bool {
    // Find the start of the logical line: walk back over physical lines,
    // continuing upward as long as the physical line above ends in `\`.
    let mut line_start = start_of_physical_line(content, match_pos);
    loop {
        if line_start == 0 {
            break;
        }
        // `line_start - 1` is the `\n` that ends the previous physical
        // line. Look at the byte(s) before it for a continuation `\`,
        // tolerating a `\r` between the `\` and the `\n` (CRLF files).
        let nl = line_start - 1;
        debug_assert!(content[nl] == b'\n');
        let mut p = nl;
        if p > 0 && content[p - 1] == b'\r' {
            p -= 1;
        }
        if p > 0 && content[p - 1] == b'\\' {
            // Previous physical line is a continuation — keep walking up.
            line_start = start_of_physical_line(content, p - 1);
        } else {
            break;
        }
    }

    // From the logical-line start, skip horizontal whitespace, require `#`,
    // skip more whitespace, then require the literal `define`.
    let mut i = line_start;
    while i < content.len() && (content[i] == b' ' || content[i] == b'\t') {
        i += 1;
    }
    if i >= content.len() || content[i] != b'#' {
        return false;
    }
    i += 1;
    while i < content.len() && (content[i] == b' ' || content[i] == b'\t') {
        i += 1;
    }
    const DEFINE: &[u8] = b"define";
    if i + DEFINE.len() > content.len() || &content[i..i + DEFINE.len()] != DEFINE {
        return false;
    }
    // Whole-word boundary on the right: `#definex` is NOT `#define`.
    let after = i + DEFINE.len();
    after >= content.len() || !is_identifier_continue(content[after])
}

/// Return the index of the first byte of the physical line containing
/// `pos` (the byte just after the preceding `\n`, or `0`).
fn start_of_physical_line(content: &[u8], pos: usize) -> usize {
    let mut i = pos;
    while i > 0 && content[i - 1] != b'\n' {
        i -= 1;
    }
    i
}

/// Scan forward from `start` past ASCII whitespace; return the
/// index of the next `(` if that's what's there, else `None`.
///
/// `pub(crate)` so the byte-span scanner in [`crate::macro_expand`]
/// reuses the same open-paren skip.
pub(crate) fn skip_ws_to_paren(content: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    while i < content.len() && content[i].is_ascii_whitespace() {
        i += 1;
    }
    if i < content.len() && content[i] == b'(' {
        Some(i)
    } else {
        None
    }
}

/// Count `\n` bytes in `content[..pos]` to derive the line number
/// of the byte at `pos` (0-based). Caller adds 1 for 1-based lines.
fn count_lines_before(content: &[u8], pos: usize) -> u32 {
    let mut n: u32 = 0;
    for &b in &content[..pos] {
        if b == b'\n' {
            n = n.saturating_add(1);
        }
    }
    n
}

/// Split `args_text` at depth-1 commas. Respects nested `()`,
/// `[]`, `{}`, double-quoted and single-quoted string literals, and
/// `//` / `/* … */` comments. Trims each part of leading/trailing
/// whitespace and returns owned `String`s so the caller doesn't
/// have to manage lifetimes across the `emit` callback.
fn split_args_depth1(args_text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let bytes = args_text.as_bytes();
    let mut i = 0usize;
    let mut paren_depth: i32 = 0;
    let mut bracket_depth: i32 = 0;
    let mut brace_depth: i32 = 0;
    // C++ template angle brackets are tracked with a heuristic
    // because `<` / `>` are ambiguous (template-open vs less-than).
    // We open the angle depth ONLY when `<` is immediately preceded
    // by an identifier-continuation character (typical of template
    // syntax: `SomeType<...>` vs binary `a < b`). We close on `>`
    // unconditionally when depth > 0. False positives (`a<b, c` with
    // no spaces) are rare in macro-argument contexts; the common
    // template-instantiation case is the dominant case.
    let mut angle_depth: i32 = 0;
    while i < bytes.len() {
        let b = bytes[i];
        // String/char/comment skipping comes first so commas inside
        // those don't split.
        match b {
            b'"' => {
                let mut j = i + 1;
                while j < bytes.len() {
                    if bytes[j] == b'\\' && j + 1 < bytes.len() {
                        j += 2;
                        continue;
                    }
                    if bytes[j] == b'"' {
                        j += 1;
                        break;
                    }
                    j += 1;
                }
                current.push_str(&args_text[i..j.min(bytes.len())]);
                i = j;
                continue;
            }
            b'\'' => {
                let mut j = i + 1;
                while j < bytes.len() {
                    if bytes[j] == b'\\' && j + 1 < bytes.len() {
                        j += 2;
                        continue;
                    }
                    if bytes[j] == b'\'' {
                        j += 1;
                        break;
                    }
                    j += 1;
                }
                current.push_str(&args_text[i..j.min(bytes.len())]);
                i = j;
                continue;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                // Line comment — skip to newline.
                let mut j = i + 2;
                while j < bytes.len() && bytes[j] != b'\n' {
                    j += 1;
                }
                current.push_str(&args_text[i..j.min(bytes.len())]);
                i = j;
                continue;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                // Block comment — skip to `*/`.
                let mut j = i + 2;
                while j + 1 < bytes.len() && !(bytes[j] == b'*' && bytes[j + 1] == b'/') {
                    j += 1;
                }
                let end = (j + 2).min(bytes.len());
                current.push_str(&args_text[i..end]);
                i = end;
                continue;
            }
            b'(' => paren_depth += 1,
            b')' => paren_depth -= 1,
            b'[' => bracket_depth += 1,
            b']' => bracket_depth -= 1,
            b'{' => brace_depth += 1,
            b'}' => brace_depth -= 1,
            b'<' => {
                // Heuristic: treat `<` as template-open only when
                // preceded by an identifier-continuation byte
                // (`SomeType<...>` opens; `a < b` doesn't).
                let prev = if i > 0 { Some(bytes[i - 1]) } else { None };
                if prev.is_some_and(is_identifier_continue) {
                    angle_depth += 1;
                }
            }
            b'>' if angle_depth > 0 => {
                angle_depth -= 1;
            }
            _ => {}
        }
        if b == b','
            && paren_depth == 0
            && bracket_depth == 0
            && brace_depth == 0
            && angle_depth == 0
        {
            out.push(current.trim().to_string());
            current.clear();
        } else {
            current.push(b as char);
        }
        i += 1;
    }
    out.push(current.trim().to_string());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect_invocations(src: &str, macro_name: &str) -> Vec<(Vec<String>, u32)> {
        let mut out: Vec<(Vec<String>, u32)> = Vec::new();
        scan_macro_invocations(src.as_bytes(), macro_name, |args, line| {
            let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            out.push((owned, line));
        });
        out
    }

    #[test]
    fn scans_simple_invocation() {
        let src = "DECLARE_RELEASE_FN(MyType)";
        let r = collect_invocations(src, "DECLARE_RELEASE_FN");
        assert_eq!(r, vec![(vec!["MyType".to_string()], 1)]);
    }

    #[test]
    fn scans_two_args() {
        let src = "DEFINE_PAIR(Alpha, Beta)";
        let r = collect_invocations(src, "DEFINE_PAIR");
        assert_eq!(r, vec![(vec!["Alpha".to_string(), "Beta".to_string()], 1)]);
    }

    #[test]
    fn whole_word_match_ignores_substring() {
        let src = "XDECLARE_RELEASE_FN(Foo) DECLARE_RELEASE_FN_BIS(Bar)";
        let r = collect_invocations(src, "DECLARE_RELEASE_FN");
        // Neither matches as a whole word.
        assert!(r.is_empty(), "got: {r:?}");
    }

    #[test]
    fn multiple_invocations_in_one_file() {
        let src = "DECLARE_RELEASE_FN(A)\nDECLARE_RELEASE_FN(B)\nDECLARE_RELEASE_FN(C)\n";
        let r = collect_invocations(src, "DECLARE_RELEASE_FN");
        assert_eq!(
            r,
            vec![
                (vec!["A".to_string()], 1),
                (vec!["B".to_string()], 2),
                (vec!["C".to_string()], 3),
            ]
        );
    }

    #[test]
    fn arg_with_template_parens_not_split() {
        let src = "DEFINE(SomeType<int, float>, OtherType)";
        let r = collect_invocations(src, "DEFINE");
        assert_eq!(
            r,
            vec![(
                vec!["SomeType<int, float>".to_string(), "OtherType".to_string()],
                1
            )]
        );
    }

    #[test]
    fn arg_with_comma_inside_string_literal_not_split() {
        let src = "DEFINE(\"a, b\", Real)";
        let r = collect_invocations(src, "DEFINE");
        assert_eq!(
            r,
            vec![(vec!["\"a, b\"".to_string(), "Real".to_string()], 1)]
        );
    }

    #[test]
    fn line_numbering_counts_newlines_before_invocation() {
        let src = "\n\n\nDECLARE(X)\n";
        let r = collect_invocations(src, "DECLARE");
        assert_eq!(r, vec![(vec!["X".to_string()], 4)]);
    }

    #[test]
    fn no_args_invocation_produces_empty_arg() {
        let src = "TRIGGER()";
        let r = collect_invocations(src, "TRIGGER");
        // Empty parens → split produces one empty-string arg slot.
        // The caller can decide whether to skip on empty.
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, vec!["".to_string()]);
    }

    #[test]
    fn macro_name_present_but_no_following_parens_skipped() {
        let src = "DECLARE_RELEASE_FN;\n"; // missing parens entirely
        let r = collect_invocations(src, "DECLARE_RELEASE_FN");
        assert!(r.is_empty());
    }

    #[test]
    fn whitespace_between_name_and_open_paren_accepted() {
        let src = "DECLARE_RELEASE_FN   (Spaced)";
        let r = collect_invocations(src, "DECLARE_RELEASE_FN");
        assert_eq!(r, vec![(vec!["Spaced".to_string()], 1)]);
    }

    // -- Lexical false positives (Finding 3) -----------------------------
    //
    // A configured macro name appearing inside a comment or a string/char/
    // raw-string literal is NOT a real invocation and must not synthesize a
    // symbol. The scanner skips lexical regions wholesale before testing a
    // candidate name.

    #[test]
    fn macro_in_line_comment_not_matched() {
        let src = "// MAKE_FN(Dead)\nint real = 0;\n";
        let r = collect_invocations(src, "MAKE_FN");
        assert!(
            r.is_empty(),
            "macro inside a // comment must not match: {r:?}"
        );
    }

    #[test]
    fn macro_in_block_comment_not_matched() {
        let src = "/* MAKE_FN(Dead) spanning\n   MAKE_FN(AlsoDead) */\nint real = 0;\n";
        let r = collect_invocations(src, "MAKE_FN");
        assert!(
            r.is_empty(),
            "macro inside a /* */ block comment must not match: {r:?}"
        );
    }

    #[test]
    fn macro_in_string_literal_not_matched() {
        let src = "const char* s = \"MAKE_FN(Dead)\";\n";
        let r = collect_invocations(src, "MAKE_FN");
        assert!(
            r.is_empty(),
            "macro inside a \"...\" string literal must not match: {r:?}"
        );
    }

    #[test]
    fn macro_in_char_literal_region_not_matched() {
        // The char literal `'('` would otherwise feed the scanner a stray
        // open-paren; skipping the literal wholesale avoids both that and a
        // name match inside a quoted region.
        let src = "char c = '('; const char* s = \"MAKE_FN(Dead)\";\n";
        let r = collect_invocations(src, "MAKE_FN");
        assert!(
            r.is_empty(),
            "macro inside a quoted region after a char literal must not match: {r:?}"
        );
    }

    #[test]
    fn macro_in_raw_string_not_matched() {
        // Raw string delimiters wrap a body that contains both the macro
        // name and balanced parens; skip_lexical jumps the whole region.
        let src = "const char* s = R\"delim(MAKE_FN(Dead))delim\";\n";
        let r = collect_invocations(src, "MAKE_FN");
        assert!(
            r.is_empty(),
            "macro inside a raw string must not match: {r:?}"
        );
    }

    #[test]
    fn real_invocation_after_comment_still_matched() {
        // Positive control: skipping lexical regions must NOT swallow a
        // genuine invocation that follows a comment/string on the same or
        // next line.
        let src = "// MAKE_FN(Dead) is documentation\nMAKE_FN(Live)\n";
        let r = collect_invocations(src, "MAKE_FN");
        assert_eq!(
            r,
            vec![(vec!["Live".to_string()], 2)],
            "the real invocation on line 2 must still be found (and only it)"
        );
    }

    #[test]
    fn real_invocation_with_string_arg_still_matched() {
        // Positive control: a real invocation whose ARGUMENT is a string
        // containing the macro name must match exactly once — the argument
        // string is part of the balanced-paren payload, not a separate
        // candidate.
        let src = "MAKE_FN(Live)\n";
        let r = collect_invocations(src, "MAKE_FN");
        assert_eq!(r, vec![(vec!["Live".to_string()], 1)]);
    }

    // -- #define-line false positive (Deliverable 1) ---------------------
    //
    // A configured macro name appearing on its OWN `#define` directive line
    // is the macro's DEFINITION, not an invocation. Matching it synthesizes
    // a junk symbol named after the macro PARAMETER. The scanner must skip
    // the define line while still matching real invocations on later lines.

    #[test]
    fn define_line_is_not_an_invocation() {
        // The macro's own `#define` must NOT produce an invocation: the
        // arg `x` is the formal parameter, not a real type.
        let src = "#define MAKE_FN(x) void x##_Release() {}\n";
        let r = collect_invocations(src, "MAKE_FN");
        assert!(
            r.is_empty(),
            "the macro's own #define line must not match as an invocation: {r:?}"
        );
    }

    #[test]
    fn define_line_skipped_but_real_invocation_after_still_matched() {
        // Sentinel: the real invocation on a later line is found and the
        // #define line above is skipped.
        let src = "#define MAKE_FN(x) void x##_Release() {}\nMAKE_FN(Foo)\n";
        let r = collect_invocations(src, "MAKE_FN");
        assert_eq!(
            r,
            vec![(vec!["Foo".to_string()], 2)],
            "only the real invocation on line 2 must match, never the #define"
        );
    }

    #[test]
    fn define_line_with_leading_whitespace_skipped() {
        // `#` may be indented (`   # define`), and whitespace may sit
        // between `#` and `define`.
        let src = "   #  define MAKE_FN(x) x\nMAKE_FN(Bar)\n";
        let r = collect_invocations(src, "MAKE_FN");
        assert_eq!(r, vec![(vec!["Bar".to_string()], 2)]);
    }

    #[test]
    fn define_line_with_backslash_continuation_skipped() {
        // A multi-line `#define` continued with a trailing `\`. The macro
        // name on the SECOND physical line is still on the directive's
        // logical line and must be skipped. The real invocation on the
        // following line still matches.
        let src = "#define MAKE_FN(x) \\\n    void MAKE_FN(x)\nMAKE_FN(Baz)\n";
        let r = collect_invocations(src, "MAKE_FN");
        assert_eq!(
            r,
            vec![(vec!["Baz".to_string()], 3)],
            "continuation lines of a #define must be skipped; only the real \
             invocation on line 3 matches"
        );
    }

    #[test]
    fn define_line_crlf_continuation_skipped() {
        // CRLF line endings: the continuation `\` sits before `\r\n`.
        let src = "#define MAKE_FN(x) \\\r\n    MAKE_FN(x)\r\nMAKE_FN(Qux)\r\n";
        let r = collect_invocations(src, "MAKE_FN");
        assert_eq!(
            r,
            vec![(vec!["Qux".to_string()], 3)],
            "CRLF continuation of a #define must be skipped"
        );
    }

    #[test]
    fn definex_word_is_not_a_define_directive() {
        // `#definex` is not the `#define` directive; an invocation on such
        // a line (contrived) is NOT skipped. Guards the whole-word check.
        let src = "#definex MAKE_FN(Real)\n";
        let r = collect_invocations(src, "MAKE_FN");
        assert_eq!(
            r,
            vec![(vec!["Real".to_string()], 1)],
            "`#definex` is not the #define directive, so the invocation matches"
        );
    }

    #[test]
    fn is_on_define_line_direct() {
        // Direct unit coverage of the helper at the macro-name offset.
        let src = b"#define MAKE_FN(x) x";
        let pos = src.windows(7).position(|w| w == b"MAKE_FN").unwrap();
        assert!(is_on_define_line(src, pos));

        let src2 = b"  MAKE_FN(Foo)";
        let pos2 = src2.windows(7).position(|w| w == b"MAKE_FN").unwrap();
        assert!(!is_on_define_line(src2, pos2));
    }
}
