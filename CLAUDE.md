# CLAUDE.md

## Project: code-graph-mcp

Rust MCP server that builds an in-memory semantic code graph from source files using tree-sitter, exposing graph query tools for AI agents over stdio.

## Build

```bash
make build                                            # cargo build --release -p code-graph-mcp
make test                                             # cargo test --workspace
make lint                                             # cargo clippy --workspace --all-targets -- -D warnings
make fmt-check                                        # cargo fmt --all --check
make snapshot-clean                                   # fail if any *.snap.new files exist (stale insta snapshots)
make snapshot-audit ARGS="<fragments>"                # fail if changed snapshots don't match expected name fragments

# Or invoke cargo directly:
cargo build --release -p code-graph-mcp               # host-target release binary
cargo test --workspace                                # full workspace test suite
cargo clippy --workspace --all-targets -- -D warnings # lint
cargo fmt --check                                     # format check
```

No CGo or C toolchain required — `tree-sitter-cpp` (and the other tree-sitter grammars) link via their pure-Rust `cc`-built crates. Build natively on each platform you need the binary for; there is no cross-compile pipeline.

### One-time setup: install pre-commit hooks

Run `make install-hooks` once after cloning. This sets `git config core.hooksPath scripts/hooks`, pointing git at the tracked hook scripts under `scripts/hooks/` so commits run pre-commit checks automatically. Currently the only check is `make snapshot-clean` — a `git commit` is refused if any `*.snap.new` files remain in the working tree (run `cargo insta review` to accept or reject pending snapshots first). Bypass with `git commit --no-verify` only when you understand the consequences (committing with pending snapshots means the recorded snapshot is stale and CI will fail on a clean checkout).

### Optional: dogfood-baseline submodules

Each language plugin has a dogfood-baseline test that parses a real upstream repo and asserts the symbol count stays within ±10% of a recorded baseline. The repos are git submodules under `external/`, pinned by tag. Tests **auto-skip** with an `eprintln!` setup hint when the submodule is not initialized — they do NOT panic and do NOT need `--ignored` to opt in. Run them by initializing the submodule(s) you care about:

```bash
make submodules                                       # init all six (shallow clones)
git submodule update --init external/ripgrep          # or just one
```

| Language | Submodule | Pin | Baseline file |
|----------|-----------|-----|---------------|
| Rust | `external/ripgrep` (BurntSushi/ripgrep) | `15.1.0` | `testdata/rust/ripgrep-baseline.txt` |
| Go | `external/logrus` (sirupsen/logrus) | `v1.9.4` | `testdata/go/logrus-baseline.txt` |
| Python | `external/requests` (psf/requests) | `v2.33.1` | `testdata/python/requests-baseline.txt` |
| C++ | `external/fmt` (fmtlib/fmt) | `12.1.0` | `crates/code-graph-lang-cpp/tests/baselines/fmt.txt` |
| C++ | `external/curl` (curl/curl) | `curl-8_20_0` | `crates/code-graph-lang-cpp/tests/baselines/curl.txt` |
| C++ | `external/abseil-cpp` (abseil/abseil-cpp) | `20260107.1` | `crates/code-graph-lang-cpp/tests/baselines/abseil-cpp.txt` |

**Drift expectation when bumping a submodule SHA:** the symbol count almost always shifts. Re-measure with the bumped SHA and update the baseline file's `symbols: N` line + the `tag:` / `commit:` headers in the same commit as the SHA bump. The baseline assertion uses ±10% tolerance, so small drift may pass without an update — but the headers should still match the pinned commit so future readers can tell what was measured. The fmt/curl/abseil baselines deliberately live next to their tests under `crates/code-graph-lang-cpp/tests/baselines/` rather than `testdata/cpp/` because they're tied to the `external/` submodule version, not the in-tree synthetic fixtures the rest of `testdata/cpp/` covers.

curl is primarily C; tree-sitter-cpp parses C as a (mostly compatible) superset and the C++ plugin filters out ERROR nodes, so the per-file parse always succeeds even when the file uses idiomatic C constructs. The aggregate symbol count is the regression contract — whatever tree-sitter-cpp could extract is what the baseline locks in.

