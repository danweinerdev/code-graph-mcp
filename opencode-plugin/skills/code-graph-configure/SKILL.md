---
name: code-graph-configure
description: Generate a well-formed .code-graph.toml for the current project by exercising the code-graph MCP server against it. Use when a repo has no .code-graph.toml, when analyze_codebase warns about missing config or API-export macros, when the index looks bloated or slow (vendored/generated trees indexed), when engine-style C++ classes (class CORE_API Foo) fail to extract, or when onboarding code-graph onto an unfamiliar codebase. Iterates explore -> analyze -> inspect -> refine until the config converges, then writes the file.
---

# code-graph configure -- derive and write a project config

The goal is a WRITTEN, well-formed `.code-graph.toml` at the project root
that makes `analyze_codebase` fast, complete, and quiet. Getting there is
iterative: explore the tree, index it, read what the graph got wrong or
ingested needlessly, refine, force re-index, compare. Expect 2-3 rounds on
an engine-scale codebase, 1 on a plain one.

**Reference for every key:** the repo's `.code-graph.toml.example` if this
IS the code-graph repo; otherwise the schema below is complete for v1.

## Round 0 -- survey before indexing

Do this with plain file exploration (ls/glob/read), NOT the graph -- the
graph does not exist yet and a bad first index wastes minutes on large
trees.

1. **Extension census.** Which of the six languages are present
   (`.cpp/.cc/.cxx/.c/.h/.hpp/.hxx`, `.rs`, `.go`, `.py/.pyi`, `.cs`,
   `.java`)? Note NONSTANDARD spellings that need `[extensions]`: `.ipp`,
   `.inl`, `.tpp`, generated `.gen.h`, etc.
2. **Dead weight.** Find big directories that carry no first-party source
   signal: vendored deps (`vendor/`, `third_party/`, `external/`),
   submodules (read `.gitmodules`), build output not already ignored by
   git (`dist/`, `out/`), test corpora, and for Unreal projects the
   engine scratch set (`Intermediate/`, `Saved/`, `Binaries/`,
   `DerivedDataCache/`, `*.uasset`, `*.umap`). Discovery already honors
   `.gitignore` -- only list things git does NOT ignore.
3. **C++ macro patterns.** Grep headers for the two extraction killers:
   - `class [A-Z_]+_API <Name>` -> each distinct `<MODULE>_API` token is a
     `macro_strip` candidate.
   - Parameterized reflection/API macros on their own lines above or
     inside classes (`UCLASS(...)`, `UFUNCTION(...)`, `GENERATED_BODY()`,
     `DECLARE_*_DELEGATE*(...)`) -> `macro_strip_with_args` candidates.
   - Token-pasting definition macros (`DEFINE_HANDLER(Name)` expanding to
     a function, struct-wrapping macros) -> `macro_define_function` /
     `macro_define_type` candidates. These are opt-in and rarer; only add
     them when a symbol the user needs is provably macro-hidden.
4. **Project root.** The toml goes at the intended root; remember a nested
   `.code-graph.toml` shadows its ancestor (that subtree becomes its own
   project), and the cache co-locates with the toml.

## Round 1 -- baseline index and inspect

1. `analyze_codebase(path=<root>)` -- use `analyze_codebase_async` + poll
   `get_analyze_status(job_id)` when the tree is huge (see
   **code-graph-indexing**). If a cache predates this session and the file
   count looks implausibly small, re-run with `force=true` once.
2. Read the result CRITICALLY:
   - `warnings` -- a no-config warning names the macro-extraction
     consequence verbatim; orphan-cache warnings name stale artifacts.
   - `files` vs the survey's expectation -- too HIGH means dead weight got
     indexed (fix with `extra_ignore`); too LOW means an extension or an
     over-broad ignore is missing files.
3. `get_symbol_summary()` -- which namespaces dominate? Vendored corpora
   showing up here confirms `extra_ignore` gaps.
4. **Extraction spot-checks** (C++ repos): pick 2-3 sentinel classes you
   KNOW exist behind API macros and `search_symbols(query="^Name$")`. A
   miss plus a `class SOMETHING_API Name` grep hit = add the macro and
   re-index. `get_file_symbols(file=<header>, count_only=true)` returning
   0 on a header full of declarations is the same signal.

## Round 2..n -- refine and converge

Write or update the toml, then `analyze_codebase(force=true)` -- config
changes NEVER apply to mtime-unchanged files without force. Compare
files/symbols/warnings against the previous round. Converged when:

- zero unexpected warnings,
- sentinel symbols extract,
- file count matches the survey expectation,
- `get_orphans(reliability="high", count_only=true)` is not dominated by
  macro artifacts.

## The file to write

Include ONLY sections that earn their place; every key is optional and
defaults are sane. Comment every non-obvious entry with WHY -- the next
reader has no session context. Shape:

```toml
[discovery]
# gitignore-syntax globs ADDED to defaults (.git, target, node_modules...).
# SINGULAR key name: `extra_ignore` -- the plural spelling silently no-ops.
extra_ignore = ["external/", "vendor/"]

[cpp]
# Bare API-export tokens, overwritten with spaces pre-parse (offsets kept).
macro_strip = ["CORE_API", "ENGINE_API"]
# Identifier-plus-(args) macros stripped the same way.
macro_strip_with_args = ["UCLASS", "UFUNCTION", "UPROPERTY", "GENERATED_BODY"]

[extensions]
# Add nonstandard extensions per language; entries start with `.`.
cpp = [".ipp"]

# [daemon] / [response] / [parsing]: leave unset unless a measured need
# exists (daemon defaults on; response cap 100KB suits default harnesses).
```

Constraints the file must respect (load-time errors otherwise): a token may
not appear in both `macro_strip` and `macro_strip_with_args`;
`macro_define_type.keyword` must be `struct` or `class` and must FIT inside
the macro-name span; extension entries start with `.`. One silent trap: a
raw string whose tag equals a stripped macro (`R"CORE_API(...)CORE_API"`)
breaks that file's parse entirely -- if a file drops to zero symbols after
adding a macro, check for tag collisions before blaming the parser.

## Report back

State what was written and why, per section; the before/after
files/symbols/warnings numbers; and the sentinel checks that now pass.
Recommend committing the toml unless the repo gitignores it deliberately
(this repo does -- check `git check-ignore .code-graph.toml` before
assuming).

## Terminal equivalent

`code-graph analyze-codebase <root> [--force] --json` and
`code-graph get-status --json` drive the same loop from a shell; the
survey steps are ordinary file inspection either way.
