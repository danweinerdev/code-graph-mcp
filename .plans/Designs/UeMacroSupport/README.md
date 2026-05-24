---
title: "UE Macro Support — parameterized macro stripping for UCLASS/UFUNCTION/UPROPERTY"
type: design
status: implemented
created: 2026-05-13
updated: 2026-05-24
tags: [cpp, tree-sitter, ue, unreal-engine, parser, config, macros]
related:
  - Designs/CppMacroStrip
  - Designs/PathNormalization
  - Designs/ResponseShapePolish
---

# UE Macro Support — parameterized macro stripping for UCLASS/UFUNCTION/UPROPERTY

## Overview

Real-world test against an 81k-file UE 4.27 codebase showed that `AActor`, `UObject`, `UActorComponent`, `AActor::Tick`, and tens of thousands of similar symbols are missing from the index — even though derived classes reference them via `Inherits` edges, leaving dangling base references. The cause is a parser blind spot: UE-style C++ wraps class and member declarations in **parameterized** preprocessor macros that today's `[cpp].macro_strip` (whole-word identifier replacement only — see `Designs/CppMacroStrip`) cannot match:

```cpp
UCLASS(BlueprintType, meta=(BlueprintSpawnableComponent))
class ENGINE_API AActor : public UObject
{
    GENERATED_BODY()

    UFUNCTION(BlueprintCallable, Category="Tick")
    virtual void Tick(float DeltaSeconds);

    UPROPERTY(EditAnywhere, Category="Animation")
    UAnimMontage* MyMontage;
};
```

The existing `macro_strip` handles `ENGINE_API` (whole-word bare identifier). It does **not** handle `UCLASS(BlueprintType, meta=(BlueprintSpawnableComponent))` (identifier + parenthesized argument list with internal commas, strings, and nested parens). The unstripped `UCLASS(...)` and `UFUNCTION(...)` tokens land in positions tree-sitter-cpp doesn't expect, producing `ERROR` nodes that swallow the class/function below them.

This design extends `CppMacroStrip` with a sibling config field — `[cpp].macro_strip_with_args` — that replaces `IDENTIFIER(...)` (identifier plus a balanced parenthesized argument list) with the same number of space characters, preserving every byte offset. The mechanism is the same space-substitution rewrite used today; the difference is the matcher: balanced-paren scanner instead of whole-word identifier scanner. Universal-ctags has had the equivalent `--regex-C++=...` patterns since 2006; we adopt the same approach.

The default value is an empty list — zero behavior change for non-UE users. UE users opt in by adding `["UCLASS", "UFUNCTION", "UPROPERTY", "USTRUCT", "UENUM", "GENERATED_BODY", "GENERATED_UCLASS_BODY", "DECLARE_DYNAMIC_MULTICAST_DELEGATE", "DECLARE_DELEGATE_OneParam", …]` to the list. A recommended UE preset will ship in `.code-graph.toml.example`.

## Goals

1. **`UCLASS(...) class ENGINE_API AActor : public UObject` produces an `AActor` symbol with the correct `Inherits → UObject` edge** — measured against an in-tree UE-style fixture and (stretch) against a public UE plugin baseline.
2. **Empty arg lists (`GENERATED_BODY()`) and complex nested args (`UFUNCTION(BlueprintCallable, meta=(DisplayName="X, Y"))`) both strip correctly** — including string-literal contents that may contain commas, parens, or matching identifier names.
3. **Zero behavior change for users with empty `macro_strip_with_args`** — the entire macro_strip path is preserved; the new field is additive.
4. **Byte offsets preserved.** A method on line 87 column 12 of the original source still reports as line 87 column 12 post-rewrite — same invariant as bare-token `macro_strip`.
5. **Strict balanced-paren matching.** Unbalanced or unterminated arg lists bail (do not strip), leaving the file as-is rather than mangling it. Silent failure beats a corrupted parse tree.

## Non-Goals

