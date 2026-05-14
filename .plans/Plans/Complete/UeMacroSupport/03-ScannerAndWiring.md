---
title: "strip_macros_with_args + two-pass preprocess"
type: phase
plan: UeMacroSupport
phase: 3
status: complete
created: 2026-05-13
updated: 2026-05-14
deliverable: "`strip_macros_with_args` scanner ships, using `skip_lexical` (Phase 2) and the owned-bytes token set from `CppConfig.macro_strip_with_args` (Phase 1). `CppParser::preprocess` runs as two passes: existing whole-word pass first, then the new parameterized pass. UE-style fixtures with `UCLASS(...)` / `UFUNCTION(...)` extract correctly when the user opts in via config. Default (no config) behavior is byte-identical to today."
tasks:
  - id: "3.1"
    title: "strip_macros_with_args scanner implementation"
    status: complete
    verification: "`fn strip_macros_with_args(content: &mut [u8], tokens: &HashSet<Vec<u8>>) -> usize` lives in `crates/code-graph-lang-cpp/src/preprocess.rs`. Returns the count of replacements. Walks `content`: (1) call `skip_lexical(content, i)` at every top-of-loop — if `Some(end)`, advance `i = end` and continue; (2) if not at an ident-start byte, advance `i = i+1`; (3) at an ident-start, walk to ident-end, COPY the identifier span into a stack `SmallVec<[u8; 64]>` (or equivalent owned-bytes scratch buffer — `Vec<u8>` is fine; the SmallVec is an optional perf nicety), end the borrow on `content`, then `tokens.contains(scratch.as_slice())`; (4) if matched, skip whitespace forward, require `(`, else continue; (5) call `find_balanced_close(content, paren_pos)` — on `None` (unbalanced), continue without rewriting (bail); on `Some(close)`, `content[ident_start..=close].fill(b' ')`. Strict balanced-paren scan: walks parens consulting `skip_lexical` on every iteration so strings and comments inside the arg list don't unbalance the count. Bail-on-unbalanced is silent — no `eprintln!`."
  - id: "3.2"
    title: "find_balanced_close helper using skip_lexical"
    status: complete
    verification: "`fn find_balanced_close(content: &[u8], open_paren: usize) -> Option<usize>` walks parens starting at `content[open_paren] == b'('`. Maintains a depth counter starting at 1; at every byte position, FIRST consult `skip_lexical` and if `Some(end)`, advance past the lexical region without affecting depth; otherwise `(` increments, `)` decrements. Returns `Some(close_pos)` when depth hits 0; `None` if EOF first. Pure function — no mutation. Unit tests: empty parens `()`, nested `(a(b)c)`, parens inside string `(\"a)b\")`, parens inside comment `(/* ) */)`, parens inside raw string `(R\"X())X\")`, EOF mid-walk (unbalanced — `None`), parens inside char literal `('('+1)`. Each test asserts the exact `Option<usize>` return."
    depends_on: ["3.1"]
  - id: "3.3"
    title: "Wire two-pass preprocess in CppParser"
    status: complete
    verification: "`CppParser::preprocess` at `crates/code-graph-lang-cpp/src/lib.rs:479` is updated. The existing `strip_macros` signature is `pub fn strip_macros<'a>(content: &'a [u8], macros: &[String]) -> Cow<'a, [u8]>` — it takes IMMUTABLE bytes and returns a fresh `Cow`. The two-pass wiring chains via `Cow::into_owned()`: (a) if BOTH `cfg.cpp.macro_strip.is_empty() && cfg.cpp.macro_strip_with_args.is_empty()`, return `Cow::Borrowed(content)` (fast-path no-op); (b) otherwise call `let cow = strip_macros(content, &cfg.cpp.macro_strip);` (pass 1, returns `Cow<[u8]>`); (c) `let mut buf: Vec<u8> = cow.into_owned();` (force ownership so pass 2 can mutate); (d) build `let tokens: HashSet<Vec<u8>> = cfg.cpp.macro_strip_with_args.iter().map(|s| s.as_bytes().to_vec()).collect();` per-call (CppParser does NOT cache `RootConfig`; preprocess receives `cfg` as a parameter — see existing signature); (e) `strip_macros_with_args(&mut buf, &tokens);` (pass 2, mutates in place); (f) return `Cow::Owned(buf)`. A test fixture with `macro_strip_with_args = []` (and any `macro_strip`) produces byte-identical output to today's CppMacroStrip behavior — pinned by an existing CppMacroStrip integration test that must stay green."
    depends_on: ["3.1"]
  - id: "3.4"
    title: "Scanner unit tests (13 cases)"
    status: complete
    verification: "Test module in `preprocess.rs` (alongside the existing `strip_macros` tests and the new `skip_lexical` tests from Phase 2) gains `strip_macros_with_args_*` tests: (a) `empty_args` — `GENERATED_BODY()` → 16 spaces, surrounding class extracts; (b) `complex_args` — `UCLASS(BlueprintType, meta=(BlueprintSpawnableComponent))` → all bytes spaces; (c) `string_literal_with_commas` — `UFUNCTION(BlueprintCallable, meta=(DisplayName=\"X, Y\"))` → balanced-paren walker honors the string; (d) `multiline` — `DECLARE_DELEGATE_TwoParams(\\n  FOnHit,\\n  AActor*, OtherActor)` → non-`\\n` bytes become spaces; `\\n` bytes are preserved verbatim so line offsets after the macro stay aligned with the original source; (e) `inside_string_no_strip` — `const char* s = \"UCLASS(Foo)\"` → no rewrite; (f) `inside_comment_no_strip` — `// UCLASS(Foo)` and `/* UCLASS(Foo) */` → no rewrite; (g) `unbalanced_paren_bails` — `UCLASS(unclosed` → no rewrite, no log; (h) `whole_word_boundary_prefix` — `MY_UCLASS_HELPER(x)` does NOT match `UCLASS`; (i) `whole_word_boundary_suffix` — `XUCLASS(x)` does NOT match `UCLASS`; (j) `disjoint_lists` — `macro_strip = [\"ENGINE_API\"], macro_strip_with_args = [\"UCLASS\"]`, fixture `UCLASS(X) class ENGINE_API AActor : public UObject {}` → both passes run, `AActor` symbol extracts with `Inherits → UObject` edge; (k) `whitespace_before_paren` — `UCLASS (BlueprintType)` (literal space) strips; (l) `user_function_named_like_macro` — fixture `void UCLASS(int x) {}` with `UCLASS` in `macro_strip_with_args` → function definition stripped → ZERO `UCLASS` symbols. This is the **documented expected behavior** pinning the 'user error → understandable failure' property. Add a test-level comment explaining the test exists to prevent silent regression of that property; (m) `generated_body_bare_and_parens` — same file mixing `GENERATED_BODY()` and bare `GENERATED_BODY` with the token in `macro_strip_with_args`. Assert surrounding class extracts correctly (the parameterized form strips, the bare form survives benignly); if the fixture surfaces a real failure mode for bare `GENERATED_BODY`, file a follow-up to revisit Decision 4 in the design."
    depends_on: ["3.3"]
  - id: "3.5"
    title: "Structural verification"
    status: complete
    verification: "`cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --all --check` clean; `cargo test -p code-graph-lang-cpp` green; `cargo test --workspace` green (the 13 new scanner tests + Phase 2's `skip_lexical` tests + Phase 1's config tests all pass); existing 49 corpus tests + `fmt`/`curl`/`abseil-cpp` C++ dogfood baselines stay within ±10% (Phase 3 default behavior is byte-identical to today on configs with empty `macro_strip_with_args`); `make snapshot-clean` passes."
    depends_on: ["3.4"]
