---
title: "Code Review: GraphPlatformExpansion Phase 2 — Typed Core Layering"
type: review
status: resolved
created: 2026-08-08
updated: 2026-08-09
tags: [review, phase-2, track-a]
related:
  - Plans/GraphPlatformExpansion
  - Specs/GraphPlatformExpansion
  - Designs/TypedCoreLayering
review_of: "Plans/GraphPlatformExpansion"
rev: "a03b241"
findings:
  - id: F-01
    severity: major
    title: "The typed core is unreachable from another crate — entry points pub(crate), response types pub(super)"
    status: fixed
  - id: F-02
    severity: minor
    title: "core/mod.rs doc comment still says '16 gated core functions' after the design doc was corrected in the same commit range"
    status: fixed
followups: []
---

# Code Review: GraphPlatformExpansion Phase 2 — Typed Core Layering

**Reviewed state:** a03b241 (range 67dc2c5^..a03b241, clean worktree)
**Review mode:** independent — four fresh-context lanes, no project lanes configured

**Alignment:** Moderate. Drift, quality, and spec-compliance all returned **Strong** with zero findings; blind-spot returned **Elevated** with the two below. Both are fixed in `4690da5`, which landed after the review and therefore supersedes it.

All four lanes independently verified the phase's central claim rather than taking it on trust: `server.rs` byte-identical, zero snapshots touched, no `Cargo.toml`/`Cargo.lock` change, full workspace suite green.

## Findings

### F-01 — Major: the typed core cannot be called from another crate
**Impugns:** FR-01, AC-01, Designs/TypedCoreLayering Decision 3
**Caught by:** review_blind_spots
**Scenario:** Phase 2 exists so a CLI or socket front-end can reach graph logic without touching rmcp. Eighteen of the ~19 core entry points were `pub(crate)`, and several response types their signatures mention (`DependencyEntry`, `CallChainResponse`, `FindPathResponse`, and others) were `pub(super)` — crate-private. A downstream crate could neither call the functions nor name what they return. The asymmetry was visible inside a single file: `pub async fn analyze_codebase` beside `pub(crate) async fn analyze_codebase_async`.
**Why it matters:** The three plan-aware lanes all read this code and passed it, because they were checking whether the migration was *faithful* — which it was. Nothing in the test suite could catch it either: every test lives inside `code-graph-tools`, so cross-crate reachability is never exercised. It would have surfaced mid-CLI-build in phase 7, well after the context was cold.
**Recommendation:** Widen the entry points and every type reachable from their signatures.

### F-02 — Minor: a stale count in a doc comment
**Impugns:** Designs/TypedCoreLayering Decision 8
**Caught by:** review_blind_spots
**Scenario:** `core/mod.rs` still read "Each of the 16 gated core functions must call this at its own entry" after the design doc had been corrected — in the same commit range — to state a set-equality invariant instead of a fixed number.
**Why it matters:** A contributor adding the next gated tool reads the code comment, not the design doc, and reproduces exactly the counting anti-pattern the design revision was written to prevent.
**Recommendation:** State the invariant, and say to verify by comparing the two sets.

## Resolution Log

### F-01 — fixed (2026-08-08)
Widened 18 entry points across `core/{query,symbols,structure,watch,analyze}.rs` and every response type reachable from their signatures, including `ClassHierarchyResponse`'s fields — a caller that can name a type but not read its fields is no better off than one that cannot name it, and every sibling response type in the module was already fully public. Commit 4690da5.

### F-02 — fixed (2026-08-08)
Rewrote the comment to state set equality and to say explicitly that it must be verified by comparing the two sets, never by counting to a remembered total — which is how it went stale. Commit 4690da5.
