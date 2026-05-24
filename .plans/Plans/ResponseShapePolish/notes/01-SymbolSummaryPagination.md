---
title: "Phase 1 Debrief: get_symbol_summary pagination + <global> rename"
type: debrief
plan: "ResponseShapePolish"
phase: 1
phase_title: "get_symbol_summary pagination + <global> rename"
status: complete
created: 2026-05-14
updated: 2026-05-14
tags: [mcp, pagination, ue, unreal-engine, ergonomics, hierarchy, diagrams, coupling, dependencies, fuzzy-match]
---

# Phase 1 Debrief: get_symbol_summary pagination + `<global>` rename

## Decisions Made

- **The `<global>` caveat in the tool description was rewritten honestly, departing from the literal phase-plan wording.** Plan said the description should advise agents to "use `namespace=\"\"` in `search_symbols` to filter to global-scope symbols". Task 1.3's implementer discovered (`crates/code-graph-graph/src/queries.rs:231`) that `Graph::search` short-circuits `namespace=""` as "no filter, return everything". The original wording would have actively misled agents. The shipped description (`server.rs:580–610`) instead explains the gap and redirects to `kind`/`query` filters.

- **The `<global>` NOTE was hoisted to immediately after the row-shape sentence** (was at char ~620 of a ~970-char description; now at ~270). Quality scanner finding #1 on Task 1.5 surfaced this: agents that pattern-match on the early prefix would miss the critical correctness trap if buried at the end.

- **Stub-state Major findings were accepted with deferral to the next task** rather than fix-then-accept. Task 1.1's intentional stub returned `Page<SummaryRow>` with `limit: 100, truncated: false` hard-coded; quality scanner correctly flagged this as a Major correctness defect on the wire envelope. The orchestrator (plan-aware) chose to proceed to Task 1.2 immediately rather than have 1.1 partially-implement pagination. Task 1.2 deleted the stub body wholesale; harm window was zero because nothing committed between tasks.

- **Per-task auto-commits were adopted** (vs. the phase-boundary commits used for PathNormalization and UeMacroSupport). Task 1.2's implementer auto-committed (`16848ae`) without orchestrator instruction; the user OK'd continuing the pattern. Subsequent commits: `ca0f46f`, `1a114aa`, `fb37f42`, `39551f6`. Finer-grained rollback at the cost of more noisy history.

- **Task 1.1's work was bundled into the Task 1.2 commit** rather than committed separately. The implementer of 1.2 staged the uncommitted 1.1 changes (`SummaryRow` type + return-shape change + Task 1.1's snapshot regen) into `16848ae`. Not ideal for granular attribution but not harmful — the two tasks form one logical "shape change" unit.

- **Tasks 1.3 and 1.4 were serialized (Waves 3a, 3b) rather than parallelized**, even though both depended only on Task 1.2. Both modified the same handler function (`get_symbol_summary` body) in `crates/code-graph-tools/src/handlers/symbols.rs`. Per the implement-skill's advisory-overlap step, serial was the cheap call.

- **Task 1.1 added the row sort earlier than the literal task brief required.** The brief said "no sort yet — Task 1.2 will overwrite this." The implementer added it anyway because without a stable sort, the regenerated `response_get_symbol_summary_whole_graph` snapshot would have been flaky (HashMap iteration is non-deterministic; `parsed_sorted` only canonicalizes object keys, not array element order). Task 1.2 kept the sort and added `byte_budget_take` around it — no thrown work.

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| `SummaryRow` type defined and exported as needed | Met | `pub(super) struct SummaryRow { namespace: String, kind: &'static str, count: u32 }` in `handlers/mod.rs:72–95` next to `Page<T>`. |
| Handler returns `Page<SummaryRow>` with `<global>` rename | Met | Empty-namespace rows render as the literal `"<global>"`; graph `Symbol.namespace` untouched. Two unit tests pin this (`summary_renames_empty_namespace_to_global`, `search_symbols_with_empty_namespace_filter_still_finds_globals_after_summary`). |
| `count_only` returns row-count total in sentinel shape | Met | Sentinel exactly `{results: [], total, offset: 0, limit: 0, truncated: false, next_offset: None}`. `total = summary.values().map(|m| m.len()).sum() as u32` (pair count, NOT symbol sum); pinned by `symbol_summary_count_only_does_not_count_individual_symbols`. |
| Tool description rewritten and snapshot accepted | Met | Description covers envelope shape, sort, defaults, `count_only`, byte-budget contract, `<global>` caveat, byte-cap detection caveat. Sibling-handler phrasing parity verified for 4 sentences. Snapshot accepted deliberately at each task; `make snapshot-clean` clean at phase close. |
| `cargo clippy --workspace --all-targets -- -D warnings` clean | Met | Verified at every wave + at phase close. |
| `cargo fmt --all --check` clean | Met | Same. |
| `make snapshot-clean` passes | Met | Same. |
| 196 KB UE-scale rejection eliminated | Met (assumed) | Cannot verify against the generic UE codebase exercised during dogfooding (private Perforce). The `symbol_summary_over_one_hundred_rows_caps_at_default_limit` test + byte-budget machinery should prevent any single response from exceeding `[response].max_bytes` (default 100 KB). Final acceptance regression is Phase 6's synthetic high-fanout fixture. |

