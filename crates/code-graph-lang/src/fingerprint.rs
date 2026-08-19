//! Default text-based symbol fingerprinting (Designs/VcsHistory Decision 5).
//!
//! `Normalized` hashes the symbol's line span with comments stripped and
//! inter-token whitespace normalized (a separator survives only where two
//! word tokens would otherwise merge) — identifiers, keywords, and literal
//! VALUES all contribute verbatim, and nothing is case-folded. The name qualifies
//! formatting, not identifiers: two spans whose only difference is layout or
//! comments fingerprint identically (AC-38); any code or literal change
//! fingerprints differently.
//!
//! **std-only by requirement, not by style.** This crate is one of the four
//! NFR-02-protected crates, so the hash is
//! [`std::collections::hash_map::DefaultHasher`] and nothing else.
//! `DefaultHasher` is unstable across Rust releases — fine for in-process
//! equality; the fingerprint cache (phase 6.2) must therefore key on binary
//! identity or treat itself as invalid across upgrades.
//!
//! **Best-effort lexing, deterministic always.** The comment/string scanner
//! is a small lexical approximation, not a parser: exotic constructs (raw
//! strings whose bodies contain escape-like sequences, C# verbatim-string
//! quote doubling, nested Python quote mixing) may strip slightly more or
//! less than a real lexer would. That imprecision only reduces *sensitivity*
//! (a change might hash equal, or a comment edit might hash different, in a
//! corner case); it never affects *stability* — the same bytes always
//! produce the same fingerprint, which is the property the transition walk
//! relies on.

use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;

use code_graph_core::{Language, Symbol};

use crate::FingerprintMode;

/// Fingerprint the symbol's `[line, end_line]` span of `content` in the
/// `Normalized` mode. Returns `None` when the span does not exist in this
/// content (line 0, or a start line beyond EOF) — the caller treats that as
/// "cannot fingerprint here", not as an empty hash.
pub fn normalized_fingerprint(content: &[u8], symbol: &Symbol, language: Language) -> Option<u64> {
    let span = span_bytes(content, symbol.line, symbol.end_line.max(symbol.line))?;
    let normalized = normalize(span, language);
    let mut hasher = DefaultHasher::new();
    hasher.write(&normalized);
    Some(hasher.finish())
}

/// The bytes of the 1-based inclusive line range `[start_line, end_line]`.
/// The end clamps to EOF; a start beyond EOF (or `0`) is `None`.
fn span_bytes(content: &[u8], start_line: u32, end_line: u32) -> Option<&[u8]> {
    if start_line == 0 {
        return None;
    }
    let mut line = 1u32;
    let mut start = None;
    let mut offset = 0usize;
    if start_line == 1 {
        start = Some(0);
    }
    for (index, &byte) in content.iter().enumerate() {
        if byte != b'\n' {
            continue;
        }
        if line == end_line {
            return start.map(|begin| &content[begin..index]);
        }
        line += 1;
        offset = index + 1;
        if line == start_line {
            start = Some(offset);
        }
    }
    let _ = offset;
    // EOF reached: the span's tail clamps to the end of content.
    start
        .filter(|&begin| begin <= content.len()) // start line existed
        .filter(|_| line >= start_line)
        .map(|begin| &content[begin..])
}

/// Comment syntax per language family.
struct CommentSyntax {
    line_marker: &'static [u8],
    block: Option<(&'static [u8], &'static [u8])>,
    /// Rust nests block comments; the C family does not.
    nested_blocks: bool,
    /// Python treats `'` as a string delimiter (and both quotes triple);
    /// the C family treats `'` as a short char literal, and Rust
    /// additionally uses bare `'` for lifetimes.
    python_strings: bool,
}

fn comment_syntax(language: Language) -> CommentSyntax {
    match language {
        Language::Python => CommentSyntax {
            line_marker: b"#",
            block: None,
            nested_blocks: false,
            python_strings: true,
        },
        Language::Rust => CommentSyntax {
            line_marker: b"//",
            block: Some((b"/*", b"*/")),
            nested_blocks: true,
            python_strings: false,
        },
        _ => CommentSyntax {
            line_marker: b"//",
            block: Some((b"/*", b"*/")),
            nested_blocks: false,
            python_strings: false,
        },
    }
}

