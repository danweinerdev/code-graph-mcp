---
title: "C++ Macro-Strip — recover class extraction from API-export macros"
type: design
status: review
created: 2026-05-07
updated: 2026-05-07
tags: [cpp, tree-sitter, ue, unreal-engine, parser, config]
related:
  - Designs/RustRewrite
  - Plans/Complete/RustRewrite/01-Foundation-And-Cpp-Parser.md
---

# C++ Macro-Strip — recover class extraction from API-export macros

## Overview

The C++ parser silently drops every class declared with an API-export macro between `class` and the class name. `class CORE_API MyClass : public UObject {};` produces zero `Symbol` records, zero `Inherits` edges, and is invisible to every downstream tool (`get_class_hierarchy`, `get_callers`, `get_callees`, `get_orphans { kind: class }`, `generate_diagram`). Unreal Engine codebases use this pattern on virtually every class — the user-reported bug is "the C++ tool surface is unusable on UE."

The bug is a tree-sitter-cpp grammar limitation: `CORE_API` (an unknown identifier with no parentheses) lands in the `name: (type_identifier)` slot of `class_specifier`, and the rest of the declaration becomes an `ERROR` node that the parser's `has_error()` guard correctly drops. The grammar is at v0.23.4 (final release; no v0.24 coming) and has no user-macro mechanism. The bug must be fixed at the application layer.

This design adds a per-root `[cpp].macro_strip` config field listing identifier tokens to remove from C++ source bytes before tree-sitter parses them. Each occurrence is replaced with the same number of space characters, preserving every byte offset and therefore every line/column position in resulting symbols. Universal-ctags's `-I IDENTIFIER` flag (a 20-year-old precedent for the same problem) does the same thing.

The default behavior is unchanged — `macro_strip = []` is the implicit default and produces zero rewrites. UE users opt in by adding their module API macros to the list.

## Goals

1. **Make `class CORE_API MyClass : public UObject {};` produce a correct `MyClass` symbol with the correct `UObject` inherits edge** — measured against a hand-crafted UE-style fixture.
2. **Zero behavior change for users with no `macro_strip` configured** — the existing 49 corpus tests, the `fmtlib/fmt` baseline, and every existing snapshot must continue to pass byte-identical.
3. **Single, well-known config knob, not a heuristic.** Users explicitly list which identifiers to strip. No regex, no auto-detection, no `*_API` heuristic that could fire inside a real identifier.
4. **Preserve line/column fidelity in symbols extracted from macro-prefixed classes.** Replacement-with-spaces, not deletion, so `MyClass` on line 42 column 16 reports as such.

## Non-Goals

- **Full preprocessor expansion** — we are not running cpp/clang. Class-line API macros are the entire scope; function-body macros, conditional compilation, and macro-generated definitions stay in the existing limitations list.
- **Auto-detecting which macros to strip.** Universal-ctags makes the user list them; we do too. Heuristics like "any uppercase token ending in `_API`" risk eating real identifiers; literal-list matching is safer.
- **Fixing the other six C++ parser limitations** documented in `CLAUDE.md`. This design only addresses the macro-prefix bug.
- **Stripping function-line API macros** as a Phase-1 deliverable (`void CORE_API DoThing()`). The substitution mechanism handles them automatically as a side effect; explicit testing is in scope for Phase 1 only if no code change is needed.
- **Per-file overrides.** The macro list is per-root.

---

## Architecture

### Where the substitution happens

```mermaid
flowchart LR
    A[indexer.rs / watch.rs] -->|fs::read| B[content: bytes]
    B --> C[plugin.preprocess&#40;content, cfg&#41;]
    C -->|default impl: Cow::Borrowed| D[cleaned: Cow&lt;[u8]&gt;]
    C -->|CppParser override: strip_macros| D
    D --> E[plugin.parse_file&#40;path, &cleaned&#41;]
    E --> F[tree-sitter parse]
    F --> G[queries: definitions / inheritance / calls / includes]
    G --> H[FileGraph]
```

Substitution is exposed as a new **`preprocess` hook on `LanguagePlugin`** with a default implementation that returns the bytes unchanged. The C++ plugin overrides it to apply `strip_macros`. Indexer and watch handler both call `preprocess` then `parse_file`.

### Trait extension

