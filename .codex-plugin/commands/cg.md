---
name: cg
description: Ask code-graph anything structural about this codebase — where a symbol is defined, what calls it, what a file depends on, what's dead, how classes relate.
argument-hint: <question about the codebase>
---

# cg — ask code-graph anything structural about this codebase

Answer the user's question with the `code-graph` MCP server instead of grepping or reading
files.

## When to use

Invoke with a natural-language question about this repo's structure — `/cg <question>`. Use it
instead of hand-picking a tool when you just want the answer.

```text
/cg where is normalize_user_path defined?
/cg what calls Graph::load?
/cg what does crates/code-graph-tools/src/indexer.rs depend on?
/cg is there any dead code under crates/code-graph-graph?
```

## Routing

| Example question | Tool |
|---|---|
| "Where is X defined?" | `search_symbols` (`brief` defaults true) |
| "What's in this file?" | `get_file_symbols` (`top_level_only` for the shape) |
| "Full detail on this one symbol?" | `get_symbol_detail` |
| "How big is this area / what kinds live here?" | `get_symbol_summary` |
| "Which class does this bare name mean?" | `find_class_candidates` |
| "What calls X?" / "blast radius of changing X?" | `get_callers` |
| "What does X call?" | `get_callees` |
| "What overrides this virtual method?" | `find_overrides` |
| "What does this class inherit / who derives from it?" | `get_class_hierarchy` |
| "What does this file include/import?" | `get_dependencies` |
| "Which files are most entangled?" | `get_coupling` |
| "Are there circular dependencies?" | `detect_cycles` |
| "What's never called?" | `get_orphans` (`reliability: "high"`) |
| "Draw me the call graph / file graph / hierarchy" | `generate_diagram` |
| "Is the index current? What's the server state?" | `get_status` |

## Notes

- **Every query tool needs an index.** If a tool reports the codebase is not indexed, run
  `/cg-index` first — do not fall back to grep.
- Answer with **paths, line numbers, and signatures**. Read source only after a tool has pinned
  the exact span, and read that span, not the file.
- Byte-budgeted paginated tools return `{results, total, offset, limit, truncated, next_offset}`.
  `truncated` means matching records remain after the count limit or byte cap; re-call with
  `offset = next_offset` — raising `limit` alone will not get you the rest. Exception: empty
  `results` with `truncated: true` and `next_offset` equal to the requested offset is a
  start-fresh marker, not a continuation. Do **not** retry unchanged: raise `[response].max_bytes`,
  rerun `analyze_codebase` to refresh cached config, then retry. `detect_cycles` is count-paginated.
- Call resolution is **syntactic, not semantic**, in all six languages. Overloads can misresolve;
  pass `min_confidence: "resolved"` on `get_callers`/`get_callees` when a clean edge set matters
  more than coverage.

## See also

The `code-graph-navigator`, `code-graph-callgraph`, `code-graph-dependencies`,
`code-graph-refactor-survey`, and `code-graph-indexing` skills for the full workflows.
