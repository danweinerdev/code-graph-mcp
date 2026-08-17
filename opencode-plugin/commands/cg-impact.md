---
name: cg-impact
description: Blast-radius analysis for a symbol before changing it — resolved callers, callees, overrides, and inheritance, via code-graph instead of grepping the name.
argument-hint: <symbol name or file:Class::method>
---

# cg-impact — what breaks if I change this?

Trace everything that reaches, or is reached by, `${ARGUMENTS}` using the `code-graph` MCP
server. Call edges are resolved from the parse tree, so this catches what a name-grep misses and
skips the comment/string/test-name noise a grep drowns in.

## Workflow

1. **Pin the symbol.** If the argument is a bare name, `search_symbols` first — a symbol ID is
   `file:name` or `file:Parent::name`, and passing an ambiguous bare name gets you the wrong one.
   For a bare class name, `find_class_candidates`.
2. **Inbound.** `get_callers(symbol_id)` — everything that would need to change. Start with
   `depth` shallow and widen; page with `next_offset` while it differs from the requested offset.
   An empty `truncated: true` page with unchanged `next_offset` is a byte-starved start-fresh
   marker: raise `[response].max_bytes`, re-run `analyze_codebase`, then retry.
3. **Outbound.** `get_callees(symbol_id)` — what this symbol relies on, i.e. what could break
   *it*.
4. **Polymorphic reach.** If it's a virtual/overridable method, `find_overrides` — the call graph
   alone will not show you sibling implementations that share the contract you are about to
   change.
5. **Type context.** `get_class_hierarchy(class)` when the symbol is a method: changing a base
   signature obligates every derived class.
6. **Visualize** when the fan-out is worth a picture:
   `generate_diagram(symbol: "…", direction: "both", format: "mermaid")`.

## Reading the results

- `CallChain = { symbol_id, file, line, depth }`. **`symbol_id` is the definition site; `file` and
  `line` are the call site.** At `depth ≥ 2` they routinely point at different files — to answer
  "where is this defined", split `symbol_id` on the rightmost `:` that is not part of `::`. Do
  not read `file`.
- `depth` is BFS distance; `1` is a direct caller.
- Unresolved hops (`printf`, `unwrap`, `Ok`, stdlib) are filtered at BFS time in all six
  languages — they never appear, and their subtrees are not traversed.
- `min_confidence: "resolved"` drops heuristic edges. It prunes **the whole downstream subtree**
  of a heuristic hop, so use it to tighten a noisy result, not to enumerate exhaustively.
- An empty `Page<CallChain>` on a `Function`/`Method`/`Class` genuinely means zero resolved hops.
  On a `Struct`/`Enum`/`Trait`/`Typedef`/`Interface` you instead get a **plain-text advisory**,
  not the JSON envelope — that is a success, not an error; follow its pointer to
  `get_class_hierarchy`/`get_symbol_detail`.

## Caveats to state in the answer

- Call resolution is a syntactic heuristic (same file > same parent > same namespace > global).
  Overloads can misresolve; treat the caller set as high-recall, not proof.
- Go has **no** inheritance edges — structural interface satisfaction is invisible, so step 4/5
  return leaves for Go interfaces.
- C++ macro-generated definitions are invisible unless `[cpp].macro_define_function` /
  `macro_define_type` are configured.

## See also

The `code-graph-callgraph` skill.