Today: `LanguagePlugin::parse_file(&self, path: &Path, content: &[u8]) -> Result<FileGraph>` (`crates/codegraph-lang/src/lib.rs:298`).

After (additive — `parse_file` signature unchanged):

```rust
pub trait LanguagePlugin {
    // ... existing methods unchanged ...

    /// Pre-parse hook for byte-level transformations (macro stripping,
    /// preprocessor shims, etc.). Default impl borrows the input
    /// unchanged — zero-cost for plugins that don't need it.
    fn preprocess<'a>(&self, content: &'a [u8], _cfg: &RootConfig) -> Cow<'a, [u8]> {
        Cow::Borrowed(content)
    }

    fn parse_file(&self, path: &Path, content: &[u8]) -> Result<FileGraph>;
}
```

Three plugins (Rust, Go, Python) require **zero changes** — the default impl handles them. Both test stubs (`FakePlugin` at `crates/codegraph-lang/src/lib.rs:441-463`, `StubPlugin` at `crates/codegraph-tools/src/indexer.rs:319-353`) also require **zero changes**. Only the C++ plugin overrides `preprocess`. Indexer and watch handler each gain one line: `let cleaned = plugin.preprocess(&content, cfg); plugin.parse_file(path, &cleaned)`.

This is a strictly-additive trait extension. Future per-plugin preprocessing (e.g., a hypothetical Python `__future__` shim) would land in the same hook with no additional surface change.

### Config schema

```toml
# .code-graph.toml
[cpp]
# Identifier tokens to remove from C++ source bytes before tree-sitter
# parses them. Each occurrence is replaced with the same number of space
# characters so byte offsets, line numbers, and column numbers in
# resulting symbols are unchanged. Use this for API-export macros that
# confuse the tree-sitter-cpp grammar by occupying the position between
# `class` and the class name.
#
# Whole-word matching: a macro listed here is replaced only when bordered
# by non-identifier characters on both sides. `CORE_API_helper` is not
# touched even when `CORE_API` is in the list.
#
# Suggested values for Unreal Engine codebases (uncomment what applies):
# macro_strip = [
#   "CORE_API", "ENGINE_API", "UMG_API", "SLATE_API", "RENDERCORE_API",
#   "NIAGARA_API", "ONLINESUBSYSTEM_API", "GAMEPLAYABILITIES_API",
#   # Add your project's MODULE_API macros here.
# ]
#
# Default: omit the [cpp] section entirely (equivalent to macro_strip = []).
# After changing this list, re-run analyze_codebase with force=true to
# invalidate the mtime-based cache — config changes do not retroactively
# re-parse files whose mtime is unchanged.
# macro_strip = []
```

The `[cpp]` section is new. `macro_strip = []` is the implicit default — backward-compatible with every existing `.code-graph.toml`.

### Substitution algorithm

Pseudocode:

```
fn strip_macros(content: &[u8], macros: &[String]) -> Cow<'_, [u8]> {
    if macros.is_empty() { return Cow::Borrowed(content); }

    let mut out = content.to_vec();  // own a copy

    for macro_name in macros {
        let pat = macro_name.as_bytes();
        let mut i = 0;
        while let Some(pos) = find_subslice(&out[i..], pat) {
            let abs = i + pos;
            let preceded_by_identifier = abs > 0
                && is_ident_byte(out[abs - 1]);
            let followed_by_identifier = abs + pat.len() < out.len()
                && is_ident_byte(out[abs + pat.len()]);

            if !preceded_by_identifier && !followed_by_identifier {
                // Whole-word match — overwrite with spaces.
                for b in &mut out[abs..abs + pat.len()] { *b = b' '; }
            }
            i = abs + pat.len();  // continue past this position regardless
        }
    }

    Cow::Owned(out)
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
```

Key properties:

