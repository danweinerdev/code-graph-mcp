---
title: "Graph Platform Expansion"
type: spec
status: approved
created: 2026-08-08
updated: 2026-08-14
tags: [daemon, cli, vcs, graph-queries, architecture, perforce]
related:
---

# Graph Platform Expansion

Implementation-gate validation for this initiative is scoped by D-0008; unrelated legacy-artifact diagnostics do not override this spec's own requirements.

## Overview
code-graph-mcp today is a single-purpose stdio MCP server: one process per agent session, one graph per process, reachable only by an MCP client, and aware only of the working tree as it exists right now. This specification defines four related pieces of work that lift those three constraints while leaving the existing tool surface behaviourally unchanged.

The four tracks are specified together because they share constraints, a sequencing dependency, and one enabling refactor. **They do not ship as a unit.** Each yields its own design and implementation plan, and each is independently releasable. Track A is a prerequisite for reaching B's CLI and C's and D's queries from any non-MCP front-end; once it lands, B, C, and D may proceed in parallel and in any order. Track C depends on nothing here and could land first.

- **Track A — Typed core layering.** Split each handler into a typed core function returning a domain value plus a thin adapter that renders it as an MCP result. Nothing in Track B or C's tool surface can be reached from a non-MCP front-end until this exists.
- **Track B — Repository-local daemon and CLI.** A long-lived process per project root that owns the graph, with the existing stdio binary reduced to a client that attaches to it, plus a command-line front-end over the same typed core.
- **Track C — Three graph queries.** Position lookup, shortest path between symbols, and community detection. Self-contained; depends on nothing else here.
- **Track D — Version-control history, provider-abstracted.** A narrow VCS provider trait with a git implementation, exposing blame-a-symbol and symbol-history. Structured so a Perforce provider can be added later without reshaping the abstraction.

Scope was informed by a survey of a comparable open-source code-intelligence server (referred to below as *the example material*), which independently arrived at several of these capabilities. Two of its capabilities were evaluated and deliberately rejected — see Non-Goals.

## Goals
- Let multiple concurrent agent sessions on one repository share a single indexed graph instead of each building and holding its own.
- Make every graph query reachable from a terminal, not only from an MCP client.
- Answer "what symbol is at this file and line?", "how does A reach B?", and "what are the de-facto modules here?" — three questions the current tool surface cannot answer at all.
- Answer "who last changed this symbol?" and "when did this symbol's logic actually change?" using version-control history, without coupling the codebase to git specifically.
- Preserve the existing 19-tool MCP wire contract exactly, so no client configuration or agent-facing behaviour changes as a side effect of this work.

## Non-Goals
- **System-wide or multi-tenant daemon.** One daemon serves exactly one project root. Cross-workspace queries, a shared discovery file outside the repository, and a daemon hosting several unrelated roots are explicitly out of scope. This narrows `Designs/SharedDaemon`, which specified the multi-tenant shape; see Constraints.
- **Full-text / regex search over file contents.** The example material offers indexed regex search, which is cheap for it because its store is content-addressed and holds file bodies. This graph stores symbols, edges, and paths — not contents — so the same capability would mean a new content store, a much larger cache, and a new invalidation axis. Free-text search remains a legitimate job for `Grep`.
- **Precise (scope-proof) name resolution.** The example material vendors a scope-graph resolution engine; in that implementation it is feature-gated off by default and covers two of its languages, with everything else falling back to the same name-matching this project already does. The cost is not justified by the delta. Call-resolution remains the documented heuristic, and ambiguity continues to be surfaced through `Confidence` and `find_class_candidates` rather than hidden.
- **Semantic / embedding-based code search.** Out of scope; requires a vector store and model dependencies disproportionate to the benefit here.
- **Consolidating the 19 flat tools into mode-dispatched tools.** Attractive alongside a CLI (one verb taxonomy for both front-ends) but a breaking wire change. Deferred to a separate decision so it can be made once, deliberately.
- **Replacing the in-memory graph with a database.** Unchanged from prior designs.
- **Perforce support itself.** This spec constrains the abstraction so Perforce can be added later; it does not deliver a Perforce provider.

## Requirements
<!-- No third-party API, protocol, or wire format is pinned by this spec. The one external contract touched — the Model Context Protocol tool surface — is consumed via the existing `rmcp` 1.5.0 dependency and is required to stay unchanged (NFR-01), so no new version pin is introduced. -->

### Functional Requirements
**Track A — Typed core layering**

