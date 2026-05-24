---
title: "Acceptance regression test"
type: phase
plan: PaginatedResponseSizeSafety
phase: 5
status: complete
created: 2026-05-11
updated: 2026-05-12
deliverable: "An integration test that builds a real-scale fixture, reproduces the 1,031-orphan scenario, and asserts every paginated response stays under the byte budget. Anti-regression coverage that catches any future change re-introducing the failure mode."
tasks:
  - id: "5.1"
    title: "Choose & build the regression fixture"
    status: complete
    verification: "A test fixture exists that produces enough orphan-eligible symbols to GUARANTEE byte-budget truncation at limit=1000 after Phase 3's record slimming. Empirical sizing: post-Phase-3 brief SymbolResult records are ~80-100 bytes serialized; at the 102400-byte default budget, the minimum N to force truncation is ~1200 records (90 bytes/record * 1200 = 108KB > 102.4KB). Fixture targets N>=1500 for headroom. Default choice: a synthetic fixture under crates/code-graph-tools/tests/fixtures/large_orphan_set/ (deterministic generation, no submodule dependency). Submodule fallback only if a future engineer measures that ripgrep at its pinned SHA satisfies the threshold using the runnable probe recipe in the task body."
  - id: "5.2"
    title: "Write the byte-budget acceptance test"
    status: complete
    verification: "New integration test at crates/code-graph-tools/tests/byte_budget_acceptance.rs: (a) builds the fixture via 5.1 and indexes it, (b) calls get_orphans with limit=1000, asserts truncated=true (the fixture is sized to guarantee this) AND serde_json::to_string(&response).len() < config.response.max_bytes + envelope_overhead (~512 bytes), (c) when truncated=true: iteratively calls with offset = previous next_offset until truncated=false; asserts every intermediate response is under budget; asserts sum of result-set lens across all pages == initial total (no overlap, no gap); (d) parallel test for search_symbols with a broad-match query; (e) the test asserts the originally-reported failure mode (74K-token payload) cannot recur — explicit comment to that effect at the top of the test file"
    depends_on: ["5.1"]
  - id: "5.3"
    title: "Add count_only smoke tests"
    status: complete
    verification: "Three smoke tests (one each for get_orphans, search_symbols, get_file_symbols) call the tool with count_only=true against the fixture, assert serde_json::to_string(&response).len() < 1024, assert total > 0, assert results == [], assert truncated == false, assert next_offset == None"
    depends_on: ["5.1"]
tags: [pagination, mcp, llm-optimization, byte-budget, regression-fix]
---

# Phase 5: Acceptance regression test

## Overview

The plan's acceptance criteria turn into executable tests here. Phases 1-4 deliver behavior; this phase pins the behavior to a real-scale repro so a future refactor that re-introduces the unbounded-response failure mode fails CI before reaching a human reviewer.

The plan's bug report is concrete: 1,031 orphan-eligible symbols on a 1,759-symbol Rust repo produced a 297,266-character payload. The test fixture must reproduce a scenario of comparable scale.

## 5.1: Choose & build the regression fixture

### Subtasks
- [ ] **Default path: build a synthetic fixture.** Post-Phase-3 record sizes (~90 bytes brief) and the 102KB default budget put the truncation threshold at ~1200 records. A synthetic fixture is deterministic, self-contained, and the simplest way to GUARANTEE the assertion that drives this test (`truncated=true` on the first call). Build it under `crates/code-graph-tools/tests/fixtures/large_orphan_set/`:
  - Either a single hand-written `lib.rs` with `1500` functions (e.g. `fn orphan_0001() {} ... fn orphan_1500() {}`), OR
  - A `build.rs`-style generator script invoked from the test's setup that writes the fixture into a `TempDir` with a fixed seed
  - Whichever variant ships, the fixture must be reproducible byte-for-byte across runs
- [ ] **Fallback path: ripgrep dogfood submodule** — only if a future engineer chooses to consolidate. Runnable probe recipe:
  ```bash
  # Initialize submodule
  git submodule update --init external/ripgrep
  # Build server in release mode
  make build
  # Run the MCP server against ripgrep and capture the get_orphans response
  # (concrete invocation depends on the project's stdio harness; pseudocode:)
  # 1) analyze_codebase(root=external/ripgrep)
  # 2) get_orphans(limit=1000)
  # 3) Measure: response payload size as bytes
  # Pass criterion: response_size_pre_truncation > 102400
  ```
  If the probe shows ripgrep's post-Phase-3 orphan payload doesn't cross the threshold, stay with the synthetic fixture. (Ripgrep at tag 15.1.0 has ~3100 symbols total per `testdata/rust/ripgrep-baseline.txt`, but the orphan-eligible subset is not pre-measured — the probe is the only way to know.)