- **Empty macro list short-circuits to `Cow::Borrowed`** — zero allocation, zero scan cost. This is the path for every non-UE user.
- **Replacement preserves byte length exactly.** Tree-sitter offsets in the resulting AST map 1:1 to positions in the original source. `MyClass` at byte 28 in the original is at byte 28 in the cleaned bytes; the symbol's `line` and `column` are unchanged.
- **Whole-word check uses ASCII identifier rules** (`[A-Za-z0-9_]`). C++ identifiers can't contain non-ASCII characters in their core spelling, so this is sufficient. Names like `CORE_API_helper` (which contain `CORE_API` as a substring) are not matched because the `_` after `CORE_API` is an identifier byte. (`$` in identifiers is a GCC/Clang extension not used in Unreal Engine codebases; `is_ident_byte` intentionally excludes it. Users with `$`-bearing identifiers adjacent to stripped macros should not list those macros.)
- **Inside string literals and comments, substitution still fires** but is harmless: `"CORE_API is great"` becomes `"          is great"` (still a valid string), `// CORE_API foo` becomes `//          foo` (still a valid comment). Tree-sitter parses both identically as far as symbol/edge extraction is concerned.
- **Raw-string-literal delimiter caveat:** A raw string literal `R"tag(content)tag"` uses a user-defined `tag` (up to 16 chars) to delimit its body. If `tag` happens to match a macro in `macro_strip` (e.g., `R"CORE_API(content)CORE_API"` with `CORE_API` stripped), both delimiters become spaces and tree-sitter fails to close the raw string — the rest of the file becomes an `ERROR` node. This pattern does not occur in any known codebase (a raw-string tag that is also an API-export macro is contrived) but is a documented limitation.
- **Multi-pass over the byte vector** handles multiple macros per line. `class FOO_API BAR_EXTRA MyClass {};` with `macro_strip = ["FOO_API", "BAR_EXTRA"]` becomes `class                  MyClass {};` after both passes; tree-sitter then parses it correctly.
- **Prefix-overlap is safe in either order.** Worked example: with `macro_strip = ["FOO", "FOO_BAR"]` and source `class FOO MyClass {}; class FOO_BAR OtherClass {};`. Pass 1 (`FOO`) blanks the standalone `FOO` (preceded by space, followed by space — both word boundaries). The `FOO` inside `FOO_BAR` is NOT touched: the trailing `_` is an identifier byte, so the whole-word check rejects the match. Pass 2 (`FOO_BAR`) finds and blanks `FOO_BAR` cleanly. Both `MyClass` and `OtherClass` extract correctly. The whole-word check is what makes ordering safe; the algorithm does not require the user to list patterns in any particular order.
- **Performance:** O(n * m) where n is file size and m is the number of macros. With m typically < 20 and file sizes typically < 100 KB, the substitution cost is negligible relative to the tree-sitter parse that follows (also O(n)).

### Wire-format and downstream impact

After the fix, with `macro_strip = ["CORE_API"]` in `.code-graph.toml`:

| Tool | Before fix on UE class | After fix |
|------|------------------------|-----------|
| `get_file_symbols` on `MyActor.h` | empty results | full symbol list including `MyClass` (Class) |
| `get_class_hierarchy { class: "MyClass" }` | "class not found" with did-you-mean fallback | correct hierarchy tree with `UObject` parent edge |
| `get_callers { symbol: "MyClass::Tick" }` | empty results | actual callers |
| `get_orphans { kind: "class" }` | misses every UE class | reports only truly-orphan classes |
| `generate_diagram --inheritance` | broken UE inheritance edges | correct inheritance graph |

No tool-surface change. No new args. No new response shapes. The pagination/envelope work from PaginationOverhaul carries over unchanged — Phase 4's `get_class_hierarchy` envelope (`{hierarchy, truncated, max_nodes, total_nodes_seen}`) wraps a now-correctly-populated tree.

---

## Design Decisions

### Decision 1: Pre-parse byte substitution, not query-level workaround

**Context:** The bug surfaces in two queries (`class_specifier` definition at `queries.rs:32-34` and `class_specifier` inheritance at `queries.rs:91-101`). A query-level fix would either (a) pattern-match a known-bad shape and recover the real class name, or (b) walk the raw AST in Rust outside the query framework.

**Options Considered:**
1. **Query-level recovery.** Match a `class_specifier` whose name is a known macro token, then reach into sibling `ERROR` nodes to find the real class name. Tree-sitter query syntax does not directly support "look at the ERROR node next to this match" — recovery would require post-query Rust code that walks raw AST children. Per the research, the broken AST has no well-formed sibling structure: the class name is buried inside `ERROR > identifier`, with the inheritance list in another `ERROR > public > identifier` chain.
2. **Pre-parse byte substitution.** Replace macro tokens with spaces before tree-sitter parses. The grammar then sees `class            MyClass : public UObject {};` and parses it correctly. All existing queries fire unmodified.
3. **Upstream grammar fix.** Contribute a tree-sitter-cpp patch that treats unknown identifiers between `class` and the class name as transparent. Out of scope — tree-sitter-cpp v0.23.4 is the final release; upstream is dormant; a patch could take months and is not under our control.

