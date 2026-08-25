---
name: code-graph-callgraph
description: Trace call relationships and class inheritance in an indexed codebase with the code-graph MCP server. Use when the user asks who calls a function, what a function calls, the impact/blast-radius of changing a function, how a value flows through calls, what overrides a virtual method, or the inheritance hierarchy of a class. Far more accurate than grepping a name because it follows resolved call/inherit edges, not text matches.
---

# code-graph call graph & hierarchy -- trace relationships

Grepping a function name finds *mentions* (definitions, comments, unrelated
same-named symbols). code-graph follows **resolved edges**, so "who calls X" and
"what does X call" come back as real call chains with depth.

**Precondition:** codebase indexed (`analyze_codebase`; see **code-graph-indexing**).

## Pick the tool

| Question | Tool |
|---|---|
| Who calls this? (upstream) | `mcp__code-graph__get_callers` |
| What does this call? (downstream) | `mcp__code-graph__get_callees` |
| Is there a call chain from A to B? | `mcp__code-graph__find_path` |
| What overrides this virtual method? | `mcp__code-graph__find_overrides` |
| Inheritance tree of a class | `mcp__code-graph__get_class_hierarchy` |
| Disambiguate a class name first | `mcp__code-graph__find_class_candidates` |

## get_callers / get_callees

Both take a `symbol_id` (`file:name` or `file:Parent::name`) and return
`Page<CallChain>` rows `{symbol_id, file, line, depth, candidates}`:
- `symbol_id` = the **definition site** of the callable reported.
- `file`/`line` = the **call site** (the edge that reached this hop). At depth >= 2
  these legitimately differ -- to answer "where is it defined" split `symbol_id`,
  don't read `file`.
- `depth` = BFS distance (1 = direct caller/callee).
- `candidates` = how many same-named definitions competed for the traversed
  edge's target. `1` = sole candidate -- NOT necessarily verified: a
  receiver-typed call (`x.foo()`) whose receiver type the index cannot check
  is a Heuristic/1 name-only guess, dropped by `min_confidence="resolved"`.
  `N >= 2` = the scope rule picked one of N -- the signal to double-check
  with `find_class_candidates`/`get_symbol_detail` when a chain looks wrong.

Levers:
- **`depth`** (default 1): raise for transitive reach -- e.g. `depth=3` for a
  blast-radius sweep. Mind the fan-out; responses are byte-capped.
- **`min_confidence`**: `"any"` (default) or `"resolved"`. Set `"resolved"` to
  drop heuristic (best-guess) edges and a Heuristic intermediate prunes its whole
  downstream subtree -- use it when you want only high-certainty chains.

Unresolved/library callers (`printf`, `unwrap`, `println!`, `fmt.Println`,
stdlib/builtins) are filtered out automatically across all six languages.

**Non-callable soft-hint:** calling `get_callers`/`get_callees` on a
Struct/Enum/Trait/Typedef/Interface returns a SUCCESS result with a *plain-text*
advisory (not the JSON envelope) pointing you at `get_class_hierarchy` /
`get_symbol_detail`. A callable with zero hops returns an empty `Page` instead.

## find_path -- connect two symbols

`find_path(from=..., to=...)` returns the SHORTEST chain of call edges between two
symbol IDs as a single object (not a `Page`): `{found, hops, hop_count,
heuristic_hops, nodes_examined, node_cap, cap_reached}`. Each hop is
`{symbol_id, file, line, entered_by, candidates}` (`entered_by`/`candidates`
are `null` only on `hops[0]`). Key readings:
- `found: false` is SUCCESS, never an error -- `cap_reached` discriminates
  "no path exists" (`false`) from "search gave up at `node_cap` nodes" (`true`;
  raise `node_cap`, default 100k, max 5M).
- `from == to` succeeds with `hop_count: 0`.
- Ties break toward fewer `heuristic_hops`; a returned path is evidence a chain
  likely exists, not proof (resolution is syntactic).
- `min_confidence="resolved"` restricts the search to verified edges (drops
  scope-rule picks AND receiver-unverified sole-candidate picks).

## Inheritance

- `get_class_hierarchy(class="Foo", depth=..., max_nodes=...)` walks **both**
  directions: `bases` (ancestors) and `derived` (descendants). Diamonds collapse
  to one canonical node; later occurrences are `{name, ref:true}` stubs.
- Generic classes have a known lookup gap: `class_hierarchy` keys by the **bare**
  name, but inheritance edges store the generic form (`Foo<T>`), so generic-class
  walks can return a leaf. If a hierarchy looks empty for a generic type, confirm
  via `search_symbols` + `get_symbol_detail`.
- `find_overrides(symbol_id=...)` lists every method overriding a given virtual /
  pure-virtual method (depth always 1).

## Recipes

- **"What breaks if I change `Graph::merge`?"** ->
  `get_callers(symbol_id="...:Graph::merge", depth=2, min_confidence="resolved")`.
- **"Walk the call tree out of `main`."** ->
  `get_callees(symbol_id="...:main", depth=3)`.
- **"Show the `Shape` class tree."** ->
  `find_class_candidates(name="Shape")` to get the exact one, then
  `get_class_hierarchy(class="Shape", depth=5)`.
- **"Does `main` ever reach `flush`?"** ->
  `find_path(from="...:main", to="...:flush")` -- one call instead of a manual
  depth-raising `get_callees` walk.

## Terminal equivalent

`code-graph get-callers`, `get-callees`, `find-path`, `find-overrides`,
`get-class-hierarchy`, `find-class-candidates` -- same args kebab-cased,
`--json` = the MCP payload byte-for-byte.

## Visualize

For a diagram instead of a list, use `mcp__code-graph__generate_diagram`
(`symbol=` mode, `direction=callees|callers|both`) -- see the
**code-graph-dependencies** skill for diagram details.