/// A byte that can extend an identifier/number token. Whitespace between two
/// word bytes is load-bearing (`fn add` must not become `fnadd`); whitespace
/// adjacent to punctuation is formatting (`add( left` and `add(left` are the
/// same code, as are `left + right` and `left+right`).
fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

/// Strip comments and normalize inter-token whitespace — a single space is
/// kept only where two word tokens would otherwise merge; whitespace
/// adjacent to punctuation is dropped entirely, so inserting a line break
/// after `(` (the canonical reformat) does not change the fingerprint.
/// String contents are copied verbatim (a changed literal IS a change in
/// `Normalized` mode). Known lexical-approximation cost: constructs whose
/// meaning depends on spacing between punctuation (`a - -b` vs `a--b`)
/// hash equal — a sensitivity loss, never an instability.
fn normalize(span: &[u8], language: Language) -> Vec<u8> {
    let syntax = comment_syntax(language);
    let mut out: Vec<u8> = Vec::with_capacity(span.len());
    let mut pending_space = false;
    let mut i = 0usize;

    let flush_space = |out: &mut Vec<u8>, pending: &mut bool, next: u8| {
        if *pending {
            if let Some(&last) = out.last() {
                if is_word(last) && is_word(next) {
                    out.push(b' ');
                }
            }
        }
        *pending = false;
    };

    while i < span.len() {
        let rest = &span[i..];

        // Line comment → skip to end of line; the newline itself becomes
        // collapsed whitespace.
        if rest.starts_with(syntax.line_marker)
            && !(syntax.block.is_some() && rest.starts_with(b"/*"))
        {
            while i < span.len() && span[i] != b'\n' {
                i += 1;
            }
            pending_space = true;
            continue;
        }

        // Block comment → skip past the closing marker (nested for Rust);
        // an unterminated block swallows the rest of the span.
        if let Some((open, close)) = syntax.block {
            if rest.starts_with(open) {
                let mut depth = 1usize;
                i += open.len();
                while i < span.len() && depth > 0 {
                    let inner = &span[i..];
                    if syntax.nested_blocks && inner.starts_with(open) {
                        depth += 1;
                        i += open.len();
                    } else if inner.starts_with(close) {
                        depth -= 1;
                        i += close.len();
                    } else {
                        i += 1;
                    }
                }
                pending_space = true;
                continue;
            }
        }

        let byte = span[i];

        // Strings: copied verbatim, including their internal whitespace.
        if byte == b'"' || byte == b'`' || (byte == b'\'' && syntax.python_strings) {
            flush_space(&mut out, &mut pending_space, byte);
            i = copy_string(span, i, &mut out, syntax.python_strings);
            continue;
        }

        // C-family short char literal ('x', '\n', 'é') — copied verbatim.
        // A quote that does not close within a literal-sized lookahead is
        // punctuation (a Rust lifetime) and passes through as one byte.
        if byte == b'\'' {
            if let Some(end) = char_literal_end(span, i) {
                flush_space(&mut out, &mut pending_space, byte);
                out.extend_from_slice(&span[i..end]);
                i = end;
                continue;
            }
        }

        if byte.is_ascii_whitespace() {
            pending_space = true;
            i += 1;
            continue;
        }

        flush_space(&mut out, &mut pending_space, byte);
        out.push(byte);
        i += 1;
    }
    out
}

/// Copy a quoted string starting at `start` verbatim into `out`, returning
/// the index just past its closing delimiter. Handles backslash escapes and,
/// for Python, triple-quoted forms. An unterminated string copies to EOF.
fn copy_string(span: &[u8], start: usize, out: &mut Vec<u8>, python: bool) -> usize {
    let quote = span[start];
    // Backticks (Go raw strings) have no escapes and no triple form.
    let escapes = quote != b'`';
    let triple = python
        && quote != b'`'
        && span.len() >= start + 3
        && span[start + 1] == quote
        && span[start + 2] == quote;
    let delimiter_len = if triple { 3 } else { 1 };
    let mut i = start + delimiter_len;
    while i < span.len() {
        if escapes && span[i] == b'\\' {
            i = (i + 2).min(span.len());
            continue;
        }
        if span[i] == quote {
            if !triple {
                i += 1;
                break;
            }
            if span.len() >= i + 3 && span[i + 1] == quote && span[i + 2] == quote {
                i += 3;
                break;
            }
        }
        i += 1;
    }
    out.extend_from_slice(&span[start..i]);
    i
}

