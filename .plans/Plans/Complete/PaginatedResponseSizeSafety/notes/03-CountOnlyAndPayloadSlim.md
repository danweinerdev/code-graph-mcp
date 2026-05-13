---
title: "Phase 3 Debrief: count_only flag + SymbolResult.file drop"
type: debrief
plan: "PaginatedResponseSizeSafety"
phase: 3
phase_title: "count_only flag + SymbolResult.file drop"
status: complete
created: 2026-05-12
---

# Phase 3 Debrief: count_only flag + SymbolResult.file drop

## Decisions Made

- **`count_only` as `Option<bool>` not bare `bool`.** Matches the existing pattern on `brief`, `force`, `top_level_only`. Default-on-absence yields `None`; handler resolves with `unwrap_or(false)`. This is consistent with the Phase 1 polish that fixed `page_extras` to fail-fast on missing-mandatory-fields — required fields use unwrap, optional fields use `Option`.
- **`limit: 0` in count_only response is a deliberate exception** to the "envelope echoes resolved limit" contract (Decision 9). Documented inline at each early-return site AND in CLAUDE.md Response shapes section (Phase 4.2). The rationale: count_only callers explicitly opted out of paging; echoing the would-have-been-resolved limit would mislead agents into thinking there's a record page to fetch.
- **count_only NOT on `get_callers`/`get_callees`** (Decision 9). Their `depth + limit` interaction already makes "how many?" cheap. Adding count_only would be feature parity for parity's sake.
- **`SearchParams.count_only` short-circuits `Graph::search` before heap allocation.** Walks the same match predicate (kind → language → namespace → pattern), increments `total` only on full predicate pass, then returns `SearchResult { symbols: vec![], total }` before `BinaryHeap<TopEntry>::with_capacity()`. The thread-local heap-push counter pins this cost win to behavior — a future refactor that re-introduces heap construction on count_only path fails immediately.
- **`SymbolResult.file` dropped universally** (Decision 10). The redundant field doubled brief-mode record size (~95 → ~62 bytes after drop). Clients recover via `code_graph_core::id_to_file(&record.id)` — Phase 1.4's documented inverse contract.
- **`CallChain.file` retained** (Decision 11). Researcher had flagged in the plan that `CallChain.file` is the call-site file, distinct from the definition file in `symbol_id`. Dropping it would lose information. Confirmed during implementation; `CallChain` untouched.
- **`limit=0` normalization moved BELOW count_only branch** (3.3 polish). Pre-polish, `params.limit` got normalized from 0→20 before the count_only check, so count_only callers passing `limit=0` saw `params.limit=20` inside the function. Benign (the count_only branch never read params.limit) but a future trap. Moved the normalization below the early-return.

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| `count_only: Option<bool>` on 3 Args structs | Met | 12 deserialization tests (4 cases × 3 structs) |
| Handler early-return sentinel envelope | Met | All 3 handlers; 3 new snapshots; < 1KB asserted |
| `SearchParams.count_only` skips heap allocation | Met | Thread-local `HEAP_PUSHES` counter test pins it |
| `SymbolResult.file` dropped from struct | Met | 15 SymbolResult-emitting snapshots regenerated |
| Two named non-snapshot consumers migrated | Met | `mixed_language.rs:204` switched to `id_to_file`; `symbols.rs:541` flipped to `is_none()` |
| `CallChain.file` untouched | Met | `code-graph-graph/` shows zero changes |
| id_to_file round-trip test | Met | New `id_to_file_recovers_dropped_file_field` |

## Deviations

- **Stale 4-field envelope detected in `detect_cycles` tool description (Phase 4.1 polish, retroactively).** `detect_cycles` uses `Page<Vec<String>>` and was updated in Phase 1 to have the 6-field envelope on the wire. Its `#[tool(description=...)]` string was NOT in the Phase 3 or 4.1 scope but was a pre-existing 4-field-envelope description. Surfaced by the 4.1 scan; cleaned up in 4.1 polish. Phase 3 didn't cause this — it was always inconsistent — but Phase 3's `SymbolResult.file` drop made the inconsistency more visible.
- **3.4 verification said "no consumers exist outside snapshot tests."** False. Two explicit consumers (`mixed_language.rs:204` and `symbols.rs:541`) were named by the plan reviewer ahead of time. Both successfully migrated. Without the reviewer's explicit pre-flag, these would have been silent runtime failures (`.expect("each result has a file field")` panics).

## Risks & Issues Encountered