---

# Phase 3: strip_macros_with_args + two-pass preprocess

## Overview

Lands the scanner that consumes `skip_lexical` (Phase 2) and the config field from Phase 1. The scanner is owned-bytes-keyed (`HashSet<Vec<u8>>`) to avoid the borrow-checker conflict that would arise from holding an immutable slice borrow into `content` across the subsequent mutable rewrite — this was the most concrete implementation gotcha flagged in the design review.

The two-pass preprocess wiring runs the existing whole-word `strip_macros` first, then the new parameterized `strip_macros_with_args`. Order matters per the design's Decision 3: whole-word and parameterized matchers are disjoint by definition, but pass-1 first is cheaper (linear scan, no paren walk) so pass-2 operates on a smaller "still has something to find" set.

Phase 3 is the first phase that produces observable behavior change for users opting into the config. Default behavior (empty `macro_strip_with_args`) is byte-identical to today's CppMacroStrip — exercised by 49 existing corpus tests and three C++ dogfood baselines.

## 3.1: strip_macros_with_args scanner implementation

### Subtasks
- [ ] Add `fn strip_macros_with_args(content: &mut [u8], tokens: &HashSet<Vec<u8>>) -> usize` to `crates/code-graph-lang-cpp/src/preprocess.rs`
- [ ] Identifier-byte helpers: the existing file has only `fn is_ident_byte(b: u8) -> bool` at `preprocess.rs:81` (alphanumeric+underscore — **continue** semantics, accepts digits). Add `fn is_ident_start(b: u8) -> bool { b.is_ascii_alphabetic() || b == b'_' }` (rejects digits as the leading byte) AND change `is_ident_byte` to `pub(crate)` if it isn't already, so the new scanner AND Phase 2's raw-string prefix check in `skip_lexical` can both reference it. Treat `is_ident_byte` as the canonical "continue" helper — do NOT introduce a second `is_ident_continue` alias
- [ ] Scratch buffer for the candidate identifier: `let mut id_buf: Vec<u8> = Vec::with_capacity(64);` declared outside the loop and `id_buf.clear()` at each ident-match attempt — keeps allocation cost amortized
- [ ] On a match: walk forward past whitespace, require `(`; on missing `(`, continue (bare identifier — not a parameterized macro use)
- [ ] Use `find_balanced_close` (Task 3.2) to find the matching `)`. On `None` (unbalanced), continue WITHOUT rewriting — silent bail per the design's Decision 2
- [ ] Once close found, overwrite `content[ident_start..=close]` byte-by-byte with spaces, preserving `\n` bytes verbatim. The loop is `for b in &mut content[ident_start..=close] { if *b != b'\n' { *b = b' '; } }`. Byte-preserving substitution keeps line/column offsets intact; `\n` preservation specifically keeps line numbers AFTER a multi-line arg list aligned with the original source. See the design's Key invariant section
- [ ] Advance `i = close + 1` after a successful strip
- [ ] Return the running replacement count (informational; not surfaced to users)