**16 new tests added** across Tasks 1.2 (7), 1.3 (2), 1.4 (3), 1.5 (4 deserialization). `cargo test --workspace`: 1179 passed / 0 failed / 2 ignored.

## Deviations

- **Honest `<global>` description vs. the phase-plan literal wording** — see Decisions Made, item 1. The plan's verification field still carries the original wording; future readers comparing the plan text to the shipped description will see the divergence. Documented here so it's not misread as drift.

- **Task 1.1 bundled into Task 1.2's commit** — `git show 16848ae` covers both. Future bisect targets at "first SummaryRow shape change" land on 16848ae, not on a separate 1.1 commit.

- **Sort added in Task 1.1 instead of Task 1.2** — see Decisions Made. Task 1.2 verification field still says "(c) sort the Vec by `(namespace, kind_str)` ascending" as task-1.2 work; readers should be aware the sort actually landed in 1.1.

- **Quality scanner findings absorbed into Task 1.6's commit** — the Task 1.5 quality-scan surfaced two Minor findings (`<global>` caveat positioning, sibling-test-message parity). Both were fixed inline by the orchestrator (not by re-dispatching the implementer) and committed as `39551f6` under Task 1.6's banner. Task 1.5's commit (`fb37f42`) carries the pre-fix wording.

- **Per-task commits were adopted mid-phase** — the prior plans (PathNormalization, UeMacroSupport) used phase-boundary commits. This phase shifted to per-task auto-commits. Subsequent ResponseShapePolish phases should choose explicitly upfront.

## Risks & Issues Encountered

- **`search_symbols(namespace="")` does not filter to global-scope only.** Discovered at Task 1.3 implementation time. `Graph::search` (queries.rs:231) treats the empty namespace as "no filter" via the `lower_ns.is_empty()` short-circuit. The phase plan's verification field for 1.3 and the planned description wording in 1.5 both promised "use namespace=\"\" for the global filter" — both wrong. Resolution: rewrote 1.5's description to document the gap honestly; updated 1.3's asymmetry test to use `query=Some("foo")` to scope the assertion meaningfully. **No code change to `Graph::search` — out of phase scope.**

- **Stub-state Major findings from quality-scanner.** Task 1.1's intentional stub body returned `Page<SummaryRow>` with hard-coded `limit: 100, truncated: false`. Quality scanner correctly identified this as wire-envelope dishonesty. Per-task review protocol says "Critical → re-dispatch implementer; Major → collect and present"; the orchestrator's judgment was that Task 1.2 (immediately following) deleted the stub wholesale, so the harm window was zero. Surfaced to user with explicit recommendation; user approved proceeding.

- **`byte_budget_take` count-cap convention is non-obvious.** When the count `limit` bites (not the byte budget), the helper returns `truncated=false, next_offset=None`. Agents must detect "more pages exist" via `offset + results.len() < total`, NOT via `truncated`/`next_offset`. CLAUDE.md's documented contract ("`truncated=false` plus `next_offset=null` means the page is the natural end of the result set") arguably conflicts with this behavior. Resolution: copied the sibling-handler parity sentence ("`results.length` may be less than `limit` when the byte cap fires, so consult `truncated`, not length, to detect partial pages") into the new description.

