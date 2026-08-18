---
title: "VCS Foundation and Blame"
type: phase
plan: GraphPlatformExpansion
phase: 5
status: in-progress
created: 2026-08-08
updated: 2026-08-18
deliverable: "A provider-abstracted version-control layer with a pure-Rust git implementation, and a blame_symbol tool answering who last changed a symbol."
tasks:
  - id: "5.1"
    title: "code-graph-vcs crate: trait, opaque RevId, registry, test double"
    status: complete
    justifies: "FR-27, FR-28, FR-29, FR-30, D-0002, AC-24, AC-34, AC-35, AC-36. D-0002 is reversibility one-way — once a hash-shaped identifier reaches a persisted field or the wire, relaxing it is a breaking change, so the newtype has to exist before any provider does."
    verification: "cargo test -p code-graph-vcs — the required operation set is exactly four, reviewer-checkable (AC-34); a test-double provider whose revision identifiers are integers implements the trait with no change to the trait or its types (AC-24); a deliberately slow double satisfies the async contract without blocking the runtime (AC-35); a second provider registers without editing the first and detection selects correctly (AC-36)."
  - id: "5.2"
    title: "code-graph-vcs-git: pure-Rust provider including a path revwalk"
    status: complete
    justifies: "FR-31, FR-47, D-0004, AC-23, AC-56. revisions_touching has no first-class equivalent in the chosen library — it needs a manual revwalk with per-commit tree diffing — and both history features depend on it, so under-sizing this task under-sizes the phase."
    verification: "cargo test -p code-graph-vcs-git against a scripted fixture repository — blame, read_at, and resolve_rev return correct results; revisions_touching returns only commits that changed the path, newest first, capped at limit, verified against git log --oneline -- <path>; cargo tree shows no VCS crate under code-graph-core, -graph, -lang, or -path-trie (AC-23) and no new native-library dependency beyond the tree-sitter grammars (AC-56, D-0004)."
    depends_on: ["5.1"]
  - id: "5.3"
    title: "Git fixture harness for temporary repositories"
    status: complete
    justifies: "Prevents unreproducible history tests. No test in the workspace creates a git repository today, so without a hermetic harness every history test would depend on contributor git config — signing, user identity, default branch name — and fail on machines that differ from the author's."
    verification: "cargo test -p code-graph-vcs-git harness:: — the harness builds a repo with scripted commits, fixed author identity, fixed timestamps, and commit.gpgsign disabled; two runs on the same script produce identical commit graphs; the harness cleans up on both the pass and fail path."
    depends_on: ["5.1"]
  - id: "5.4"
    title: "blame_symbol tool with staleness detection"
    status: complete
    justifies: "FR-32, FR-36, NFR-10, NFR-11, AC-21, AC-22, AC-44, AC-45. Blame against a moved working tree silently attributes the wrong lines, which is worse than refusing — the graph's span refers to a file state that no longer exists."
    verification: "cargo test -p code-graph-tools blame_symbol — per-line attribution matches git blame --porcelain -L <line>,<end_line> for the same revision, using the git output as oracle rather than hand-asserted values (AC-21); in a directory under no supported VCS the tool reports unavailability as a success and every other tool behaves normally (AC-22); with a deliberately slow provider a concurrent non-history query returns in its normal time (AC-44, NFR-10); a file modified after indexing without a matching commit returns results flagged stale; the tool description meets the agent-facing lens (AC-45)."
    depends_on: ["5.2", "5.3", "5.5"]
  - id: "5.5"
    title: "Absorb the adversarial-review findings on the git provider"
    status: complete
    justifies: "Review 2986df0-series adversarial findings M3/M4/M5 on the phase 5 provider: blame's `at: None` contract claimed working-tree attribution while gix blames committed state; revisions_touching walked unbounded history with no node cap (minutes of CPU on engine-scale repos for a stale path); a shallow-clone boundary hard-errored the whole call instead of terminating the walk the way git log does. blame_symbol builds directly on these operations, so the findings must land before the tool does."
    verification: "cargo test -p code-graph-vcs-git — a --depth-1 file:// shallow clone returns its boundary commit from revisions_touching instead of erroring; the revwalk visit cap is asserted structurally (bounded loop, cap constant documented); the trait doc for blame's at: None names the provider default revision (Git: HEAD) rather than promising working-tree attribution."
    depends_on: ["5.2", "5.3"]
