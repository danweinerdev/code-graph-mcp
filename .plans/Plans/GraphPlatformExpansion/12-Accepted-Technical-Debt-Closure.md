---
title: "Accepted Technical Debt Closure"
type: phase
plan: GraphPlatformExpansion
phase: 12
status: planned
created: 2026-08-22
updated: 2026-08-23
deliverable: "Every still-open, non-macOS, non-Perforce follow-up accepted by the completed GraphPlatformExpansion phase reviews is resolved through code, tests, or an explicit contract correction, with Linux and Windows evidence and no reopened completed phase."
tasks:
  - id: "12.1"
    title: "Close graph-query correctness and response-contract debt"
    status: complete
    justifies: "FR-05, FR-22, FR-24, FR-25, AC-16, AC-29, AC-32, NFR-03, and NFR-08; prevents false cap diagnostics, overflow-dependent community output, and undocumented response-size behavior recorded by review 16."
    verification: >-
      cargo test -p code-graph-graph callgraph and cargo test -p code-graph-tools --test snapshot_responses and cargo test -p code-graph-tools --test query_perf pass; an exact-frontier exhausted search reports cap_reached=false, community accumulation saturates deterministically, DetectCommunitiesResponse emits top-level members_per_community: u32 immediately after granularity with 10 default, 0-to-default, and max-100 clamp semantics, equal-cost path behavior is documented and test-pinned, and find_path's deliberate max_bytes exemption is stated in its production description and CLAUDE.md; make verify passes.
  - id: "12.2"
    title: "Harden the typed-core boundary and indexed-state contract"
    status: complete
    justifies: "FR-01, FR-02, FR-04, AC-01, AC-28, NFR-02, and NFR-10; prevents a future rmcp dependency leak or direct handler consumer from bypassing the honest indexed-state guard, and retains the queued synchronous caller's progress sink, as recorded by reviews 15 and 17."
    verification: >-
      cargo test -p code-graph-tools and cargo test -p code-graph-cli pass; a cross-crate compile fixture imports the typed core without rmcp, every public query entry either receives the honest indexed flag or is visibility-restricted so it cannot bypass the guard, all queued synchronous followers retain and drive their abstract progress sinks after promotion without notifying async aliases or reporting under the slot lock, the structural source scan covers the complete relevant source tree, and one canonical contract location replaces duplicated or contradictory guard prose; make verify passes.
  - id: "12.3"
    title: "Harden VCS ownership, detection, and blame semantics"
    status: complete
    justifies: "FR-27, FR-29, FR-32, FR-36, AC-21, AC-22, AC-34, AC-35, AC-44, and NFR-10; prevents deleted nested repositories from being treated as owned, misleading unavailability, and VCS discovery from blocking unrelated async queries, as recorded by review 15."
    verification: >-
      cargo test -p code-graph-vcs-git and cargo test -p code-graph-tools --test blame_symbol pass; ownership checks fail closed for deleted nested-clone and broken-gitlink fixtures, linked-worktree and provider-bound-elsewhere reasons are exact, every provider detection runs through spawn_blocking with no cache, the core no longer spells git-specific HEAD while the trait still has exactly four required operations, inert diagnostics are removed or made reachable, and empty-span attribution remains observable alongside divergence; make verify passes.
  - id: "12.4"
    title: "Bound symbol-history work and make uncertainty explicit"
    status: planned
    depends_on: ["12.3"]
    justifies: "FR-29, FR-33, FR-35, FR-36, AC-19, AC-20, AC-37, AC-44, and NFR-10; prevents provider failures from becoming false absence tombstones, deep histories from buffering hundreds of full snapshots, and synchronous cache I/O from blocking runtime workers, as recorded by review 18."
    verification: >-
      cargo test -p code-graph-tools --test symbol_history and cargo test -p code-graph-vcs-git pass; provider object/peel/blob failures are Operation unless tree absence is proven, skip-adjacent transitions have a test-pinned uncertainty contract through the existing skipped list, a maximum-window history walk retains at most 8 source snapshots targeting 32 MiB with one oversized source admitted alone and only one active AST/fingerprint operation, shard reads and parse/fingerprint CPU work execute through blocking-safe boundaries, and rename-to-HEAD observability is documented without promising the unqueryable removed ID; make verify passes.
  - id: "12.5"
    title: "Consolidate fingerprints and bound sidecar lifecycle"
    status: planned
    depends_on: ["12.4"]
    justifies: "FR-34, FR-37, AC-38, AC-39, AC-46, and NFR-02; prevents six language implementations from drifting, repeated reparsing of known unfingerprintable spans, key aliasing, and unbounded dead-identity sidecar growth, as recorded by reviews 18 and 20."
    verification: >-
      cargo test -p code-graph-lang and cargo test -p code-graph-lang-cpp and cargo test -p code-graph-lang-rust and cargo test -p code-graph-lang-go and cargo test -p code-graph-lang-python and cargo test -p code-graph-lang-csharp and cargo test -p code-graph-lang-java and cargo test -p code-graph-tools --lib fingerprint_cache and cargo test -p code-graph-tools --test symbol_history pass; all six plugins use one shared override helper, all six pin unlocatable-span degradation and cross-parser determinism, literal-insensitive failures cache a visible versioned outcome, key and AST streams use unambiguous framing, config identity includes extraction-relevant settings only, unreadable binaries do not collapse to identity zero, and a cross-process-locked sidecar trim triggers above 256 MiB or 100,000 completed shards and trims oldest completed shards toward 192 MiB and 75,000 while preserving temporary files and current-walk keys; extractor-span caveats are agent-visible and make verify passes.
  - id: "12.6"
    title: "Harden CLI and daemon fallback, cleanup, and rendering"
    status: planned
    justifies: "FR-12, FR-16, FR-17, FR-18, FR-19, FR-20, AC-09, AC-10, AC-11, AC-12, AC-40, and NFR-01; prevents permanent dead-owner metadata overhead, split-install fallback drift, leaked test daemons, positional swallowing surprises, and incorrect human rendering, as recorded by review 19."
    verification: >-
      cargo test -p code-graph-cli and cargo test -p code-graph-mcp --test daemon_serve and cargo test -p code-graph-mcp --test daemon_proxy pass; attach-only removes metadata only after proving the owner dead, Decision 7 fallback requires initialize.serverInfo.version to equal the CLI package version before matching the not-indexed error and the fallback wording is an explicit package-versioned contract, dead-owner and live-incompatible paths are independently pinned, optional-bool help gives an unambiguous invocation form, source scans walk every CLI source file, process fixtures clean up through RAII, tables preserve later-row fields and use unicode-width display measurement with bounded wrapping, and daemon results are classified from the command/response contract rather than generic JSON parseability; no daemon or MCP framing changes, machine payload parity remains byte-identical, and make verify passes.
  - id: "12.7"
    title: "Close resolver and candidate-count exposure seams"
    status: planned
    justifies: "FR-48, AC-57, NFR-11, and D-0007; prevents unresolved override tokens from appearing as resolved candidate-1 rows and ensures the full adapter path pins contested candidate counts, as recorded by review 21."
    verification: >-
      cargo test -p code-graph-tools --test candidate_count and cargo test -p code-graph-tools --test watch_virtual_overrides and cargo test -p code-graph-tools --test snapshot_responses and cargo test -p code-graph-tools --test snapshot_tools_list pass; bare unresolved override keys emit no provisional rows, a real candidates>=2 edge is snapshotted through the rmcp adapter, and trait/docs/tests state which confidence/count invariant future language-specific resolve_call overrides must preserve without deriving either signal from the other; make verify passes.
  - id: "12.8"
    title: "Pin residual Windows path, ACL, shutdown, and dogfood behavior"
    status: planned
    depends_on: ["12.1", "12.2", "12.3", "12.4", "12.5", "12.6", "12.7"]
    justifies: "NFR-13, AC-60, and D-0014; closes the remaining native-Windows test omissions recorded by reviews 19 and 22 without expanding the accepted single-user security scope."
    verification: >-
      On a native Windows runner, make submodules, make dogfood-required, and make verify pass; cargo test -p code-graph-mcp --test daemon_serve, cargo test -p code-graph-mcp --test daemon_proxy, cargo test -p code-graph-cli, and cargo test -p code-graph-tools --test path_normalization pass with bounded shutdown diagnostics, exact invoking-SID validation against an icacls-saved SDDL ACE, existing-path casing convergence plus the documented nonexistent/remove seam, explicit short-form/long-form canonicalization equivalence, and a dogfood-required gate that fails if any baseline auto-skips. A Linux make verify run at the same final candidate also passes.