## Test

```bash
# Full workspace test suite (unit + integration tests)
cargo test --workspace

# Run a single crate's tests
cargo test -p code-graph-tools

# Parse-test harness (manual inspection: parse one file/dir and dump symbols+edges)
cargo run -p code-graph-parse-test -- <directory>
```

## Architecture

The workspace is split into language-agnostic core crates plus per-language plugin crates:

| Crate | Responsibility |
|-------|----------------|
| `crates/code-graph-mcp` | Binary — `rmcp`-based stdio MCP server entry point |
| `crates/code-graph-core` | Shared types (`Symbol`, `Edge`, `SymbolKind`, `EdgeKind`), `RootConfig` for `.code-graph.toml` |
| `crates/code-graph-lang` | `LanguagePlugin` trait + `LanguageRegistry` (extension → plugin dispatch) |
| `crates/code-graph-graph` | In-memory `Graph` (nodes, forward + reverse adjacency, file index), JSON cache persistence |
| `crates/code-graph-tools` | Tool handlers, parallel `analyze_codebase` discovery + indexer, watcher (notify-debouncer-full) |
| `crates/code-graph-lang-cpp` | C++ language plugin — tree-sitter-cpp queries + scope-aware call resolution |
| `crates/code-graph-lang-rust` | Rust language plugin — tree-sitter-rust queries; impl/trait extraction, use-tree expansion, macro invocation calls |
| `crates/code-graph-lang-go` | Go language plugin — tree-sitter-go queries; method-receiver extraction, all import forms (single/grouped/aliased/dot/blank), direct + selector_expression calls |
| `crates/code-graph-lang-python` | Python language plugin — tree-sitter-python queries; class/method/decorator handling, both import forms (`import` + `from … import`), multi-base inheritance, `.py` and `.pyi` |

As of the Phase 7 cutover, **all four languages — C++, Rust, Go, and Python — are live in the binary**. Cross-language collisions (e.g. an `init` symbol that exists in C++, Go, and Python) stay isolated via the `(Language, name)`-keyed `SymbolIndex` at `crates/code-graph-lang/src/lib.rs:116`.

```
AI Agent <-stdio/MCP-> [code-graph-mcp (rmcp server)]
                              |
                     +--------+--------+
                     |                 |
              [Tool Handlers]    [Graph]
              (code-graph-tools)  (code-graph-graph)
                     |                 |
              [LanguageRegistry] [In-memory graph + JSON cache]
              (code-graph-lang)
                     |
              [C++ Plugin] [Rust Plugin] [Go Plugin] [Python Plugin]
              (code-graph-lang-cpp, code-graph-lang-rust, code-graph-lang-go, code-graph-lang-python)
                     |
              [tree-sitter + tree-sitter-cpp + tree-sitter-rust + tree-sitter-go + tree-sitter-python]
```

## Configuration

Each indexed root may contain a `.code-graph.toml` controlling discovery and parsing knobs. The file is read once per `analyze_codebase` call from `<root>/.code-graph.toml` and cached on the server for subsequent watch events.

Schema (all keys optional; defaults shown):

