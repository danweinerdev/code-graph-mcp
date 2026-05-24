---
title: "skip_lexical lexer state machine"
type: phase
plan: UeMacroSupport
phase: 2
status: complete
created: 2026-05-13
updated: 2026-05-14
deliverable: "`fn skip_lexical(content: &[u8], i: usize) -> Option<usize>` is added to `crates/code-graph-lang-cpp/src/preprocess.rs`. The function recognizes 5 lexical regions (`//…`, `/*…*/`, `\"…\"`, `'…'`, `R\"DELIM(…)DELIM\"`) per the C++ rules in the design's `skip_lexical` specification. Unit-tested independently of the scanner that will consume it. Out-of-scope cases (line continuations, trigraphs) documented as non-goals."
tasks:
  - id: "2.1"
    title: "skip_lexical signature and dispatch shell"
    status: complete
    verification: "Function signature `fn skip_lexical(content: &[u8], i: usize) -> Option<usize>` added to `crates/code-graph-lang-cpp/src/preprocess.rs`. Returns `Some(end)` where `end` is the byte position immediately past the closing delimiter when `content[i]` starts a recognized lexical region; returns `None` for any other byte. The dispatch shell: peek `content[i]` and (if `i+1 < len`) `content[i+1]` to decide which region kind, then delegate to a per-region helper fn. Returns `None` if `i >= content.len()`. Pure-function — no state, no mutation, no side effects."
  - id: "2.2"
    title: "Line and block comments with comment-before-string precedence"
    status: complete
    verification: "(a) `//` at `content[i..i+2]` → scan forward to first `\\n`; return `Some(newline_pos + 1)`. EOF before `\\n` → return `Some(content.len())`. (b) `/*` at `content[i..i+2]` → scan forward to first `*/`; return `Some(close_pos + 2)`. EOF before `*/` → return `Some(content.len())` (unterminated block comment is treated as consuming to EOF; tree-sitter will surface it as ERROR). (c) Nesting is NOT supported (C++ disallows it; documented in a code comment). (d) Comment dispatch precedes string dispatch in the dispatch shell — `//` followed by `\"` does NOT enter string mode. Unit tests cover each of (a)/(b)/(c)/(d) plus the EOF-truncated variants."
    depends_on: ["2.1"]
  - id: "2.3"
    title: "Double-quoted strings and single-quoted char literals with escape rules"
    status: complete
    verification: "(a) `\"…\"` at `content[i]` → scan forward; a `\"` byte closes the literal IFF the number of consecutive `\\` bytes immediately preceding it is EVEN (0, 2, 4, …). Odd `\\` counts mean the `\"` is escaped. Return `Some(close_pos + 1)`. (b) `'…'` follows the same rule with `'` as the closer. (c) EOF before close → `Some(content.len())` (unterminated literal). (d) Implementation: walk forward; when candidate closer found, walk backward counting `\\` bytes until a non-`\\` byte is hit or the start of the literal is reached. Document this in a code comment as the canonical C++-standard escape interpretation. (e) Inside a string/char literal, comment-like patterns (`//`, `/*`) are NOT special — only the close-delimiter rule applies. Unit tests: `\"foo\"`, `\"foo\\\\\\\"bar\"` (escaped quote), `\"foo\\\\\\\\\"`-bar (escaped backslash before close), `'\\''` (escaped single-quote), `'\\\\n'` (newline escape), EOF-truncated, comment-pattern-inside-string."
    depends_on: ["2.1"]
  - id: "2.4"
    title: "Raw-string R\"DELIM(...)DELIM\" with delimiter-tag extraction"
    status: complete
    verification: "(a) `R\"` at `content[i..i+2]` (case-sensitive `R`, plain `\"`, NOT prefixed by an identifier-continue byte at `i-1` — `xR\"...\"` is NOT a raw string, the `R\"` belongs to the identifier `xR`) → extract the delimiter tag by scanning forward from `i+2` until the first `(`; the tag is the byte span `[i+2..paren_pos]`. (b) The tag can be empty (`R\"(...)\"`) or up to 16 chars per the C++ standard (we DO NOT enforce the 16-char ceiling — scanning continues until `(`). (c) Once the tag is captured, scan for the literal close sequence `)<tag>\"`. Return `Some(close_pos + tag.len() + 2)`. (d) EOF before tag's `(` or before the close → `Some(content.len())`. (e) Bytes inside a raw-string body are NOT subject to the `\\`-escape rule from 2.3; only the literal close-sequence terminates. (f) IMPORTANT: the prefix check at `(a)` — verify `i == 0 || !is_ident_continue(content[i-1])` to avoid misreading `xR\"...\"` (where `xR` is a multi-letter identifier) as a raw-string. (g) Encoding-prefixed raw strings (`u8R\"...\"`, `L R\"...\"`, etc.) — out of scope for MVP; the prefix `R\"` check rejects them as not-raw-strings; they fall through to be dispatched as ordinary double-quoted strings, where the `u8`/`L`/etc. prefix is just leading ordinary bytes. Acceptable. Unit tests: `R\"(simple)\"`, `R\"DELIM(complex)DELIM\"`, `R\"(with \\)\\n\\\" inside)\"`, `R\"TAG(…<no close>` (EOF), prefix-rejection (`xR\"...\"` not entering raw mode)."
    depends_on: ["2.1"]
  - id: "2.5"
    title: "Unit tests and documented out-of-scope cases"
    status: complete
    verification: "Test module in `preprocess.rs` (under `#[cfg(test)] mod tests { ... }` alongside the existing `strip_macros` tests) gains `skip_lexical_*` tests covering: (a) ordinary code byte (return `None`); (b) each of the 5 region types with a normal close; (c) each of the 5 region types with EOF-truncation; (d) comment-before-string precedence (`//\"text` — string-like pattern inside line comment); (e) escape rules for `\"…\"` and `'…'`; (f) raw-string with empty tag, complex tag, prefix-rejection; (g) consecutive regions (`/*a*/\"b\"` — running `skip_lexical` repeatedly walks past both). Documented out-of-scope cases (in code comments AND in a `// OUT OF SCOPE` block near the function): line continuations (`\\` before EOL inside `//` comment), trigraphs (`??/`, `??(`), digraphs (`<%`, `%>`, etc.), `\\u`-escaped delimiter close, encoding-prefixed raw strings."
    depends_on: ["2.2", "2.3", "2.4"]
  - id: "2.6"
    title: "Structural verification"
    status: complete
    verification: "`cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --all --check` clean; `cargo test -p code-graph-lang-cpp` green (existing tests + the new `skip_lexical_*` tests). Existing `strip_macros` tests (the whole-word pass from CppMacroStrip) stay byte-identical green; `skip_lexical` is a new pure function that doesn't touch them yet."
    depends_on: ["2.5"]
