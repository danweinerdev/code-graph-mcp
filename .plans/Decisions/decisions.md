---
title: "Decision Ledger"
type: decision-log
status: active
created: 2026-08-08
updated: 2026-08-20
tags: [decisions]
related: []
decisions:
  - id: D-0001
    kind: answered-question
    status: accepted
    date: 2026-08-08
    decided_by: user
    statement: "The code-graph daemon is repository-local: exactly one daemon per project root, with all of its runtime state stored inside that repository. There is no system-wide, multi-tenant, or cross-workspace daemon."
    question: "Is narrowing Designs/SharedDaemon to a repository-local daemon confirmed, and is that design superseded or retained as a deferred future phase?"
    rejected:
      - "Multi-tenant daemon hosting many workspaces keyed by absolute root path"
      - "Discovery file outside the repository (XDG data directory)"
      - "Cross-workspace query tools (list_workspaces / select_workspace)"
    rationale: "System-wide state is not wanted yet. Keeping the graph, cache, and daemon endpoint inside a single project makes locality one concept instead of three and removes the multi-root graph registry and per-root locking that a shared daemon requires. The repository-local shape is a strict subset of the multi-tenant one, so expanding later remains open."
    confirmation: "No code path creates, reads, or requires a file outside the discovered project root for daemon operation; grep for XDG/home-directory paths in daemon startup and discovery."
    scope:
      - Specs/GraphPlatformExpansion
      - Designs/SharedDaemon
    tags: [daemon, architecture, multi-tenancy, scope]
    reversibility: two-way
  - id: D-0002
    kind: decision
    status: accepted
    date: 2026-08-08
    decided_by: user
    statement: "Version-control access is abstracted behind a provider trait, and revision identity is an opaque token. No trait signature, stored field, or wire type may assume a git object hash, a fixed length, or a hexadecimal alphabet."
    rejected:
      - "Git-specific history integration with SHA-typed revision identifiers"
    rationale: "Perforce support is a stated future requirement, and it identifies revisions by changelist number and #rev specifier rather than by hash. Constraining revision identity to an opaque token costs nothing now and is expensive to retrofit once a hash-shaped field reaches the MCP wire contract."
    confirmation: "The provider trait can be implemented for a system whose revision identifiers are integers without changing the trait, its types, or any wire type (Specs/GraphPlatformExpansion AC-24)."
    scope:
      - Specs/GraphPlatformExpansion
    tags: [vcs, perforce, git, abstraction, wire-contract]
    reversibility: one-way
  - id: D-0003
    kind: decision
    status: superseded
    date: 2026-08-08
    decided_by: user-approved
    statement: "The workspace builds without a C toolchain, and that property is preserved. Any git backend must be a pure-Rust implementation rather than C-library bindings."
    rejected:
      - "libgit2 bindings (git2 / libgit2-sys) as the git backend"
    rationale: "The workspace builds today with no C compiler — even the tree-sitter grammars compile through a pure-Rust path. A bindings-based backend would make a C toolchain a build prerequisite on every platform and for every contributor, which is a larger cost than the git backend itself. The pure-Rust option supplies the blame and history-walk operations the provider trait needs."
    confirmation: "A full workspace build succeeds in an environment with no C compiler available (Specs/GraphPlatformExpansion AC-56)."
    scope:
      - Specs/GraphPlatformExpansion
    tags: [build, dependencies, git, toolchain]
    reversibility: two-way
    superseded_by: D-0004
  - id: D-0004
    kind: decision
    status: accepted
    date: 2026-08-08
    decided_by: user-approved
    statement: "The git backend must be a pure-Rust implementation, not bindings to a C library. The workspace already requires a C compiler for the tree-sitter grammars; the constraint is to add no further native-library dependency, not to eliminate C compilation."
    rejected:
      - "libgit2 bindings (git2 / libgit2-sys) as the git backend"
      - "Treating the workspace as C-compiler-free (factually incorrect — see rationale)"
    rationale: "Supersedes D-0003, whose reasoning rested on a false premise. The six tree-sitter grammar crates compile generated `parser.c` and `scanner.c` through the `cc` crate into static archives, so a C compiler is already a build prerequisite; `cc` is a pure-Rust build driver, not a pure-Rust compiler. The decision itself is unchanged but rests on a different and narrower argument: the existing C compilation is a handful of self-contained generated files with no external library, whereas libgit2-sys vendors a large C library and introduces system-library discovery and cross-compilation burden. Adding that is a materially bigger step than what the grammars already cost."
    confirmation: "No crate in the dependency graph links a vendored or system native library beyond the tree-sitter grammar archives; a release build requires no pkg-config or system library discovery (Specs/GraphPlatformExpansion AC-56)."
    scope:
      - Specs/GraphPlatformExpansion
    tags: [build, dependencies, git, toolchain]
    reversibility: two-way
    supersedes: D-0003
  - id: D-0005
    kind: answered-question
    status: accepted
    date: 2026-08-08
    decided_by: user
    statement: "Symbol history matches symbols across revisions by exact, case-sensitive (name, kind). Any rename, including a case-only rename, reports as Removed followed by Introduced rather than Modified."
    question: "How are symbols matched across revisions in symbol_history — exact, case-insensitive, or similarity-based rename detection?"
    rejected:
      - "Case-insensitive matching"
      - "Similarity-based rename detection"
    rationale: "Five of the six supported languages allow Foo and foo to coexist as distinct symbols, so case-folding would silently interleave two symbols' histories — a plausible-looking wrong answer, which is worse than an obviously incomplete one. Rename detection is materially more work and sits inside the no-rename-tracking Non-Goal. Exact matching is also defensible on its merits: a renamed function is a different symbol to every caller."
    confirmation: "A case-only rename in a fixture repository produces Removed then Introduced, not Modified (Plans/GraphPlatformExpansion task 6.3)."
    scope:
      - Designs/VcsHistory
      - Plans/GraphPlatformExpansion
    tags: [vcs, symbol-history, matching, case-sensitivity]
    reversibility: two-way
  - id: D-0006
    kind: decision
    status: accepted
    date: 2026-08-08
    decided_by: user
    statement: "Both fingerprint sensitivities are committed deliverables for all six supported languages. LiteralInsensitive is not an optional per-language override; every language plugin gets an AST-backed implementation."
    rejected:
      - "Shipping Normalized universally and LiteralInsensitive only where a plugin happens to override the hook"
    rationale: "A per-language gap makes the same question return an answer in one language and an unsupported-mode response in another, which is a ragged surface for an agent to reason about. Normalized ships first everywhere via the text default; the six AST overrides are then a required rollout step rather than opportunistic work."
    confirmation: "No language returns None for either fingerprint mode; the per-language support matrix in CLAUDE.md shows full coverage (Plans/GraphPlatformExpansion phase 8)."
    scope:
      - Designs/VcsHistory
      - Plans/GraphPlatformExpansion
    tags: [vcs, fingerprint, scope, languages]
    reversibility: two-way
  - id: D-0007
    kind: decision
    status: accepted
    date: 2026-08-08
    decided_by: user
    statement: "A field on an LLM-facing response earns its place by removing a round-trip. Per-hop resolution detail stays because it saves the caller a follow-up query; the confidence arithmetic itself stays internal. Candidate count is the stronger signal and is specified as FR-48 rather than approximated."
    rejected:
      - "Dropping per-hop resolution detail to honour a strict reading of FR-23"
      - "Approximating candidate count with the existing binary resolved/heuristic tag"
    rationale: "These responses are consumed by agents, not humans. An indicator that only changes hedging language in prose is close to worthless — the agent softens its wording and moves on. An indicator that lets the agent skip a query, or tells it exactly what to verify next, changes behaviour. A binary heuristic tag is a one-bit projection of 'N candidates competed'; N is what a caller can act on, so it is specified properly rather than faked."
    confirmation: "New or changed fields on a tool response can be justified by naming the round-trip they remove or the next action they enable; a field that only qualifies prose is challenged in review."
    scope:
      - Specs/GraphPlatformExpansion
      - Designs/GraphQueries
    tags: [api-design, llm-facing, confidence, wire-contract]
    reversibility: two-way
  - id: D-0008
    kind: decision
    status: accepted
    date: 2026-08-10
    decided_by: user
    statement: "GraphPlatformExpansion implementation is blocked only by validation errors in that plan and its governing GraphPlatformExpansion spec and designs; validation errors confined to unrelated legacy artifacts do not block its tasks."
    rejected:
      - "Treating repository-wide legacy SDD validation debt as a blocker for GraphPlatformExpansion tasks"
    rationale: "The validator follows transitive related links into many historical artifacts outside this initiative. Those findings are real repository debt but do not establish a defect in the active plan, its governing contracts, or its implementation evidence."
    confirmation: "Before advancing a task, filter validation diagnostics to Plans/GraphPlatformExpansion, Specs/GraphPlatformExpansion, and the four designs directly related by the plan; stop only for diagnostics in that governing set."
    scope:
      - Plans/GraphPlatformExpansion
      - Specs/GraphPlatformExpansion
      - Designs/GraphQueries
      - Designs/TypedCoreLayering
      - Designs/RepoLocalDaemon
      - Designs/VcsHistory
    tags: [validation, implementation, legacy-artifacts, workflow]
    reversibility: two-way
  - id: D-0009
    kind: answered-question
    status: accepted
    date: 2026-08-12
    decided_by: user
    statement: "Generic long-running jobs are exposed additively in get_status through job, job_previous_terminal, job_pending_count, and job_pending_ids, while the existing analyze_job fields remain analyze-only compatibility projections."
    question: "How should get_status expose detect_communities_async jobs without changing the meaning of the existing analyze_job fields?"
    rejected:
      - "Overloading analyze_job fields to report non-analyze query jobs"
      - "Keeping query jobs invisible from get_status"
    rationale: "A shared FIFO and polling vocabulary should be observable without making existing clients interpret a community query as an analysis. Additive generic fields expose the actual global job slot, while analyze-only projections preserve the established contract."
    confirmation: "get_status snapshots contain both generic job fields and unchanged analyze_job projections; a running community job appears in job but not analyze_job."
    scope:
      - Plans/GraphPlatformExpansion
      - Designs/RepoLocalDaemon
    tags: [jobs, status, wire-contract, detect-communities, compatibility]
    reversibility: two-way
  - id: D-0010
    kind: answered-question
    status: superseded
    date: 2026-08-12
    decided_by: user
    statement: "The shared long-running-job FIFO admits at most 32 pending jobs; covered analyze requests still coalesce at capacity, while additional distinct analyze or community jobs are rejected with a retryable queue-full tool error."
    question: "Should the shared FIFO remain unbounded to preserve FR-41 literally, or be capped at 32 pending jobs?"
    rejected:
      - "An unbounded pending-job FIFO"
    rationale: "A fixed bound prevents hostile or accidental memory growth and prevents graceful daemon shutdown from being delayed by an unlimited amount of admitted work. Thirty-two matches the existing terminal-history bound and leaves useful burst capacity."
    confirmation: "Specs/GraphPlatformExpansion FR-41 and AC-50, Designs/RepoLocalDaemon Decision 7, and Plans/GraphPlatformExpansion phase 4 cite this decision. Tests fill all 32 pending slots, prove a 33rd distinct job is rejected without consuming an ID or guard, prove covered requests still coalesce, and prove promotion reopens capacity."
    scope:
      - Specs/GraphPlatformExpansion
      - Designs/RepoLocalDaemon
      - Plans/GraphPlatformExpansion
    tags: [jobs, queue, backpressure, daemon, availability]
    reversibility: two-way
    superseded_by: D-0011
  - id: D-0011
    kind: decision
    status: accepted
    supersedes: D-0010
    date: 2026-08-14
    decided_by: user-approved
    statement: "Analyze aliases are individually pollable through an alias-ID status query. The 32-entry pending bound counts every admitted, non-terminal pending analyze request, including absorbed followers; once full, even an otherwise covered request receives a retryable queue-full error."
    rejected: [Returning the canonical job ID for absorbed async requests, Allowing unbounded absorbed aliases outside the pending cap, Coalescing covered requests after the pending-request limit is reached]
    rationale: "Distinct async handles need a usable polling path, and every retained handle consumes memory. Counting followers against the same live pending bound prevents unbounded alias growth."
    scope: [Specs/GraphPlatformExpansion, Designs/RepoLocalDaemon, Plans/GraphPlatformExpansion]
    tags: [analyze, queue, aliases, polling, backpressure]
    reversibility: two-way
  - id: D-0012
    kind: decision
    status: superseded
    date: 2026-08-14
    decided_by: user-approved
    statement: "For paginated responses,  means more matching results remain, whether the page was cut by the response byte budget or the requested record limit.  resumes after the final emitted record in either case."
    rejected: [Restricting truncated to byte-budget clipping and silently treating count-limited pages as complete, Adding a separate continuation field]
    rationale: "A record limit is an upper bound, not proof of natural completion. One continuation signal prevents later records from becoming unreachable without expanding every Page response."
    scope: [Specs/GraphPlatformExpansion, Plans/GraphPlatformExpansion, CLAUDE.md]
    tags: [pagination, response-contract, backward-compatibility]
    reversibility: two-way
    superseded_by: D-0013
  - id: D-0013
    kind: decision
    status: accepted
    supersedes: D-0012
    date: 2026-08-14
    decided_by: user-approved
    statement: "For paginated responses, `truncated: true` means more matching results remain, whether the page was cut by the response byte budget or the requested record limit. `next_offset` resumes after the final emitted record in either case."
    rejected: [Restricting truncated to byte-budget clipping and silently treating count-limited pages as complete, Adding a separate continuation field]
    rationale: "A record limit is an upper bound, not proof of natural completion. One continuation signal prevents later records from becoming unreachable without expanding every Page response."
    scope: [Specs/GraphPlatformExpansion, Plans/GraphPlatformExpansion, CLAUDE.md]
    tags: [pagination, response-contract, backward-compatibility]
    reversibility: two-way
  - id: D-0014
    kind: decision
    status: accepted
    date: 2026-08-18
    decided_by: user
    statement: "The daemon's security scope is one local user working in a local project with multiple sessions. Owner-only ACLs and permissions remain as hygiene, but cross-account isolation is not a claimed or tested guarantee: no second-local-account denial testing, no multi-user hardening, and no expansion of this scope without explicit user approval."
    rejected:
      - "Second-local-account denial testing as a phase 11 acceptance gate"
      - "Multi-user or service-account daemon hardening"
    rationale: "The daemon exists so multiple sessions of the same user share one graph instance in one local project. Multi-account isolation adds account provisioning and audit surface for a threat model the tool does not serve; the owner-only DACL/permission hygiene already landed is sufficient for the intended scope."
    confirmation: "Phase 11 acceptance references single-user checks only (owner-only DACL inspection, no inherited ACEs); no test or task requires provisioning a second account."
    scope:
      - Plans/GraphPlatformExpansion
      - Specs/GraphPlatformExpansion
      - Designs/RepoLocalDaemon
    tags: [daemon, security, scope, windows]
    reversibility: two-way