- **`cargo insta accept --snapshot <name>` silently mismatches** the filename pattern. Today the CLI flagged the snapshot as "skipped" without an error; the `.snap.new` had to be manually `mv`'d to `.snap` to apply the regeneration. Brittle CLI ergonomics — see Skill Opportunities.

- **One transient API 529 Overloaded error** when dispatching Task 1.2's implementer agent. Retry succeeded; no follow-up work required.

## Lessons Learned

- **Quality-scanner is intent-blind by design and correctly flags transitional stubs as bugs.** The orchestrator's plan-aware context lets them defer findings when the next task is the natural fix. This isn't "the scanner was wrong" — it's "the scanner saw a snapshot of a deliberately-broken state". Defer-and-document is the right move; just be explicit with the user.

- **Verification fields that cite specific external behaviors (e.g., "search_symbols(namespace="") returns globals") can be wrong.** Plan authors don't always trace the codebase to confirm. Implementer time is the next defense; the implementer of 1.3 caught this by running the test and seeing 0 results when it expected 1. **Lesson:** verification fields naming external symbols should be cross-checked against the codebase during the readiness audit, not at implementation time.

- **Sibling-handler phrasing parity matters more than expected.** Agents pattern-match on production tool descriptions. A near-paraphrase of "consult `truncated`, not length, to detect partial pages" loses the agent-pattern-match value the parity was supposed to provide. Task 1.5 copied four sentences verbatim from `get_callers` / `get_callees` / `get_file_symbols` / `get_orphans` for exactly this reason.

- **Description position matters.** The `<global>` caveat was originally placed at the end of the description (char ~620 of ~970). A pattern-matching agent reading the early prefix would never encounter the critical "`search_symbols(namespace=\"\")` doesn't filter to globals" warning. Quality scanner caught this; the fix was a 5-minute re-paragraph. **Lesson:** put the load-bearing correctness traps EARLY in the description, not at the end.

- **`Cow::Borrowed` fast paths are correct but easy to miss.** `CppParser::preprocess` in the previous plan (UeMacroSupport) carefully short-circuits to avoid allocation when neither macro list is populated; this phase's analogue is the `count_only=true` early-return that skips flatten + sort + byte_budget_take entirely. Both follow the same pattern: check the cheap predicate, return the sentinel/borrowed form, skip the expensive path. Worth lifting as a "cheap-path-first" idiom note somewhere.

- **Tool descriptions are agent-facing production behavior.** Edits to `#[tool(description=...)]` strings should always go through the Agent-facing-tool-descriptions lens of `quality-scanner`. The action-verb operational-correctness check ("if an agent does X, does it work?") caught the misleading `namespace=""` guidance before it shipped.

## Impact on Subsequent Phases

- **Phase 6 (CLAUDE.md sweep) must document `Graph::search` empty-namespace semantics.** The CLAUDE.md "Per-language parser facts" or "Core invariants" section should pick up a line like: "`search_symbols(namespace=\"\")` is 'no filter' — there is no way to query for global-scope symbols only. Use `get_symbol_summary` to confirm they exist." This is a real codebase invariant that future agents need.

- **Phase 6 must propagate the `count_only` description pattern to remaining paginated tools.** Phases 3 (`generate_diagram`), 4 (`get_dependencies`), 5 (`detect_cycles`) all add or modify paginated tool descriptions. Each should include the same byte-cap caveat sentence, the `next_offset = resume` sentence, and (where applicable) `count_only` semantics — verbatim with the existing 5 paginated handlers for agent pattern-match parity.

- **Phase 5 (`search_symbols` suggestions) inherits the discovery** that `namespace=""` isn't a global filter. The proposed `suggestions: Vec<String>` field for `^…$` anchored zero-hit queries doesn't need to special-case global-scope; the existing fallback (return all-matching-other-filters) is what users get today.

- **Phase 4 (`get_coupling` + `get_dependencies` + `.ini` filter) extends the `Page<T>` envelope to two more tools.** Phase 1's `byte_budget_take` integration pattern (resolve defaults → flatten → sort → byte-budget-take → emit Page) is directly reusable; new tools in Phase 4 should follow the same shape. Watch for the same stub-state Major-findings problem if implementers split type-definition from body-refactor across separate tasks.