### Notes
The owned-bytes token set (`HashSet<Vec<u8>>`, NOT `HashSet<&[u8]>`) is load-bearing per the design review. Holding `let id: &[u8] = &content[id_start..i]` then calling `find_balanced_close(content, j)` would simultaneously hold a shared borrow on `content` and pass it as a borrow to the helper — a compile error. Copy the ident span into the scratch `Vec<u8>` first, release the `content` borrow, then look up.

The bail-on-unbalanced is silent — no `eprintln!`. Per the design's Decision 2, files with `#if 0 UCLASS(unclosed` produce no warning noise because that block was already broken in the source; the strip wasn't going to help.

## 3.2: find_balanced_close helper using skip_lexical

### Subtasks
- [ ] Add `fn find_balanced_close(content: &[u8], open_paren: usize) -> Option<usize>` in `preprocess.rs`
- [ ] Precondition: `content[open_paren] == b'('`. Assert in debug builds; production builds can rely on the caller's contract
- [ ] Walk `i = open_paren + 1` with `depth: u32 = 1`; loop until `depth == 0` (return `Some(i - 1)`) or `i >= content.len()` (return `None`)
- [ ] At each iteration top: `if let Some(end) = skip_lexical(content, i) { i = end; continue; }` — this is what makes parens inside strings/comments/raw-strings not affect the depth counter
- [ ] Otherwise: `match content[i] { b'(' => depth += 1, b')' => depth -= 1, _ => () }; i += 1;`
- [ ] When `depth == 0` after a `)`, the close position is `i - 1` (the byte that was just decremented past)
- [ ] Add the 7 unit tests listed in the verification field. Each test asserts the exact `Option<usize>` return

### Notes
The walker delegating to `skip_lexical` is what makes this scanner correct on UE arg lists that contain strings with commas, parentheses, or other tokens. Without it, `UFUNCTION(meta=(DisplayName="X)Y"))` would be parsed as unbalanced because the `)` inside the string would decrement depth prematurely.

## 3.3: Wire two-pass preprocess in CppParser

