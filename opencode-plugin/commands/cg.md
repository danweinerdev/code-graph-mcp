---
name: cg
description: Ask code-graph anything structural about this codebase -- where a symbol is defined, what calls it, what a file depends on, what's dead, how classes relate.
argument-hint: <question about the codebase>
---

# cg -- ask code-graph anything structural about this codebase

Answer the user's question with the `code-graph` MCP server instead of grepping or reading
files.

## When to use

Invoke with a natural-language question about this repo's structure -- `/cg <question>`. Use it
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
| "What symbol is at file:line?" | `get_symbol_at` (span containment, not goto-definition) |
| "What calls X?" / "blast radius of changing X?" | `get_callers` |
| "What does X call?" | `get_callees` |
| "Does A ever reach B?" | `find_path` (`found`/`cap_reached` discriminate no-path from gave-up) |
| "What overrides this virtual method?" | `find_overrides` |
| "What does this class inherit / who derives from it?" | `get_class_hierarchy` |
| "What does this file include/import?" | `get_dependencies` |
| "Which files are most entangled?" | `get_coupling` |
| "Are there circular dependencies?" | `detect_cycles` |
| "What are the natural module clusters?" | `detect_communities` |
| "What's never called?" | `get_orphans` (`reliability: "high"`) |
| "Who last changed this symbol?" | `blame_symbol` (`available:false` + `reason` is a clean answer, not an error) |
| "When did this symbol's content actually change?" | `symbol_history` (transitions only; reformat commits invisible) |
| "Draw me the call graph / file graph / hierarchy" | `generate_diagram` |
| "Is the index current? What's the server state?" | `get_status` |
| "How's that async index job doing?" | `get_analyze_status` (by `job_id`) |

## Notes

- **Every query tool needs an index.** If a tool reports the codebase is not indexed, run
  `/cg-index` first -- do not fall back to grep.
- Answer with **paths, line numbers, and signatures**. Read source only after a tool has pinned
  the exact span, and read that span, not the file.
- Paginated tools return `{results, total, offset, limit, truncated, next_offset}`. When
  `truncated` is true and `next_offset` differs from the requested `offset`, re-call with
  `offset = next_offset` -- raising `limit` alone will not get you the rest. An empty
  `truncated: true` page with `next_offset == offset` is a byte-starved start-fresh marker,
  not a continuation: raise `[response].max_bytes`, re-run `analyze_codebase`, then retry.
- Call resolution is **syntactic, not semantic**, in all six languages. Overloads can misresolve;
  pass `min_confidence: "resolved"` on `get_callers`/`get_callees` when a clean edge set matters
  more than coverage.

## See also

The `code-graph-navigator`, `code-graph-callgraph`, `code-graph-dependencies`,
`code-graph-refactor-survey`, and `code-graph-indexing` skills for the full workflows.
