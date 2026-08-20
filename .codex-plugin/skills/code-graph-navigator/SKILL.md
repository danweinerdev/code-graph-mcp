---
name: code-graph-navigator
description: Find and inspect symbols (functions, methods, classes, structs, enums, traits, interfaces) in an indexed codebase using the code-graph MCP server instead of grep. Use when the user asks where something is defined, to locate a function/class/type by name, to list what a file contains, what symbol encloses a given line, to count symbols in an area, to find which class a name refers to, or who last changed a symbol and when its content changed. Covers C/C++, Rust, Go, Python, C#, and Java.
---

# code-graph navigator -- locate & inspect symbols

The code-graph MCP server holds a structural index of the codebase (parsed with
tree-sitter, language-aware scoping and namespaces). For "where is X / what is X
/ what's in this file", these tools beat grep: they match symbols, not text, and
return precise IDs, kinds, files, and line numbers.

**Precondition:** the codebase must be indexed. If a query tool errors with a
not-indexed message, run `analyze_codebase` first (see the **code-graph-indexing**
skill).

## Pick the tool by question

| The user wants... | Tool | Notes |
|---|---|---|
| Find a symbol by name | `mcp__code-graph__search_symbols` | substring/regex `query`; filter by `kind`, `namespace`, `language`, `subtree` |
| Everything defined in a file | `mcp__code-graph__get_file_symbols` | `top_level_only`, `brief`, `count_only` |
| Full info for one symbol | `mcp__code-graph__get_symbol_detail` | needs the `symbol_id` |
| A census of an area | `mcp__code-graph__get_symbol_summary` | counts grouped by `(namespace, kind)` |
| "Which class is `Foo`?" | `mcp__code-graph__find_class_candidates` | every Class/Struct/Interface/Trait named `Foo` |
| "What symbol is at line N?" | `mcp__code-graph__get_symbol_at` | `file` + `line` (1-based) -> enclosing symbols, innermost first. Span containment, **not** goto-definition |
| "Who last changed this symbol?" | `mcp__code-graph__blame_symbol` | VCS blame clipped to the symbol's span; `available:false` + `reason` when no VCS/untracked (a success, not an error) |
| "When did its *content* change?" | `mcp__code-graph__symbol_history` | AST-fingerprint transitions only (`introduced`/`modified`/`removed`); reformat-only commits invisible; `mode="literal_insensitive"` also ignores literal values |

## Symbol IDs

IDs are `file:name` (free function) or `file:Parent::name` (method). `search_symbols`
and `get_file_symbols` return them; feed them to `get_symbol_detail`,
`get_callers`/`get_callees`, `find_overrides`, etc. To recover the file from an ID,
rsplit on the rightmost `:` that is not part of `::`.

## Recipes

- **"Where is `resolve_edges` defined?"** ->
  `search_symbols(query="resolve_edges")`. Narrow noise with
  `kind="function"` or `namespace="..."`. Want fuzzy/typo-tolerant matching?
  pass `near=true`.
- **"What's in `src/graph.rs`?"** ->
  `get_file_symbols(file="<abs path>")`; add `top_level_only=true` to skip
  nested methods, or `count_only=true` for just the total.
- **"How big is the `Nfs` namespace?"** ->
  `get_symbol_summary(namespace="Nfs")` -- returns counts per kind, not a giant list.
- **"Is `Buffer` a class or a struct, and where?"** ->
  `find_class_candidates(name="Buffer")` -- disambiguates same-named types across files.
- **"What function is at `graph.rs:412`?"** ->
  `get_symbol_at(file="<abs path>", line=412)` -- innermost enclosing symbol first
  (a method sorts before its class).
- **"Who wrote this function / when did it last really change?"** ->
  `blame_symbol(symbol="...")` for line-level attribution at the committed state;
  `symbol_history(symbol="...")` for the changelist/commit transitions where the
  symbol's CONTENT changed (renames report `removed`+`introduced`, never `modified`).

## Pagination & response shape

Paginated tools return `{results, total, offset, limit, truncated, next_offset}`.
To get more, **raise `limit`** (default 100, max 1000) or, when `truncated=true` and
`next_offset` differs from the requested `offset`, resume with `offset = next_offset`.
An empty `truncated: true` page with `next_offset == offset` is a byte-starved start-fresh
marker, not a continuation: raise `[response].max_bytes`, re-run `analyze_codebase`, then
retry. Do not assume `results.length == limit` means "done" -- check `truncated`. Use
`count_only=true` / `brief=true` to keep responses small when you only need totals or names.

## Terminal equivalent

Every tool here is also a `code-graph` CLI subcommand (kebab-cased:
`search-symbols`, `get-file-symbols`, `get-symbol-at`, `blame-symbol`,
`symbol-history`, ...). `--json` prints the exact MCP payload, so everything this
skill says about response shapes applies verbatim. Exit `0` = success (including
empty results), `1` = tool error, `2` = operational failure.

## When to fall back to grep

code-graph indexes **definitions and call/include/inherit edges** -- not free text.
Use grep/glob for: comments, log strings, TODOs, config/`.toml`/`.json`, build
files, generated code, or any language code-graph doesn't parse. For finding a
*definition* or *usages* of a code symbol, prefer code-graph.