- **FR-01**: Every tool-facing handler shall expose a function whose return type is a domain value (for example `Page<CallChain>`, `AnalyzeResult`, `HierarchyNode`), not an MCP wire type. A caller shall be able to obtain structured results without constructing, inspecting, or deserialising an `rmcp` type.
- **FR-02**: A thin adapter layer shall render each typed result into the MCP result the corresponding tool returns today. The adapter is the only place that knows about `rmcp`.
- **FR-03**: The typed result type shall be able to represent all three outcomes the current surface produces: a structured payload, a plain-text advisory (the non-callable soft-hint returned by `get_callers`/`get_callees`, which is a success and is deliberately not the `Page<CallChain>` envelope), and a user-visible tool error. Collapsing the advisory case into either "payload" or "error" is not acceptable.
- **FR-04**: A query issued before the codebase is indexed shall produce a domain-level error that a caller can detect and discriminate without MCP serialisation. (Today this guard is `require_indexed`, which returns a pre-rendered MCP error; that is the behaviour being replaced, not a prescription of where the replacement lives.)
- **FR-05**: Pagination and byte-budget behaviour (`byte_budget_take`, `[response].max_bytes`, `truncated`/`next_offset` semantics) shall be preserved and shall remain available to the typed core, since it governs payload shape rather than transport.

**Track B — Repository-local daemon**

- **FR-06**: A daemon shall serve exactly one project root. The root shall be discovered by the same upward walk that discovers `.code-graph.toml`, so that daemon locality, configuration locality, and cache locality are one concept.
- **FR-07**: All daemon runtime state (transport endpoint and metadata) shall live under `<project_root>/.code-graph/`. No file outside the repository shall be created, read, or required.
- **FR-08**: The existing stdio binary shall continue to be the entry point named in MCP client configuration, and shall attach to the daemon for its project root, starting one if none is running. No user-facing MCP configuration change shall be required to adopt the daemon.
- **FR-09**: Multiple concurrent client sessions attached to one daemon shall observe a single shared graph, including index state, so that indexing performed for one session is immediately queryable by another.
- **FR-10**: The daemon shall terminate after a configurable idle period. The setting shall live in `.code-graph.toml`, shall have a documented default, and shall accept a value meaning "never exit".
- **FR-11**: The idle timer shall run only when there are zero attached clients **and** no analyze job is in flight. It shall be cancelled by a new attachment. It shall not count down while an analyze job is in flight even with zero clients attached; when that job reaches a terminal state the timer shall begin again from zero rather than resuming a partial count. The daemon shall persist the cache before exiting.
- **FR-12**: The daemon shall record the identity of the binary that started it. A client built from a different binary shall not silently attach to a stale daemon; it shall cause the old daemon to be replaced.
- **FR-13**: At most one daemon shall exist per project root. Concurrent start attempts from several sessions shall converge on one daemon without corrupting state or leaving orphans.
- **FR-14**: Watch mode shall be owned by the daemon rather than by an individual session, so one watcher serves all attached clients.
- **FR-15**: The analyze job slot shall be shared across attached clients. Progress and terminal results for a job started by one session shall be observable by another.
- **FR-16**: If the daemon cannot be started or attached to, the binary shall fall back to serving in-process, exactly as it does today, and shall report the fallback. Daemon unavailability shall degrade performance, never availability.

**Track B — Command-line interface**

- **FR-17**: A command-line front-end shall expose the graph queries, invoking the same typed core functions as the MCP adapter. No query logic shall be duplicated between the two front-ends.
- **FR-18**: The CLI shall be able to run against a daemon when one is available and standalone when one is not, with identical output for identical inputs.
- **FR-19**: The CLI shall support a machine-readable output mode whose payload matches what the MCP surface returns for the same query, alongside a human-oriented default.
- **FR-20**: The CLI shall distinguish, by exit status, between success, a user-visible tool error (for example an unknown symbol), and an operational failure (for example an unreadable cache).

**Track C — Graph queries**