---

# Accepted Technical Debt Closure

## Overview
Phases 1-9 and 11 are complete and frozen-reviewed, but their final review artifacts deliberately accepted bounded follow-ups rather than blocking otherwise-correct releases. This phase closes every still-open implementation, contract, test, performance, and platform-certification item from reviews 15-22. It does not rewrite completed task evidence or mutate frozen reviews; the source review remains historical evidence and this phase is the implementation record for its follow-ups.

Phase 10 macOS completion and the Perforce provider are excluded. Follow-ups already resolved after their source review are also excluded from implementation: the Python decorator pin and candidate-count doc/fixture corrections (`49a5a21`), watch override resolution (`8091042`), Rust lifetime-list fingerprinting and the revived literal-insensitive path (phase 8), the original Linux post-Windows verification (`14e2681`), and the D-0014 specification reconciliation (`385cc31`). Review 22’s suggested extra D-0014 annotation is historical decision-evidence bookkeeping, not product debt, and is explicitly excluded under this phase’s approved boundary. The coverage table below records those exclusions so “all debt” cannot silently mean “all prose ever written in a review.”

Execution may proceed in parallel for 12.1, 12.2, 12.3, 12.6, and 12.7. Task 12.4 follows provider hardening; 12.5 follows the history contract; 12.8 follows every implementation task so its native Windows run certifies the actual final candidate.

