---
name: code-graph-callgraph
description: Trace call relationships and class inheritance in an indexed codebase with the code-graph MCP server. Use when the user asks who calls a function, what a function calls, the impact/blast-radius of changing a function, how a value flows through calls, what overrides a virtual method, or the inheritance hierarchy of a class. Far more accurate than grepping a name because it follows resolved call/inherit edges, not text matches.
---

# code-graph call graph & hierarchy — trace relationships

Grepping a function name finds *mentions* (definitions, comments, unrelated
same-named symbols). code-graph follows **resolved edges**, so "who calls X" and
"what does X call" come back as real call chains with depth.

**Precondition:** codebase indexed (`analyze_codebase`; see **code-graph-indexing**).

## Pick the tool

| Question | Tool |
|---|---|
| Who calls this? (upstream) | `mcp__code-graph__get_callers` |
| What does this call? (downstream) | `mcp__code-graph__get_callees` |
| What overrides this virtual method? | `mcp__code-graph__find_overrides` |
| Inheritance tree of a class | `mcp__code-graph__get_class_hierarchy` |
| Disambiguate a class name first | `mcp__code-graph__find_class_candidates` |

## get_callers / get_callees

Both take a `symbol_id` (`file:name` or `file:Parent::name`) and return
`Page<CallChain>` rows `{symbol_id, file, line, depth}`:
- `symbol_id` = the **definition site** of the callable reported.
- `file`/`line` = the **call site** (the edge that reached this hop). At depth ≥ 2
  these legitimately differ — to answer "where is it defined" split `symbol_id`,
  don't read `file`.
- `depth` = BFS distance (1 = direct caller/callee).

Levers:
- **`depth`** (default 1): raise for transitive reach — e.g. `depth=3` for a
  blast-radius sweep. Mind the fan-out; responses are byte-capped.
- **`min_confidence`**: `"any"` (default) or `"resolved"`. Set `"resolved"` to
  drop heuristic (best-guess) edges and a Heuristic intermediate prunes its whole
  downstream subtree — use it when you want only high-certainty chains.

Unresolved/library callers (`printf`, `unwrap`, `println!`, `fmt.Println`,
stdlib/builtins) are filtered out automatically across all six languages.

**Non-callable soft-hint:** calling `get_callers`/`get_callees` on a
Struct/Enum/Trait/Typedef/Interface returns a SUCCESS result with a *plain-text*
advisory (not the JSON envelope) pointing you at `get_class_hierarchy` /
`get_symbol_detail`. A callable with zero hops returns an empty `Page` instead.

## Inheritance

- `get_class_hierarchy(class="Foo", depth=…, max_nodes=…)` walks **both**
  directions: `bases` (ancestors) and `derived` (descendants). Diamonds collapse
  to one canonical node; later occurrences are `{name, ref:true}` stubs.
- Generic classes have a known lookup gap: `class_hierarchy` keys by the **bare**
  name, but inheritance edges store the generic form (`Foo<T>`), so generic-class
  walks can return a leaf. If a hierarchy looks empty for a generic type, confirm
  via `search_symbols` + `get_symbol_detail`.
- `find_overrides(symbol_id=…)` lists every method overriding a given virtual /
  pure-virtual method (depth always 1).

## Recipes

- **"What breaks if I change `Graph::merge`?"** →
  `get_callers(symbol_id="…:Graph::merge", depth=2, min_confidence="resolved")`.
- **"Walk the call tree out of `main`."** →
  `get_callees(symbol_id="…:main", depth=3)`.
- **"Show the `Shape` class tree."** →
  `find_class_candidates(name="Shape")` to get the exact one, then
  `get_class_hierarchy(class="Shape", depth=5)`.

## Visualize

For a diagram instead of a list, use `mcp__code-graph__generate_diagram`
(`symbol=` mode, `direction=callees|callers|both`) — see the
**code-graph-dependencies** skill for diagram details.