tags: [cpp, tree-sitter, ue, unreal-engine, parser, config, macros]
---

# Phase 2: skip_lexical lexer state machine

## Overview

`skip_lexical` is the lexer state machine that recognizes the 5 C++ regions where bytes must not be rewritten: line comments, block comments, double-quoted strings, single-quoted char literals, and raw strings. The function is written from scratch — the existing `strip_macros` (whole-word, from CppMacroStrip) deliberately has no lexer because whole-word replacement inside a string literal is harmless. The parameterized scanner in Phase 3 cannot rely on that property; it needs a real lexer.

This phase ships `skip_lexical` as a standalone pure function with its own unit tests. No production code consumes it yet — that wiring is Phase 3. Decoupling here lets the lexer's correctness be established independently of the scanner's correctness; a bug found in one phase is unambiguous about which side it lives on.

This phase is parallel-safe with Phase 1.

## 2.1: skip_lexical signature and dispatch shell

### Subtasks
- [ ] Open `crates/code-graph-lang-cpp/src/preprocess.rs`; locate the existing `strip_macros` function and its helpers
- [ ] Add `fn skip_lexical(content: &[u8], i: usize) -> Option<usize>` (`pub(crate)` if needed by Phase 3's `strip_macros_with_args`; otherwise file-private)
- [ ] Dispatch shell: peek `content[i]` (return `None` if `i >= content.len()`); peek `content[i+1]` for `/`-prefixed regions; pick the per-region helper based on the byte pair
- [ ] Doc comment naming each region recognized and pointing to the per-region helpers
- [ ] Reserve the per-region helper fns as stubs returning `None` for Tasks 2.2/2.3/2.4 to fill in

### Notes
The function is intentionally pure-`&[u8]`. No `&mut`, no `&str`, no UTF-8 assumptions — operating on raw bytes is correct because C++ source is byte-oriented (UTF-8 multi-byte sequences inside string literals are just sequences of high bytes; nothing in the lexer needs to interpret them).

## 2.2: Line and block comments with comment-before-string precedence

### Subtasks
- [ ] Implement the `//` line-comment helper: scan forward from `i+2` until `\n` or EOF; return `Some(newline_pos + 1)` for normal close, `Some(content.len())` for EOF-truncated
- [ ] Implement the `/*` block-comment helper: scan forward from `i+2` until `*/` (i.e., `content[k] == b'*' && content[k+1] == b'/'`) or EOF; return `Some(close_pos + 2)` for normal, `Some(content.len())` for EOF
- [ ] Nesting: C++ does NOT support nested block comments. Document this in a code comment. The implementation does NOT recurse into inner `/*`
- [ ] Confirm dispatch shell from 2.1 routes `/` followed by `/` or `*` to these helpers BEFORE the string helpers from 2.3 — comment-before-string precedence
- [ ] Unit tests: `// foo\n`, `// foo<EOF>`, `/* foo */`, `/* foo<EOF>`, `// \"unterminated`, `/* // nested-line */` (block comment dominates)

### Notes
The EOF-truncated cases return `Some(content.len())` rather than `None` because the caller (the scanner in Phase 3) interprets `None` as "this byte is not a region opener; advance by 1 and re-scan." Returning `Some(content.len())` says "I recognized this as a region; it extends to EOF; advance past EOF (i.e., terminate the scan)."

## 2.3: Double-quoted strings and single-quoted char literals with escape rules

### Subtasks
- [ ] Implement the `"…"` string-literal helper: walk forward from `i+1`; when a `"` byte is encountered, count preceding `\` bytes; if even (including zero) → closer found, return `Some(pos + 1)`; if odd → continue past the escaped quote
- [ ] Implement the `'…'` char-literal helper using the same rule with `'` as the closer
- [ ] EOF in both: `Some(content.len())` (treat as unterminated; tree-sitter handles the resulting ERROR)
- [ ] Document the odd-`\`-count rule in a code comment with a worked example: `"\\"` is a 4-byte string containing one backslash; the second `\` is preceded by ONE `\` (odd), so it's escaped and the third byte is the close. Walk-backward-counting-`\` is the canonical implementation
- [ ] Inside a string/char literal, comment-like sequences (`//`, `/*`) are not special — only the close rule applies. Do NOT call back into the dispatch shell from inside a string body
- [ ] Unit tests per the verification field

### Notes
The odd-`\`-count rule is the most common bug surface in C++ string lexers. A test fixture with `"\\\""` (escaped backslash followed by escaped quote — total 4 bytes between delimiters) catches the common mistake of "look at the byte right before the quote." Include this in 2.5's tests explicitly.

## 2.4: Raw-string R"DELIM(...)DELIM" with delimiter-tag extraction

### Subtasks
- [ ] Implement the raw-string helper: when dispatch shell sees `R"` at `content[i..i+2]`, FIRST verify the prefix is not part of a larger identifier by checking `i == 0 || !is_ident_continue(content[i-1])`. If part of identifier, return `None` (fall through to ordinary-byte handling)
- [ ] If the prefix check passes, extract the delimiter tag: scan from `i+2` until `(` (or EOF); the tag is `content[i+2..paren_pos]`
- [ ] If EOF before `(`, return `Some(content.len())`
- [ ] Once tag captured, build the close sequence `)<tag>"` (as a `Vec<u8>` or `[u8]` slice search target); scan forward from `paren_pos + 1` for the first occurrence
- [ ] Return `Some(close_pos + tag.len() + 2)` (the `2` accounts for the closing `)` at `close_pos` and the `"` at the end)
- [ ] EOF before close → `Some(content.len())`
- [ ] Encoding-prefixed raw strings (`u8R"`, `LR"`, `uR"`, `UR"`) — explicitly out of scope; the prefix check at `i-1` will see the encoding prefix as identifier-continue and reject. These fall through to ordinary dispatch, which will eventually hit the inner `"` and enter ordinary-string mode — incorrect but documented as a non-goal
- [ ] Unit tests per the verification field

### Notes
The prefix check (`!is_ident_continue(content[i-1])`) is the load-bearing correctness requirement here. Without it, `xR"foo(bar)foo"` (where `xR` is a variable name and the `"foo(bar)foo"` is a method call's string argument) gets misread as a raw-string opener, the close-sequence search fails, and a chunk of the file is "consumed" by the lexer — producing zero or wrong symbols downstream. This is exactly the kind of subtle bug that only the test from 2.5 will catch.

## 2.5: Unit tests and documented out-of-scope cases

### Subtasks
- [ ] Add `#[cfg(test)] mod skip_lexical_tests { use super::*; ... }` in `preprocess.rs` (or extend the existing tests module)
- [ ] Tests per the verification field. Each test asserts the exact returned `Option<usize>` and walks the consumed region byte-by-byte
- [ ] Add an out-of-scope documentation block as a doc comment on `skip_lexical`:
  ```
  /// Out of scope (treated as ordinary bytes; documented limitations):
  ///   - Line continuations: `\` at EOL inside a `//` comment does NOT extend the comment.
  ///   - Trigraphs: `??/` (alias for `\`), `??(`, `??)` are not interpreted.
  ///   - Digraphs: `<%`, `%>`, `:>`, `<:`, `%:` are not interpreted.
  ///   - Encoding-prefixed raw strings (`u8R"…"`, `LR"…"`, etc.) — the prefix check
  ///     intentionally rejects them; they fall through to ordinary `"…"` mode which
  ///     is incorrect but acceptable for the design's intended workloads.
  ///   - `\u`-escaped delimiter characters in raw strings — raw strings don't process
  ///     escapes by definition; not an issue.
  ```

### Notes
Documenting the out-of-scope cases in the function's own doc comment (not buried in a design doc) means a future reader debugging a parse failure can find the limitation list in one place.

## 2.6: Structural verification

### Subtasks
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] Run `cargo fmt --all --check`
- [ ] Run `cargo test -p code-graph-lang-cpp`
- [ ] Run `cargo test --workspace`
- [ ] Run `make snapshot-clean`

### Notes
No snapshot regenerations expected in Phase 2. `skip_lexical` is dead code (nothing calls it yet) until Phase 3 wires it in.

## Acceptance Criteria
- [ ] `fn skip_lexical(content: &[u8], i: usize) -> Option<usize>` added to `preprocess.rs`
- [ ] 5 region types handled per the design's `skip_lexical` specification
- [ ] Comment-before-string precedence in the dispatch
- [ ] Odd-`\`-count escape rule for `"…"` and `'…'`
- [ ] Raw-string prefix check rejects identifier-prefixed `R"`
- [ ] Out-of-scope cases documented in the function's doc comment
- [ ] Unit tests cover normal close, EOF-truncated, and edge cases for each region
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo fmt --all --check` clean
- [ ] `make snapshot-clean` passes