## Debt Coverage
| Source | Still-open follow-ups owned here | Already resolved or excluded |
|---|---|---|
| Review 16, Graph Queries | `find_path` byte-budget contract; exact-frontier `cap_reached`; `members_per_community` echo; saturating community weights; shortest-path determinism wording | Task-evidence checkpoint bookkeeping is historical, not product debt |
| Review 17, Typed Core | rmcp-free cross-crate compile pin; honest indexed-state boundary; `require_indexed` prose; adapter/core contract duplication | Phase-2 revision accounting is historical; CLI already routes through typed core with an honest flag |
| Review 15, VCS/Blame | repository ownership fail-open arms; provider-bound wording; inert diagnostic; synchronous detection; git-specific default revision; empty-span wording; queued-sync progress-sink retention (owned by 12.2) | none |
| Review 18, Symbol History | provider failures mapped to tombstones; blob error wording; skipped-transition uncertainty; cold-window buffering; blocking-pool mechanism test; synchronous shard reads; config over-invalidation; binary identity zero; duplicate key builders; key delimiter; rename observability | Rust lifetime lexing and dead literal-insensitive arm were superseded by phase 8 |
| Review 19, CLI | dead-owner metadata; unversioned fallback; missing owner/incompatibility tests; optional-bool positional behavior; incomplete source scan; missing RAII cleanup; Windows stop bound; renderer defects; JSON/Text inference | none |
| Review 20, Fingerprints | shared override helper; five-language degradation/determinism coverage; end-to-end skip contract; uncached unfingerprintable spans; sidecar pruning; AST framing; fallback-discrimination pin; negative-literal rationale; extractor-span tool caveat | Python decorator boundary pin landed in `49a5a21` |
| Review 21, Candidate Count | unresolved override rows; full-adapter contested-count snapshot; future override invariant | edge/find-overrides docs, count-staleness docs, and impossible fixtures landed in `49a5a21`; watch override resolution landed in `8091042` |
| Review 22, Windows | NTFS casing pin/limitation; strict DACL grant parsing; short/long path pin; dogfood baseline execution | the original Linux rerun landed in `14e2681`; spec reconciliation landed in `385cc31`; the suggested D-0014 annotation is historical evidence bookkeeping; macOS wording belongs to excluded phase 10 |

## 12.1: Close graph-query correctness and response-contract debt
### Subtasks
- [x] Make exact-frontier exhaustion report `cap_reached: false` when no work remains; retain `true` only when the cap actually stops a non-empty frontier.
- [x] Add graph-level tests for target found at the cap, unreachable target with exactly cap-sized reachability, and a genuinely truncated non-empty frontier.
- [x] Use saturating community weight accumulation and pin the overflow boundary without constructing billions of edges.
- [x] Add top-level `members_per_community: u32` immediately after `granularity` in `DetectCommunitiesResponse`; echo the resolved default 10 (`0` also resolves to 10) clamped to max 100 in MCP/CLI rendering, schemas, descriptions, and snapshots as an additive field satisfying FR-24/AC-32.
- [x] Tighten shortest-path determinism prose to graph-state determinism unless a stable merge-order-independent parent rule is implemented and regression-tested.
- [x] Document `find_path` as a deliberate `[response].max_bytes` exemption in both its production tool description and CLAUDE.md; retain `node_cap` as its bounded-work lever and state that the response is an indivisible valid path, never a partial continuation.