---




# Decision Ledger

Machine-readable record of decided truths that outlive the document they were made in — design choices, concept definitions, and answered design questions that constrain work elsewhere. Choices a spec, design, or plan already states in full stay in that artifact. The frontmatter `decisions[]` array is canonical; see `shared/decision-log.md` in the plugin for the admission test, entry schema, lifecycle rules, and collision procedure.

Entries are append-only: an accepted entry is never edited except to mark it superseded. A change of mind is a new entry that supersedes the old one.

## D-0001 — Repository-local daemon

Supersedes the artifact `Designs/SharedDaemon` (draft, deferred, 2026-04-28) rather than a prior ledger entry — no ledger existed when that design was written. That design's own Decision 1 rejected Unix domain sockets in favour of HTTP with a bearer token, on the grounds that sockets force a per-platform code path. That trade-off was made in service of the multi-tenant model; with a single daemon per repository the discovery problem it solved no longer exists, so the transport question reopens and is deferred to design (see `Specs/GraphPlatformExpansion` OQ-02).

`Designs/SharedDaemon` has been marked `status: superseded`. Its multi-tenant content remains readable as the record of a considered alternative.

## D-0003 → D-0004 — Pure-Rust git backend

D-0003 was superseded the same day it was written, on a factual correction rather than a change of mind. Its premise — that the workspace builds with no C compiler — is wrong: `tree-sitter-{cpp,c-sharp,go,java,python,rust}` all depend on the `cc` crate and compile generated `parser.c`/`scanner.c` into static archives (verified by inspecting the build-script output and the emitted `.o`/`.a` artifacts). The `cc` crate is a pure-Rust *driver* for a system C compiler, not a replacement for one.