- **Auto-detecting UE macros.** Same rationale as `CppMacroStrip` Decision 1: heuristics like "any all-caps identifier ending in `_API`" or "any identifier followed by parens at file scope" risk eating user functions. Users list the macro names explicitly. We ship a recommended UE preset in `.code-graph.toml.example`; users copy it.
- **Cross-line macros** (`DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams(\n    FOnHit,\n    AActor*, OtherActor)`). MVP scope; the balanced-paren scanner naturally handles them as long as the lexer doesn't choke on newlines inside the paren block — and it doesn't (we operate on raw bytes). So this is supported as a side effect, not as a tested goal.
- **Macro expansion** — we still do not run cpp/clang. `UCLASS(...)` → spaces, not `→ public:` (the actual expansion). Reflection-introduced members (`StaticClass()`, `GetClass()`) remain invisible. This is the same documented limitation in `CppMacroStrip` Non-Goals.
- **`#define` extraction.** Macro bodies in headers are not parsed.
- **Other languages.** Rust, Go, Python, C#, Java have no equivalent problem; the new field lives under `[cpp]`.
- **Per-file overrides.** Per-root only, same as `macro_strip`.

---

## Architecture

### Where the strip happens

```mermaid
flowchart LR
    A[content: bytes] --> B[CppParser::preprocess]
    B --> C{macro_strip<br/>whole-word}
    C --> D{macro_strip_with_args<br/>balanced-paren NEW}
    D --> E[cleaned: Cow&lt;[u8]&gt;]
    E --> F[tree-sitter parse]
    F --> G[FileGraph]
```