---

# Phase 5: VCS Foundation and Blame

## Overview

Two new crates mirroring the language-plugin split — `code-graph-vcs` for the trait and types, `code-graph-vcs-git` for the implementation and its dependency — plus the first history tool. The abstraction is shaped so a Perforce provider can be added later without reshaping it (D-0002).

Independent of phases 1, 2, and 3. Gates phase 6.

## 5.1: code-graph-vcs crate: trait, opaque RevId, registry, test double

### Subtasks
- [x] Create `crates/code-graph-vcs` following the `code-graph-lang` crate conventions
- [x] Define `RevId(String)` — constructed only by providers, never parsed by callers
- [x] Define `Commit`, `BlameHunk`, and a `thiserror`-based `VcsError`
- [x] Define the async `VcsProvider` trait with exactly four required operations
- [x] Implement `VcsRegistry` with detection, mirroring `LanguageRegistry`'s shape
- [x] Add an integer-revision test double and a deliberately slow double
- [x] Add the crate to the workspace members list

### Notes
Revision boundary: the abstraction exists, is registrable, and is proven against a non-git provider — with no git dependency anywhere yet.

The trait is async because the *provider* is allowed to be slow, not because any library is async: gitoxide is blocking by design and a Perforce provider shells out to a network client. `async_trait` is needed for `Box<dyn VcsProvider>` because native async-fn-in-trait is not object-safe.

Keep the required set at four. Churn, recent-commits, and diff rendering are all additive later; a narrow trait is what makes a second provider plausible.

### Completion Evidence

- Verified: 2026-08-16
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `273484ed1a35923107d9907b4eab89beb208a317`
- Identity recheck: `git rev-parse HEAD` at 2026-08-16 00:00 matched `273484ed1a35923107d9907b4eab89beb208a317`
- Focused review: `git show 273484ed1a35923107d9907b4eab89beb208a317`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `273484ed1a35923107d9907b4eab89beb208a317`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-vcs && cargo clippy -p code-graph-vcs --all-targets -- -D warnings && cargo fmt --all --check && git diff --check` | `.` | PASS (`exit 0`) | `Five unit tests and doc tests passed; Clippy emitted no denied warnings; formatting and diff checks passed.` |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `intent-blind quality review` | `working tree task diff before commit` | PASS | `PASS: duplicate-registration regression gap fixed; follow-up review found no defects.` |

### Trap
Typing `RevId` as anything that encodes git's shape — a `[u8; 20]`, a validated-hex `String`, or a `Sha` alias. It will look tidy and it forecloses Perforce, whose revisions are changelist numbers and `#rev` specifiers. D-0002 is marked one-way for exactly this reason.

## 5.2: code-graph-vcs-git: pure-Rust provider including a path revwalk

### Subtasks
- [x] Create `crates/code-graph-vcs-git` with the pure-Rust git dependency confined to it
- [x] Implement `blame`, `read_at`, and `resolve_rev` over the library's APIs
- [x] Implement `revisions_touching` as a revwalk with per-commit tree diffing against each parent, filtered and capped
- [x] Wrap all blocking work in `spawn_blocking`
- [x] Implement working-tree detection for registry selection
- [x] Add the `cargo tree` confinement assertions to CI

### Notes
Revision boundary: git history is reachable through the trait; no tool exposes it yet.

Effort is not evenly distributed across the four operations. `blame` is genuinely supplied by the library — including rename tracking and shallow-history handling — and `read_at`/`resolve_rev` are close to direct calls. **`revisions_touching` is the real work**: there is no first-class `git log -- <path>` equivalent, so it means a manual revwalk plus tree diffing. Both history features lean on it hardest.

### Completion Evidence

