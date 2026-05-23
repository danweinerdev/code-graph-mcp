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

use crate::preprocess::find_balanced_close;

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
        // Look for the macro name as a whole word.
        if let Some(rel) = find_subslice(&content[i..], macro_bytes) {
            let pos = i + rel;
            // Whole-word boundary on the LEFT.
            let left_ok = pos == 0 || !is_identifier_continue(content[pos - 1]);
            let after = pos + macro_bytes.len();
            // Whole-word boundary on the RIGHT.
            let right_ok = after >= content.len() || !is_identifier_continue(content[after]);
            if left_ok && right_ok {
                // Scan forward past whitespace looking for `(`.
                let paren_pos = skip_ws_to_paren(content, after);
                if let Some(open) = paren_pos {
                    if let Some(close) = find_balanced_close(content, open) {
                        let args_slice = &content[open + 1..close];
                        let line = count_lines_before(content, pos) + 1;
                        let args_text = match std::str::from_utf8(args_slice) {
                            Ok(s) => s,
                            Err(_) => {
                                // Non-UTF8 args region — skip this
                                // invocation rather than emit garbage.
                                i = close + 1;
                                continue;
                            }
                        };
                        let parts = split_args_depth1(args_text);
                        let parts_refs: Vec<&str> = parts.iter().map(|s| s.as_str()).collect();
                        emit(&parts_refs, line);
                        // Advance past the close-paren.
                        i = close + 1;
                        continue;
                    }
                }
                // Not followed by parens — skip past the matched name.
                i = after;
                continue;
            }
            // Not a whole-word match (e.g. `MACROname` or `xMACRO`).
            // Advance past the offending byte to avoid infinite loop.
            i = pos + 1;
        } else {
            break;
        }
    }
}

/// Byte-level substring search. Returns the offset of `needle`
/// inside `haystack`, or `None` if absent. Standalone (rather than
/// using the `bytes` crate or `memchr`) so the cpp crate stays
/// dependency-slim.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let last = haystack.len() - needle.len();
    for i in 0..=last {
        if &haystack[i..i + needle.len()] == needle {
            return Some(i);
        }
    }
    None
}

/// Whether `byte` is part of an identifier continuation — letter,
/// digit, or `_`. Used for the whole-word match guard.
fn is_identifier_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Scan forward from `start` past ASCII whitespace; return the
/// index of the next `(` if that's what's there, else `None`.
fn skip_ws_to_paren(content: &[u8], start: usize) -> Option<usize> {
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
}
