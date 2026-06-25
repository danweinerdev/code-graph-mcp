---
name: code-graph-refactor-survey
description: Survey a codebase for dead code, refactor impact, and structural health using the code-graph MCP server. Use when the user asks to find unused/uncalled functions, dead code, orphans, the blast radius of a change, what's safe to delete, or wants an overall map of an unfamiliar codebase before making changes. Combines orphan detection, call-graph reach, coupling, and cycles into a workflow.
---

# code-graph refactor survey — dead code, impact & orientation

This skill is a **workflow** over several code-graph tools for the higher-level
questions: "what can I delete?", "what does this change touch?", "help me get
oriented in this repo." Use the single-purpose skills (navigator, callgraph,
dependencies) for atomic queries.

**Precondition:** codebase indexed (`analyze_codebase`; see **code-graph-indexing**).

## Find dead / unused code — get_orphans

`mcp__code-graph__get_orphans` returns symbols with **zero incoming call edges**
(uncalled functions/methods). Key levers:
- `kind` — restrict to `function` / `method`.
- `subtree` — scope to a path prefix (audit one module at a time).
- `reliability` — **`"high"`** drops known false positives (virtual methods,
  macro-synthesized symbols) so the list is closer to genuinely-dead code;
  `"all"` (default) is exhaustive but noisier.
- `brief` / `count_only` — keep the response small.

⚠️ "Orphan" means *no incoming call edge in the graph*, NOT "provably unused."
Entry points (`main`), exported API, test functions, reflection/macro/DI targets,
trait/interface methods invoked dynamically, and callbacks all legitimately show
as orphans. Treat the list as **candidates to investigate**, never an
auto-delete list. Cross-check each with `get_callers` and a grep for dynamic/string
references before recommending removal.

## Gauge refactor impact — blast radius

Before changing a function:
1. `get_callers(symbol_id=…, depth=2, min_confidence="resolved")` — who depends on it.
2. `get_coupling(file=…, direction="both")` — which files are entangled.
3. For a type change: `find_overrides` / `get_class_hierarchy` to catch
   subclasses and overriding methods.

## Orient in an unfamiliar codebase

A fast top-down pass:
1. `get_symbol_summary()` (no namespace) — the namespace/kind census: where the
   mass of the code lives.
2. `get_symbol_summary(namespace="<biggest one>")` — drill into the largest areas.
3. `detect_cycles()` — circular-dependency hot spots worth knowing up front.
4. `get_coupling` on the central files surfaced above — the architectural spine.
5. `generate_diagram(class=…)` / `(symbol=…, format="mermaid")` for a picture of a
   key hierarchy or call path.

## Health-check checklist

- **Dead code:** `get_orphans(reliability="high", subtree="<module>")` per module.
- **Circular deps:** `detect_cycles()` — aim for an empty result.
- **Over-coupling:** `get_coupling` on suspected god-files; high counts in both
  directions flag refactor targets.

Throughout, prefer these structural queries over grep sweeps: they follow real
edges, so the impact and dead-code findings reflect actual call/include structure
rather than name collisions. Keep grep for the dynamic/string references the graph
can't see — and factor those in before any delete recommendation.