- Verified: 2026-08-16
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `0f7560ee71af947e86a20398582b8f0e0447d733`
- Identity recheck: `git rev-parse HEAD` at 2026-08-16 00:00 matched `0f7560ee71af947e86a20398582b8f0e0447d733`
- Focused review: `git show 0f7560ee71af947e86a20398582b8f0e0447d733`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `0f7560ee71af947e86a20398582b8f0e0447d733`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-vcs-git && cargo clippy -p code-graph-vcs-git --all-targets -- -D warnings && cargo fmt --all --check && git diff --check` | `.` | PASS (`exit 0`) | `Nine provider and fixture tests passed; Clippy, formatting, and diff checks passed; gix is confined to code-graph-vcs-git.` |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `intent-blind quality review` | `working tree task diff before commit` | PASS | `PASS: root path safety, per-parent mode/OID comparison, merge/root behavior, and blocking isolation reviewed.` |

## 5.3: Git fixture harness for temporary repositories

### Subtasks
- [x] Build a helper that creates a temp repo and applies a scripted sequence of commits
- [x] Pin author name, email, and timestamps; disable commit signing; pin the initial branch name
- [x] Provide the scripts phase 6 needs: reformat-only commit, logic-change commit, move-within-file commit, literal-only change
- [x] Ensure cleanup on both the pass and fail path
- [x] Determinism test: the same script twice produces identical commit graphs

### Notes
Revision boundary: history tests become writable and hermetic.

This is scaffolding, and it is sourced by the failure it prevents rather than by a requirement id: without pinned identity and disabled signing, every history test fails on contributor machines that have `commit.gpgsign = true` or a different `init.defaultBranch`. Building it as its own task keeps phases 5 and 6 from each growing a private half-version.

Pin the commit fixtures here rather than in phase 6 — the reformat-vs-logic pair is what AC-19 tests, and it belongs with the harness that produces it.

### Completion Evidence

- Verified: 2026-08-16
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `92c019cfd4e2cc0b1c1c51aefc0f6a362f2f6b44`
- Identity recheck: `git rev-parse HEAD` at 2026-08-16 00:00 matched `92c019cfd4e2cc0b1c1c51aefc0f6a362f2f6b44`
- Focused review: `git show 92c019cfd4e2cc0b1c1c51aefc0f6a362f2f6b44`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `92c019cfd4e2cc0b1c1c51aefc0f6a362f2f6b44`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-vcs-git harness:: && cargo clippy -p code-graph-vcs-git --all-targets -- -D warnings && cargo fmt --all --check && git diff --check` | `.` | PASS (`exit 0`) | `Five deterministic fixture-harness tests passed; Clippy, formatting, and diff checks passed.` |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `intent-blind quality review` | `working tree task diff before commit` | PASS | `PASS: environment isolation and child-test execution checks added; final review finding was fixed.` |

## 5.4: blame_symbol tool with staleness detection

### Subtasks
- [x] Resolve `(file, name, kind)` against the graph to a line span
- [x] Call `blame(path, Some((line, end_line)), at)` and shape the hunks
- [x] Flag stale results (on-disk content diverged from the blamed revision) rather than suppressing or silently returning them
- [x] Handle the no-VCS and untracked-file cases as success-shaped results
- [x] Register the tool with a description meeting the agent-facing lens
- [x] Oracle-based blame test plus the no-VCS, staleness, and slow-provider tests

### Notes
Revision boundary: the first history feature is live end to end.