### Notes
Revision boundary: graph queries have honest cap, overflow, determinism, and response-shape contracts, with one additive community field and no partial-path wire state. `find_path` remains a single `FindPathResponse`; this task closes the review finding by making the exemption explicit rather than inventing invalid pagination semantics.

The additive community field’s declared wire order is `...Page` fields, `granularity`, `members_per_community`, `termination`, `iterations`, `node_count`, `edge_count`, `degenerate`; it must be reflected byte-identically by MCP and CLI. Existing `Page<T>` continuation semantics and the intentional `detect_cycles` exemption are out of scope and must not move. Description snapshots must retain every named argument’s default/ceiling and the exact non-Page response shape while adding this field.

### Trap
Do not run a serialized `find_path` through `byte_budget_take` or truncate `hops`: either produces a response that no longer proves a source-to-target path and violates FR-22. The accepted closure is an explicit exemption, not a malformed partial path.

### Completion Evidence

- Verified: 2026-08-23
- Repository: `~/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `97078e1d54c1639c4d8855d296af53974478ed8a`
- Identity recheck: `git rev-parse HEAD` at 2026-08-23T00:44:57-07:00 matched `97078e1d54c1639c4d8855d296af53974478ed8a`
- Focused review: `git show 97078e1d54c1639c4d8855d296af53974478ed8a`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `97078e1d54c1639c4d8855d296af53974478ed8a`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-graph callgraph` | `.` | PASS (`exit 0`) | 28 callgraph tests passed, including exact-frontier, target-at-cap, nonempty-frontier, leaf-at-cap, and equal-cost regressions. |
| `cargo test -p code-graph-tools --test snapshot_responses` | `.` | PASS (`exit 0`) | 59 response snapshot tests passed; default and member-capped community responses include the resolved cap. |
| `cargo test -p code-graph-tools --test query_perf` | `.` | PASS (`exit 0`) | Harness passed; its opt-in dogfood benchmark remained ignored as designed. |
| `cargo test -p code-graph-tools --test snapshot_tools_list` | `.` | PASS (`exit 0`) | 33 description snapshots passed, including both changed production descriptions. |
| `cargo fmt --all --check` | `.` | PASS (`exit 0`) | Rust formatting check passed. |
| `make verify` | `.` | PASS (`exit 0`) | Full gate passed: clippy denied warnings, formatting, workspace tests, no pending snapshots, and plugin mirrors in sync. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `intent-blind review-quality pass` | Complete task diff at `97078e1d54c1639c4d8855d296af53974478ed8a` | PASS | Initial cap-bypass finding was fixed; a second fresh review reported no findings and PASS/Aligned. |

## 12.2: Harden the typed-core boundary and indexed-state contract
### Subtasks
- [x] Add a cross-crate compile fixture or doctest that consumes every typed-core family without importing or depending directly on `rmcp`.
- [x] Audit public handler and core entry points; make bypass-only wrappers crate-private or require the real indexed state so no public consumer silently hardcodes `indexed=true`.
- [x] Retain each queued synchronous analyze request’s original `Arc<dyn ProgressSink>` through pending compaction and promotion; fan progress to all synchronous followers without attaching sinks to async aliases or holding the slot lock while reporting.
- [x] Add a deterministic test with two synchronous followers plus one async alias; use a blocking/reentrant sink to prove both sync sinks receive promoted phase/progress events, the async alias receives none, reporting holds no slot lock, and every waiter receives the same terminal result.
- [x] Reconcile `core/mod.rs`, handler, CLI, and CLAUDE.md guard wording with the actual single honest guard boundary.
- [x] Replace hardcoded source-file allowlists in architecture tests with complete directory traversal so newly added files cannot evade dependency scans.
- [x] Consolidate duplicated adapter/core contract prose into one canonical comment per contract and link the thin adapter rather than copying it.
- [x] Correct the parent plan’s stale accepted-followup prose where later work already closed resolver and platform items; never edit frozen review findings to simulate closure.