- **FR-21**: A position-lookup query shall accept a file path and a line number and return the symbol(s) whose span encloses that line, innermost first. It shall use the existing `Symbol.line` and `Symbol.end_line` fields and shall not require a change to the symbol record or the cache format. This query answers "what am I looking at"; it is not goto-definition and shall not be described as such.
- **FR-22**: A shortest-path query shall accept two symbols and return the path between them over the call graph as an ordered sequence from source to target, or an explicit not-found result. Search shall be bounded by an explicit, documented cap on nodes examined; reaching the cap shall yield the same not-found result rather than a partial path, an error, or an unbounded walk. The cap shall have a default and shall be overridable per call, with the resolved value echoed in the response so a caller can distinguish "no path exists" from "cap reached".
- **FR-23**: The shortest-path query shall prefer higher-confidence edges, treating `Confidence::Resolved` as cheaper than `Confidence::Heuristic`. The *weighting* shall be internal: no numeric confidence value, score, or cost shall appear on any wire type. Reporting **which** hops were heuristically resolved is permitted and encouraged, because it lets a caller verify the weak link without a second round-trip (D-0007); what is forbidden is exposing the arithmetic.
- **FR-24**: A community-detection query shall partition the graph into clusters approximating de-facto modules. Clusters shall be returned ranked by descending member count. Both the number of clusters returned and the number of members listed per cluster shall be capped by documented, per-call-overridable limits, with the resolved values echoed in the response. A cluster whose membership was truncated shall be distinguishable from one returned whole, and shall carry its true total.
- **FR-25**: Community detection shall be deterministic: the same graph shall always yield the same partition, independent of hash iteration order or thread scheduling. Node visitation shall therefore follow a stable total order derived from path or symbol id, and every tie shall break on a stated rule rather than on iteration order.
- **FR-44**: Community detection shall operate over file-level aggregation by default, treating files as nodes and the aggregated call and include edges between them as weighted links. Symbol-level granularity may be offered as an option; the response shall state which granularity produced the result. (Resolves OQ-03. Rationale: "de-facto modules" is a file-level question, the node count is orders of magnitude smaller than symbol-level, the pairwise weights already exist in `get_coupling`, and aggregating over files dampens the per-edge error inherent in heuristic call resolution rather than clustering on top of it.)
- **FR-45**: The chosen algorithm shall be near-linear in the number of edges and shall require no tuning parameter to produce a usable default partition. It shall converge on a stated condition — no label change in a full pass, or a documented iteration ceiling — and shall report which of the two ended it.
- **FR-46**: A degenerate partition shall be reported rather than presented as a result. Where detection yields a single community containing substantially all nodes, or as many communities as nodes, the response shall say so explicitly, since both outcomes mean the partition carries no information about module structure.
- **FR-26**: All three queries shall be reachable from both front-ends and shall follow the existing pagination and byte-budget conventions where they return lists.

**Track D — Version-control history**

- **FR-27**: A provider trait shall abstract version-control access. Its required operation set shall be limited to: blame a line range of a file, list revisions touching a file, read a file's contents at a revision, and resolve a revision specifier.
- **FR-28**: Revision identity shall be an opaque token (D-0002). No wire type, trait signature, or stored field shall assume a git object hash, a fixed length, or a hexadecimal alphabet. Perforce identifies revisions by changelist number and `#rev` specifier.
- **FR-29**: The provider trait shall be asynchronous. A provider is permitted to be network-bound and slow; the abstraction shall not assume local, fast object access. The trait's asynchrony is about the *provider* being allowed to be slow, not about the underlying library being async — no mature native-Rust git library offers async local object access, so a blocking implementation dispatched to a blocking-task pool satisfies this requirement. A future Perforce provider, which shells out to a network client, needs exactly the same treatment. (Resolves OQ-04.)
- **FR-30**: Provider selection shall follow the existing language-plugin pattern — a registry with detection — so that adding a provider does not require modifying existing providers.
- **FR-31**: A git provider shall be supplied. Its dependency shall be confined to its own crate; neither the core, graph, language, nor path-trie crates shall gain a version-control dependency.
- **FR-49**: **Deferred.** Generic long-running jobs and asynchronous whole-graph query execution, including `detect_communities_async`, are not part of Phase 4. Any future async whole-graph facility requires its own specification and plan rather than sharing the analyze queue.

- **FR-48**: Where an edge's target was chosen from several same-named candidates, the number of candidates that competed shall be recoverable by a caller, and shall be surfaced by the tools that report resolved edges (`get_callers`, `get_callees`, `find_path`, `generate_diagram`). A binary resolved/heuristic tag is a lossy projection of this: "three candidates competed" tells a caller what to disambiguate, where "heuristic" only tells it to be uneasy. Storing the count is expected to require a resolver change and a cache-format version bump; that cost is accepted rather than worked around (D-0007).

- **FR-47**: The git backend shall be a pure-Rust implementation rather than bindings to a C library (D-0004). The workspace already requires a C compiler — the six tree-sitter grammar crates compile generated `parser.c` and `scanner.c` via the `cc` crate — so the constraint is **not** that the build stays C-free. It is that no *further* native-library dependency is added: the existing C is a handful of self-contained generated files with no external library, whereas a bindings-based backend vendors a large C library and brings system-library discovery and cross-compilation burden with it.
- **FR-32**: A blame-a-symbol query shall resolve a symbol to its line span and return authorship for that span, attributing lines to revisions and authors.
- **FR-33**: A symbol-history query shall report the revisions at which a named symbol's content actually changed, distinguishing introduction, modification, and removal. It shall not report a change for a revision in which the symbol merely moved within the file or was reformatted.
- **FR-34**: Symbol-history shall offer at least two comparison sensitivities: one that ignores formatting and comments, and one that additionally ignores literal values — so that "did the logic change" can be separated from "did a string change". Two is a floor, not a target; the caller shall select which is applied.
- **FR-35**: Symbol-history shall reuse the existing language plugins to interpret historical file contents. The parser interface already accepts a byte buffer rather than a path, so historical content shall be parsed through the same plugins without a filesystem round-trip.
- **FR-36**: When the working tree is not under any supported version-control system, history tools shall report that history is unavailable as a normal, non-error outcome. Absence of a VCS shall not degrade any other tool.
- **FR-37**: Symbol fingerprints computed for symbol-history may be cached under `<project_root>/.code-graph/`. The cache shall be keyed such that a stale entry cannot be served for changed content, and its absence or corruption shall cause recomputation rather than an error. (Resolves OQ-05.)