```toml
[discovery]
# Glob patterns added to the default ignore set (.git, target, node_modules, etc.).
# Patterns follow the `ignore` crate's gitignore syntax.
extra_ignores = []

# Maximum parallel discovery threads (0 = num_cpus).
max_threads = 0

[parsing]
# Maximum parallel parse threads (0 = num_cpus). The two thread pools share
# the host concurrency budget — the indexer caps the sum at num_cpus.
max_threads = 0

[cpp]
# Identifier tokens to remove from C++ source bytes before tree-sitter parses
# them. Each occurrence is whole-word matched (bordered by non-identifier
# bytes on both sides) and overwritten with spaces of the same length, so
# byte offsets, line numbers, and column numbers reported in extracted
# symbols match the original source. Default: `[]` (no rewriting).
#
# Use this for API-export macros that confuse the tree-sitter-cpp grammar
# by occupying the position between `class` and the class name. UE example:
#
#   [cpp]
#   macro_strip = ["CORE_API", "ENGINE_API", "UMG_API", "MYGAME_API"]
#
# With the list above, `class CORE_API AActor : public UObject {};` extracts
# correctly as a Class symbol with a UObject `Inherits` edge.
macro_strip = []

[extensions]
# Per-language file-extension overrides, layered on top of each plugin's
# built-in extension list. Three semantics:
#
#   1. <lang> lists ADD extensions to that language's claim. A file matching
#      `[extensions].cpp` dispatches to the C++ plugin even when no
#      plugin's defaults claim it.
#   2. A user addition WINS over a default-claim collision. If
#      `[extensions].python = [".h"]`, `.h` files dispatch to Python even
#      though C++ defaults claim `.h`. Two `[extensions].<lang>` lists
#      claiming the same extension is a load-time error (no tiebreak).
#   3. `disabled` SUPPRESSES extensions entirely, regardless of which
#      plugin or override would claim them. `disabled` wins over both
#      defaults and additions.
#
# Each entry must start with `.` and is lowercased at load. Empty strings
# are dropped with an `eprintln!` notice. Built-in defaults per language:
# cpp = [.cpp .cc .cxx .c .h .hpp .hxx], rust = [.rs], go = [.go],
# python = [.py .pyi].
disabled = []
cpp = []
rust = []
go = []
python = []
```

A sample `.code-graph.toml.example` ships at the repo root; copy it to `.code-graph.toml` in any indexed root and customize as needed.

### Cache invalidation

Changes to `[cpp].macro_strip` between `analyze_codebase` calls do NOT retroactively re-parse files whose mtime is unchanged (the cache uses mtime-based stale checking). To apply a new `macro_strip` list to already-indexed files, re-run `analyze_codebase` with `force=true`.

Same caveat applies to `[extensions]` changes. Adding a new extension to `[extensions].<lang>` brings new files into discovery and they're parsed normally. But moving an extension *into* `[extensions].disabled` does NOT remove already-cached entries for those files — the cache file retains them until re-run with `force=true`. The watch path picks up `[extensions]` changes immediately for new events (it consults the cached `RootConfig`'s extensions slice on every reindex), so files that move into `disabled` simply stop reindexing on subsequent edits, but their pre-existing graph entries persist until the next forced rebuild.

## MCP Tools (15 total)

The tool surface mirrors the Go implementation; the PaginationOverhaul plan retrofitted defensive caps and pagination envelopes for UE-scale codebases. Tools are grouped by purpose:

**Indexing:** `analyze_codebase` (with JSON cache + mtime-based incremental re-index)
**Symbol queries:** `get_file_symbols` (with `top_level_only`/`brief` + `limit`/`offset`, default limit 100, max 1000), `search_symbols` (with `namespace`, `limit`/`offset`, `brief` default true), `get_symbol_detail`, `get_symbol_summary` (counts by namespace/kind)
**Call graph:** `get_callers`, `get_callees` (both with `limit`/`offset`, default limit 100, max 1000)
**Dependencies:** `get_dependencies`
**Structural analysis:** `detect_cycles`, `get_orphans` (with `kind` + `limit`/`offset`/`brief`, default limit 20, max 1000), `get_class_hierarchy` (with `depth` for transitive walk + `max_nodes` budget, default 250, max 1000), `get_coupling` (outgoing/incoming/both)
**Visualization:** `generate_mermaid` (call graph, file deps, or inheritance tree)
**Watch mode:** `watch_start`, `watch_stop` (auto-reindex on file changes via notify-debouncer-full)

### Response shapes

List-shaped paginated tools (`get_orphans`, `get_file_symbols`, `get_callers`, `get_callees`, `search_symbols`) return the shared `Page<T>` envelope: `{ results: T[], total: u32, offset: u32, limit: u32 }`. `total` is the pre-pagination match count; `offset`/`limit` are echoed as the resolved values actually used (so silent clamp-to-1000 is visible to clients). Sort order is `symbol_id` ascending for symbol lists; `(depth, symbol_id)` ascending for `get_callers`/`get_callees` so the closest results appear on page 1. `limit = 0` is treated as "use default".