### Notes
Revision boundary: the typed core is mechanically consumable without MCP, every externally reachable query path has an honest indexed-state contract, queued synchronous callers retain progress after promotion, and structural tests cover future source additions. Wire behavior remains byte-identical.

No public API compatibility promise exists for workspace-internal handler functions, but prefer visibility narrowing over adding redundant state parameters when only the server uses a wrapper. Protected core crates remain free of async, I/O, MCP, and new dependencies under NFR-02.

### Completion Evidence

- Verified: 2026-08-23
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `75fea77bcf1fceebbd8e5a8a7a8fedfc36fcd8f1`
- Identity recheck: `git rev-parse HEAD` at 2026-08-23 00:00 matched `75fea77bcf1fceebbd8e5a8a7a8fedfc36fcd8f1`
- Focused review: `git show 75fea77bcf1fceebbd8e5a8a7a8fedfc36fcd8f1`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `75fea77bcf1fceebbd8e5a8a7a8fedfc36fcd8f1`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `make verify` | `.` | PASS (`exit 0`) | `full repository gate passed: clippy denied warnings, formatting passed, workspace tests passed, snapshots were clean, and generated plugin mirrors were synchronized.` |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `Targeted verification: cargo test -p code-graph-tools; cargo test -p code-graph-cli; cargo test -p code-graph-tools --test typed_core_consumer; cargo test -p code-graph-tools --test blame_symbol; cargo test -p code-graph-tools --test symbol_history; cargo fmt --all --check; cargo clippy --workspace --all-targets -- -D warnings` | `task 12.2 implementation at 75fea77bcf1fceebbd8e5a8a7a8fedfc36fcd8f1` | PASS | `all targeted commands passed with exit 0; the fixture compiled every typed-core family without rmcp, recursive architecture scans covered nested Rust sources, history adapters honored real indexed state, and progress fan-out tests passed.` |

## 12.3: Harden VCS ownership, detection, and blame semantics
### Subtasks
- [x] Make repository ownership fail closed when parent discovery errors and the queried path is missing, including deleted nested clones and broken gitlinks over outer-repository paths.
- [x] Distinguish provider-bound-elsewhere, linked-worktree/different-checkout, no-provider, and genuinely untracked-path unavailability reasons.
- [x] Remove the unreachable non-`Unavailable` startup breadcrumb or preserve actionable provider-open errors so the branch is real and testable.
- [x] Run `gix::discover` and equivalent provider detection in `spawn_blocking` on each history request; do not add provider caching or an invalidation policy in this phase.
- [x] Change the existing required resolve operation to `resolve_rev(spec: Option<&str>)`, where `None` requests the provider default, and remove the core’s literal `HEAD`; retain exactly four required `VcsProvider` operations under FR-27/AC-34.
- [x] Preserve both divergence and no-attributable-lines information when a blamed span produces no hunks.
- [x] Add hermetic fixtures for all ownership and reason branches plus a concurrency test proving slow detection delays only history tools.

### Notes
Revision boundary: provider selection and ownership are fail-closed and provider-neutral, blame errors say what happened, and no detection work blocks unrelated async queries. The git implementation remains confined to `code-graph-vcs-git`; no Perforce implementation or fifth required trait method is introduced.

Linked worktrees of one repository are different checkouts, not different repositories. Preserve success-shaped unavailability for expected absence; reserve tool errors for provider operations that genuinely failed.

### Trap
Do not “fix” ownership by accepting any path whose lexical parent lies under the bound root. Nested repositories and gitlinks are exactly where lexical containment and repository ownership diverge.

### Completion Evidence