- **JSON-key consumers don't fail at compile time.** Phase 3.4's biggest risk: `cargo check`/`clippy` doesn't catch `serde_json::Value` indexing on dropped fields. The two known consumers panic or assertion-fail at test runtime. Mitigated by: explicit named migration in 3.4's subtasks + a workspace-wide `rg --type rust '"file"' crates/code-graph-tools/` sweep + the final `cargo test --workspace` confirming nothing broke.
- **Stale comments after `file` drop.** Multiple comments in `symbols.rs` and `structure.rs` referenced `file: "/big.cpp"` in example serializations or claimed "~95 bytes per record." After the drop, records are ~62-70 bytes. 3.4 polish caught one ("fits ~3 records" should be "fits ~4 records"). The byte-budget acceptance test in Phase 5 also encoded the new size implicitly via the 1500-orphan fixture sizing math.
- **Comment in `search_symbols` count_only path (3.2 polish)** referenced "byte_budget_take debug_assert on limit > 0" — but `byte_budget_take` is never called by `search_symbols` (architectural exception per Decision 12). Scanner caught the doc-vs-code mismatch.

## Lessons Learned

- **Plan reviewer's pre-flag of the two non-snapshot consumers (`mixed_language.rs:203`, `symbols.rs:341`) was load-bearing.** Without it, the 3.4 implementer would have shipped the change, the workspace would have built, but two integration tests would have failed at runtime in ways the snapshot regen wouldn't catch. The plan reviewer's "Critical" severity for this finding (during plan review, not code review) was correct.
- **Wire-format breaking changes need a deliberate sweep beyond compile errors.** `rg '"file"'` was the right tool. The pattern repeats: "I dropped a field, did I find every consumer?" — and the answer is always "use grep on the field name, in literal-string form too."
- **The `Graph::search` heap-not-touched test (3.3) pins a cost win to behavior.** This was an unprompted addition by the implementer. Worth recognizing: when an optimization avoids work (heap allocation, mtime stat, etc.), a test that asserts the avoided-work-stays-avoided is more durable than asserting only the externally-visible result. Future refactors that accidentally re-introduce the work fail loudly.

## Impact on Subsequent Phases

- Phase 4 inherits a Decision 10 + Decision 11 distinction that needs to be encoded in the design doc addendum: `SymbolResult.file` dropped (recovered via id), `CallChain.file` retained (it's a different field semantically). This distinction shows up in 4.5.
- Phase 4 inherits the `id_to_file` recovery contract as a public API surface. CLAUDE.md Core invariants need to reference it as the documented way clients recover file paths.
- Phase 5 inherits a record-size that's ~30% smaller than pre-Phase-3. The 1500-orphan fixture sizing math (5.1) was redone to account for this; an older estimate of N=1200 would have under-sized the fixture.

## Skill Opportunities

### Wire-format-break sweep
- **What you did repeatedly:** When dropping a wire field, run `cargo test --workspace` AND `rg --type rust '"<field>"' crates/`, then audit JSON-key consumers (`.get("field")`, `value["field"]`, `.expect("...field...")`) that compile-checks won't catch.
- **Where it belongs:** A `/sdd-planner:wire-break` skill, or documented as a recipe in CLAUDE.md.
- **Why a skill:** The compile-error pathway misses JSON-key consumers. A formalized audit recipe (grep for literal "field", grep for `.expect`-pattern matches, grep for `as_str().unwrap()` on suspected paths) would surface every consumer in one pass.
- **Rough shape:** Input — `(field_name, struct_name, scope_directory)`. Output — a checklist of consumer sites grouped by typed vs JSON-key access. Invocation — at the start of any wire-format-break task.

### Heap-not-touched / cost-pinning test pattern
- **What you did repeatedly:** Phase 3.3 added a `#[cfg(test)] thread_local!` counter on heap pushes to pin the cost win. This pattern would apply elsewhere — any optimization that avoids work could use the same scaffold.
- **Where it belongs:** A documented pattern in CLAUDE.md's test conventions section, OR a small `cost_counter!` macro in `code-graph-core` that generates the boilerplate.
- **Why a skill:** Manually scaffolding a thread-local counter, increment sites, reset/read helpers, and a two-phase assertion is ~30 lines of test infrastructure. A macro or pattern doc compresses this to ~5 lines per cost-pin.
- **Rough shape:** Input — `(counter_name, increment_sites)`. Output — the thread-local counter, the `#[cfg(test)]` bump helper, and reset/read helpers. Invocation — when implementing any optimization that "skips work" and a future refactor could silently re-introduce the work.