**Track B — Daemon transport**

- **FR-38**: The Linux MVP daemon shall prefer a Unix domain socket and shall fall back to a TCP socket bound to loopback only if the UDS cannot be established. The fallback shall be reported, not silent. The transport abstraction shall retain explicit seams for deferred macOS and Windows implementations. (Resolves OQ-02 for the Linux MVP.)
- **FR-39**: On Linux, UDS access shall be restricted to the invoking user by owner-only runtime-directory and socket permissions. On the TCP fallback, where loopback does not itself exclude other local users, the daemon shall additionally require a per-daemon credential presented by the client. That credential shall be stored under `<project_root>/.code-graph/` with owner-only permissions and regenerated for each daemon instance.
- **FR-40**: Clients shall discover which transport is in use from the daemon's repository-local metadata rather than by probing, so that a client never attempts a connection the daemon is not serving.

**Track B — Analyze request compaction**

- **FR-41**: Analyze requests arriving while another scan is in flight shall queue rather than reject or run concurrently. The queue is analyze-only and has at most 32 **pending requests** after compaction, counting canonical entries and every follower. The running scan is never modified, coalesced into, upgraded, or replaced. A distinct request requiring a 33rd pending entry shall receive a retryable queue-full tool error.
- **FR-42**: Pending compaction shall use canonical invocation paths only. An equal or ancestor pending path absorbs an incoming request as a follower without creating a canonical entry, provided the pending-request capacity remains available. An incoming ancestor replaces all covered pending descendants at the earliest displaced FIFO position; those displaced requests become followers. Disjoint pending paths preserve FIFO order. Configuration identity, provenance, and configuration transitions do not participate in compaction.
- **FR-43**: Every follower shall receive the satisfying compacted scan's terminal result or error. The server retains every absorbed or displaced asynchronous job ID as an internal alias of the satisfying scan; synchronous callers wait for and return its ordinary `AnalyzeResult`, while asynchronous callers poll their canonical or alias ID through the analyze-only `get_analyze_status` tool. This tool is not generic job infrastructure. Every non-terminal pending request, including synchronous and asynchronous followers, consumes the same 32-request capacity. The compacted scan's effective `force` shall be the logical OR of all attached and replaced requests: extra forced work is acceptable, but a force request must not be lost. This phase requires neither a generic job projection nor a `coalesced_by` wire field.

