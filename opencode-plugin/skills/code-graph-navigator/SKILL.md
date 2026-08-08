---
name: code-graph-navigator
description: Find and inspect symbols (functions, methods, classes, structs, enums, traits, interfaces) in an indexed codebase using the code-graph MCP server instead of grep. Use when the user asks where something is defined, to locate a function/class/type by name, to list what a file contains, to count symbols in an area, or to find which class a name refers to. Covers C/C++, Rust, Go, Python, C#, and Java.
---

# code-graph navigator — locate & inspect symbols

The code-graph MCP server holds a structural index of the codebase (parsed with
tree-sitter, language-aware scoping and namespaces). For "where is X / what is X
/ what's in this file", these tools beat grep: they match symbols, not text, and
return precise IDs, kinds, files, and line numbers.

**Precondition:** the codebase must be indexed. If a query tool errors with a
not-indexed message, run `analyze_codebase` first (see the **code-graph-indexing**
skill).

## Pick the tool by question

| The user wants… | Tool | Notes |
|---|---|---|
| Find a symbol by name | `mcp__code-graph__search_symbols` | substring/regex `query`; filter by `kind`, `namespace`, `language`, `subtree` |
| Everything defined in a file | `mcp__code-graph__get_file_symbols` | `top_level_only`, `brief`, `count_only` |
| Full info for one symbol | `mcp__code-graph__get_symbol_detail` | needs the `symbol_id` |
| A census of an area | `mcp__code-graph__get_symbol_summary` | counts grouped by `(namespace, kind)` |
| "Which class is `Foo`?" | `mcp__code-graph__find_class_candidates` | every Class/Struct/Interface/Trait named `Foo` |

## Symbol IDs

IDs are `file:name` (free function) or `file:Parent::name` (method). `search_symbols`
and `get_file_symbols` return them; feed them to `get_symbol_detail`,
`get_callers`/`get_callees`, `find_overrides`, etc. To recover the file from an ID,
rsplit on the rightmost `:` that is not part of `::`.

## Recipes

- **"Where is `resolve_edges` defined?"** →
  `search_symbols(query="resolve_edges")`. Narrow noise with
  `kind="function"` or `namespace="..."`. Want fuzzy/typo-tolerant matching?
  pass `near=true`.
- **"What's in `src/graph.rs`?"** →
  `get_file_symbols(file="<abs path>")`; add `top_level_only=true` to skip
  nested methods, or `count_only=true` for just the total.
- **"How big is the `Nfs` namespace?"** →
  `get_symbol_summary(namespace="Nfs")` — returns counts per kind, not a giant list.
- **"Is `Buffer` a class or a struct, and where?"** →
  `find_class_candidates(name="Buffer")` — disambiguates same-named types across files.

## Pagination & response shape

Paginated tools return `{results, total, offset, limit, truncated, next_offset}`.
To get more, **raise `limit`** (default 100, max 1000) or resume with
`offset = next_offset` when `truncated=true`. Do not assume `results.length == limit`
means "done" — check `truncated`. Use `count_only=true` / `brief=true` to keep
responses small when you only need totals or names.

## When to fall back to grep

code-graph indexes **definitions and call/include/inherit edges** — not free text.
Use grep/glob for: comments, log strings, TODOs, config/`.toml`/`.json`, build
files, generated code, or any language code-graph doesn't parse. For finding a
*definition* or *usages* of a code symbol, prefer code-graph.