`CppParser::preprocess` is the existing override of `LanguagePlugin::preprocess` introduced by `CppMacroStrip`. The new strip is a second pass *after* the existing whole-word pass. Order matters: the new pass needs to see un-mangled identifiers to find the macro name. Running the whole-word pass first cannot corrupt the new pass because whole-word and parameterized macro names are disjoint by definition — a macro listed in both fields is a config error (see [Decision 4](#decision-4-config-validation)).

Mutation strategy: same as `macro_strip`. The first pass either of the two strips returns a `Cow::Owned` and converts subsequent reads to operate on the owned buffer in place; if both lists are empty (the common non-UE case), `preprocess` returns `Cow::Borrowed` unchanged.

### The balanced-paren scanner

Both passes live in `crates/code-graph-lang-cpp/src/preprocess.rs` (the actual file housing `strip_macros`; the design previously referred to a non-existent `macro_strip.rs`). The new `strip_macros_with_args` function and its lexer helper `skip_lexical` are added to this file. **There is no existing `skip_lexical` to reuse** — the current `strip_macros` (whole-word) intentionally does NOT skip string/comment regions because whole-word replacement inside a string is harmless (`"CORE_API is great"` → `"          is great"`, still a valid string). The parameterized scanner cannot rely on that property; `skip_lexical` is written from scratch.

```rust
// Pseudo-Rust. Lives in crates/code-graph-lang-cpp/src/preprocess.rs

/// Replace each occurrence of `IDENT(...)` with same-length spaces, for
/// every IDENT in `tokens`. Returns the number of replacements performed
/// (zero on a no-op).
///
/// `tokens` keys are owned `Vec<u8>` so the inner loop can copy the
/// candidate identifier span into a stack `SmallVec<[u8; 64]>` and look
/// it up by reference without holding an immutable borrow into `content`
/// across the subsequent `find_balanced_close` mutable borrow. This is
/// the correct Rust idiom; the alternative (`tokens: &HashSet<&[u8]>`
/// with `id: &content[..]` held across `find_balanced_close`) does not
/// compile because of the simultaneous shared+mutable borrow on `content`.
fn strip_macros_with_args(content: &mut [u8], tokens: &HashSet<Vec<u8>>) -> usize {
    let mut i = 0;
    let mut replacements = 0;
    let mut id_buf: SmallVec<[u8; 64]> = SmallVec::new();
    while i < content.len() {
        // Skip strings, chars, comments — we don't rewrite inside them.
        if let Some(end) = skip_lexical(content, i) {
            i = end;
            continue;
        }
        if !is_ident_start(content[i]) {
            i += 1;
            continue;
        }
        let id_start = i;
        while i < content.len() && is_ident_continue(content[i]) {
            i += 1;
        }
        // Copy the candidate identifier so the borrow on `content` ends
        // before we mutate it.
        id_buf.clear();
        id_buf.extend_from_slice(&content[id_start..i]);
        if !tokens.contains(id_buf.as_slice()) {
            continue;
        }
        // Skip whitespace between identifier and `(`.
        let mut j = i;
        while j < content.len() && content[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= content.len() || content[j] != b'(' {
            continue; // bare identifier, not a parameterized macro use
        }
        let arg_end = match find_balanced_close(content, j) {
            Some(k) => k,
            None => continue, // unbalanced — bail, leave bytes intact
        };
        // Overwrite the matched span with spaces, BUT preserve `\n`
        // bytes so line numbers after a multi-line macro arg list stay
        // aligned with the original source. See "Key invariant" below.
        for b in &mut content[id_start..=arg_end] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
        replacements += 1;
        i = arg_end + 1;
    }
    replacements
}
```

### `skip_lexical` specification

`skip_lexical(content, i)` returns `Some(end)` if `content[i]` is the first byte of a lexical region (comment or string literal) and `end` is the byte position immediately past the closing delimiter, or `None` if `content[i]` is a normal code byte. The lexer state machine handles, in order:

1. **`//…` line comment.** Opens at `//`; closes at the next `\n`. A trailing `\` before the newline is **not** tracked as a line continuation (out of MVP scope; documented limitation: a `//` comment with a `\`-EOL continuation will be incorrectly treated as terminating at the first newline). Trigraphs are out of scope (`??/` is not detected).
2. **`/*…*/` block comment.** Opens at `/*`; closes at the first `*/`. Nesting is not supported (C++ doesn't support nested block comments).
3. **`"…"` string literal.** Opens at `"`; closes at the first `"` not preceded by an odd number of `\` bytes. Specifically: scan forward; when a `"` is seen, walk backward counting consecutive `\` bytes; if even (including zero), this is the closer. Standard C++ rule; correctly handles `\\"` (escaped backslash before close-quote) and `\"` (escaped quote).
4. **`'…'` char literal.** Same escape rules as `"…"`, closing on `'` rather than `"`. Multi-char escapes (`'\''`, `'\n'`, `'\x41'`) handled by the same odd-`\`-count rule.
5. **`R"DELIM(…)DELIM"` raw string literal.** Opens at `R"`. Extract the delimiter tag by scanning forward until the first `(` — the tag is at most 16 chars per the C++ standard but the scanner doesn't enforce that ceiling. Once the tag is captured, scan for the closing sequence `)DELIM"`. Bytes inside a raw string body are NOT subject to the `\` escape rule — only the literal close sequence terminates.

Comment precedence over string precedence: when `content[i] == b'/'`, check `content[i+1]` for `/` or `*` *before* treating `i` as a possible string open. A line comment cannot itself contain an unterminated string; only the next `\n` terminates the comment, regardless of `"` or `'` characters inside.

Bytes inside any of the five regions above are **never** stripped: `strip_macros_with_args` advances `i` past them by consulting `skip_lexical` at every iteration's top.

**Out-of-scope (documented limitations):**
- `\`-at-EOL line continuations (`// … \` then next line) — the scanner closes at the first `\n`.
- Trigraphs (`??(`, `??)`, `??=`, etc.) — C++17 removed them; C++14 and earlier accepted them. Treat as ordinary bytes.
- Digraphs (`<%`, `%>`, `:>`, `<:`, `%:`) — preserved as ordinary bytes; have no semantic meaning to the stripper.
- `#define` body multi-line continuations — out of scope; the stripper doesn't track preprocessor state.

**Key invariant: the fill preserves length.** Every byte between (and including) the identifier start and the matched close-paren becomes a space, **except `\n` bytes, which are preserved**. Byte offsets after the strip equal byte offsets before; line numbers (which tree-sitter and the parser report by counting `\n` bytes) also stay aligned, even when an arg list spans multiple source lines as in UE's `DECLARE_DELEGATE_TwoParams(\n  FOnHit,\n  AActor*, OtherActor)`. Column numbers within the macro line lose their original character meaning (everything is space) but the surrounding source's reported positions remain accurate — which is what tree-sitter, query results, and downstream symbol records need. The whole-word `strip_macros` doesn't need this carve-out because identifier patterns cannot contain `\n`.

### Raw-string collision (carried over from `CppMacroStrip`, with a two-pass twist)

The CppMacroStrip design documents a known limitation: a raw-string tag identical to a stripped macro token (e.g., `R"UCLASS(…)UCLASS"`) would have its delimiters overwritten, breaking tree-sitter's raw-string close detection and turning the rest of the file into an `ERROR` node. The new parameterized strip inherits the same limitation. CLAUDE.md C++ Limitation 7; no fix in scope for MVP.

**The two-pass interaction makes it slightly worse, in a way an implementor must understand:**

- Pass 1 (whole-word) rewrites the raw-string tag `UCLASS` → spaces *anywhere it appears as a bare token*, including inside a raw-string opener like `R"UCLASS(…)UCLASS"`. After pass 1, both the opening tag and the closing tag are spaces; tree-sitter's lexer can't close the raw string; the file becomes `ERROR`. (This is the existing CppMacroStrip behavior — pass 1 has no `skip_lexical`.)
- Pass 2's `skip_lexical` *would* skip raw-string bodies correctly if the tag were intact. But pass 1 may have already corrupted the tag, so pass 2's lexer can't even recognize the raw-string boundary; it sees ordinary bytes where the raw-string body used to be and may strip `UCLASS(…)` patterns *inside* the original raw-string content.

In practice, both passes are reading from / writing to the same buffer, and pass 1's corruption is locked in before pass 2 runs. The end result is the same as today on UE: a file with a raw-string tag that collides with a stripped macro produces zero symbols. The fix is identical for both passes: either rename the raw-string tag, or remove the colliding macro from both lists. The workaround is mentioned in CLAUDE.md C++ Limitation 7; the limitation entry must be updated to mention both `macro_strip` and `macro_strip_with_args` as triggers (currently it names only `macro_strip`).

### Config schema

```toml
[cpp]
# (existing) Whole-word identifier tokens.
macro_strip = ["ENGINE_API", "CORE_API", "COREUOBJECT_API"]

# (NEW) Identifier tokens that take parenthesized arguments. Listed names
# strip both the identifier AND the balanced `(...)` argument list that
# immediately follows (whitespace permitted between identifier and `(`).
# Each occurrence is replaced with the same number of space characters,
# preserving byte offsets, line numbers, and column numbers in resulting
# symbols. Use this for UE reflection / generated-body macros that confuse
# the tree-sitter-cpp grammar by interposing between attribute keywords
# and the class or member they decorate.
#
# Recommended UE preset (uncomment to enable):
#   macro_strip_with_args = [
#       # Reflection / metadata (UE4 + UE5):
#       "UCLASS", "USTRUCT", "UENUM", "UFUNCTION", "UPROPERTY",
#       "UINTERFACE", "UDELEGATE", "UPARAM", "UMETA",
#       # Generated-body markers:
#       "GENERATED_BODY", "GENERATED_UCLASS_BODY", "GENERATED_USTRUCT_BODY",
#       "GENERATED_UINTERFACE_BODY", "GENERATED_IINTERFACE_BODY",
#       # Delegate macro families:
#       "DECLARE_DYNAMIC_MULTICAST_DELEGATE",
#       "DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam",
#       "DECLARE_DYNAMIC_MULTICAST_DELEGATE_TwoParams",
#       "DECLARE_DYNAMIC_MULTICAST_DELEGATE_ThreeParams",
#       "DECLARE_DELEGATE", "DECLARE_DELEGATE_OneParam",
#       "DECLARE_DELEGATE_TwoParams", "DECLARE_DELEGATE_RetVal",
#       "DECLARE_MULTICAST_DELEGATE", "DECLARE_EVENT",
#   ]
# Additional macros commonly needed (add manually if your codebase uses them):
#   - "DEPRECATED" / "UE_DEPRECATED" (deprecation annotations with version+message args)
#   - "TEXT_MULTILINE", "NSLOCTEXT" (localization helpers at class scope)
# DO NOT add conditional-compilation macros like "WITH_EDITOR" / "WITH_EDITORONLY_DATA"
# to this list — they appear in `#if` contexts and are not always followed by `(`;
# stripping them breaks the parse. Bare attribute macros without args belong in
# `[cpp].macro_strip`, NOT this list.
macro_strip_with_args = []
```

### `RootConfig` field

```rust
// crates/code-graph-core/src/config.rs (in CppConfig)

#[derive(Debug, Default, Clone, serde::Deserialize)]
#[serde(default)]
pub struct CppConfig {
    pub macro_strip: Vec<String>,
    pub macro_strip_with_args: Vec<String>, // NEW
}
```

Validation in `RootConfig::load` (mirroring the existing `macro_strip` path):
- Empty-string entries dropped with `eprintln!` (the workspace channel for load-time diagnostics — CLAUDE.md "Core invariants: workspace deliberately has NO `tracing` dep"). NOT lowercased — C++ macro names are case-sensitive (`UCLASS` ≠ `uclass`); silently lowercasing would corrupt user config.
- Within-list duplicates silently deduped (paste-mistakes happen).
- Cross-list intersection (same token in both `macro_strip` AND `macro_strip_with_args`) is **conditionally** rejected — see [Decision 4](#decision-4-config-validation) for the bare-vs-parameterized resolution.

New `ConfigError` variant:

```rust
// crates/code-graph-core/src/config.rs

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    // ... existing variants ...
    #[error("[cpp] macro '{token}' may not appear in both `macro_strip` and `macro_strip_with_args` (ambiguous strip target — remove it from one list or the other)")]
    MacroStripConflict { token: String },
}
```

### Cache invalidation

Same rule as `macro_strip`. Changes to `[cpp].macro_strip_with_args` do not retroactively re-parse files with unchanged mtime. To apply a new entry, re-run `analyze_codebase` with `force=true`. Documented in CLAUDE.md "Cache invalidation" — needs one-line addendum: "Changes to `[cpp].macro_strip` or `[cpp].macro_strip_with_args` do NOT…".

---

## Design Decisions

### Decision 1: Separate config field, not extended syntax in `macro_strip`

**Context:** We could overload `macro_strip` to mean "strip whole-word IDENT, OR strip IDENT(...) if a trailing `()` is detected." Saves a field; user types fewer words.

**Options Considered:**
1. Single field, syntactic auto-detection (strip `IDENT(...)` if `IDENT` is in the list AND followed by `(`).
2. Single field with sigil (`"UCLASS()"` vs `"CORE_API"`).
3. Two fields (`macro_strip` and `macro_strip_with_args`).

**Decision:** Option 3.

**Rationale:** Option 1 changes the semantics of an existing field. A user with `macro_strip = ["MY_HELPER"]` who has a call `MY_HELPER(x)` would suddenly find that call stripped — silent breakage on upgrade. Option 2 is cute but invents syntax that doesn't appear elsewhere in TOML; copy/paste errors are likely. Option 3 is the maximally additive change: existing configs unchanged, new behavior opt-in, intent stays self-documenting in the field name. Documentation cost is the same.

### Decision 2: Strict balanced-paren matching, no recovery on unbalanced

**Context:** A real file may contain `#if 0 ... UCLASS( unbalanced` or a macro use spanning a comment that the lexer misreads. What to do?

**Options Considered:**
1. Best-effort match: strip up to end-of-line if no closing paren found in 200 bytes.
2. Bail: don't strip this occurrence, leave bytes intact, move on.
3. Bail and emit a warning to stderr.

**Decision:** Option 2.

**Rationale:** Option 1 risks mangling code that's already in `#if 0` — we don't track preprocessor state. The "occurrence" never executes anyway; leaving it alone is no worse than today (where the file is already broken by the same macro). Option 3 floods stderr on files with `#if 0` blocks containing macro lookalikes; the noise hides real failures. The `eprintln!` channel is precious; reserve it for failures the user can act on. The `replacements` counter is internal; if a user observes "this file should have indexed but didn't," the diagnostic path is "scan the file for `UCLASS(...` with unbalanced parens," which is straightforward.

### Decision 3: Whole-word pass runs before parameterized pass

**Context:** Order of the two passes affects whether `CORE_API UCLASS(X) class Foo {}` strips cleanly.

**Options Considered:**
1. Whole-word first, parameterized second.
2. Parameterized first, whole-word second.

**Decision:** Option 1.

**Rationale:** The two matchers are disjoint by definition — a macro listed in `macro_strip` is bare-identifier; a macro in `macro_strip_with_args` requires a following `(`. Running them in either order produces the same byte output for any valid config. We pick whole-word first because it's the cheaper pass (linear scan, no paren-walk), so on files with many bare-token hits the second pass operates on a smaller "still has something to find" set. Performance-irrelevant on most codebases; documented for clarity.

### Decision 4: Config validation

**Context:** What constitutes an invalid config? Two cases:
- Same identifier listed in both `macro_strip` and `macro_strip_with_args`. Ambiguous intent.
- Same identifier listed twice in `macro_strip_with_args`. Redundant but harmless.

**Decision:** Reject identifier-in-both with `ConfigError::MacroStripConflict { token }`. Silently deduplicate within `macro_strip_with_args`.

**Rationale:** The conflict case is real ambiguity — does the user want bare-token replacement *and* `IDENT(...)` replacement? The semantics differ (the parameterized form strips more). A loud error at config load (already the pattern for `extensions` collisions in `CppMacroStrip` per CLAUDE.md) protects users from a silent surprise. Within-list duplicates are paste-mistakes; silent dedup matches the `macro_strip` behavior and saves a low-value error message.

**Mixed bare-and-parameterized usage of the same token** (e.g., a codebase using both `GENERATED_BODY` bare and `GENERATED_BODY()` with parens): this is the realistic edge case the reject rule prevents. The intended workaround is:
- List the token in `macro_strip_with_args` only.
- The parameterized form (`GENERATED_BODY(...)`) strips correctly.
- The bare form (`GENERATED_BODY` with no following `(`) is left intact. This is harmless when the bare form is itself a no-op macro that disappears after preprocessing in the real build — tree-sitter parses it as an identifier reference, which doesn't interfere with the surrounding class declaration. If a specific UE version uses bare `GENERATED_BODY` in a position where tree-sitter trips on it, the user can confirm the failure mode and we can revisit allowing both lists with explicit precedence (e.g., "parameterized form takes precedence when both apply").

Test coverage: a fixture combining `GENERATED_BODY()` and bare `GENERATED_BODY` in the same file with the token in `macro_strip_with_args` — assert the class extracts correctly (the parameterized form strips, the bare form is benign). If this assertion fails on a real UE fixture, the design needs a follow-up to allow the same token in both lists.

### Decision 5: Cache invalidation on config change

**Context:** Changes to `macro_strip_with_args` must re-parse affected files to take effect; mtime-based cache stale-check doesn't notice.

**Decision:** Same rule as `macro_strip`: `force=true` on `analyze_codebase` to apply changes.

**Rationale:** Both fields are inputs to `preprocess`, which runs before parse. Either one is a "parse-output-defining" config; both need the same invalidation discipline. CLAUDE.md's existing wording covers both with a tiny edit. The alternative (auto-invalidate the cache on config change) requires hashing the config into the cache header; out of scope for MVP.

---

## Error Handling

| Failure | Detection | Response |
|---|---|---|
| Unbalanced `(` in macro use | `find_balanced_close` returns `None` | Leave bytes intact, don't strip this occurrence, continue scan. No log. |
| Identifier in both `macro_strip` and `macro_strip_with_args` | `RootConfig::load` validation | `ConfigError::MacroStripConflict { token }` — loud, blocks analyze, names the offending token. |
| Stripped macro name collides with raw-string delimiter tag | (existing limitation, documented) | Tree-sitter fails to close the raw string; rest of file becomes `ERROR`; file produces zero symbols. Workaround: rename the raw-string tag or drop the macro for this file. Same as `CppMacroStrip` limitation 7. |
| Macro inside `/* */` or `//` or string literal | `skip_lexical` advances past the lexical region | No strip happens inside. Correct by construction. |
| Macro spanning multiple source lines | `find_balanced_close` walks past newlines | Strips correctly. Documented as supported side effect. |
| Empty arg list (`GENERATED_BODY()`) | `find_balanced_close` returns the index of the `)` immediately after `(` | Strips correctly; replaces 16 bytes with 16 spaces. |

---

## Testing Strategy

### Unit tests (in `crates/code-graph-lang-cpp`)

1. `strip_macros_with_args_empty_args` — `GENERATED_BODY()` → 16 spaces, no symbol shift.
2. `strip_macros_with_args_complex_args` — `UCLASS(BlueprintType, meta=(BlueprintSpawnableComponent))` → all bytes spaces; class symbol below it on the line extracts correctly.
3. `strip_macros_with_args_string_literal_with_commas` — `UFUNCTION(BlueprintCallable, meta=(DisplayName="X, Y"))` → balanced-paren walker honors the string literal, doesn't bail on the inner comma.
4. `strip_macros_with_args_multiline` — `DECLARE_DELEGATE_TwoParams(\n    FOnHit,\n    AActor*, OtherActor)` → all bytes spaces, line offsets after the macro intact.
5. `strip_macros_with_args_inside_string_no_strip` — `const char* s = "UCLASS(Foo)"` → no rewrite.
6. `strip_macros_with_args_inside_comment_no_strip` — `// UCLASS(Foo)` and `/* UCLASS(Foo) */` → no rewrite.
7. `strip_macros_with_args_unbalanced_paren_bails` — `UCLASS(unclosed` → no rewrite, no log.
8. `strip_macros_with_args_whole_word_match_only` — `MY_UCLASS_HELPER(x)` does NOT match `UCLASS` in the list (whole-word prefix-boundary check); also `XUCLASS(x)` (suffix-side) does NOT match (whole-word boundary on both sides).
9. `strip_macros_with_args_disjoint_lists` — `macro_strip = ["ENGINE_API"], macro_strip_with_args = ["UCLASS"]`, fixture `UCLASS(X) class ENGINE_API AActor : public UObject {}` → both stripped, `AActor` symbol extracts with `Inherits → UObject`.
9a. `strip_macros_with_args_whitespace_before_paren` — `UCLASS (BlueprintType)` (with a literal space between identifier and `(`) → strips correctly. Older UE4 code is occasionally formatted this way.
9b. `strip_macros_with_args_user_function_named_like_macro` — fixture defining `void UCLASS(int x) {}` (a function named `UCLASS`) with `UCLASS` in `macro_strip_with_args` → function definition is stripped → zero `UCLASS` symbols extracted. Pinned as the **documented expected behavior**: users should not list macro names that collide with real function names in their codebase. The test prevents silent regression of the "user error → understandable failure" property.
9c. `strip_macros_with_args_generated_body_bare_and_parens` — same file mixing `GENERATED_BODY()` and bare `GENERATED_BODY` with the token in `macro_strip_with_args`. Asserts the surrounding class extracts correctly. If this test fails on a real UE fixture, [Decision 4](#decision-4-config-validation) must revisit.

### Integration tests (in `crates/code-graph-tools/tests/`)

10. `ue_fixture_extraction.rs` — synthetic UE-style fixture (Actor.h, Object.h, ActorComponent.h with `UCLASS`, `UFUNCTION`, `UPROPERTY`, `GENERATED_BODY()`, and `ENGINE_API`-style export macros). Recommend preset in `.code-graph.toml`. Index, then assert:
    - `search_symbols("^AActor$").total >= 1`
    - `search_symbols("^UObject$").total >= 1`
    - `get_class_hierarchy("UObject")` returns `AActor` and `UActorComponent` in `derived`.
    - `search_symbols("^Tick$")` finds the `AActor::Tick` method (parent = `AActor`).
11. `ue_fixture_no_config.rs` — same fixture, NO `macro_strip_with_args`. Assert: zero symbols extracted from `AActor` (today's behavior; pinned as the negative baseline). Removing this test on the "fix the parser the right way" project is the success criterion.

### Dogfood baseline (stretch — Phase 2)

12. Add a `external/UnrealEngine-Public-Plugin` (or similar small UE plugin with reflection macros — e.g., `OpenPF2`, `ChaosVD`) as an optional submodule, mirroring the existing `external/fmt` / `external/curl` / `external/abseil-cpp` pattern. Baseline at `crates/code-graph-lang-cpp/tests/baselines/ue-plugin.txt` with `symbols: N` from a known-good run. Auto-skip if uninitialized; CI may opt-in later. SHA bump protocol per CLAUDE.md.

### Snapshot tests

13. **Snapshot tests intentionally NOT added for the UE fixtures.** An early version of this design called for `crates/code-graph-tools/tests/snapshots/ue_fixture_extraction__snapshot.snap` capturing the full `get_class_hierarchy` and `get_file_symbols` output. The Phase 4 implementation chose explicit `assert!`/`assert_eq!` assertions against specific symbols/fields instead — and the chosen approach is the more durable one for this scenario, for two reasons:
    - **Semantic strength.** The explicit assertions check exactly what matters: `AActor` extracts, `UObject` extracts, the diamond walks correctly, `AActor::Tick` line numbers are preserved. A snapshot test pins the entire JSON wire shape including fields the test doesn't actually care about (`column`, `end_line`, `signature` formatting, namespace ordering), and would flap on irrelevant changes.
    - **Less brittle to unrelated growth.** Adding a new optional field to `get_class_hierarchy`'s response (a future polish item) would regenerate the snapshot but pass the explicit assertions unchanged. Snapshot regenerations carry no semantic signal — reviewers tend to blanket-accept rather than reviewing every field shift, defeating the snapshot's intended purpose.
    
    Wire-shape regression coverage is provided by the existing `tools_list` snapshots in `crates/code-graph-tools/tests/snapshots/` (which pin tool descriptions and parameter schemas) plus the response-shape snapshots from PaginatedResponseSizeSafety (which pin `Page<T>` and the `count_only` sentinel). A new fixture-specific snapshot would not add coverage those don't already provide.

### Structural Verification

- `cargo clippy --workspace --all-targets -- -D warnings` after every commit.
- The `strip_macros_with_args` byte-buffer mutation does involve some unsafe-adjacent patterns (slice indexing in a hot loop). Stay in safe Rust. Use `&mut [u8]` with bounds-checked indices, not pointer arithmetic.

### Anti-regression

14. The `ue_fixture_no_config.rs` test (item 11) is the explicit anti-regression for "the feature is opt-in and harmless when off."
15. The existing `fmt`, `curl`, `abseil-cpp` baselines (per CLAUDE.md "Dogfood-baseline submodules") MUST stay within ±10% of recorded symbol counts. The new pass is a no-op on configs without `macro_strip_with_args`; any drift there is a bug.

---

## Migration / Rollout

1. **PR 1: Config field + validation.** `RootConfig::CppConfig.macro_strip_with_args`, `ConfigError::MacroStripConflict` variant, sample in `.code-graph.toml.example`. Config-layer unit tests in this PR: (a) field round-trips through TOML deserialization, (b) empty-string entries dropped with `eprintln!`, (c) within-list dedup is silent, (d) cross-list conflict produces `MacroStripConflict { token }`. No `preprocess` change yet (the new field exists but isn't consumed).
2. **PR 2: Balanced-paren strip + `skip_lexical` + tests.** `strip_macros_with_args` and `skip_lexical` implementations in `crates/code-graph-lang-cpp/src/preprocess.rs`. Unit tests 1–9c. `CppParser::preprocess` wires the new pass (whole-word first, parameterized second).
3. **PR 3: UE integration tests + docs.** Tests 10 + 11 + 13. Adds the recommended UE preset to `.code-graph.toml.example`. CLAUDE.md updates: C++ Supported list gains "Parameterized API macros via `[cpp].macro_strip_with_args` (UE reflection macros)"; the **specific** Cache-invalidation sentence is edited from `"Changes to [cpp].macro_strip or [extensions]..."` to `"Changes to [cpp].macro_strip, [cpp].macro_strip_with_args, or [extensions]..."`; Limitation 7 (raw-string-delimiter collision) updated to mention both fields and the two-pass interaction (pass 1 may corrupt the tag before pass 2's `skip_lexical` runs).
4. **PR 4 (stretch): UE dogfood baseline.** Test 12, submodule pin, baseline file.

**Rollback:** Drop the `macro_strip_with_args` field from `RootConfig::CppConfig`, revert `CppParser::preprocess` to call only the existing pass. Users with the field configured get a TOML warning ("unknown field"); they remove it. Zero data corruption.

**User onboarding:** Document the recommended UE preset prominently in CLAUDE.md and `.code-graph.toml.example`. A UE user's first action after analyzing is "AActor isn't in the index?" → search for "UCLASS" in docs → find the preset → paste it → `analyze_codebase(force=true)`. Two-minute fix.
