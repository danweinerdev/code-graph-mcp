---
title: "Phase 2 Debrief: Typed Core Layering"
type: debrief
status: complete
plan: GraphPlatformExpansion
phase: 2
phase_title: "Typed Core Layering"
created: 2026-08-09
updated: 2026-08-09
tags: [refactor, track-a, wire-compatibility]
related:
  - Plans/GraphPlatformExpansion
  - Designs/TypedCoreLayering
---

# Phase 2 Debrief: Typed Core Layering

Six tasks, ten commits, ~4,300 lines moved. Every handler body now lives in `core/` returning `ToolResult<T>`; the handlers are one-line adapters.

## Decisions Made

- **The core takes `indexed: bool` in three of six modules.** `core::watch` receives `&Arc<ServerInner>` and reads `inner.indexed` directly, so its guard genuinely double-checks. `core::{query,symbols,structure}` receive only `&RwLock<Graph>` and cannot see the flag, so they take it as a parameter and the adapter passes `true`. Changing the handler signatures to carry `ServerInner` would have restored the double-check at the cost of Decision 3's guarantee — not a trade worth making. Design Decision 8 was amended to record this.
- **`ClassHierarchyResponse`'s fields were widened along with the struct.** Naming a type you cannot read is no more useful than not naming it, and every sibling response type was already fully public.
- **Fixes were batched before re-review**, per the phase 1 lesson. One review cycle instead of two.

## Requirements Assessment

FR-01 through FR-05 realized; NFR-01, NFR-02, NFR-04, NFR-05 satisfied; AC-01, AC-02, AC-03, AC-28, AC-29, AC-41 covered with cited tests. The spec-compliance lane verified each independently, including running the commands rather than trusting the plan's claims.

## Deviations

- **AC-41 was factually wrong and had to be corrected, not merely satisfied.** It claimed `tracing` appears nowhere in the dependency graph; `rmcp` pulls it transitively and always has. Restated as "no crate declares a direct dependency and nothing here logs through it", which is what was meant and is checkable.
- **Decision 8's "16 gated call sites" was stale** — phase 1's three tools made it 19 sites over 18 functions. Restated as a set-equality invariant.
- **The phase is left `in-progress` with all tasks complete.** The blind-spot findings were fixed after the review, so certification would need a fresh cycle.

## Risks & Issues Encountered

- **The typed core shipped unreachable from any other crate.** Entry points `pub(crate)`, response types `pub(super)`. This is the phase's headline lesson: the three plan-aware lanes all passed the code because the migration *was* faithful, and no test could catch it because every test lives inside the crate. Only the intent-blind lane, asking "what would a fresh reader trip over", found it.
- **A doc comment outlived the design doc it contradicted**, corrected in the same commit range. Code comments are not swept when a design is amended.

## Lessons Learned

- **Faithfulness and usefulness are different properties, and different reviewers see them.** Three lanes verified the refactor preserved behaviour. None asked whether the result achieved its purpose. Budget for the intent-blind lane specifically.
- **A test suite entirely inside one crate cannot test cross-crate contracts.** Both this phase's Major finding and phase 1's would have been caught by exercising the real boundary — the `#[tool]` wrapper there, another crate here.
- **Batching fixes before re-review works.** Phase 1 spent two cycles by fixing findings piecemeal; phase 2 spent one.

## Impact on Subsequent Phases

- **Phase 7 (CLI) is now genuinely unblocked** — it was not before the visibility fix, and would have discovered that itself.
- **Phase 3** is unaffected; the daemon serves the existing `CodeGraphServer` and does not touch the core layering.
- **A cross-crate smoke test** — a tiny consumer that calls a `core::` function and reads a response field — would make the FR-01 guarantee permanent rather than a point-in-time fix. Worth adding when phase 7 creates a second crate that can host it.

## Skill Opportunities

- The two Major findings across phases 1 and 2 share a shape: a contract that no test exercises because the tests sit on the wrong side of the boundary. A convention of one test per boundary — wrapper, crate edge — would catch the class.
