//! Byte-preserving expansion for `[cpp].macro_define_type` matches.
//!
//! tree-sitter does NOT expand macros, so a struct/class definition hidden
//! inside a macro invocation is invisible — the parser sees the macro
//! identifier, not the type it expands to. This module rewrites a configured
//! macro invocation IN PLACE into the real C++ the macro would produce, so
//! tree-sitter parses the type natively and the existing extraction loops
//! recover the type symbol AND its members (methods, nested types) AND its
//! inheritance/call edges.
//!
//! Example. With `macro_define_type = [{ name = "EXPORT_STRUCT" }]`:
//! ```text
//! EXPORT_STRUCT(Foo, (
//!     int32_t bar;
//!     void method();
//! ));
//! ```
//! is rewritten to (byte-for-byte the same length, every `\n` at its original
//! offset):
//! ```text
//! struct        Foo {
//!     int32_t bar;
//!     void method();
//!   } ;
//! ```
//! which tree-sitter parses as `struct Foo { int32_t bar; void method(); };`.
//!
//! # Byte-preservation contract (load-bearing)
//!
//! The output buffer is the SAME LENGTH as the input and preserves every `\n`
//! at its original byte offset. tree-sitter positions index into the
//! preprocessed buffer and are reported as on-disk file positions, so any
//! length or newline drift would corrupt every downstream symbol's
//! line/column. We only ever overwrite bytes IN PLACE with the keyword text,
//! spaces, `{`, or `}`. We never insert or delete bytes. The
//! [`blank_range`] helper preserves `\n` exactly like
//! `strip_macros_with_args` does.
//!
//! # NON-goals (documented limitations)
//!
//! - **Body-vs-namespace scope is not discriminated.** A configured macro
//!   name invoked anywhere — file scope, inside a function body, inside a
//!   namespace — is expanded. Same caveat as `macro_strip` /
//!   `macro_define_function`: choose macro names that don't collide with
//!   other uses.
//! - **Keyword must FIT in the macro-name span.** The keyword (`struct`/
//!   `class`) is written over the leading bytes of the macro name; if it's
//!   longer than the macro name we cannot expand without changing length, so
//!   the invocation is skipped with a warning. Engine macro names are long
//!   in practice (`EXPORT_STRUCT` = 13 ≥ `struct` = 6), so this normally
//!   passes.
//! - **Raw-string-delimiter collision.** Shared with `macro_strip`: a raw
//!   string whose tag equals a configured macro name can be mis-scanned.
//!   The scanner skips lexical regions via `skip_lexical`, so an invocation
//!   inside a string/comment is never expanded, but the same delimiter-
//!   collision edge applies.

use code_graph_core::MacroDefineType;

use crate::macro_define::{is_identifier_continue, is_on_define_line, skip_ws_to_paren};
use crate::preprocess::{find_balanced_close, skip_lexical};

/// One scanned macro invocation, expressed as byte spans into the source.
///
/// All indices are byte offsets into the `content` slice passed to
/// [`scan_macro_invocation_spans`]. Half-open spans are `[start, end)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InvocationSpans {
    /// `[start, end)` of the macro name identifier.
    pub name: (usize, usize),
    /// Index of the open paren `(` immediately following the name (modulo
    /// whitespace).
    pub open_paren: usize,
    /// Per-argument byte spans `[start, end)` within `content`, split on
    /// top-level (depth-1) commas. Spans are NOT trimmed — they cover the
    /// raw argument bytes between commas (including surrounding
    /// whitespace). An empty `()` yields one zero-width span.
    pub args: Vec<(usize, usize)>,
    /// Index of the matching close paren `)`.
    pub close_paren: usize,
}