**Decision:** Option 2. Pre-parse byte substitution.

**Rationale:** The AST is too broken to recover from. Tree-sitter's whole point is "give me an AST and I'll query it" — when the AST is wrong, queries are the wrong abstraction. Substitution gives tree-sitter clean input and lets all existing extraction logic (definitions, inheritance, calls, includes) work unchanged. Universal-ctags solved the same problem with the same approach 20 years ago via its `-I` flag; the precedent is strong. Option 1's post-query AST walking would add a fragile parallel codepath for one specific bug class; option 3 is not available.

### Decision 2: Literal identifier list, not regex

**Context:** UE module API macros all match the pattern `[A-Z][A-Z0-9_]*_API`. A regex like `\b[A-Z][A-Z0-9_]*_API\b` would catch every UE module's macro automatically without the user listing them.

**Options Considered:**
1. **Regex pattern with `_API` suffix heuristic.** Auto-strips any uppercase token ending in `_API`. Catches new modules without user intervention.
2. **Literal token list.** User explicitly names each macro. Predictable, safe, no false positives.
3. **Both — literal list + optional regex.** Maximum flexibility.

**Decision:** Option 2. Literal token list.

**Rationale:** The cost of a false positive is higher than the cost of listing modules explicitly. A regex like `[A-Z]+_API` would happily strip a real type named `OPENAL_API` (a third-party library type) or a constexpr constant. The user knows their macro list; we don't. Universal-ctags made the same call (`-I IDENTIFIER` is a list, not a pattern). Option 3 doubles the config surface for marginal benefit; if regex demand surfaces, it's an additive non-breaking extension later.

### Decision 3: Replace with spaces, not deletion

**Context:** A substitution that replaces `CORE_API` with empty string would shift every subsequent byte's offset, including the line/column tree-sitter reports for symbols. Replacement with the same number of spaces preserves offsets exactly.

**Options Considered:**
1. **Delete the macro entirely.** Shifts byte offsets; tree-sitter-reported line/column for `MyClass` no longer matches the original source. Symbol records would point to the wrong text in the user's editor.
2. **Replace with spaces.** Same byte count; offsets preserved; line numbers unchanged; column numbers unchanged.
3. **Track an offset map and translate AST positions back.** Most accurate but adds a non-trivial offset-translation layer to every symbol creation site.

**Decision:** Option 2. Replace with spaces.

**Rationale:** Option 2 is correct enough for every realistic use case. The only downside is that a user opening `MyActor.h` at the reported line/column sees `class            MyClass` rendered with whitespace where `CORE_API` used to be — but wait, no, they don't, because we're modifying the *bytes tree-sitter sees*, not the file on disk. The user's editor still shows the original source. The reported line/column is correct as-is. Option 3 is over-engineered for no observable benefit.

### Decision 4: `preprocess` hook on `LanguagePlugin` with default impl, NOT a `parse_file` signature change

**Context:** Three trait-extension shapes were considered. The C++ plugin needs access to the macro list at parse time.

**Options Considered:**
1. **Add a `preprocess(content: &[u8], cfg: &RootConfig) -> Cow<[u8]>` method to `LanguagePlugin` with a default impl returning `Cow::Borrowed(content)`.** Indexer calls preprocess then parse_file. Two-step abstraction; preprocess is a public extension point.
2. **Extend `parse_file` signature to take `&RootConfig`.** Single trait method changes; preprocess logic lives inside each plugin's `parse_file`. The trait change ripples to all four plugins, both test stubs (`FakePlugin`, `StubPlugin`), the indexer call site, and the watch handler call site at `crates/codegraph-tools/src/handlers/watch.rs:310`.
3. **Configure plugins at construction time.** `LanguageRegistry::default_with_config(&RootConfig)`. Plugins cache the config. Re-construct registry when config changes.

**Decision:** Option 1. `preprocess` hook with default impl.