### Non-Functional Requirements
- **NFR-01**: The existing 19 MCP tools shall be wire-compatible after this work. Response field names, ordering guarantees, envelope shapes, null-serialisation behaviour, and the documented paging-resume contract shall be unchanged. Existing snapshot tests shall pass without rebaselining.
- **NFR-02**: `code-graph-core`, `code-graph-graph`, `code-graph-lang`, and `code-graph-path-trie` shall gain no new third-party dependency from this work, and shall remain free of MCP, async, and I/O concerns where they are today.
- **NFR-03**: New algorithmic output shall be deterministic and therefore snapshot-testable, consistent with the repository's `insta` conventions.
- **NFR-04**: `make verify` — clippy with warnings denied, rustfmt, the full workspace test suite, pending-snapshot check, and the plugin-mirror drift gate — shall pass at every commit.
- **NFR-05**: Logging shall continue to use `eprintln!`. No `tracing` dependency shall be introduced.
- **NFR-06**: The daemon shall not open a network-reachable listener by default. Its endpoint shall be restricted to the local machine and to the invoking user. *[Scope note added 2026-08-20 per D-0014: the restriction MECHANISMS (owner-only runtime state, loopback-only TCP, per-instance credential) remain in place as hygiene, but cross-account isolation is not a claimed or TESTED guarantee — the daemon serves one local user's sessions in one local project. See D-0014 and the AC-60 amendment.]*
- **NFR-07**: The current plan's MVP shall be fully supported and acceptance-tested on Linux. Platform-dependent code shall remain behind explicit transport, path, process, and permission seams; macOS and Windows behavior may remain stubbed, ignored, or best-effort until their deferred phases.
- **NFR-08**: Shortest-path and community detection shall complete within an interactive budget on the largest corpus the project already tests against, and shall bound their work rather than degrade unboundedly.
- **NFR-09**: Attaching to a warm daemon shall be materially faster than the current cold path, which reloads or rebuilds the graph per session. This is the primary user-visible benefit of Track B and shall be measured rather than assumed.
- **NFR-10**: Version-control operations shall not block unrelated queries. A slow provider shall degrade only the history tools.
- **NFR-11**: Tool descriptions for any new MCP tool shall meet the repository's agent-facing-description standard: every argument documented with default and ceiling, response envelope named rather than implied, and suggested actions that operationally produce the claimed result.
- **NFR-12**: The deferred macOS phase shall make the complete GraphPlatformExpansion surface natively supported and acceptance-tested on macOS without weakening NFR-06.
- **NFR-13**: The deferred Windows phase shall make the complete GraphPlatformExpansion surface natively supported and acceptance-tested on Windows, including named-pipe, ACL, and Windows-path behavior, without weakening NFR-06.

## User Stories
- As an engineer running two agent sessions on one repository, I want the second session to query the graph immediately, so that I do not pay the indexing cost twice or hold two copies of the graph in memory.
- As an engineer with an agent that hit an indexing timeout, I want the index to already exist when the session starts, so that a long analyze is a one-time cost rather than a per-session one.
- As an engineer in a terminal, I want to ask the graph who calls a function without starting an agent session, so that I can use it in scripts and while debugging.
- As an agent looking at a compiler error or a diff hunk, I want to turn `file:line` into a symbol, so that I can use the rest of the graph — which is entirely name-addressed — from the position-addressed information I actually have.
- As an engineer investigating an unfamiliar dependency, I want to ask how one function reaches another, so that I can see the actual chain instead of walking callers by hand.
- As an engineer orienting in an unfamiliar codebase, I want the graph to tell me what its de-facto modules are, so that I can find the seams without reading the directory tree and guessing.
- As an engineer about to change a function, I want to know who last changed it and when its logic last changed, so that I know whom to ask and whether recent churn is real or cosmetic.
- As a team on Perforce, I want history features to work against our depot eventually, so that adopting this tool does not require being on git.

## Acceptance Criteria
- [ ] **AC-01**: For every existing tool, a caller can obtain a structured result by calling a typed function, without referencing any `rmcp` type. (FR-01)
- [ ] **AC-02**: The full existing snapshot suite passes unmodified after the Track A refactor. (FR-02, NFR-01)
- [ ] **AC-03**: Calling the typed core for `get_callers` on a non-callable kind yields a result distinguishable from both a structured page and an error, and the MCP adapter renders it as the plain-text advisory success it produces today. (FR-03)
- [ ] **AC-04**: Two clients attached to one daemon on the same root: one runs `analyze_codebase`, and the other's next query returns results from that index without indexing again. (FR-09)
- [ ] **AC-05**: Starting the stdio binary with no daemon running results in a daemon serving that root, with its state under `<project_root>/.code-graph/` and nothing created outside the repository. (FR-06, FR-07, FR-08)
- [ ] **AC-06**: With the idle timeout set to a short value, a daemon with no attached clients exits after that interval; a daemon with an attached client does not; and a daemon with an analyze in flight and no clients does not. (FR-10, FR-11)
- [ ] **AC-07**: After a daemon exits on idle, the cache on disk reflects the most recent index, and the next session loads it rather than re-indexing. (FR-11)
- [ ] **AC-08**: Several clients starting simultaneously against a root with no running daemon converge on exactly one daemon, with no orphaned processes or stale endpoints. (FR-13)
- [ ] **AC-09**: A client built from a different binary than the running daemon does not attach to it; the daemon is replaced. (FR-12)
- [ ] **AC-10**: With the daemon prevented from starting, every existing tool still answers correctly in-process, and the fallback is reported. (FR-16)
- [ ] **AC-11**: For at least one query of each distinct response shape — a `Page<T>` envelope (`get_callers`), a non-`Page` tree (`get_class_hierarchy`), a flattened envelope with a conditional field (`search_symbols`, exercised both with and without `suggestions`), a dual-page response (`get_coupling` with `direction: "both"`), and a non-JSON body (`generate_diagram` with `format: "mermaid"`) — CLI machine-readable output and the MCP tool payload are equivalent for the same inputs. (FR-17, FR-19)
- [ ] **AC-12**: The CLI returns distinct exit statuses for success, an unknown-symbol tool error, and an operational failure. (FR-20)
- [ ] **AC-13**: Position lookup on a line inside a method nested in a class returns the method first and the class after it. (FR-21)
- [ ] **AC-14**: Position lookup on a line belonging to no symbol returns an empty result, not an error and not a nearest-neighbour guess. (FR-21)
- [ ] **AC-15**: Position lookup requires no cache-version bump; an existing cache built before this work is readable unchanged. (FR-21, NFR-01)
- [ ] **AC-16**: Shortest path between two connected symbols returns a sequence whose first element is the requested source and whose last is the requested target, in which every adjacent pair corresponds to an edge present in the graph. Between two unconnected symbols it returns an explicit not-found. With the node cap set to a value smaller than the graph requires, it also returns not-found — never a partial path — and the response distinguishes that case from a genuine absence of any path. (FR-22)
- [ ] **AC-17**: Where two paths of equal hop count exist and one traverses only resolved edges, the resolved path is returned. (FR-23)
- [ ] **AC-18**: Community detection run twice on an unchanged graph produces byte-identical output, and is captured by a snapshot test. (FR-25, NFR-03)
- [ ] **AC-19**: On a repository where a function was reformatted in one commit and had its logic changed in another, symbol-history reports the logic commit and not the reformatting commit. (FR-33, FR-34)
- [ ] **AC-20**: On a repository where a function moved to a different line without changing, symbol-history reports no change for that revision. (FR-33)
- [ ] **AC-21**: For a symbol in a fixture repository, blame-a-symbol's per-line attribution matches `git blame --porcelain -L <line>,<end_line> -- <path>` for the same revision, revision-for-revision and author-for-author. The git command output is the oracle; no attribution is hand-asserted. (FR-32)
- [ ] **AC-22**: In a directory that is not a working copy of any supported VCS, history tools report unavailability as a success-shaped result, and all other tools behave normally. (FR-36)
- [ ] **AC-23**: No version-control crate appears in the dependency graph of `code-graph-core`, `code-graph-graph`, `code-graph-lang`, or `code-graph-path-trie`. (FR-31, NFR-02)
- [ ] **AC-24**: The provider trait can be implemented for a system whose revision identifiers are integers, without changing the trait, its types, or any wire type. Demonstrated by review against Perforce's model, or by a test double. (FR-28)
- [ ] **AC-25**: On Linux, the preferred UDS is beneath an owner-only runtime directory and is owner-only; TCP fallback binds only to loopback and refuses callers without the per-instance credential. These enforcement properties establish that the endpoint is neither remotely reachable nor usable by another local UID. (NFR-06)
- [ ] **AC-26**: On the largest initialised dogfood corpus, time-to-first-successful-query for a session attaching to an already-indexed warm daemon is bounded and does not grow with corpus size, whereas the current cold path does. Measured on at least two corpora of materially different size (for example `external/ripgrep` and `external/abseil-cpp`), with both numbers recorded in the plan's notes. The pass condition is the absence of corpus-size scaling in the warm-attach path, not a fixed millisecond target. (NFR-09)
- [ ] **AC-27**: `make verify` passes. (NFR-04)
- [ ] **AC-28**: A typed-core query invoked before any index exists returns a domain error that the caller can discriminate from a payload and from an advisory, without parsing a rendered message; the MCP adapter renders it as the same error the tool returns today. (FR-04)
- [ ] **AC-29**: A typed-core call whose result exceeds `[response].max_bytes` returns a truncated payload with `truncated: true` and a `next_offset` strictly past the last emitted record, and re-calling at that offset resumes without gap or repetition — verified against the typed value, not a serialised string. (FR-05)
- [ ] **AC-30**: With one daemon and two attached clients, a file edited on disk is reflected in both clients' query results after a single watch-driven reindex, and exactly one filesystem watcher exists for the root. (FR-14)
- [ ] **AC-31**: Session A starts `analyze_codebase_async`; Session B, which never called it, observes that job in `get_status.analyze_job` — including its progress while running and its terminal result once complete. (FR-15)
- [ ] **AC-32**: Community detection returns clusters ordered by descending member count; both the cluster cap and the per-cluster member cap are enforced, are echoed in the response, and a cluster truncated by the member cap is distinguishable from a whole one and reports its true total. (FR-24)
- [ ] **AC-33**: Position lookup, shortest path, and community detection are each invocable from both the MCP surface and the CLI, and their list-returning responses honour the same pagination and byte-budget contract as the existing tools. (FR-26)
- [ ] **AC-34**: The provider trait's required operation set is exactly: blame a line range, list revisions touching a file, read file contents at a revision, and resolve a revision specifier. A reviewer can confirm no fifth required operation was added. (FR-27)
- [ ] **AC-35**: A provider implementation that awaits on each operation — simulating network-bound access — satisfies the trait without blocking the runtime, demonstrated by a deliberately slow test double. (FR-29)
- [ ] **AC-36**: A second provider can be registered without editing the first, and detection selects the correct provider for a given working tree. Demonstrated with a test double alongside the git provider. (FR-30)
- [ ] **AC-37**: Symbol-history parses historical file contents through the existing language plugins from an in-memory buffer, with no temporary file written to disk. (FR-35)
- [ ] **AC-38**: Fingerprinting a symbol that was reformatted — whitespace and comments only — yields an unchanged fingerprint under the formatting-insensitive mode. (FR-34)
- [ ] **AC-39**: Fingerprinting a symbol in which only a string or numeric literal changed yields a changed fingerprint under the formatting-insensitive mode and an unchanged one under the literal-insensitive mode. (FR-34)
- [ ] **AC-40**: The same CLI invocation against the same repository produces identical machine-readable output whether a daemon is running or not. (FR-18)
- [ ] **AC-41**: No crate in the workspace declares a direct `tracing` dependency, and diagnostic output uses `eprintln!`. (`tracing` does appear in the dependency graph transitively, pulled by `rmcp`; that is pre-existing and outside this work's control. The requirement is that no code here logs through it.) (NFR-05)
- [ ] **AC-42**: The Linux MVP is exercised natively on Linux, including UDS, loopback-TCP fallback, owner permissions, idle lifecycle, and the POSIX stale-socket-inode path. Platform seams remain isolated so ignored/deferred macOS and Windows implementations are not prerequisites for this criterion. (NFR-07)
- [ ] **AC-43**: Shortest path and community detection each complete within an interactive budget on the largest initialised dogfood corpus, and both enforce their caps rather than degrading, with the timings recorded. (NFR-08)
- [ ] **AC-44**: With a deliberately slow version-control provider, a concurrent non-history query returns in its normal time — the slow provider delays only the history tools. (NFR-10)
- [ ] **AC-45**: Each new MCP tool's description names its response envelope, documents every argument with default and ceiling, and its suggested actions operationally produce the results they claim — reviewed under the repository's agent-facing-description lens. (NFR-11)
- [ ] **AC-46**: A symbol-history query for a symbol whose content has not changed is served from the fingerprint cache on the second invocation; deleting or corrupting the cache causes recomputation of the same answer, not an error. (FR-37)
- [ ] **AC-47**: On Linux, the daemon establishes a UDS transport by default. With UDS establishment forced to fail, it falls back to loopback TCP, reports the fallback, and remains fully functional. (FR-38)
- [ ] **AC-48**: On the TCP fallback, a client that does not present the daemon's secret is refused; the secret file is owner-only; and a new daemon instance does not accept the previous instance's secret. (FR-39)
- [ ] **AC-49**: A client determines the active transport from repository-local metadata and connects on the first attempt, with no fallback probing of a transport the daemon is not serving. (FR-40)
- [ ] **AC-50**: Analyze requests are serialized with at most 32 pending analyze requests after compaction, including followers. The running scan remains unchanged, and a distinct request requiring a 33rd entry receives a retryable queue-full error. (FR-41)
- [ ] **AC-51**: Canonical-path compaction absorbs requests under equal or ancestor pending paths, replaces pending descendants with an incoming ancestor at the earliest displaced FIFO position, and preserves disjoint FIFO order without a configuration identity/provenance gate. (FR-42)
- [ ] **AC-52**: Followers poll their alias or canonical ID for the satisfying compacted scan terminal success or error, and effective force is ORed across every attached or replaced request. (FR-43)
- [ ] **AC-53**: Community detection reports file granularity by default, and the granularity used is present in the response. (FR-44)
- [ ] **AC-54**: Community detection reports which termination condition ended it — convergence or iteration ceiling — and requires no caller-supplied tuning parameter to return a partition. (FR-45)
- [ ] **AC-55**: On a synthetic graph engineered to collapse into one community, and on one with no edges at all, the response flags the partition as degenerate rather than returning it as an ordinary result. (FR-46)
- [ ] **AC-58**: **Deferred.** Generic asynchronous whole-graph jobs, including `detect_communities_async`, are not delivered by Phase 4. (FR-49)
- [ ] **AC-57**: For an edge whose target was selected from N same-named candidates, the tools that report that edge expose N. A caller can distinguish "one candidate, unambiguous" from "five candidates, one picked by scope rule" without issuing another query. (FR-48)
- [ ] **AC-56**: After the git provider lands, the workspace's set of native-library dependencies is unchanged from before it: the only C compiled into the build remains the tree-sitter grammar sources, no crate links a vendored or system library, and a release build requires no `pkg-config` or system-library discovery. (FR-47, D-0004)
- [ ] **AC-59**: On a native macOS runner, the Phase 10 acceptance matrix accounts for every completed task and acceptance criterion in phases 1–9; all applicable workspace and acceptance suites pass, the daemon exercises its macOS local transport and security boundary including denial from another local account, and CLI behavior matches Linux wire/machine output. (NFR-12)
- [ ] **AC-60**: On a native Windows runner, the Phase 11 acceptance matrix accounts for every completed task and acceptance criterion in phases 1–9; all applicable workspace and acceptance suites pass, the daemon exercises named-pipe and loopback-TCP fallback paths, deterministic security-descriptor inspection of the daemon's runtime state establishes ACL enforcement within the D-0014 scope, and Windows path behavior is covered rather than inferred from Linux. (NFR-13) *[Amended 2026-08-20 per D-0014 (recorded 2026-08-18, user-decided): the original text required "pipe-security-descriptor inspection and another-local-account denial". D-0014 scopes the daemon to one local user's sessions in one local project — cross-account isolation is not a claimed guarantee, so the another-local-account denial check was removed from scope, and the delivered inspection covers the runtime DIRECTORY's DACL (owner-only, no inherited ACEs, exactly one grant naming the invoking user — the state that holds the TCP secret and daemon metadata); the pipe object itself carries the default SD and is uninspected. This amendment reconciles the spec text D-0014's scope field already governed but never edited.]*

## Constraints
- **This spec supersedes `Designs/SharedDaemon` (D-0001).** That design specified a multi-tenant daemon: one process hosting many workspaces keyed by absolute root path, an HTTP transport with a bearer token, a discovery file under the user's XDG data directory, and `list_workspaces` / `select_workspace` tools. Its Decision 1 explicitly rejected Unix domain sockets. The direction here is deliberately narrower — one daemon per project root, all state inside the repository, no cross-workspace capability — on the grounds that system-wide state is not yet wanted. The two cannot both stand as written, and the earlier design is marked `superseded`. Because the repository-local shape is a strict subset of the multi-tenant one, a later expansion to multi-tenancy remains open and would not be blocked by anything specified here.
- The MCP tool surface is a published contract. Tool descriptions are production behaviour that agents pattern-match on, not documentation.
- The single `unsafe` opt-in in the workspace is scoped to one memory-map site in `code-graph-graph`. Nothing in this work may widen it.
- Symbol spans are line-granular. `Symbol` carries `line`, `column`, and `end_line`, but no end column. Position lookup and blame-a-symbol are therefore line-resolved, and cannot disambiguate two symbols that begin and end on the same line.
- The cache is a versioned binary format with a silent-re-index-on-mismatch policy. Any change to the symbol record forces a version bump; the tracks specified here are expected to require none.
- Call resolution remains a syntactic heuristic in all six languages. Shortest-path results inherit that imprecision and must not be presented as proof of reachability.
- Windows path handling retains its documented seams and known boundaries, but native Windows correctness is deferred to Phase 11 / AC-60 rather than inferred from the Linux MVP.

## Dependencies
- Track B and Track C's CLI exposure depend on Track A. Track C's query implementations do not.
- Track D depends on Track A only for front-end exposure; the provider trait and git implementation are independent.
- Track D depends on the existing language-plugin parse interface accepting a byte buffer rather than a path — confirmed present.
- New third-party dependencies are confined outside the protected core crates: an argument parser for the CLI, a git library for the git provider, a hash function for symbol fingerprinting, and binary-crate-only `getrandom` / `sysinfo` / `fs2` support for daemon credentials, safe process identity, and crash-released file locking. Platform-specific seams may compile conditionally, but Linux is the only support gate in the MVP phases.
- The decision ledger lives at `Decisions/decisions.md`; D-0001 records the repository-local daemon supersession described in Constraints.

## Resolved Questions
**OQ-01 — RESOLVED (D-0001).** *Is narrowing `Designs/SharedDaemon` to a repository-local daemon confirmed?* Yes. The repository-local model stands and `Designs/SharedDaemon` is marked `superseded`. Recorded as D-0001; see Constraints.

**OQ-02 — RESOLVED for MVP scope.** *What is the transport?* Linux uses UDS first and authenticated loopback TCP as fallback (FR-38 through FR-40). The transport seam retains the named-pipe shape, but native Windows implementation and validation are deferred to Phase 11 / AC-60; macOS completion is deferred to Phase 10 / AC-59.

**OQ-03 — RESOLVED.** *Which community-detection algorithm?* A near-linear, parameter-free label-propagation variant over **file-level** aggregation, with a stable node order and stated tie-breaks. Specified in FR-44 through FR-46. The file-granularity default is the substantive part of the answer: it matches the question being asked, shrinks the node count by orders of magnitude, reuses weights that already exist, and avoids compounding the error in heuristic call resolution. A modularity-optimising algorithm can be added later behind an option without disturbing FR-24, FR-25, or FR-45.

**OQ-04 — RESOLVED.** *Which git library?* A pure-Rust implementation, not bindings (FR-47, D-0004). There is no native async Rust git library for local object access — the mature pure-Rust option is blocking by design, with async support limited to network transports — so the async provider trait is satisfied by dispatching blocking calls to `tokio`'s blocking-task pool, which the workspace already has (FR-29). That is the same mechanism a future Perforce provider needs, since shelling out to a network client is equally blocking. Note the correction recorded in D-0004: the workspace is *not* C-compiler-free today, so the argument is "add no further native library", not "stay pure Rust end-to-end".

**OQ-05 — RESOLVED.** *Should symbol-history fingerprints be cached?* Yes, under `<project_root>/.code-graph/`, keyed so a stale entry cannot be served and degrading to recomputation on absence or corruption (FR-37).

**OQ-06 — RESOLVED, and reframed.** *Does the shared analyze slot need a larger retention window?* No. The replacement queue retains only followers necessary to complete absorbed or replaced pending requests; D-0011 bounds all such pending requests at 32 and makes async aliases individually pollable. It compacts pending canonical paths, never touches the running scan, and ORs force; it has no configuration identity/provenance gate or generic-job retention requirement (FR-41 through FR-43).

## Open Questions
- No specification-level question remains — **non-blocking** — all six raised during specification were answered before approval and are recorded under Resolved Questions above; the tuning-level questions that remain are design and plan concerns, not requirements gaps.