/// Call `emit(spans)` for every invocation of `macro_name` in `content`,
/// yielding byte spans (not text). The span analogue of
/// [`crate::macro_define::scan_macro_invocations`]: it shares the same
/// lexical-region skipping, whole-word boundary rule, `#define`-line guard,
/// and depth-1 comma splitting — but reports OFFSETS so the caller can
/// rewrite bytes in place.
pub(crate) fn scan_macro_invocation_spans<F>(content: &[u8], macro_name: &str, mut emit: F)
where
    F: FnMut(InvocationSpans),
{
    let macro_bytes = macro_name.as_bytes();
    if macro_bytes.is_empty() {
        return;
    }

    let mut i: usize = 0;
    while i < content.len() {
        // Skip lexical regions wholesale (comments, strings, raw strings)
        // so a macro name inside a literal/comment is never expanded.
        if let Some(end) = skip_lexical(content, i) {
            i = end.max(i + 1);
            continue;
        }

        let end = i + macro_bytes.len();
        if end <= content.len() && &content[i..end] == macro_bytes {
            let left_ok = i == 0 || !is_identifier_continue(content[i - 1]);
            let right_ok = end >= content.len() || !is_identifier_continue(content[end]);
            if left_ok && right_ok {
                // Skip the macro's own `#define` directive line.
                if is_on_define_line(content, i) {
                    i = end;
                    continue;
                }
                if let Some(open) = skip_ws_to_paren(content, end) {
                    if let Some(close) = find_balanced_close(content, open) {
                        let args = split_arg_spans(content, open + 1, close);
                        emit(InvocationSpans {
                            name: (i, end),
                            open_paren: open,
                            args,
                            close_paren: close,
                        });
                        i = close + 1;
                        continue;
                    }
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
}

/// Split the argument region `content[start..close)` at depth-1 commas,
/// returning the byte span of each argument. Mirrors
/// [`crate::macro_define`]'s `split_args_depth1` (nested `()`/`[]`/`{}`,
/// string/char literals, comments, and a template `<...>` heuristic suppress
/// splits) but tracks OFFSETS instead of copying text.
fn split_arg_spans(content: &[u8], start: usize, close: usize) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut seg_start = start;
    let mut i = start;
    let mut paren_depth: i32 = 0;
    let mut bracket_depth: i32 = 0;
    let mut brace_depth: i32 = 0;
    let mut angle_depth: i32 = 0;

    while i < close {
        // Skip lexical regions (strings/chars/comments/raw strings) so
        // commas inside them don't split. `skip_lexical` returns the byte
        // past the region; clamp to `close` so we never overrun.
        if let Some(lex_end) = skip_lexical(content, i) {
            i = lex_end.max(i + 1).min(close);
            continue;
        }
        match content[i] {
            b'(' => paren_depth += 1,
            b')' => paren_depth -= 1,
            b'[' => bracket_depth += 1,
            b']' => bracket_depth -= 1,
            b'{' => brace_depth += 1,
            b'}' => brace_depth -= 1,
            b'<' => {
                let prev = if i > start {
                    Some(content[i - 1])
                } else {
                    None
                };
                if prev.is_some_and(is_identifier_continue) {
                    angle_depth += 1;
                }
            }
            b'>' if angle_depth > 0 => angle_depth -= 1,
            b',' if paren_depth == 0
                && bracket_depth == 0
                && brace_depth == 0
                && angle_depth == 0 =>
            {
                out.push((seg_start, i));
                seg_start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push((seg_start, close));
    out
}

/// Overwrite `content[start..end)` with spaces, leaving `\n` bytes in place
/// so line offsets after the span stay aligned. Mirrors the newline-
/// preserving fill in `strip_macros_with_args`. A no-op when `start >= end`.
fn blank_range(content: &mut [u8], start: usize, end: usize) {
    if start >= end {
        return;
    }
    for b in &mut content[start..end] {
        if *b != b'\n' {
            *b = b' ';
        }
    }
}

/// Trim a span to its inner non-whitespace bounds. Returns `(s, e)` with
/// `s >= start`, `e <= end`, and both ends on non-whitespace (or `s == e`
/// for an all-whitespace span). Used to find the "trimmed text" of the body
/// argument for the paren-wrapping check.
fn trimmed_bounds(content: &[u8], start: usize, end: usize) -> (usize, usize) {
    let mut s = start;
    let mut e = end;
    while s < e && (content[s] as char).is_ascii_whitespace() {
        s += 1;
    }
    while e > s && (content[e - 1] as char).is_ascii_whitespace() {
        e -= 1;
    }
    (s, e)
}

/// Expand every configured `macro_define_type` invocation in `content`,
/// returning an owned, same-length buffer with the macros rewritten into
/// native C++. `path` is used only for warning messages.
///
/// Returns `None` when there is nothing to do (every entry has no match, or
/// the entry list is empty) so the caller can keep a borrowed fast path.
/// Returns `Some(buf)` (an owned copy with rewrites applied) when at least
/// one invocation of a configured macro was found — even if some individual
/// invocations were skipped (out-of-range arg index, keyword doesn't fit).
pub(crate) fn expand_macro_define_types(
    content: &[u8],
    entries: &[MacroDefineType],
    path: &str,
) -> Option<Vec<u8>> {
    if entries.is_empty() {
        return None;
    }

    // Collect all rewrite operations against the ORIGINAL bytes first, so
    // overlapping scans never read already-blanked bytes; then apply them to
    // a fresh owned copy.
    let mut ops: Vec<RewriteOp> = Vec::new();

    for entry in entries {
        if entry.name.is_empty() {
            // Defensive: config-load rejects empties, but never trust.
            continue;
        }
        // Collect this entry's invocation spans first, then plan rewrites.
        // Collecting up front avoids a closure that would have to capture
        // both `entry` and `&mut ops` at once.
        let mut spans: Vec<InvocationSpans> = Vec::new();
        scan_macro_invocation_spans(content, &entry.name, |s| spans.push(s));
        for span in &spans {
            if let Some(op) = plan_rewrite(content, entry, span, path) {
                ops.push(op);
            }
        }
    }

    if ops.is_empty() {
        // No configured macro matched anywhere. Borrowed fast path upstream.
        return None;
    }

    let mut buf = content.to_vec();
    for op in ops {
        op.apply(&mut buf);
    }
    Some(buf)
}

/// A planned, byte-preserving rewrite of one macro invocation. Stores
/// absolute byte indices so it can be applied to a copy of the source.
struct RewriteOp {
    /// `(name_start, keyword_bytes)` — write `keyword` over the leading
    /// bytes of the macro name.
    keyword_write: (usize, Vec<u8>),
    /// Ranges to blank to spaces (newline-preserving), in `[start, end)`.
    blanks: Vec<(usize, usize)>,
    /// Single-byte position to set to `{`.
    open_brace: usize,
    /// Single-byte position to set to `}`.
    close_brace: usize,
}

impl RewriteOp {
    fn apply(&self, buf: &mut [u8]) {
        let (kw_start, ref kw) = self.keyword_write;
        buf[kw_start..kw_start + kw.len()].copy_from_slice(kw);
        for &(s, e) in &self.blanks {
            blank_range(buf, s, e);
        }
        buf[self.open_brace] = b'{';
        buf[self.close_brace] = b'}';
    }
}

/// Plan the in-place rewrite for one invocation. Returns `None` (with an
/// `eprintln!` warning) when the invocation can't be expanded — out-of-range
/// arg index, empty name argument, or the keyword doesn't fit in the
/// macro-name span.
fn plan_rewrite(
    content: &[u8],
    entry: &MacroDefineType,
    spans: &InvocationSpans,
    path: &str,
) -> Option<RewriteOp> {
    let (n_start, n_end) = spans.name;
    let keyword = entry.keyword.as_bytes();

    // (3) FIT CHECK: the keyword must fit in the macro-name span. We can't
    // expand without changing length otherwise.
    if keyword.len() > n_end - n_start {
        eprintln!(
            "code-graph-mcp: [cpp].macro_define_type '{}' in {}: keyword \"{}\" ({} bytes) does not \
             fit the macro name ({} bytes); skipping this invocation",
            entry.name,
            path,
            entry.keyword,
            keyword.len(),
            n_end - n_start
        );
        return None;
    }

    // (2) Resolve name_arg / body_arg. body_arg defaults to the last arg.
    let nargs = spans.args.len();
    let name_idx = entry.name_arg;
    let body_idx = entry.body_arg.unwrap_or(nargs.saturating_sub(1));
    let (Some(&name_span), Some(&body_span)) = (spans.args.get(name_idx), spans.args.get(body_idx))
    else {
        eprintln!(
            "code-graph-mcp: [cpp].macro_define_type '{}' in {}: name_arg={} / body_arg={} out of \
             range (invocation has {} arg(s)); skipping",
            entry.name, path, name_idx, body_idx, nargs
        );
        return None;
    };

    // The type name is the trimmed inner of name_span. If it's empty
    // (e.g. `MACRO( , (...))`), there is no type to extract — skip.
    let (type_name_start, type_name_end) = trimmed_bounds(content, name_span.0, name_span.1);
    if type_name_start >= type_name_end {
        eprintln!(
            "code-graph-mcp: [cpp].macro_define_type '{}' in {}: name argument is empty; skipping",
            entry.name, path
        );
        return None;
    }

    // The body argument's trimmed bounds. If the trimmed body is wrapped in
    // a single pair of `(...)`, we blank that wrapping `(` and its matching
    // `)` to spaces (the members inside are already valid C++ member
    // declarations). Otherwise the body is used as-is.
    let (body_inner_start, body_inner_end) = trimmed_bounds(content, body_span.0, body_span.1);
    let body_is_paren_wrapped = body_inner_start < body_inner_end
        && content[body_inner_start] == b'('
        && matching_close_is_body_end(content, body_inner_start, body_inner_end);

    // Inner body content range + the optional wrapping `)` to blank.
    let (content_start, content_end, wrap_close) = if body_is_paren_wrapped {
        let close = body_inner_end - 1; // the matching ')'
        (body_inner_start + 1, close, Some(close))
    } else {
        (body_inner_start, body_inner_end, None)
    };

    let mut blanks: Vec<(usize, usize)> = Vec::new();

    // (4a) Keyword goes at [n_start, n_start + keyword.len()); blank the rest
    // of the macro name.
    blanks.push((n_start + keyword.len(), n_end));

    // (4d) The opening brace slot is the single byte immediately before the
    // body's content. We blank everything from the open paren up to (but not
    // including) that slot — covering the open paren, the type name's
    // trailing whitespace, the comma separating name_arg from body_arg, and
    // the optional wrapping `(` — EXCEPT the type-name bytes themselves,
    // which we leave untouched so the name floats right after the keyword.
    debug_assert!(content_start > type_name_end);
    let brace_slot = content_start - 1;
    // Blank [open_paren, type_name_start) and [type_name_end, brace_slot),
    // leaving the type-name bytes (4c) in place.
    blanks.push((spans.open_paren, type_name_start));
    blanks.push((type_name_end, brace_slot));

    // (4e) Blank the wrapping `)` if the body was paren-wrapped.
    if let Some(wc) = wrap_close {
        blanks.push((wc, wc + 1));
    }

    // (4f) Blank everything between the inner body content and the macro's
    // close paren (whitespace/newlines preserved by blank_range); the macro
    // close paren itself becomes `}`. Any trailing `;` after it is left as-is.
    let close_brace = spans.close_paren;
    if content_end < close_brace {
        blanks.push((content_end, close_brace));
    }

    Some(RewriteOp {
        keyword_write: (n_start, keyword.to_vec()),
        blanks,
        open_brace: brace_slot,
        close_brace,
    })
}

/// Whether the `(` at `open` has its matching `)` exactly at `end - 1`
/// (i.e. the trimmed body is a single balanced parenthesized group, not
/// `(a) (b)` or `(a) + b`). Used to decide whether to unwrap the body.
fn matching_close_is_body_end(content: &[u8], open: usize, end: usize) -> bool {
    debug_assert!(content[open] == b'(');
    let mut depth: i32 = 0;
    let mut i = open;
    while i < end {
        if let Some(lex_end) = skip_lexical(content, i) {
            i = lex_end.max(i + 1).min(end);
            continue;
        }
        match content[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return i == end - 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_graph_core::{FileGraph, SymbolKind};
    use code_graph_lang::LanguagePlugin;
    use std::path::Path;

    fn entry(name: &str) -> MacroDefineType {
        MacroDefineType {
            name: name.to_string(),
            name_arg: 0,
            body_arg: None,
            keyword: "struct".to_string(),
        }
    }

    /// Assert byte-length and newline-offset preservation for a rewrite.
    fn assert_preserves_layout(input: &[u8], output: &[u8]) {
        assert_eq!(input.len(), output.len(), "byte length must be preserved");
        let in_nl: Vec<usize> = input
            .iter()
            .enumerate()
            .filter(|(_, &b)| b == b'\n')
            .map(|(i, _)| i)
            .collect();
        let out_nl: Vec<usize> = output
            .iter()
            .enumerate()
            .filter(|(_, &b)| b == b'\n')
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            in_nl, out_nl,
            "every '\\n' must stay at its original offset"
        );
    }

    fn parse(src: &[u8]) -> FileGraph {
        let p = crate::CppParser::new().expect("CppParser::new");
        p.parse_file(Path::new("/test.cpp"), src)
            .expect("parse_file")
    }

    fn find<'a>(fg: &'a FileGraph, name: &str) -> Option<&'a code_graph_core::Symbol> {
        fg.symbols.iter().find(|s| s.name == name)
    }

    // -- span scanner ----------------------------------------------------

    #[test]
    fn span_scanner_basic() {
        let src = b"EXPORT_STRUCT(Foo, (int x;))";
        let mut spans: Vec<InvocationSpans> = Vec::new();
        scan_macro_invocation_spans(src, "EXPORT_STRUCT", |s| spans.push(s));
        assert_eq!(spans.len(), 1);
        let s = &spans[0];
        assert_eq!(&src[s.name.0..s.name.1], b"EXPORT_STRUCT");
        assert_eq!(src[s.open_paren], b'(');
        assert_eq!(src[s.close_paren], b')');
        assert_eq!(s.args.len(), 2);
        assert_eq!(&src[s.args[0].0..s.args[0].1], b"Foo");
    }

    #[test]
    fn span_scanner_skips_define_line() {
        let src = b"#define EXPORT_STRUCT(name, ...) struct name { __VA_ARGS__ }\nEXPORT_STRUCT(Bar, (int y;))";
        let mut spans: Vec<InvocationSpans> = Vec::new();
        scan_macro_invocation_spans(src, "EXPORT_STRUCT", |s| spans.push(s));
        assert_eq!(spans.len(), 1, "only the real invocation, not the #define");
        let s = &spans[0];
        assert_eq!(&src[s.args[0].0..s.args[0].1], b"Bar");
    }

    // -- (a)/(b) length + newline preservation ---------------------------

    #[test]
    fn expand_preserves_length_and_newlines() {
        let src = b"EXPORT_STRUCT(Foo, (\n    int bar;\n));\n";
        let out = expand_macro_define_types(src, &[entry("EXPORT_STRUCT")], "/t.cpp")
            .expect("expansion applied");
        assert_preserves_layout(src, &out);
    }

    // -- (c) rewritten bytes parse as a real struct ----------------------

    #[test]
    fn expand_paren_wrapped_body_parses_struct_with_members() {
        // The member method carries a BODY (`{}`) — the C++ plugin extracts
        // only `function_definition`s (with body), never bare declarations
        // (limitation #5), so an in-body method must be defined inline to
        // surface as a Symbol.
        let src = b"EXPORT_STRUCT(Foo, (\n    int bar;\n    void method() {}\n));\n";
        let out = expand_macro_define_types(src, &[entry("EXPORT_STRUCT")], "/t.cpp")
            .expect("expansion applied");
        assert_preserves_layout(src, &out);
        let text = String::from_utf8(out.clone()).unwrap();
        assert!(
            text.contains("struct")
                && text.contains("Foo")
                && text.contains('{')
                && text.contains('}'),
            "rewritten text must be a struct: {text:?}"
        );
        let fg = parse(&out);
        let foo = find(&fg, "Foo").expect("Foo struct symbol");
        assert_eq!(foo.kind, SymbolKind::Struct);
        let method = find(&fg, "method").expect("method symbol");
        assert_eq!(method.kind, SymbolKind::Method);
        assert_eq!(method.parent, "Foo", "method parent must be Foo");
    }

    // -- (f) bare (non-paren-wrapped) body works -------------------------

    #[test]
    fn expand_bare_body_parses_struct() {
        // Body arg is NOT wrapped in parens. The member `run` carries a body
        // so it surfaces as a Method (declarations are not extracted).
        let src = b"EXPORT_STRUCT(Bar, int x; void run() {})\n";
        let out = expand_macro_define_types(src, &[entry("EXPORT_STRUCT")], "/t.cpp")
            .expect("expansion applied");
        assert_preserves_layout(src, &out);
        let fg = parse(&out);
        let bar = find(&fg, "Bar").expect("Bar struct symbol");
        assert_eq!(bar.kind, SymbolKind::Struct);
        let run = find(&fg, "run").expect("run method symbol");
        assert_eq!(run.parent, "Bar");
    }

    // -- class keyword + inheritance -------------------------------------

    #[test]
    fn expand_class_keyword_with_inheritance() {
        let mut e = entry("EXPORT_CLASS");
        e.keyword = "class".to_string();
        // name arg includes a base-class clause so inheritance is recovered.
        let src = b"EXPORT_CLASS(Derived : public Base, (\n    void m();\n));\n";
        let out = expand_macro_define_types(src, &[e], "/t.cpp").expect("expansion applied");
        assert_preserves_layout(src, &out);
        let fg = parse(&out);
        let d = find(&fg, "Derived").expect("Derived class symbol");
        assert_eq!(d.kind, SymbolKind::Class);
        assert!(
            fg.edges.iter().any(|edge| {
                edge.kind == code_graph_core::EdgeKind::Inherits
                    && edge.from == "Derived"
                    && edge.to == "Base"
            }),
            "expected Derived -> Base inherits edge; edges: {:?}",
            fg.edges
        );
    }

    // -- (d) too-short keyword → buffer unchanged + no panic -------------

    #[test]
    fn keyword_too_long_for_macro_name_skips_invocation() {
        // Macro name `ES` (2 bytes) is shorter than `struct` (6 bytes).
        let src = b"ES(Foo, (int x;))\n";
        let out = expand_macro_define_types(src, &[entry("ES")], "/t.cpp");
        // The only invocation can't be expanded → no ops → None (borrowed
        // fast path upstream), i.e. buffer is left unchanged.
        assert!(
            out.is_none(),
            "too-short keyword must skip the invocation, leaving nothing to expand"
        );
    }

    // -- (e) #define line not expanded -----------------------------------

    #[test]
    fn define_line_not_expanded() {
        let src = b"#define EXPORT_STRUCT(name, ...) struct name { __VA_ARGS__ }\n";
        let out = expand_macro_define_types(src, &[entry("EXPORT_STRUCT")], "/t.cpp");
        assert!(out.is_none(), "the #define line alone must not be expanded");
    }

    // -- (g) malformed / unbalanced input → no panic, no corruption ------

    #[test]
    fn unbalanced_parens_no_panic_no_expansion() {
        let src = b"EXPORT_STRUCT(Foo, (int x;\n"; // missing close parens
        let out = expand_macro_define_types(src, &[entry("EXPORT_STRUCT")], "/t.cpp");
        // find_balanced_close bails → no invocation → None.
        assert!(out.is_none());
    }

    #[test]
    fn out_of_range_args_skipped_no_panic() {
        // name_arg=5 is out of range for a 2-arg invocation.
        let mut e = entry("EXPORT_STRUCT");
        e.name_arg = 5;
        let src = b"EXPORT_STRUCT(Foo, (int x;))\n";
        let out = expand_macro_define_types(src, &[e], "/t.cpp");
        assert!(
            out.is_none(),
            "out-of-range name_arg skips the only invocation"
        );
    }

    #[test]
    fn empty_entry_list_is_none() {
        let src = b"EXPORT_STRUCT(Foo, (int x;))\n";
        assert!(expand_macro_define_types(src, &[], "/t.cpp").is_none());
    }

    #[test]
    fn macro_inside_comment_not_expanded() {
        let src = b"// EXPORT_STRUCT(Foo, (int x;))\nvoid real() {}\n";
        let out = expand_macro_define_types(src, &[entry("EXPORT_STRUCT")], "/t.cpp");
        assert!(out.is_none(), "a macro inside a comment must not expand");
    }

    #[test]
    fn explicit_body_arg_index_works() {
        // Three args: name, junk, body. body_arg=2. Macro name must be long
        // enough for `struct` (6 bytes) to fit in the FIT CHECK.
        let mut e = entry("DEFINE_THREE");
        e.body_arg = Some(2);
        let src = b"DEFINE_THREE(Foo, ignored, (int z; void zz() {}))\n";
        let out = expand_macro_define_types(src, &[e], "/t.cpp").expect("expansion applied");
        assert_preserves_layout(src, &out);
        let fg = parse(&out);
        assert!(
            find(&fg, "Foo").is_some(),
            "Foo must extract with explicit body_arg"
        );
        let zz = find(&fg, "zz").expect("zz method from the body_arg=2 body");
        assert_eq!(zz.parent, "Foo");
    }

    // -- parser-reported position accuracy (the point of byte preservation) ---
    //
    // The byte-preservation contract claims downstream symbol line/column never
    // drift after a rewrite. The cases above only assert `\n`-offset
    // preservation on the rewritten BUFFER; these parse the rewrite and assert
    // the positions tree-sitter actually reports. Mirrors the spirit of
    // `preprocess.rs` case (k).

    #[test]
    fn multiline_expansion_keeps_downstream_symbol_positions() {
        // A multi-line invocation followed by a free function on a known line.
        // The body lines between the macro and the function must not shift the
        // function's reported line, and the struct must stay on line 1.
        //
        // NB: the invocation is terminated with `;` (the natural form — every
        // engine `EXPORT_STRUCT(...)` ends in a semicolon). Without it, C++
        // grammar reads `struct Foo {...} after()` as a single declaration
        // whose declarator is `after` (a function returning `struct Foo`),
        // collapsing `after` onto the struct's line. That is a C++ parsing
        // subtlety, NOT byte-offset drift — the `;` closes the struct
        // declaration so the function is a separate top-level definition.
        let src = b"EXPORT_STRUCT(Foo, (\n    int x;\n));\nvoid after() {}\n";
        let out = expand_macro_define_types(src, &[entry("EXPORT_STRUCT")], "/t.cpp")
            .expect("expansion applied");
        assert_preserves_layout(src, &out);
        let fg = parse(&out);

        // `after` is on line 4 (1-based): macro line 1, body lines 2-3, `});`
        // closes on line 3, `void after()` on line 4.
        let after = find(&fg, "after").expect("after function symbol");
        assert_eq!(after.line, 4, "trailing function line drifted");

        // The `Foo` struct stays on line 1 (the invocation line). tree-sitter
        // anchors the `struct_specifier` at the injected `struct` keyword, which
        // is written over the macro name's leading bytes at column 0.
        let foo = find(&fg, "Foo").expect("Foo struct symbol");
        assert_eq!(foo.kind, SymbolKind::Struct);
        assert_eq!(foo.line, 1, "struct line drifted off the invocation line");
        assert_eq!(foo.column, 0, "struct column not at the keyword start");
    }

    #[test]
    fn two_multiline_invocations_second_line_correct() {
        // Two multi-line invocations of the SAME macro in one file. Both must
        // extract, and the SECOND type's reported line must be correct —
        // proving the first rewrite did not shift the second's byte offsets
        // (hence line numbers). The function-side analogue lives in the
        // `macro_define_function` suite; this is the type-side version.
        //
        // Each invocation is `;`-terminated (see the position-accuracy test
        // above for why): otherwise C++ grammar folds the second `struct` into
        // the first as a declarator and `Bar` never surfaces as its own type.
        let src = b"EXPORT_STRUCT(Foo, (\n  int a;\n));\nEXPORT_STRUCT(Bar, (\n  int b;\n));\n";
        let out = expand_macro_define_types(src, &[entry("EXPORT_STRUCT")], "/t.cpp")
            .expect("expansion applied");
        assert_preserves_layout(src, &out);
        let fg = parse(&out);

        let foo = find(&fg, "Foo").expect("first struct Foo");
        assert_eq!(foo.kind, SymbolKind::Struct);
        assert_eq!(foo.line, 1);

        let bar = find(&fg, "Bar").expect("second struct Bar");
        assert_eq!(bar.kind, SymbolKind::Struct);
        // `Bar` starts on line 4 (after Foo's three lines: 1, 2, 3).
        assert_eq!(
            bar.line, 4,
            "second invocation line shifted by the first rewrite"
        );
    }

    // -- call-edge recovery from inside an expanded body ----------------------

    #[test]
    fn call_edge_recovered_from_expanded_body() {
        // A method inside the expanded struct body calls a free function in the
        // same file. We advertise "call edges recovered"; assert the `Calls`
        // edge actually lands in the parsed FileGraph.
        let src = b"void helper() {}\nEXPORT_STRUCT(Foo, ( void m() { helper(); } ))\n";
        let out = expand_macro_define_types(src, &[entry("EXPORT_STRUCT")], "/t.cpp")
            .expect("expansion applied");
        assert_preserves_layout(src, &out);
        let fg = parse(&out);

        // Sentinel: the method extracts under the revealed struct.
        let m = find(&fg, "m").expect("method m must extract under Foo");
        assert_eq!(m.parent, "Foo");

        let has_call = fg
            .edges
            .iter()
            .any(|e| e.kind == code_graph_core::EdgeKind::Calls && e.to == "helper");
        assert!(
            has_call,
            "expected a Calls edge to `helper` from the expanded body; edges: {:?}",
            fg.edges
        );
    }

    // -- exact-fit keyword boundary -------------------------------------------

    #[test]
    fn exact_fit_keyword_boundary_expands() {
        // Complement to `keyword_too_long_for_macro_name_skips_invocation`. The
        // fit check is `keyword.len() > name.len() → skip`, so an EQUAL-length
        // macro name must still expand. Macro name `STRUCT` (6) == `struct`
        // keyword (6).
        let src = b"STRUCT(Foo, (int x; void run() {}))\n";
        let out = expand_macro_define_types(src, &[entry("STRUCT")], "/t.cpp")
            .expect("equal-length keyword must still expand");
        assert_preserves_layout(src, &out);
        let fg = parse(&out);
        let foo = find(&fg, "Foo").expect("Foo struct symbol from exact-fit name");
        assert_eq!(foo.kind, SymbolKind::Struct);
        let run = find(&fg, "run").expect("run method from exact-fit body");
        assert_eq!(run.parent, "Foo");
    }
}