**Rationale:** Option 1 has the smallest blast radius — the trait extension is strictly additive (default impl handles every existing plugin and stub for free), and only the C++ plugin overrides. Three production plugins, two test stubs, and zero corpus tests need updates. The two call sites (indexer + watch handler) each gain one line: `let cleaned = plugin.preprocess(&content, cfg);` followed by passing `&cleaned` to the existing `parse_file`. Option 2 was originally chosen on a "future-proofing" argument but Decision 6 in this same design rejects pre-emptive abstraction as YAGNI; applying that consistency here, "the preprocess hook is the only consumer" doesn't argue *against* a hook — it argues for the smallest hook possible. Option 3 ties config into construction-time data, but the config can change between `analyze_codebase` calls (the user edits `.code-graph.toml` and re-indexes); reconstructing the registry on every analyze is more invasive than per-call dispatch.

### Decision 5: Default behavior is "do nothing" — opt-in only

**Context:** Should the design ship with a default UE macro list (e.g., the well-known Epic module APIs) so out-of-the-box UE indexing works?

**Options Considered:**
1. **Ship a default list of UE module API macros.** Users with UE codebases get correct indexing without configuration.
2. **Ship empty default; users opt in.** Existing users see zero behavior change; UE users add `macro_strip` themselves.

**Decision:** Option 2. Empty default.

**Rationale:** A default list would mean a user with a non-UE codebase that happens to define a type or constant called `CORE_API` (for whatever reason) would see it silently stripped from their source. The `*_API` pattern is not formally namespaced; a non-UE library that uses `CORE_API` as a type or constant name (rare but possible) would silently lose its symbols with a non-empty default. The blast radius of the wrong default is "your symbols disappear and you don't know why." The blast radius of opt-in is "you have to read the docs once." Opt-in wins. Documentation will list the suggested UE macro set in commented-out form in the sample `.code-graph.toml` and in the CLAUDE.md `[cpp]` section, so the friction for a UE user is "uncomment three lines."

### Decision 6: Single `[cpp]` section, not nested

**Context:** Future C++-specific config might include other knobs (e.g., a setting for templated method call resolution). Should the schema anticipate that with a `[cpp.parsing]` or similar nested structure?

**Decision:** Single flat `[cpp]` section with `macro_strip` as the only field.

**Rationale:** YAGNI. Adding a future field is a non-breaking schema change. Pre-emptive nesting adds visual complexity now for hypothetical future organization. If future C++ knobs cluster naturally into sub-categories, the section can be reorganized later (also a non-breaking change because TOML is flexible about table layout and `#[serde(default)]` propagates).

---

## Error Handling

- **Empty `macro_strip` list:** `strip_macros` returns `Cow::Borrowed(content)` immediately. No allocation, no scan, no behavior change. This is the path for every existing user.
- **Macro string is empty (`""` in the list):** **Filtered at config-load time.** When `CppConfig` is deserialized, a normalization pass drains entries where `s.is_empty()` and emits a `tracing::warn!` per dropped entry. This is not a debug assertion (would only fire in debug builds and silently hang in release on an infinite loop) — it's an unconditional filter. The substitution loop is allowed to assume every pattern has length > 0.
- **Macro string contains non-identifier characters (e.g., `"CORE_API()"`):** The substitution does literal byte-equality matching, not regex; it would happily replace `CORE_API()` if it appeared verbatim. Documentation warns against listing patterns that aren't bare identifiers.
- **`.code-graph.toml` parse error in `[cpp]` section:** Existing `RootConfig::load` error path — `analyze_codebase` returns the toml parse error to the agent. No change.
- **No `[cpp]` section in `.code-graph.toml`:** Resolves to `CppConfig::default()` (empty `macro_strip`). Backward-compatible.
- **Substitution produces an unexpected result for some pattern:** e.g., substituting `MyClass` itself (user error). The class would simply not be extracted. This is the user listing the wrong identifier, not a system bug. Documentation will warn against listing non-macro identifiers.
- **Raw-string-literal delimiter collision** (described in the architecture section): a raw string `R"CORE_API(content)CORE_API"` with `CORE_API` in `macro_strip` will have its delimiters corrupted, producing an `ERROR` node from that point through end-of-file. The pattern does not occur in any known codebase. If a user reports it, the workaround is to remove that specific macro from `macro_strip` for the affected file (or rename the raw-string tag in the source). Not silently destructive — the resulting parse failure produces zero symbols for the file, which is observable.
- **Cache invalidation:** `analyze_codebase` uses an mtime-based stale check (`stale_paths` at `crates/codegraph-tools/src/handlers/analyze.rs:89-117`); changes to `macro_strip` between two `analyze_codebase` calls do NOT retroactively invalidate cached symbols for files whose mtime hasn't changed. Users who add macros to the list after an initial index must re-run `analyze_codebase` with `force=true` to re-parse all files. Documented in the sample `.code-graph.toml` comment block.