- **Cache schema break (Phase 4 D10) is unaffected.** Phase 1 touched only response shape, no graph or cache types.

- **Per-task commit pattern is now the default.** Subsequent phases should keep this unless the user objects. Phase commits are the existing convention from PathNormalization / UeMacroSupport; per-task commits this phase produced finer-grained rollback at the cost of `git log` noise.

- **Cross-phase synthetic fixtures.** Phase 1's `>100-row fixture test` (`symbol_summary_over_one_hundred_rows_caps_at_default_limit`) builds a tiny synthetic graph inline. Phase 6's "acceptance regression" plans a larger synthetic high-fanout fixture; the inline pattern from Phase 1 may be inadequate for Phase 6's needs. Consider committing the Phase 6 fixture as a fixture file (per `crates/code-graph-tools/tests/fixtures/ue_synthetic/` from UeMacroSupport) rather than inline.

## Skill Opportunities

User-confirmed list (Phase 1 noted these as worth building before Phase 2 starts):

### 1. `/close-phase` — atomic phase-doc + plan README status update

- **What I did repeatedly:** at the end of each task and at end-of-phase, edited TWO YAML frontmatters: the phase doc's `status:` field AND (at phase close) the plan README's `phases[].status` entry. I forgot one twice during Phase 1 — the asymmetry between phase-doc updates (every task) vs. plan-README updates (only at phase close) is easy to miss.
- **Where it belongs:** new `/sdd-planner:close-phase` slash command (paired with `/sdd-planner:start-phase`).
- **Why a skill:** prevents stale `status: in-progress` in the plan README after a phase completes; bumps both `updated:` fields to today's date atomically.
- **Rough shape:** invocation `/sdd-planner:close-phase <plan-name> <phase-id>`. Reads `.plans/Plans/<plan>/README.md` to confirm phase exists; updates phase doc frontmatter `status: complete` and plan README `phases[id].status: complete`; bumps both `updated:` fields; reports the diff. If all phases are now complete, moves the plan from `Active/` to `Complete/` and prompts for `/debrief` run on any phases still missing notes.

### 2. `make test-summary` — aggregated test counts

- **What I did repeatedly:** after every test run, ran `cargo test --workspace 2>&1 | grep -E "test result" | awk '{p+=$4; f+=$6; i+=$8} END {print "Total passed:", p, "/ failed:", f, "/ ignored:", i}'` to get a single line "1179 passed / 0 failed / 2 ignored" instead of scrolling through 30+ per-binary blocks. Tweaked the awk 3 times during Phase 1 before it was stable.
- **Where it belongs:** Makefile target.
- **Why a skill:** the awk is brittle (one stray "test result" line in stderr breaks the column-index assumptions); making it a vetted one-liner means everyone gets the same totals.
- **Rough shape:** `make test-summary` → runs `cargo test --workspace`, captures output, prints `1179 passed / 0 failed / 2 ignored / 31 binaries`. Returns non-zero exit if anything failed. Optional `make test-summary CRATE=code-graph-tools` for single-crate scope.

### 3. `make snapshot-accept FILE=<name>` — normalize the insta accept dance

- **What I did repeatedly:** `cargo insta accept --snapshot <filename>` silently mismatched the snapshot name today (returned `insta review finished / skipped: ...`). Had to manually `mv crates/code-graph-tools/tests/snapshots/<name>.snap.new <name>.snap` to apply the regenerated snapshot. The CLI flag's pattern-matching semantics are not obvious.
- **Where it belongs:** Makefile target.
- **Why a skill:** removes a brittle CLI failure mode; gives a canonical "accept this one snapshot" entry point. Pre-validates that the `.snap.new` file actually exists before attempting promotion.
- **Rough shape:** `make snapshot-accept FILE=snapshot_tools_list__tools_list_get_symbol_summary` → searches `**/snapshots/*.snap.new` for the named file, `mv`'s it to the corresponding `.snap`, runs `make snapshot-clean` to verify nothing else is pending. Errors loudly if the file isn't found or if more than one matches.