/// If a `'` at `start` opens a short character literal, return the index
/// just past its closing `'`; otherwise `None` (a lifetime or stray quote).
fn char_literal_end(span: &[u8], start: usize) -> Option<usize> {
    const LOOKAHEAD: usize = 12;
    let mut i = start + 1;
    if i < span.len() && span[i] == b'\\' {
        i += 2; // the escaped byte
                // Multi-byte escapes: '\u{1F600}', '\x7f' — scan to the quote.
        while i < span.len() && i - start < LOOKAHEAD {
            if span[i] == b'\'' {
                return Some(i + 1);
            }
            i += 1;
        }
        return None;
    }
    // Unescaped: one scalar, up to 4 UTF-8 bytes, then the closing quote.
    let mut consumed = 0usize;
    while i < span.len() && consumed < 4 {
        if span[i] == b'\'' {
            // ''' would be an empty literal — not a literal, pass through.
            return (consumed > 0).then_some(i + 1);
        }
        if span[i] == b'\n' {
            return None;
        }
        i += 1;
        consumed += 1;
    }
    (i < span.len() && span[i] == b'\'').then_some(i + 1)
}

// ---------------------------------------------------------------------------
// Shared AST fingerprint walk (phase 8, Designs/VcsHistory Decision 5)
// ---------------------------------------------------------------------------
//
// The per-language `fingerprint_symbol` overrides all share one shape: parse
// the SAME bytes the preceding `parse_file` saw, locate the symbol's subtree
// from its span, and hash a deterministic pre-order walk of node kinds and
// leaf texts — comments invisible under both modes, literal VALUES included
// under `Normalized` and excluded (kind only) under `LiteralInsensitive`.
// The walk lives here so six plugins share one implementation; each plugin
// supplies only its literal/comment kind predicates.
//
// Determinism discipline (phase-8 plan note): the walk is a tree-cursor
// pre-order — sibling order comes from the tree, never from a hash-ordered
// collection — so the same bytes always hash the same. Structure bytes
// (`(`/`)` on enter/exit) make sibling regrouping visible: `(A (B))` and
// `(A) (B)` hash differently even when their leaf texts concatenate
// identically.

/// Locates the AST node for `symbol`'s span in a freshly parsed tree of the
/// same content the span was extracted from: exact start `(row, column)`
/// and end-row match, outermost (first in pre-order) named node winning
/// when several named nodes share the exact same span. Returns `None` when
/// no node matches — synthesized symbols (`[cpp].macro_define_function`)
/// and heavily error-recovered spans.
///
/// **The located node is the one the EXTRACTOR recorded, wrappers
/// excluded.** Every extractor records `def_node.start_position()` of the
/// inner definition node — a C++ `template_declaration` wrapper starts at
/// the `template` keyword, a different position, so it can never
/// exact-match: the walk fingerprints the inner `function_definition` /
/// `class_specifier`, and a template-parameter-list-only edit
/// (`template<typename T>` → `template<typename T, int N>`) is INVISIBLE
/// under both modes. Same boundary as Rust outer attributes and Python
/// decorators: the span convention is the extractor's, and the
/// fingerprint honestly covers exactly that span (pinned per language by
/// the `mod fingerprint` suites).
pub fn locate_symbol_node<'t>(
    root: tree_sitter::Node<'t>,
    symbol: &Symbol,
) -> Option<tree_sitter::Node<'t>> {
    if symbol.line == 0 {
        return None;
    }
    let start_row = symbol.line as usize - 1;
    let start_column = symbol.column as usize;
    let end_row = symbol.end_line.max(symbol.line) as usize - 1;

    let mut cursor = root.walk();
    'outer: loop {
        let node = cursor.node();
        if node.is_named()
            && node.start_position().row == start_row
            && node.start_position().column == start_column
            && node.end_position().row == end_row
        {
            return Some(node);
        }
        // Descend only into subtrees that can contain the span start.
        let contains = node.start_position().row <= start_row && node.end_position().row >= end_row;
        if contains && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                continue 'outer;
            }
            if !cursor.goto_parent() {
                break 'outer;
            }
        }
    }
    None
}