No new error paths surface to MCP callers. The substitution layer is silent on success and silent on no-op; the only way it can affect tool output is by producing different (correct, after this fix) symbols for macro-prefixed classes.

---

## Testing Strategy

### Unit tests in `codegraph-lang-cpp`

A new test module (or extension to `tests/corpus.rs`) covering the substitution layer + the recovered extraction:

- **`class_with_single_api_macro_extracts_correctly`** — `class CORE_API MyClass : public UObject {};` with `macro_strip = ["CORE_API"]` produces a `MyClass` symbol with parent `UObject` inherits edge.
- **`class_with_two_api_macros_extracts_correctly`** — `class FOO_API BAR_EXTRA MyClass : public Base {};` with both macros listed produces `MyClass` correctly.
- **`class_with_unlisted_macro_still_broken`** — confirms opt-in: `class CORE_API MyClass {};` with empty `macro_strip` produces zero symbols (preserves the existing buggy behavior for users who haven't opted in). Anti-regression for Decision 5.
- **`uclass_above_macro_class_extracts_correctly`** — `UCLASS()\nclass CORE_API MyClass : public UObject {};` produces both the `UCLASS()` macro call edge AND the `MyClass` symbol.
- **`function_with_inline_api_macro`** — `void CORE_API DoThing();` with `macro_strip = ["CORE_API"]` produces a `DoThing` function symbol. Free side-effect of the substitution.
- **`api_macro_inside_string_literal_unaffected`** — `const char* msg = "CORE_API is great";` with `macro_strip = ["CORE_API"]` parses correctly; no symbol-level effect.
- **`api_macro_inside_raw_string_literal_unaffected`** — `const char* s = R"(CORE_API is in a raw string)";` with `macro_strip = ["CORE_API"]` parses correctly; the string content is opaque to extraction. Confirms the safe case for raw strings (the unsafe case — macro-as-tag — is documented as a limitation, not a passing test).
- **`identifier_containing_macro_substring_unchanged`** — `void CORE_API_helper() {}` with `macro_strip = ["CORE_API"]` produces a `CORE_API_helper` function symbol unchanged. Validates the whole-word check.
- **`prefix_overlap_macros_both_extract_correctly`** — `class FOO MyClass {}; class FOO_BAR OtherClass {};` with `macro_strip = ["FOO", "FOO_BAR"]` produces both `MyClass` and `OtherClass`. Validates the worked-example claim from the architecture section that whole-word matching makes prefix-overlap order-safe. Run with both `["FOO", "FOO_BAR"]` and `["FOO_BAR", "FOO"]` orderings — both must produce the same result.
- **`empty_macro_list_short_circuits`** — confirm via either a property (no allocation when list is empty — could be a benchmark) or by behavioral equivalence (parse output identical to pre-fix code path).
- **`empty_string_macro_filtered_out_at_config_load`** — `CppConfig` deserialized from `macro_strip = ["", "CORE_API", ""]` resolves to `["CORE_API"]` (empty entries dropped). Anti-regression for the infinite-loop risk noted in Error Handling.
- **`byte_offset_preservation`** — parse a class with a macro prefix; assert the symbol's `line` and `column` match the position of `MyClass` in the *original* source bytes. Documents Decision 3.

### Snapshot test (end-to-end)

A new fixture `testdata/ue/MyActor.h` containing 4–6 representative classes:

```cpp
// Synthetic UE-style header — not actual UE code, hand-crafted for testing.
class CORE_API AActor : public UObject { ... };
class ENGINE_API APawn : public AActor { ... };
class GAMEPLAY_API ACharacter : public APawn { ... };
class FOO_API BAR_EXTRA UDoubleMacro : public AActor { ... };
class UNoMacro : public AActor { ... };  // baseline: still works without strip
```

Plus a `.code-graph.toml` with `macro_strip = ["CORE_API", "ENGINE_API", "GAMEPLAY_API", "FOO_API", "BAR_EXTRA"]`.

New snapshot `response_get_class_hierarchy_ue_aactor.snap`:
- Confirms `AActor` is recognized as a class with `UObject` base.
- Confirms `APawn`, `ACharacter` chain inheritance.
- Confirms `UDoubleMacro` extracts with both macros stripped.
- Confirms `UNoMacro` is unaffected.

### Anti-regression suite

- **All 49 existing C++ corpus tests pass unchanged.** No fixture in the existing corpus uses macro-prefixed classes; verified in research. The new behavior is additive.
- **Existing `engine.cpp` snapshots pass unchanged** (`response_get_class_hierarchy_engine.snap` and friends). The fixture has no API macros; no `macro_strip` in its `.code-graph.toml`.
- **`fmtlib/fmt` baseline parse-test produces 32 symbols, 244 edges** (the Phase 1 baseline). fmt does not use UE-style macros. No regression.

### End-to-end integration test (config flows through pipeline)

A new test in `crates/codegraph-tools/tests/` that calls `index_directory` (or `analyze_codebase`) against a fixture directory containing `MyActor.h` (the UE-style snapshot fixture) AND a `.code-graph.toml` with `macro_strip = ["CORE_API", "ENGINE_API", …]`. Asserts the macro-prefixed class symbols appear in the resulting `Graph`. This closes the "config actually threads end-to-end through indexer → preprocess → parse_file" gap that pure unit tests don't cover — a future refactor that accidentally passes `RootConfig::default()` everywhere would make all unit tests pass but break the production pipeline.

A parallel watch-mode test should confirm that `try_reindex_file` also picks up the cached `macro_strip` from `inner.config` and applies preprocessing on file change events.

### Plugin trait change verification

- All 4 language plugins (cpp, rust, go, python) compile and pass tests with the additive `preprocess` trait method (default impl handles three; only C++ overrides).
- Both test stubs (`FakePlugin`, `StubPlugin`) compile and pass with no changes — they inherit the default impl.
- The indexer's call site at `crates/codegraph-tools/src/indexer.rs:158-161` is updated to call `preprocess` then `parse_file`; the watch handler's call site at `crates/codegraph-tools/src/handlers/watch.rs:310` is updated similarly.
- `cargo test --workspace` passes; no test stub or out-of-tree implementor is broken.

### Optional: UE dogfood baseline

Following the Phase-7 retro pattern (`requests@v2.32.3` for Python), a future PR could add a UE dogfood baseline: clone a small public UE-derived header set at a pinned tag, run parse-test with a UE-tuned `.code-graph.toml`, and gate the result at ±10% with `#[ignore]` + graceful skip on missing fixture. This is **not** in scope for the design's first implementation phase — Epic licensing makes vendoring difficult and the synthetic fixture above already covers the test-correctness need. Track as a follow-on.

### Structural Verification

Per `shared/languages/rust.md`:

- **`cargo clippy --workspace --all-targets -- -D warnings`** must pass on every commit. No `#[allow]` to suppress findings on the substitution function or the trait change.
- **`cargo fmt --all --check`** must pass.
- **`cargo test --workspace`** — full suite, including new corpus tests, snapshot test, and the trait-signature changes propagating through every plugin.
- **`cargo insta review`** — review the one new snapshot (`response_get_class_hierarchy_ue_aactor`); no existing snapshots should regenerate.

No `unsafe` introduced; `miri` not required.

---

## Migration / Rollout

### Single-PR delivery

The trait change ripples across all four language plugins plus the indexer; splitting into multiple PRs would leave the workspace in a non-buildable intermediate state. Single-PR (one commit, or a small phase-grouped commit chain) is the right scope.

### Backward compatibility

- **Config side: fully backward-compatible.** Existing `.code-graph.toml` files have no `[cpp]` section; that resolves to `CppConfig::default()` with empty `macro_strip`; behavior is identical to today.
- **Plugin trait side: strictly additive.** The new `preprocess` method has a default implementation (`Cow::Borrowed(content)`). Out-of-tree implementors compile and behave identically; no signature change to `parse_file`. In-tree: only the C++ plugin overrides `preprocess`. The two test stubs (`FakePlugin` at `crates/codegraph-lang/src/lib.rs:441-463`, `StubPlugin` at `crates/codegraph-tools/src/indexer.rs:319-353`) inherit the default impl and require no changes. The three other production plugins (rust/go/python) inherit the default impl and require no changes.
- **Wire format: completely unchanged.** Tools, args, response shapes, and snapshots for every existing fixture are untouched.

### Order of operations within the PR

1. Add `CppConfig` to `crates/codegraph-core/src/config.rs`. Add the `cpp: CppConfig` field to `RootConfig` with `#[serde(default)]`. Implement a custom deserializer (or a post-load normalization in `RootConfig::load`) that drains empty-string entries from `macro_strip` with a `tracing::warn!` per dropped entry — the substitution loop assumes every pattern has length > 0.
2. Implement `strip_macros(content: &[u8], macros: &[String]) -> Cow<'_, [u8]>` as a free function (or associated function on `CppParser`) in `crates/codegraph-lang-cpp/src/`. Unit tests for the substitution algorithm itself (whole-word boundary, multiple-macro multi-pass, prefix-overlap order safety, empty-list short-circuit, byte-offset preservation, raw-string-tag-doesn't-match-macro positive case).
3. Add the `preprocess` default-impl method to the `LanguagePlugin` trait at `crates/codegraph-lang/src/lib.rs:298`. Override it on `CppParser` to call `strip_macros(content, &cfg.cpp.macro_strip)`. **No other plugin or stub needs changes** — the default impl handles them all.
4. Update the indexer at `crates/codegraph-tools/src/indexer.rs:158-161` to call `let cleaned = plugin.preprocess(&content, cfg);` then pass `&cleaned` to `parse_file`.
5. Update the watch handler at `crates/codegraph-tools/src/handlers/watch.rs:310` similarly: insert a `preprocess` call between the `fs::read` and the existing `plugin.parse_file(&path_owned, &content)`. The cached `inner.config.read()` (which `try_reindex_file` already accesses at line ~260) is the source of `cfg`.
6. Add the UE-style snapshot fixture (`testdata/ue/MyActor.h`) and the snapshot test.
7. Add the integration-level test in `codegraph-tools` that runs `index_directory` against the UE fixture with a `.code-graph.toml` containing `macro_strip` configured, asserting the macro-prefixed class symbols appear end-to-end (closes the "config actually flows through to the C++ plugin" gap).
8. Update `.code-graph.toml` sample at the repo root with the commented-out UE macro list and the cache-invalidation note.
9. Update `CLAUDE.md`:
   - Configuration section: document the new `[cpp]` schema and the cache-invalidation behavior (re-run `analyze_codebase` with `force=true` after editing `macro_strip`).
   - C++ Parser Limitations section: note that macro-prefixed class extraction is supported via `[cpp].macro_strip` config, with the raw-string-delimiter caveat as a documented limitation.
10. Update `crates/codegraph-lang-cpp/src/lib.rs` doc comments where they describe parser behavior.

### Documentation surfaces

The design touches three documentation surfaces and they must agree:

- **`.code-graph.toml` sample** — shows the `[cpp]` section with commented-out UE macros and a one-paragraph explanation.
- **`CLAUDE.md` Configuration section** — formal schema for the `[cpp]` section.
- **`CLAUDE.md` C++ Parser Limitations section** — note that macro-prefixed classes are now supported via `macro_strip` config. Update or replace the entry that today says "Macro-generated definitions are not visible" if the wording overlaps (it does not strictly: macro-generated *definitions* and macro-prefixed *declarations* are different, and only the latter is fixed here).

### Suggested macro lists for common projects

Documentation should include a practical opt-in shortcut for the common UE case:

```toml
[cpp]
macro_strip = [
  "CORE_API", "ENGINE_API", "UMG_API", "SLATE_API",
  "RENDERCORE_API", "NIAGARA_API", "ONLINESUBSYSTEM_API",
  "GAMEPLAYABILITIES_API", "CHAOS_API", "PHYSX_API",
  # Project-local: list your <PROJECTNAME>_API macros here
  "MYGAME_API",
]
```

Users with codebases that don't use UE conventions (any other custom macro shop) just substitute their own macro identifiers. The mechanism is project-agnostic.

### Follow-on work (not in this design)

- **Auto-detection / regex support** if user demand surfaces. Additive non-breaking extension.
- **UE dogfood baseline** with a publicly-available representative header set, following the `requests@v2.32.3` pattern from RustRewrite Phase 7.
- **Function-line API macro stripping documentation** — confirm `void CORE_API DoThing()` works as a side effect of class-line substitution (it should), document if so.