### Subtasks
- [ ] Locate `impl LanguagePlugin for CppParser` and its `preprocess` method (likely in `crates/code-graph-lang-cpp/src/lib.rs`)
- [ ] Today (post-CppMacroStrip): the method handles `macro_strip` only. Existing signature at `lib.rs:479`: `fn preprocess<'a>(&self, content: &'a [u8], cfg: &RootConfig) -> Cow<'a, [u8]>` — `cfg` is a parameter, NOT cached on `CppParser`
- [ ] Existing `strip_macros` signature: `pub fn strip_macros<'a>(content: &'a [u8], macros: &[String]) -> Cow<'a, [u8]>` — takes `&[u8]` (NOT `&mut`), returns a fresh `Cow`. Mutate-in-place chaining is wrong; use `Cow::into_owned()` to materialize between passes
- [ ] Update the body to:
  - If BOTH `cfg.cpp.macro_strip.is_empty() && cfg.cpp.macro_strip_with_args.is_empty()`, return `Cow::Borrowed(content)` (fast-path no-op)
  - `let cow = strip_macros(content, &cfg.cpp.macro_strip);` (pass 1)
  - `let mut buf: Vec<u8> = cow.into_owned();` (force ownership; on pass-1-no-op `cow` is `Borrowed` so `into_owned` allocates a copy — acceptable because we've already decided pass 2 has work to do)
  - `let tokens: HashSet<Vec<u8>> = cfg.cpp.macro_strip_with_args.iter().map(|s| s.as_bytes().to_vec()).collect();` (constructed per call)
  - `strip_macros_with_args(&mut buf, &tokens);` (pass 2, mutates buf)
  - `Cow::Owned(buf)`
- [ ] Verify the existing CppMacroStrip integration tests stay green (no change to whole-word behavior when `macro_strip_with_args` is empty)

### Notes
The two-pass order (whole-word first, parameterized second) is established by Decision 3 in the design. The disjoint-lists property (validated in Phase 1) means either order produces the same byte output for any valid config; whole-word first is just cheaper.

## 3.4: Scanner unit tests (13 cases)

### Subtasks
- [ ] Add the 12 tests from this task's `verification` field
- [ ] Tests live in `preprocess.rs`'s test module, parallel to the Phase 2 `skip_lexical` tests and the existing `strip_macros` tests
- [ ] Use small inline byte-string fixtures; assert on the returned `String::from_utf8(buf).unwrap()` content after substitution (or directly on `&[u8]` if avoiding UTF-8 round-trip)
- [ ] Test (l) `user_function_named_like_macro` — add a doc comment to the test explaining it pins the expected behavior: "If a user lists a macro name that collides with a real function in their codebase, that function disappears. This is by design — users should choose macro names that don't collide. The test prevents silent regression of this property."
- [ ] Test (m) `generated_body_bare_and_parens` — add a doc comment explaining that if this test fails on real UE code, Decision 4 in the design must revisit (allow same token in both lists). Until then, the test pins the "bare form survives benignly" assumption
- [ ] Each test asserts: (a) the substituted byte content, (b) the line/column offsets of surviving tokens remain unchanged from input

### Notes
Tests (h) and (i) — the whole-word boundary tests — are the most often-broken regression target. The scanner must NOT match `MY_UCLASS_HELPER` against `UCLASS` even though `UCLASS` is a prefix. The is-ident-continue check after the matched span is the safety net; the test fixture proves it works.

## 3.5: Structural verification

### Subtasks
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] Run `cargo fmt --all --check`
- [ ] Run `cargo test --workspace`
- [ ] Run `make snapshot-clean`
- [ ] Run the C++ dogfood baselines if submodules initialized: `cargo test -p code-graph-lang-cpp fmt`, `... curl`, `... abseil-cpp` — each within ±10% of recorded baseline (the new pass is a no-op when `macro_strip_with_args` is empty, which is the default for these baselines' configs)

### Notes
The ±10% guarantee on existing baselines is the hard line against accidental regressions in the whole-word pass-1 path or in the two-pass plumbing. A drift outside ±10% means pass-2 is firing when it shouldn't (probably a token-set construction bug or a fast-path miss).

## Acceptance Criteria
- [ ] `strip_macros_with_args` ships, owned-bytes-keyed, using `skip_lexical` and `find_balanced_close`
- [ ] `find_balanced_close` correctly counts parens past lexical regions
- [ ] `CppParser::preprocess` runs both passes; fast-path no-op when both lists empty
- [ ] 13 scanner unit tests pass (cases a–m, including the user-function-named-like-macro pin and the GENERATED_BODY bare+parens pin)
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo fmt --all --check` clean
- [ ] `make snapshot-clean` passes
- [ ] `fmt`/`curl`/`abseil-cpp` baselines within ±10%