**Staleness mechanism, reconciled to the codebase.** The design's preferred
mtime accessor is not cheaply reachable: the in-memory `Graph` carries no
per-file mtimes (they exist only as a column in the rkyv cache archive,
stat'd fresh at save/load — `persist/mod.rs::mtime_nanos`/`stale_paths`),
so exposing them at query time would mean new in-memory state, which the
design's own fallback forbids trading for. The fallback applies, upgraded:
gix attributes the **committed state** (`at: None` = HEAD, per task 5.5's
contract fix), the graph's spans come from the **on-disk** file, and the
handler compares on-disk bytes against `read_at(blamed rev)` — divergence
sets `stale: true` with a reason. That detects the actual misattribution
hazard (span/attribution referring to different file states) more directly
than an index-time mtime would, and it never silently returns possibly-wrong
attribution while claiming otherwise.

Line-granular attribution is a real limitation — `Symbol` has no end column — so a symbol sharing a line with another gets that line attributed to both. State it in the description.

### Completion Evidence

- Verified: 2026-08-18
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `0dd91151c42820ae2143721621cd7a29e92639f6`
- Identity recheck: `git rev-parse HEAD` at 2026-08-18 17:05 matched `0dd91151c42820ae2143721621cd7a29e92639f6`
- Focused review: `git show 0dd91151c42820ae2143721621cd7a29e92639f6`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `0dd91151c42820ae2143721621cd7a29e92639f6`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check` | `.` | PASS (`exit 0`) | `79 test binaries, 1,852 tests, 0 failures natively on Windows — including the 8-test blame_symbol suite (porcelain oracle AC-21, no-VCS success AC-22, gated-provider isolation AC-44, staleness, untracked, did-you-mean, bad-at) and the 24-tool count/route fixtures; clippy denied no warnings; formatting clean.` |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `agent-facing description lens (AC-45)` | `tools_list_blame_symbol snapshot` | PASS | `Description names the response envelope field-by-field, documents both arguments with defaults, states the committed-state/staleness contract, the line-granularity limitation, and the success-shaped unavailability trichotomy.` |
Hand-asserting expected authors and SHAs in the blame test. The fixture's commit ids change whenever the harness script changes, so hand-asserted values rot into either constant maintenance or a disabled test. Diff against `git blame --porcelain` output and let git be the oracle.

## 5.5: Absorb the adversarial-review findings on the git provider

### Subtasks
- [ ] Correct the `VcsProvider::blame` doc contract: `at: None` selects the provider's default revision (Git: HEAD, committed state), not working-tree attribution
- [ ] Bound `revisions_touching`'s manual revwalk with a visited-commit cap, mirroring `find_path`'s node-cap discipline
- [ ] Treat an unreadable parent commit as a history boundary (shallow clone), terminating the walk the way `git log` does instead of erroring the call
- [ ] Shallow-clone regression test via `git clone --depth 1 file://…` in the fixture harness

### Notes
Revision boundary: the provider operations `blame_symbol` builds on are
contract-honest and bounded before the tool consumes them.

These are review findings M3/M4/M5 from the adversarial full-range review of
this branch, filed against the already-committed tasks 5.1–5.2. M3 is a
documentation-contract fix (gix has no working-tree blame; the tool layer
compensates with the 5.4 staleness flag). M4/M5 change walk termination only:
results for healthy full-history repositories are unchanged.

### Completion Evidence

- Verified: 2026-08-18
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `6dcbf28022de7d60ec6bde846f736bec45ee0896`
- Identity recheck: `git rev-parse HEAD` at 2026-08-18 16:26 matched `6dcbf28022de7d60ec6bde846f736bec45ee0896`
- Focused review: `git show 6dcbf28022de7d60ec6bde846f736bec45ee0896`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `6dcbf28022de7d60ec6bde846f736bec45ee0896`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-vcs-git && cargo clippy -p code-graph-vcs -p code-graph-vcs-git --all-targets -- -D warnings && cargo fmt --all --check` | `.` | PASS (`exit 0`) | `10 harness tests passed natively on Windows, including the new --depth 1 file:// shallow-clone regression returning exactly the boundary commit; clippy denied no warnings; formatting clean.` |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `focused diff review` | `git show 6dcbf28` | PASS | `Walk-termination changes only (cap + boundary); healthy-repo results unchanged, pinned by the 9 pre-existing harness tests staying green; blame contract now names the provider default revision.` |

## Acceptance Criteria

- [ ] **AC-21**: Blame-a-symbol matches `git blame --porcelain` for the same span and revision (FR-32).
- [ ] **AC-22**: With no supported VCS, history tools report unavailability as a success and nothing else degrades (FR-36).
- [ ] **AC-23**: No VCS crate under `code-graph-core`, `-graph`, `-lang`, or `-path-trie` (FR-31, NFR-02).
- [ ] **AC-24**: The trait supports an integer-revision provider with no change to the trait or any wire type (FR-28, D-0002).
- [ ] **AC-34**: The required operation set is exactly four (FR-27).
- [ ] **AC-35**: A slow provider satisfies the trait without blocking the runtime (FR-29).
- [ ] **AC-36**: A second provider registers without editing the first; detection selects correctly (FR-30).
- [ ] **AC-44**: A slow provider delays only history tools (NFR-10).
- [ ] **AC-45**: The `blame_symbol` description meets the agent-facing-description lens (NFR-11).
- [ ] **AC-56**: Native-library dependencies unchanged after the git provider lands (FR-47, D-0004).
- [ ] **AC-27**: `make verify` passes (NFR-04).
- [ ] FR-27 through FR-32, FR-36, and FR-47 realized; NFR-02 and NFR-10 satisfied.

## Phase Completion Evidence

Pending — not complete.