/// Hashes one subtree per the mode. `is_literal` and `is_comment` classify
/// node KINDS (grammar spellings, e.g. `"string_literal"`, `"comment"`):
/// a comment subtree is skipped entirely under both modes; a literal
/// subtree contributes its kind plus (under `Normalized` only) its full
/// source text; every other leaf contributes its text; every node
/// contributes its kind and structure brackets.
pub fn ast_fingerprint(
    node: tree_sitter::Node<'_>,
    content: &[u8],
    mode: FingerprintMode,
    is_literal: fn(&str) -> bool,
    is_comment: fn(&str) -> bool,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    // Iterative pre-order with explicit enter/exit marks; recursion-free so
    // pathological nesting cannot overflow the stack.
    let mut cursor = node.walk();
    'outer: loop {
        let current = cursor.node();
        let kind = current.kind();
        let mut descend = true;
        if is_comment(kind) || kind == "," {
            // Comments are invisible under both modes. So are commas:
            // formatters ADD trailing commas when breaking a list across
            // lines (rustfmt always, gofmt necessarily), and the tree
            // structure already encodes element boundaries — hashing the
            // separator would make a reformat-only commit report
            // `modified`, exactly the false positive AC-38 forbids.
            descend = false;
        } else {
            hasher.write(b"(");
            hasher.write(kind.as_bytes());
            hasher.write(b"\x1f");
            if is_literal(kind) {
                if mode == FingerprintMode::Normalized {
                    hasher.write(&content[current.byte_range()]);
                }
                descend = false; // the value (or its absence) is the leaf
            } else if current.child_count() == 0 {
                hasher.write(&content[current.byte_range()]);
            }
        }
        if descend && cursor.goto_first_child() {
            continue;
        }
        // The exit bracket pairs with the enter bracket: skipped nodes
        // (comments, commas) wrote no `(`, so they get no `)`.
        if !(is_comment(kind) || kind == ",") {
            hasher.write(b")");
        }
        loop {
            if cursor.node().id() == node.id() {
                break 'outer;
            }
            if cursor.goto_next_sibling() {
                continue 'outer;
            }
            if !cursor.goto_parent() {
                break 'outer;
            }
            hasher.write(b")");
        }
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_graph_core::SymbolKind;

    fn symbol(line: u32, end_line: u32) -> Symbol {
        Symbol {
            name: "target".to_string(),
            kind: SymbolKind::Function,
            file: "/fixture.rs".to_string(),
            line,
            column: 0,
            end_line,
            signature: String::new(),
            namespace: String::new(),
            parent: String::new(),
            language: Language::Rust,
        }
    }

    fn fp(content: &str, line: u32, end_line: u32, language: Language) -> Option<u64> {
        normalized_fingerprint(content.as_bytes(), &symbol(line, end_line), language)
    }

    #[test]
    fn reformat_only_change_keeps_the_fingerprint(/* AC-38 */) {
        let original = "fn add(left: u32, right: u32) -> u32 {\n    left + right\n}\n";
        let reformatted =
            "fn add(\n    left: u32,\n    right: u32\n) -> u32 {\n        left + right\n}\n";
        assert_eq!(
            fp(original, 1, 3, Language::Rust),
            fp(reformatted, 1, 6, Language::Rust),
            "whitespace-only reformatting must not change the fingerprint"
        );
    }

    #[test]
    fn comment_only_change_keeps_the_fingerprint() {
        let with_comment = "fn f() {\n    // note one\n    body(); /* old */\n}\n";
        let different_comment = "fn f() {\n    // a different note\n    body();\n}\n";
        assert_eq!(
            fp(with_comment, 1, 4, Language::Rust),
            fp(different_comment, 1, 4, Language::Rust)
        );
    }

    #[test]
    fn rust_nested_block_comment_is_stripped_whole() {
        let nested = "fn f() {\n    /* outer /* inner */ still comment */\n    body();\n}\n";
        let plain = "fn f() {\n    body();\n}\n";
        assert_eq!(
            fp(nested, 1, 4, Language::Rust),
            fp(plain, 1, 3, Language::Rust)
        );
    }

    #[test]
    fn literal_and_code_changes_change_the_fingerprint() {
        let base = "fn f() -> u32 {\n    1 + 2\n}\n";
        let literal = "fn f() -> u32 {\n    1 + 3\n}\n";
        let logic = "fn f() -> u32 {\n    2 * 2\n}\n";
        assert_ne!(
            fp(base, 1, 3, Language::Rust),
            fp(literal, 1, 3, Language::Rust)
        );
        assert_ne!(
            fp(base, 1, 3, Language::Rust),
            fp(logic, 1, 3, Language::Rust)
        );
    }

    #[test]
    fn string_contents_are_verbatim_including_whitespace_and_markers() {
        let one = "fn f() { log(\"a  b // not a comment\"); }\n";
        let two = "fn f() { log(\"a b // not a comment\"); }\n";
        assert_ne!(
            fp(one, 1, 1, Language::Rust),
            fp(two, 1, 1, Language::Rust),
            "whitespace inside a string literal is a real change"
        );
    }

    #[test]
    fn rust_lifetimes_do_not_derail_comment_stripping() {
        let one = "fn f<'a>(x: &'a str) -> &'a str { x } // note\n";
        let two = "fn f<'a>(x: &'a str) -> &'a str { x } // different note\n";
        assert_eq!(fp(one, 1, 1, Language::Rust), fp(two, 1, 1, Language::Rust));
    }

    #[test]
    fn char_literals_are_verbatim() {
        let one = "fn f() -> char { 'x' } // c\n";
        let two = "fn f() -> char { 'y' } // c\n";
        assert_ne!(fp(one, 1, 1, Language::Rust), fp(two, 1, 1, Language::Rust));
    }

    #[test]
    fn python_hash_comments_strip_but_hash_in_string_stays() {
        let one = "def f():\n    return \"#tag\"  # trailing\n";
        let two = "def f():\n    return \"#tag\"\n";
        let changed = "def f():\n    return \"#gat\"\n";
        assert_eq!(
            fp(one, 1, 2, Language::Python),
            fp(two, 1, 2, Language::Python)
        );
        assert_ne!(
            fp(two, 1, 2, Language::Python),
            fp(changed, 1, 2, Language::Python)
        );
    }

    #[test]
    fn python_triple_quoted_strings_copy_verbatim() {
        let one = "def f():\n    s = \"\"\"a # not comment\n    b\"\"\"\n    return s\n";
        let two = "def f():\n    s = \"\"\"a # not comment\n     b\"\"\"\n    return s\n";
        assert_ne!(
            fp(one, 1, 4, Language::Python),
            fp(two, 1, 4, Language::Python)
        );
    }

    #[test]
    fn go_backtick_raw_strings_copy_verbatim() {
        let one = "func f() string {\n\treturn `a  b`\n}\n";
        let two = "func f() string {\n\treturn `a b`\n}\n";
        assert_ne!(fp(one, 1, 3, Language::Go), fp(two, 1, 3, Language::Go));
    }

    #[test]
    fn span_edges_behave() {
        let content = "line1\nline2\nline3\n";
        // end clamps to EOF
        assert!(fp(content, 2, 99, Language::Rust).is_some());
        // start beyond EOF -> None
        assert_eq!(fp(content, 99, 100, Language::Rust), None);
        // line 0 -> None
        assert_eq!(fp(content, 0, 1, Language::Rust), None);
        // inverted span clamps to one line via end_line.max(line)
        assert!(fp(content, 2, 1, Language::Rust).is_some());
    }

    #[test]
    fn same_input_is_deterministic() {
        let content = "fn f() {\n    body();\n}\n";
        assert_eq!(
            fp(content, 1, 3, Language::Rust),
            fp(content, 1, 3, Language::Rust)
        );
    }
}