Tree-shaped `get_class_hierarchy` returns `{ hierarchy: HierarchyNode, truncated: bool, max_nodes: u32, total_nodes_seen: u32 }`. `total_nodes_seen` counts unique class names actually walked — diamond inheritance costs one budget slot per shared ancestor, not one per arm reaching it. When `truncated: true`, the partial tree is well-formed (already-recursed children stay; new children are skipped after the budget is hit) and the agent can retry with a larger `max_nodes`. Hard ceiling: `max_nodes ≤ 1000`.

## Code Conventions

- All tool handlers return `Result<CallToolResult, McpError>`; user-visible errors travel as `CallToolResult` with the error flag set, not `Err`
- State guards check indexed state via `ServerInner::require_indexed()` before executing query handlers
- `LanguagePlugin` trait: `extensions()`, `parse_file(path, content)`, `preprocess(content, cfg)`, `resolve_edges(symbols, file_graph, registry)` — `preprocess` is a default-impl hook for byte-level transformations (e.g. C++ `[cpp].macro_strip`); plugins inherit `Cow::Borrowed(content)` unless they need to rewrite
- All stored file paths are absolute
- Symbol ID format: `file:name` for free functions, `file:Parent::name` for methods
- `SymbolKind` and `EdgeKind` derive `Serialize`/`Deserialize` and serialize as readable JSON strings (e.g. `"function"`, `"calls"`)
- Snapshot tests for tool wire format use `insta` (snapshots in `crates/code-graph-tools/tests/snapshots/`)

### Test conventions

- **Shared test helpers live in `super::test_helpers::*`.** When adding a paginated handler test module, `use super::test_helpers::{body_text, page_parts}` rather than defining local copies. The `test_helpers` module is `pub(super)` under `crates/code-graph-tools/src/handlers/mod.rs` and is the canonical home for assertion utilities every paginated tool's tests need. Re-creating these locally is a duplication the codebase already cleaned up once.
- **Diagnostic sentinels before discriminator assertions in timing-dependent tests.** When a test depends on async timing or file IO (watch-mode reindex tests are the canonical case), assert a low-stakes baseline first ("a no-macro class extracts") before asserting the discriminator ("a macro-prefixed class extracts"). The baseline assertion's failure message names the most likely root cause (timing, IO, file-write race) so the failure mode is self-diagnosing. The pattern in `tests/watch_cpp_macro_strip.rs` (`UObject` sentinel before `AActor` check) is the example.
- **Test fixtures with names matching the project's `.gitignore` rules need `git add -f`.** `.code-graph.toml` is gitignored because it's a per-user-root config users shouldn't commit, but test fixtures sometimes need that exact filename for `RootConfig::load` to find them (e.g. `testdata/ue/.code-graph.toml`). When adding such a fixture, `git add -f <path>` is required, and `git status` must show the file as staged before commit. The `cargo test` command does NOT catch a silently-excluded fixture — the test runs against the local-filesystem copy and passes locally; only a fresh-checkout CI run reveals the missing file. If you're unsure whether a test fixture is in this trap, run `git check-ignore <path>` — a hit means you need `-f`.

### Implementer conventions

- **Verify a workspace dependency exists before adopting it.** When a task instruction names a dep (e.g. "use `tracing::warn!`"), check `Cargo.toml` first. If the dep isn't present, that's a signal: the instruction may be derived from a convention assumption that doesn't apply to this workspace. Flag the deviation in your report and ask for confirmation rather than silently adding the dep (which is scope expansion) or shipping broken code (which doesn't compile). The workspace's deliberate "no `tracing` dep" convention (per `crates/code-graph-tools/src/handlers/watch.rs`) is the canonical example: `eprintln!` is the established channel for out-of-handler warnings.

### Quality-scanner: project-specific lenses

When dispatching `planner:quality-scanner` for changes in this repo, the standard 5 lenses (Correctness, Safety, Maintainability, Testing, Over-Engineering) apply per the global agent definition. Additionally evaluate against these two repo-local lenses when the diff scope includes them:

**Agent-facing tool descriptions** — applies when the diff touches `#[tool(description=…)]` strings in `crates/code-graph-tools/src/server.rs` (or analogous agent-readable description fields). Description text in these positions is *production behavior*, not documentation — agents pattern-match on it to decide whether to call the tool and with what arguments. A misleading description (e.g. "raise `offset` for more results" when `offset` is a skip-count) is functionally a bug. Writers rarely test their own copy by following the suggested action.