- Verified: 2026-08-23
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `dd1bdcbf095eff024aafcdcb4ef1d86dcf8f8beb`
- Identity recheck: `git rev-parse HEAD` at 2026-08-23 00:00 matched `dd1bdcbf095eff024aafcdcb4ef1d86dcf8f8beb`
- Focused review: `git show dd1bdcbf095eff024aafcdcb4ef1d86dcf8f8beb`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `dd1bdcbf095eff024aafcdcb4ef1d86dcf8f8beb`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `make verify` | `.` | PASS (`exit 0`) | `full repository gate passed: clippy denied warnings, formatting passed, all workspace tests passed, pending snapshots were absent, and generated plugin mirrors were synchronized.` |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `Targeted verification: cargo test -p code-graph-vcs; cargo test -p code-graph-vcs-git; cargo test -p code-graph-tools --test blame_symbol; cargo test -p code-graph-tools --test symbol_history; cargo test -p code-graph-cli; cargo test -p code-graph-mcp; cargo clippy --workspace --all-targets -- -D warnings; cargo tree -p code-graph-vcs-git` | `task 12.3 implementation and four-lane focused review at dd1bdcbf095eff024aafcdcb4ef1d86dcf8f8beb` | PASS | `all targeted commands passed; 8 VCS tests, 18 Git-provider tests, 14 blame tests, and 13 symbol-history tests pin fail-closed ownership, exact selection reasons, uncached blocking-isolated detection, provider-neutral defaults, detector error propagation, and composed empty-span/divergence diagnostics; dependency inspection showed no new native library.` |

## 12.4: Bound symbol-history work and make uncertainty explicit
### Subtasks
- [ ] Map revision parse/object/peel/blob failures to `Operation`; emit `NotFound` only after proving the immutable revision tree lacks the path.
- [ ] Add partial-clone, pruned-object, and missing-blob regressions proving failures are not cached as absence tombstones or described as “no history.”
- [ ] Keep the global `skipped` list as the sole uncertainty signal because it already lets clients reconstruct skip-adjacent uncertainty from the same response; document that reconstruction and pin it through the wire response instead of adding a redundant per-entry boolean.
- [ ] Replace whole-window snapshot prefetch with ordered batches retaining at most 8 source snapshots and targeting at most 32 MiB of source bytes; admit one oversized source alone so every revision remains examinable, parse/fingerprint one snapshot at a time, drop its AST before advancing, and preserve oldest-to-newest transition semantics across batch boundaries.
- [ ] Move shard reads and parse/fingerprint CPU work into batched blocking boundaries; inject a gated slow parser/cache double proving runtime isolation rather than relying on source inspection.
- [ ] Instrument tests to assert a `window=500` walk’s live source-buffer high-water mark is at most 8 snapshots and 32 MiB unless one source itself exceeds 32 MiB; then assert it is the only retained source and only one AST/fingerprint operation is active. Do not claim a hard bound below the largest individual input.
- [ ] Clarify rename-to-HEAD observability: the current ID can report its introduction, while the old removed ID is no longer queryable from the current graph.

### Notes
Revision boundary: a deep cold history walk has bounded source prefetch and one-at-a-time AST work, is async-safe, and never converts provider failure into authoritative absence. Public entry ordering and exact `(name, kind)` matching remain unchanged.

Chunking must carry the previous symbol state across boundaries and must not emit a synthetic transition at each chunk edge. Fingerprint-key and sidecar lifecycle changes belong to 12.5.

### Completion Evidence

Pending — not complete.

## 12.5: Consolidate fingerprints and bound sidecar lifecycle
### Subtasks
- [ ] Extract one shared `code-graph-lang::fingerprint` helper for the six plugin overrides while retaining each language’s node-location and literal predicates.
- [ ] Add unlocatable-span degradation and cross-parser-instance determinism tests for Rust, Go, Python, C#, and Java, matching the existing C++ discrimination.
- [ ] Add a tool-level `literal_insensitive` unfingerprintable-span regression asserting the visible skip reason through `symbol_history`.
- [ ] Persist a versioned cache outcome for known unfingerprintable spans so repeated walks do not reparse them; corruption still recomputes rather than errors under FR-37.
- [ ] Narrow `config_identity` to extraction-relevant configuration and centralize all `FingerprintKey` construction in one builder.
- [ ] Replace executable-identity fallback `0` with a deterministic non-aliasing fallback that does not claim two unreadable binaries are the same build.
- [ ] Replace delimiter-only key and AST streams with unambiguous length-prefix or equivalent framing, including raw `0x1f` adversarial tests.
- [ ] Add a process-shared sidecar maintenance lock and trim when completed shards exceed 256 MiB or 100,000 files, selecting oldest eligible completed shards toward both 192 MiB and 75,000-file low-water marks; trigger at sidecar open and after each 256 successful writes.
- [ ] Never prune temp files or keys retained by the current walk. Concurrent readers treat a missing pruned shard as a cache miss and recompute; tests must pin atomic reads/writes and harmless cross-process eviction rather than promise permanent retention of every live identity.
- [ ] Make the LiteralInsensitive no-fallback property explicit in the boundary tests, document why `negative_literal` is traversed rather than classified as a literal node, and add the extractor-span caveat to the agent-facing history description within its byte budget.