- [ ] Document fixture origin and scale (orphan count, expected total payload bytes pre-truncation, pinning info) in a top-of-fixture-file comment

### Notes
The plan reviewer flagged a sizing risk: at the previously-assumed ~288 bytes/record (pre-Phase-3 with the `file` field), 1000 records produced 288KB easily; at the post-Phase-3 ~90 bytes/record, 1000 records is ~90KB which sits UNDER the 102KB budget. The synthetic fixture targets N=1500 explicitly to leave headroom and force `truncated=true` on the first call. Without that headroom, the acceptance test could silently pass without exercising the truncation path — the false-green failure mode the reviewer warned about.

Keep the synthetic fixture small in line count (<200 lines including any generator) and isolated under `tests/fixtures/` so it's distinct from the production indexer fixtures.

## 5.2: Write the byte-budget acceptance test

### Subtasks
- [ ] Create `crates/code-graph-tools/tests/byte_budget_acceptance.rs`
- [ ] Test 1: `get_orphans_under_budget_at_limit_1000`
  - Build/index the fixture
  - Call `get_orphans` with `limit=1000` (no offset)
  - Assert `truncated=true` (the fixture is sized to guarantee this — if false, the fixture isn't doing its job)
  - Assert `serde_json::to_string(&response.body).len() < max_bytes + envelope_overhead` where `envelope_overhead` is a generous constant (~512 bytes) to account for envelope keys, commas, and JSON whitespace if any
  - Loop, advancing `offset = next_offset` each iteration, until `truncated=false`
  - Assert every iteration's payload is under budget
  - Assert across-iterations sum of `results.len()` equals `original total` (no records lost; no overlap)
- [ ] Test 2: `search_symbols_under_budget_at_limit_1000` — parallel pattern with a broad match query (e.g. `name` substring "_" or similar that hits most of the fixture)
- [ ] Test top-of-file doc comment names the user-reported failure mode explicitly so a future engineer who sees the test failing immediately understands the regression contract
- [ ] If the fixture is the ripgrep submodule and not initialized, the test auto-skips with an `eprintln!` setup hint matching the existing dogfood pattern (CLAUDE.md anchors: "Tests auto-skip with an `eprintln!` setup hint if uninitialized")

### Notes
The envelope_overhead constant is the smell here — a too-generous value masks real regression, too-tight value flakes. Aim for empirical: measure the envelope wrapping a single representative record, double it. Document the chosen number.

The test runs against the SAME byte budget the server enforces (via the test fixture's `.code-graph.toml`'s `[response].max_bytes`, or default if absent). This pins the contract: byte budget enforced -> response fits.

## 5.3: Add count_only smoke tests

### Subtasks
- [ ] Same file as 5.2 (`byte_budget_acceptance.rs`)
- [ ] One test per tool: `count_only_under_1kb_{orphans,search_symbols,file_symbols}`
- [ ] Each calls the tool with `count_only=true`, asserts:
  - `serde_json::to_string(&response).len() < 1024`
  - `response.total > 0` (sanity: fixture has matches)
  - `response.results.is_empty()`
  - `response.truncated == false`
  - `response.next_offset.is_none()`

### Notes
Trivial tests by line count, but they pin the 1KB contract against future changes that might accidentally bloat the count-only response (e.g. someone adding a metadata field to the envelope).

## Acceptance Criteria
- [ ] Regression test reliably fails when byte budget is bypassed (verified by temporarily disabling the budget locally and observing the failure, then re-enabling)
- [ ] Test runs in default `cargo test --workspace` (no `--ignored` opt-in)
- [ ] Synthetic fixture (if used) is deterministic and committed; submodule fixture (if used) auto-skips when uninitialized
- [ ] All count_only smoke tests assert < 1KB response shape
- [ ] Anti-regression intent documented at top of test file
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo fmt --all --check` clean