Checklist:
- Every named arg in the description is documented with its default and ceiling.
- The verb in any suggested action operationally produces the claimed result. "Raise `limit` to get more results" is correct; "raise `offset` to get more results" is wrong (offset skips, doesn't expand).
- The response envelope shape is named, not implied. If the tool returns a paginated `{results, total, offset, limit}` wrapper, say so; don't let agents guess they need to index into `["results"]`.
- When an agent should pick non-default values is at least hinted — "default 100; raise for symbols with high fan-in" beats "default 100, max 1000" alone.
- Plurality and units match the field type.

This lens caught two real agent-misleading bugs in CppMacroStrip Phase 4 (the "raise via offset" wording on `get_callers`/`get_callees` and the over-confident "typical depth=1/2 walks" claim on `get_class_hierarchy`).

**Documentation read cold** — applies when the diff touches `*.md`, `.code-graph.toml.example`, or other agent-readable docs. Read the modified sections AND the surrounding sections cold — without context from the implementer's commit message or task description — exactly as a future contributor or AI agent would encounter them.

Checklist:
- **Framing contradictions across sibling sections.** A feature documented in two places (e.g. once under "Supported Patterns" and once under "Known Limitations") should convey consistent signals. A feature that's "supported with caveat X" belongs in one location, not two with conflicting headlines. Caught a real instance in CppMacroStrip Phase 3.
- **Stale references that became more visible.** A reference like "the sample `.code-graph.toml` ships at the repo root" is wrong if the file is `.code-graph.toml.example`. When new content sits adjacent to a stale reference, fix the stale line — its visibility just doubled.
- **"Must contain phrase X" load-bearing strings.** Some doc requirements are explicitly that a phrase appears (e.g. `force=true` in both CLAUDE.md and the sample TOML for cache invalidation). When the diff touches either file, grep for the phrase to confirm it survives the edit. Use `make snapshot-audit ARGS=...`-style assertions for this class of check; or just `grep -l <phrase> CLAUDE.md .code-graph.toml.example`.
- **Documentation that promises behavior must match implementation.** "Default is 250" must match what the code resolves; "supports X" must match what's wired through.

## C++ Parser Limitations

Validated against tree-sitter-cpp v0.23.4.

### Supported C++ Patterns

- Free functions, qualified methods (`Class::method`), inline methods in class bodies
- Classes, structs, enums (including `enum class`), typedefs, `using` aliases
- Function pointer typedefs (`typedef void (*Callback)(int)`)
- Operator overloads (`operator+`, `operator==`, etc.) — both in-class and free
- Auto return types (trailing `-> T` and deduced)
- Nested classes/structs (Parent field set correctly)
- Lambda call edges (calls inside and to lambdas)
- All call patterns: free, method, arrow, qualified, template
- Macro-prefixed class declarations (`class CORE_API MyClass : public Base {};`) when listed in `[cpp].macro_strip` (see the Configuration section). Default behavior with no `[cpp]` section leaves these declarations broken, preserving zero behavior change for non-UE users — see Known Limitation 7 below for the raw-string-delimiter caveat that comes with this opt-in.

### Known Limitations

1. **Macro-generated definitions** — Macros like `DEFINE_HANDLER(name)` that expand to function definitions are not visible to tree-sitter (it sees the macro call, not the expansion). Macro invocations that look like function calls ARE captured as call edges.

2. **Complex template metaprogramming** — Deeply nested template specializations may produce incomplete or error-containing AST nodes. The parser skips error nodes gracefully.

3. **Call resolution is heuristic** — Call edges are resolved via scope-aware heuristic matching (same file > same class > same namespace > global). This is syntactic, not semantic — overloaded functions may resolve to the wrong candidate.

4. **C++ cast expressions** — `static_cast`, `dynamic_cast`, `const_cast`, `reinterpret_cast` are filtered out (tree-sitter parses them as call expressions).

5. **Forward declarations excluded** — Only `function_definition` (with body) produces symbols. Forward declarations (`void foo();`) are intentionally excluded to avoid duplicates.

6. **Template method calls** — `obj.foo<T>()` via `template_method` node type is not matched in tree-sitter-cpp v0.23.4. These calls fall through to the regular `field_expression` pattern when possible.

7. **`macro_strip` raw-string-delimiter collision** — when `[cpp].macro_strip` is configured, the C++ plugin's `preprocess` hook whole-word-replaces each listed macro with same-length spaces before tree-sitter parses. A raw string literal whose delimiter tag is also a stripped macro (e.g. `R"CORE_API(content)CORE_API"` with `CORE_API` in `macro_strip`) has both delimiters overwritten and tree-sitter fails to close the raw string — the rest of the file becomes an `ERROR` node and zero symbols extract from it. The pattern does not occur in any known codebase (a raw-string tag that is also an API-export macro is contrived) but is documented because the failure mode is silent at the file level. Workaround: drop the offending macro from `macro_strip` for the affected file or rename the raw-string tag in the source.

## Rust Parser Limitations

Validated against tree-sitter-rust v0.24.0.

### Supported Rust Patterns

- Free functions, methods inside `impl` blocks (`Type::method`), default methods inside `trait` blocks (extracted as `Function`, not `Method` — only `impl` ancestry promotes to `Method`)
- Structs, enums (all variant kinds), traits, type aliases (`type` items)
- Generics — both type-bound (`fn foo<T: Display>`) and where-clause (`fn foo<T> where T: Display`) forms
- Lifetime parameters (`fn longest<'a>(x: &'a str)`)
- `async fn`, `const fn`, `unsafe fn` — extracted as `Function` (or `Method` inside an `impl`)
- Nested modules — `mod a { mod b { fn x() {} } }` populates `Symbol.namespace = "a::b"`; `mod_item`s themselves are namespace anchors and do NOT produce Symbol records
- All `use`-tree forms expanded to dotted paths: simple, scoped, grouped, nested grouped (`use std::{io::{self, Read}, collections::HashMap}` → 3 edges), wildcard (`use foo::*`), aliased (`use foo as bar` records the path `foo`), `self`-in-list, and `extern crate alloc`
- All call patterns: direct, method via `field_expression`, scoped, turbofish, macro invocation, chained calls
- Trait impls (`impl Trait for Type`) produce `Inherits` edges from the implementing type to the trait — including generic impls (`impl<T> Trait for Vec<T>`) and impls with `where` clauses
- Trait-impl method parent disambiguation: in `impl Trait for Type { fn m() }` the method's parent is `Type`, never `Trait`. The trait identity lives only on the `Inherits` edge.

### Known Limitations

1. **`macro_rules!` definitions are not extracted as symbols.** Only macro *invocations* produce `Calls` edges. The definition queries deliberately do not match `macro_definition` nodes (the tree-sitter-rust 0.24 wrapping node for `macro_rules!` blocks). An anti-regression test in `code-graph-lang-rust` (`macro_rules_definition_produces_zero_symbols`) asserts that `macro_rules! foo { ... }` yields zero Symbol records.

2. **`#[derive(...)]` and proc-macro attributes are NOT captured as call edges.** They parse as `attribute_item` nodes, not `macro_invocation`, and the call queries only target `macro_invocation`. Multiple `#[derive(Debug, Clone, ...)]` attributes on a struct contribute zero `Calls` edges.

3. **Forward declarations excluded.** Trait method declarations without bodies (`fn bar();`) parse as `function_signature_item` and do NOT produce Symbol records — only `function_item` (which requires a body) is matched. Default methods inside trait bodies (with bodies) and methods inside `impl` blocks DO produce symbols.

4. **Call resolution is heuristic** — same as C++. Edges resolve via scope-aware heuristic matching (same file > same parent > same namespace > global). This is syntactic, not semantic — overloaded functions may resolve to the wrong candidate.

5. **Complex use trees expanded but lifetime/generic constraints not represented.** Each terminal path in a `use` tree becomes one edge; lifetime parameters and generic bounds in the surrounding code are not part of the graph. Generic impls record the type-field text verbatim — methods inside `impl<T> Trait for Vec<T>` carry parent `Vec<T>` (with the generic in the parent string), not bare `Vec`. The `Inherits` edge's `from` field follows the same rule.

## Go Parser Limitations

Validated against tree-sitter-go v0.25.0.

### Supported Go Patterns

- Free functions, methods (with receiver type as parent — both pointer `(s *T)` and value `(s T)` forms, including generic receivers `(s *T[U])` where the bare `T` is recorded as parent)
- Structs (`type T struct { ... }`), interfaces (`type T interface { ... }`), type aliases (`type ID = string`), defined types (`type Count int`, `type Handler func(...)` → `Typedef`)
- Generic functions (Go 1.18+, `func Map[T any](...)`) — type-parameter list survives in the captured signature; the bare name is recorded as the symbol name
- `init()` and `main()` are extracted as ordinary functions — no special-casing
- Package name from `package_clause` populates `Symbol.namespace` (Go packages are flat — single-level, no nested module path)
- All call patterns: direct (`foo()`), method/field selector (`obj.M()`), package-qualified (`fmt.Println()`), chained (`a.B().C()` → 2 edges, one per chain link), `go fn()`, `defer fn()`, calls inside closure literals (`func_literal`)
- All import forms via `import_spec`: single (`import "fmt"`), grouped (`import (...)`), aliased (`import f "fmt"` — alias dropped, path captured), dot (`import . "testing"`), blank (`import _ "image/png"`)
- Package-level closure fallback: a call inside a `var H = func() { foo() }` reports the file path as `from` (no enclosing function declaration), mirroring the C++ lambda-at-global-scope behavior

### Known Limitations

1. **Structural interface implementation produces no edges.** Go interfaces are satisfied structurally — a concrete type implements an interface by having the right method set, with no syntactic declaration. The parser emits zero `Inherits` edges for Go. `get_class_hierarchy` on a Go interface returns the interface as a leaf node with empty `bases` and `derived` (anti-regression test in `crates/code-graph-tools/tests/mixed_language.rs::get_class_hierarchy_for_go_interface`).

2. **Embedded struct fields produce no `Inherits` edge.** `type T struct { Bar }` is structural composition (method-set promotion at runtime), not inheritance — no edge is emitted. An anti-regression test in `code-graph-lang-go` (`embedded_struct_field_produces_no_inherits_edge`) asserts a fixture with an embedded field yields zero `Inherits` edges.

3. **Method dispatch is heuristic.** Same as the C++ and Rust plugins — call edges resolve via scope-aware heuristic matching (same file > same parent > same namespace > global). This is syntactic, not semantic; methods on different receiver types that share a name may resolve to the wrong candidate.

4. **`go.mod` and vendor directories are NOT consulted.** Discovery walks files and respects `.gitignore`; module-path resolution is out of scope. Import paths (e.g. `"github.com/sirupsen/logrus"`) are recorded verbatim in the `Includes` edge's `to` field — the default `resolve_include` basename match against the FileIndex is correctly a no-op for module paths.

5. **Generic type parameters and constraints not represented in symbol records.** Generic types are recognized in receiver positions (`func (s *Server[T]) M()` → parent recorded as `Server`, not `Server[T]`) so methods on a generic struct group with the bare type name. The type-parameter list `[T]` and any constraints (`[T any]`, `[T comparable]`) survive in the captured signature text only — they are not part of the symbol record's structured fields.

6. **`raw_string_literal` (backtick) imports are intentionally NOT matched.** Backtick-delimited import paths are valid Go grammar but not idiomatic and not produced by `gofmt`; the import query only matches `interpreted_string_literal`. An anti-regression test in `code-graph-lang-go` (`backtick_import_produces_no_includes_edge`) asserts backtick imports produce zero `Includes` edges.

7. **Forward declarations excluded.** Interface method elements (`type R interface { Read() }`) parse as `method_elem` nodes (no body) and are NOT matched by the definition query — only `method_declaration` (with a body and receiver) produces method symbols. The interface method set is implicit; only the interface type itself becomes a `Symbol`.

## Python Parser Limitations

Validated against tree-sitter-python v0.25.0.

### Supported Python Patterns

- Free functions, methods inside classes (parent = enclosing class name), nested classes (inner class records the *immediate* enclosing outer class as parent — not a dotted path)
- `async def` — extracted as `Function` (or `Method` inside a class) with no separate kind. tree-sitter-python 0.25 wraps `async def` as a `function_definition`, not a separate `async_function_definition` node, so a single query path covers both sync and async forms.
- `class` definitions with single (`class D(B)`), multiple (`class D(A, B, C)` → 3 `Inherits` edges), and qualified (`class D(module.Base)` → 1 edge with `to = "module.Base"`) inheritance. Keyword arguments in superclasses (`class C(metaclass=Meta)`, `class C(total=False)`) are filtered as non-bases.
- Decorators are transparent for definition extraction. `@property` / `@staticmethod` / `@classmethod` / `@abstractmethod` / custom decorators all wrap `decorated_definition > function_definition`; queries match the inner `function_definition` directly.
- All call patterns: direct (`foo()`), attribute (`obj.method()`), chained (`a.b().c()` → 2 edges, one per chain link), constructor calls (`MyClass()` — recorded as a call to `MyClass`, agent interprets as construction), `super()`, calls inside list/dict/set comprehensions, calls inside `lambda` (lambda is transparent for the enclosing-function walk), calls inside default argument expressions
- All import forms: `import foo` → `to = "foo"`; `import foo.bar` → `to = "foo.bar"`; `import foo as f` → `to = "foo"` (alias dropped); `from foo import bar` → `to = "foo"` (the *module* is the dependency, NOT the imported name); `from foo.bar import baz` → `to = "foo.bar"`; `from . import utils` → `to = ".utils"` (relative imports preserved verbatim); `from typing import List, Dict` → 1 edge with `to = "typing"`; `from __future__ import annotations` → `to = "__future__"`
- `.pyi` stub files indexed identically to `.py`. `def f(x: int) -> str: ...` (a stub with `...` body) still parses as `function_definition` and produces a Function symbol.

### Known Limitations

1. **Call resolution is especially noisy due to dynamic typing.** `PythonParser` does NOT override `resolve_call` — the default scope-aware heuristic (same file > same class > same namespace > global) is the documented contract. Python's runtime polymorphism means most call resolutions are best-effort: `obj.foo()` cannot be resolved to a concrete `foo` without type inference, which is out of scope for a tree-sitter-based static analyzer. (Rationale documented in the Phase 7.1 verification: `PythonParser` accepts the default `resolve_call` for this reason.)

2. **Decorators are transparent for definition extraction.** `@property` / `@staticmethod` / `@classmethod` produce ordinary `Method` symbols with no separate flag. `@abstractmethod` is NOT flagged as a separate kind — it parses as a method like any other. The decoration metadata is not preserved as a structured field in the symbol record.

3. **Type hints not extracted as edges.** `def f(x: SomeType) -> OtherType` does not produce edges to `SomeType` or `OtherType`. Only call sites and explicit imports drive the dependency graph.

4. **Conditional imports NOT extracted.** Patterns like `if TYPE_CHECKING: import expensive_module` are wrapped in `if_statement > block` and the import queries do not enter conditional bodies — `extract_imports`'s module-top-level guard filters them out. `try: import x except ImportError: ...` is filtered for the same reason. Anti-regression tests in the Python plugin's import tests cover both the `if`-block and `try`-block forms.

5. **`from __future__` records `__future__` as the module path.** `from __future__ import annotations` produces an `Includes` edge with `to = "__future__"`. Handled via the dedicated `future_import_statement` node kind, NOT `import_from_statement` — tree-sitter-python 0.25 tags the future-import line with its own node type.

6. **Forward declarations don't apply.** Python doesn't have C/Go-style forward declarations. `.pyi` stubs are indexed identically to `.py` files (the grammar is the same; `...` body still parses as a function body).

7. **Method dispatch is heuristic.** Same as the C++/Rust/Go plugins — call edges resolve via scope-aware heuristic matching (same file > same parent > same namespace > global). This is syntactic, not semantic; methods on different classes that share a name may resolve to the wrong candidate, especially given Python's duck-typing tradition.