CLAUDE.md's workspace note — "No CGo/C toolchain — tree-sitter grammars build via pure-Rust `cc`" — is the likely source of the error and reads as stronger than the truth. It is accurate about CGo and about `cc` being pure Rust; it is misleading about whether a C compiler is required.

The conclusion is unchanged. D-0004 restates it on the argument that actually holds: don't add a vendored native library on top of the small, self-contained C the grammars already bring.

## D-0002 — Opaque revision identity

`reversibility: one-way` because the constraint's whole value is being applied before any revision identifier reaches a persisted field or the MCP wire contract. Once a hash-shaped identifier ships in a response, relaxing this is a breaking change rather than a refactor.

## D-0008 — Validation scope for GraphPlatformExpansion

Repository-wide validation still reports legacy structural debt through transitive `related` links. During this plan, diagnostics outside the active plan and its directly governing spec/design set are reported but do not stop task progression; diagnostics inside that set remain blocking.

## D-0014 — Single-local-user daemon security scope

Decided during the phase 11 pull-forward, when the remaining 11.2 security items were being enumerated. The daemon's reason to exist is multiple sessions of the *same* user sharing one graph instance in one local project (D-0001's repository-local shape), so the second-local-account denial check and any multi-user hardening are out of scope, not deferred work. What stays: the owner-only hygiene that already shipped — `0o600`/`0o700` on Unix, the SID-resolved owner-only DACL with inheritance stripped on Windows, and the per-instance TCP secret — plus the native regression test pinning that DACL shape. Any future scope expansion (shared machines, service accounts, CI runners with mixed users) requires explicit user approval first.

*Amendment record (2026-08-20):* the phase 11 gate review (artifact 22) found that spec AC-60's original wording implied cross-account denial evidence this decision had already scoped out. AC-60 in Specs/GraphPlatformExpansion was amended in place citing D-0014, and NFR-06 gained a matching scope note; the plan README records the correction. This entry is the governing truth for that wording — any future re-broadening of AC-60 requires the explicit scope-expansion approval this decision names.