### Notes
Revision boundary: all language plugins share one fingerprint control flow, cache outcomes are deterministic and unambiguously keyed, and dead sidecar state is bounded without weakening corruption self-healing or concurrent reads.

The fingerprint sidecar is disposable and separate from `.code-graph-cache.db`; a sidecar format/key version change may abandon old entries for pruning, but must never require a main graph cache bump. The 256 MiB/100,000 high-water and 192 MiB/75,000 low-water thresholds are internal constants, not configuration or wire surface. If protected current-walk keys leave no eligible shard before a low-water mark, stop successfully, emit one `eprintln!` diagnostic with residual size/count, and retry at the next maintenance trigger.

### Trap
Do not make pruning “delete every identity except the current process.” Prune oldest completed shards only under the process-shared maintenance lock; never touch temp files, and keep cache misses non-authoritative so concurrent eviction can cost recomputation but cannot change a history answer.

### Completion Evidence

Pending — not complete.

## 12.6: Harden CLI and daemon fallback, cleanup, and rendering
### Subtasks
- [ ] Remove dead-owner `daemon.json` only after lock/process-identity revalidation proves no live owner, so later CLI calls avoid the permanent spawn/handshake/fallback double hop.
- [ ] Retain the existing initialize response and require `serverInfo.version == env!("CARGO_PKG_VERSION")` before Decision 7 may match the byte-exact not-indexed wording and retry standalone. Treat that wording as a package-versioned compatibility contract: any wording/discriminator change requires a package-version bump. Mismatch returns the original daemon result without fallback.
- [ ] Split stale-metadata tests so dead-owner, live-compatible, and live-incompatible attach-only branches are independently exercised; pin that attach-only never triggers replacement.
- [ ] Document optional-bool flags with positional-first or `--flag=<bool>` syntax and add clap regressions for filenames literally named `true`/`false`.
- [ ] Make no-rmcp/no-handlers structural scans traverse every CLI source file, including future modules.
- [ ] Wrap spawned daemon fixtures in kill-on-drop RAII guards so assertion failures cannot leak processes or hold TempDirs.
- [ ] Add the scoped `unicode-width` dependency to `code-graph-cli` and fix human table rendering with Unicode display-width measurement, bounded cell wrapping, and union-of-all-row columns in deterministic order.
- [ ] Classify daemon results from the command contract, not generic JSON parseability: `generate-diagram --format mermaid` and the fixed non-callable advisory branches are Text; all declared JSON response shapes are Value. Keep the raw MCP/daemon byte stream unchanged and preserve `--json` payload bytes and plain-text output exactly.
- [ ] Retain all existing daemon fallback breadcrumbs on stderr and add no `tracing` dependency.

### Notes
Revision boundary: stale daemon state self-cleans safely, split installations fail or fall back honestly, CLI argument behavior is explainable, tests cannot leak daemons, and human rendering is correct without changing machine output.

No daemon or MCP framing metadata is added and `serverInfo.version` retains its standard package-version meaning. The renderer’s output-kind table is exhaustive over CLI subcommands and dynamic advisory branches. MCP tool payloads and CLI `--json` output remain byte-identical under NFR-01/AC-11/AC-40. Windows shutdown timing belongs to 12.8 because it requires native certification.

### Trap
Do not delete `daemon.json` merely because one connection attempt failed. Replacement, saturation, startup publication, and transport fallback all create temporary connection failures while a valid owner may still exist.

### Completion Evidence

Pending — not complete.

## 12.7: Close resolver and candidate-count exposure seams
### Subtasks
- [ ] Filter unresolved `Overrides` targets before reverse traversal so a bare provisional token cannot produce a candidate-1 `find_overrides` row.
- [ ] Add graph/core/handler regressions distinguishing unresolved override tokens from resolved contested overrides.
- [ ] Add a full rmcp adapter snapshot with a real `candidates >= 2` edge; retain the existing core-level literal-key assertions.
- [ ] State and test the `LanguagePlugin::resolve_call` contract future overrides must satisfy for `Confidence`, `candidates`, and `min_confidence`; keep the signals independent under D-0007.
- [ ] Re-audit the review-21 follow-ups already closed by later commits and add no duplicate compatibility shim or cache invalidation.

### Notes
Revision boundary: no unresolved override token reaches an agent-facing row, contested counts are pinned through the complete adapter, and future language-specific resolvers have an executable signal contract.

Do not globally drop unresolved Calls from storage: scoped cache growth deliberately preserves provisional targets, and newer Go markers have their own tracked lifecycle. Filter at the resolved-node traversal boundary shared by other call tools.

### Completion Evidence

Pending — not complete.

## 12.8: Pin residual Windows path, ACL, shutdown, and dogfood behavior
### Subtasks
- [ ] Add a native NTFS regression proving mixed casing of an existing file converges to one canonical graph key; retain an explicit known limitation for nonexistent/remove-event casing if the OS cannot canonicalize it.
- [ ] Add a dedicated short-form/long-form Windows path equivalence fixture independent of the runner’s TEMP spelling.
- [ ] Save the runtime directory DACL through `icacls /save`, parse its SDDL ACEs, and require exactly one full-control allow ACE whose SID exactly matches `whoami /user`; do not infer identity from localized account-name text, path text, or summary output.
- [ ] Bound Windows CLI/daemon shutdown waits and report the last observed process/control-file state on timeout.
- [ ] Add `make dogfood-required`: preflight every pinned dogfood checkout, set a harness flag that promotes any baseline auto-skip to failure, run all eight listed baselines, and emit an executed/pass/fail count. Run it after `make submodules` on the native runner.
- [ ] Run the full native Windows matrix after tasks 12.1-12.7, including daemon serve/proxy, CLI parity, path normalization, VCS/history, resolver, and fingerprint rows affected by this phase.
- [ ] Run Linux `make verify` at the same final candidate and record both platform identities in phase evidence.

### Notes
Revision boundary: the remaining Windows omissions are directly pinned and the complete debt-closure candidate is certified on Windows and Linux. This task does not claim or test cross-account isolation; D-0014’s one-local-user/one-local-project scope remains binding.

Native Windows evidence is mandatory. Cross-compilation, Wine, or Linux path simulation cannot complete this task. Phase 10 macOS seams remain untouched.

### Completion Evidence

Pending — not complete.

## Acceptance Criteria
- [ ] Every still-open implementation, contract, test, performance, and Windows-certification follow-up in final reviews 15-22 is mapped to exactly one completed Phase 12 task; already-resolved, macOS, Perforce, and historical-evidence items are explicitly dispositioned in the coverage table.
- [ ] `find_path`, shortest-path cap reporting, and community responses satisfy FR-05, FR-22, FR-24, FR-25, AC-16, AC-29, and AC-32 with agent-facing descriptions matching runtime behavior.
- [ ] Typed-core consumers cannot acquire MCP coupling or bypass honest indexed-state gating, satisfying FR-01, FR-02, FR-04, AC-01, AC-28, and NFR-02.
- [ ] VCS and symbol-history failures, blocking boundaries, memory bounds, and cache behavior satisfy FR-27, FR-29, FR-32-FR-37, AC-19-AC-22, AC-34, AC-35, AC-37, AC-44, AC-46, and NFR-10.
- [ ] CLI machine output remains byte-identical across daemon and standalone paths while stale metadata, rendering, cleanup, and argument behavior satisfy FR-12, FR-16-FR-20, AC-09-AC-12, and AC-40.
- [ ] Candidate-count and override behavior preserve FR-48, AC-57, NFR-11, and D-0007 without collapsing confidence and candidate count into one signal.
- [ ] Native Windows evidence closes the remaining path, ACL, shutdown, and dogfood omissions within NFR-13, AC-60, and D-0014; Linux `make verify` passes at the same final candidate.
- [ ] Every task lands as a focused native-SCM revision with its named focused tests and `make verify` passing; no pending snapshots or plugin drift remain.
- [ ] A fresh four-lane phase review over the complete frozen Phase 12 range returns Aligned with no open findings before the phase is marked complete.

## Phase Completion Evidence
Pending — not complete.
