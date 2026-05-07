---
title: "Phase 2 Debrief: Graph Engine & LLM Optimizations"
type: debrief
plan: "RustRewrite"
phase: 2
phase_title: "Graph Engine & LLM Optimizations"
status: complete
created: 2026-04-28
---

# Phase 2 Debrief: Graph Engine & LLM Optimizations

## Decisions Made

- **No internal locking on `Graph`.** The Go binary uses `sync.RWMutex` inside `Graph`; the Rust port keeps `Graph` lock-free and exposes `pub use parking_lot::RwLock` so callers (Phase 3's `ServerInner`) wrap it externally. This matches the design's `ServerInner { graph: RwLock<Graph>, ... }` shape and avoids the "double-locked" anti-pattern when readers hold a snapshot via the outer lock and the inner lock immediately re-acquires for each method call.
- **`FileEntry { language, symbol_ids }` instead of Go's `map[string][]string`.** Adding `language` per file in Phase 2.1 means cache v2 (Phase 3) can persist the language without re-deriving it from the file extension at load time. Cheap to add now; expensive to bolt on once the cache format is in flight.
- **One submodule per task.** `graph.rs` (2.1), `queries.rs` (2.2), `callgraph.rs` (2.3), `algorithms.rs` (2.4), `diagrams.rs` (2.5). Each task adds methods to `impl Graph` in its own file. This was a coordination decision (see Lessons Learned) rather than a pure architectural one — but it also reads better than a single 1500-line `graph.rs`.
- **Graph storage fields raised to `pub(crate)`.** The query/callgraph/algorithm/diagram modules need direct read access to `nodes`/`adj`/`radj`/`files`/`includes`. Threading getter methods through every method body would be noise; `pub(crate)` keeps the public API clean and lets sibling modules participate in the impl. Verified no `pub` slip in any of the five fields.
- **All public enums marked `#[non_exhaustive]`.** Phase 1 set the convention; Phase 2 added `RegistryError::DuplicateLanguage` and `RegistryError::InvalidExtension` cleanly because of it. New variants are semver-compatible additions.
- **Iterative Tarjan via parent-tracking frames.** The recursive Tarjan in Go relies on the implicit call stack. The plan calls for explicit-stack iteration in Rust to avoid overflow on deep include graphs. Used a `Vec<Step<N>>` worklist where each `Process` frame carries its parent and neighbor cursor; on frame finalization, propagate `lowlink` into the parent. Verified correctness against 6 fixtures including cross-SCC edges.
- **Class hierarchy stays recursive.** The plan's iterative-stack requirement is specific to Tarjan (deep include graphs); class hierarchies are realistically <1000 deep, so a recursive `build_hierarchy` is fine. **But** the Go reference uses `defer delete(onPath, name)` for unconditional cleanup; the Rust port uses a `PopGuard` RAII struct for the same reason — panic during recursion would otherwise leave a stale `on_path` entry.
- **Per-DFS-path tracking, not global visited.** This is the diamond-inheritance fix from `Designs/LLMOptimization/notes/01-Implementation.md`. The 4-class chain `Root←Base←{MixinA,MixinB}←Derived` at depth=3 must fully expand `Base` under both arms; a global visited set would short-circuit the second visit. The regression test asserts both copies of `Base` carry `bases=[Root]`, and the fix was mutation-tested by the implementer (temporarily reverting `on_path.remove` and watching the test fail).
- **`IndexMap` for deterministic Mermaid output.** Go iterates `map[string]bool` (randomized per process); the Rust port uses `indexmap::IndexMap` so node-ID assignment (`n0`, `n1`, ...) is byte-stable across invocations of `render_mermaid` for a fixed `DiagramResult`. The BFS layer above is **not** deterministic (it walks `HashMap`-backed adj/radj); documented this explicitly so future test authors don't assume cross-invocation byte-equality.
- **`SearchParams.language: Option<Language>` is the first consumer of Phase 1's `Symbol::language` field.** It's an exact-match filter, not a substring. Phase 1's `language` field would otherwise be a write-only ornament until Phase 5 ships the second parser.
- **`search_symbols(pattern, kind)` legacy wrapper passes `limit: 100`.** Carry-forward from `Designs/LLMOptimization/notes/01-Implementation.md` — the did-you-mean candidate pool was previously capped at 20 and missed close matches. Documented in the function's doc comment.
- **`incoming_coupling` for includes O(N×M) scan preserved.** Go iterates the entire `includes` map per file lookup. Phase 3 may add a reverse-include index; Phase 2 mirrors Go's complexity for parity.
- **`diagram_inheritance` default depth = 2 (not 1).** Faithfully ported from `diagram.go:183`. Call/file diagrams default to 1; inheritance defaults to 2. Distinct because two-hop class hierarchies are the common viewing depth for inheritance trees.

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| Graph struct + algorithms ported with full coverage | Met | 191 tests; full surface ported across 5 submodules |
| Diamond-inheritance regression test passes | Met | Asserts both copies of shared ancestor expand fully; verified to fail when per-DFS-path tracking is reverted (mutation-tested by implementer in 2.4) |
| LLM optimizations: brief default semantics, pagination envelope, namespace filter, language filter, summary | Partially Met | Pagination envelope (`SearchResult { symbols, total }`), namespace and language filters, and `symbol_summary` all done. **Brief default semantics** — the wire-format flag for "brief" output — is a Phase 3 handler-layer concern (the engine returns full `Symbol` objects; the MCP handler decides which fields to emit). Phase 3 wires this up. |
| Mermaid renderer in `codegraph-graph` produces valid output for all three diagram types | Met | `render_mermaid` covers call/file/inheritance. 6 dedicated tests including determinism, styled mode, direction passthrough, and edge-without-label fallback. |
| Concurrent reader/writer test passes | Met | 10 readers + 2 writers, 1.5s wall time, seed-symbol consistency probe (replaced a vacuous structural-invariant check after quality review). |
| Lint, format, and test gates green | Met | fmt/clippy/test/doc all clean; zero `#[allow]`, zero `unsafe` (workspace `unsafe_code = "forbid"` enforced) |
| Tarjan SCC iterative, no recursion | Met | Explicit `Vec<Step>` worklist; verified across 6 fixtures including cross-SCC edges |
| `class_hierarchy` widened filter accepts {Class, Struct, Interface, Trait} | Met | Tested with both Trait and Interface roots; Function-kind correctly rejected |

## Deviations

- **Wave 2 serialized instead of parallel.** The plan's dependency graph allows 2.2/2.3/2.4 to run in parallel (all depend only on 2.1). In practice, all three add methods to the same `impl Graph` block — three concurrent agents editing `crates/codegraph-graph/src/graph.rs` would have produced merge conflicts. Decision: split each task into its own submodule (`queries.rs`, `callgraph.rs`, `algorithms.rs`) and dispatch sequentially. Lost the parallelism but gained clean diffs and zero conflicts. The submodule split also produced a more readable codebase.
- **Test fixture extraction landed in 2.4, not at phase start.** Phase 2.3's quality review flagged that `sym`, `make_fg`, `call_edge`, `inherit_edge`, `include_edge` were duplicating across modules with diverging signatures. Phase 2.4's review found the duplication had reached 4 copies. Extraction landed as part of 2.4's follow-up commit (`94e75f1`). Earlier extraction would have saved ~30 lines of churn but the right time to extract was when divergence was actually a problem, not preemptively.
- **`diagram_inheritance_default_depth_is_two` was vacuously true on first submission.** The 3-class fixture meant that depth=1 and depth=2 produced the same edge set (all edges already collected during the start node's depth-0 processing). Quality review caught it; fixed in the 2.5 follow-up commit (`3a3ac87`) by extending to a 5-class chain with second-hop assertions.
- **`pub use parking_lot::RwLock`** instead of an internally-locked `Graph`. Plan said "Graph wrapped behind parking_lot::RwLock for production use" — could have been read as an internal wrap. Re-reading the design (`Designs/RustRewrite/README.md` line 306: `config: RwLock<RootConfig>` on `ServerInner`) confirmed the lock lives at `ServerInner`, not inside `Graph`. The re-export is a small ergonomic affordance ensuring callers don't accidentally import `std::sync::RwLock`.
- **`cargo doc --workspace --no-deps` gate.** The phase doc subtask 2.7 calls this out, but the verification field for 2.7 doesn't list it. First run of 2.7 produced 3 doc-link warnings (private-item links in public doc comments). Fixed inline; gate now strictly enforced.

## Risks & Issues Encountered

- **`impl Graph` file conflict risk on parallel waves.** Three concurrent agents editing the same `Graph` impl block would have raced. Resolved by serializing Wave 2 tasks (2.2, 2.3, 2.4) and giving each its own submodule. Cost: ~3-5 minutes of wall time per task vs hypothetical parallel run; benefit: zero conflict resolution.
- **Iterative Tarjan parent-lowlink propagation.** The classic gotcha of converting recursive Tarjan to iterative is timing the parent's lowlink update — it must run AFTER the child finishes processing, equivalent to the recursive return. Mishandled, the algorithm produces wrong SCCs (often missing an SCC entirely). The implementer used option (b) from the brief: include `Option<parent>` in every Step frame, and propagate `lowlinks[parent] = min(lowlinks[parent], lowlinks[child])` when a `Process` frame finalizes. Verified by hand-tracing 2-cycle, 3-cycle, mixed, and cross-SCC-edge cases.
- **Panic-safety for `on_path` in `class_hierarchy`.** First submission used naked `insert` / `remove` calls. If a recursive call panicked, `remove` would be skipped and `on_path` would carry a stale entry — corrupting sibling DFS paths. Quality review flagged it; fixed in 2.4's follow-up with a `PopGuard` RAII struct whose `Drop` impl removes the name unconditionally. Matches Go's `defer delete(onPath, name)`. The workspace uses `panic = "unwind"` (default), so this is a real (if unlikely) bug class.
- **Non-deterministic BFS output across `HashMap`-backed adj/radj.** `IndexMap` fixes determinism in `render_mermaid` (node-ID assignment), but the BFS layer above feeds it edge orderings that vary per process. Documented the boundary in the diagrams module doc; tests that care about byte-equality construct `DiagramResult` directly.
- **Vacuous test assertions slipped through twice.** First time: `cpp_cast_does_not_produce_call_edge` in Phase 1.5 tested only `static_cast` (the other three keywords never appeared in the source). Second time: `diagram_inheritance_default_depth_is_two` in 2.5. Both caught by quality review. Both fixes were small. Pattern: "the test passes for both the buggy and the fixed code" is the failure mode to watch for; the fix is to construct fixtures where buggy/fixed produce demonstrably different output.
- **Doc-link warnings on private items.** `cargo doc --workspace --no-deps` produced 3 warnings: two for `[\`mermaid_label\`]` and `[\`Graph::remove_file_unsafe\`]` linking from public doc comments to private items, one redundant explicit link target. Fixed by replacing the doc-link syntax with bare backtick-code spans for private references and removing the redundant explicit target. The `cargo doc` gate is now genuinely useful.

## Lessons Learned

- **File-overlap analysis is mandatory before parallel waves.** The `/implement` skill's "advisory overlap analysis" step exists for exactly this reason. For Phase 2, that analysis correctly flagged Wave 2 as risky and the right answer was to serialize. Future phases that add methods to a shared struct (Phase 3 will add to `ServerInner`, Phases 5/6/7 to a yet-to-be-defined trait impl) should default to one-submodule-per-task in the plan rather than gambling on agent coordination.
- **Mutation testing for regression-protection assertions.** The diamond-inheritance test author actually reverted the fix to confirm the test caught the regression (per the implementer's report on 2.4). This is the discipline the plan's "verified by reverting the per-DFS-path tracking and watching the test fail" line was asking for. Worth elevating to a project-wide convention: any test marketed as a "regression test" should ideally be confirmed-failing under the buggy code at least once during development.
- **Default the deferred-pattern fix to RAII.** Rust has no `defer`. Three Phase 2 tasks would have benefited from the pattern (`on_path` in 2.4, future lock-release in 2.6, future cache-write in Phase 3). Each ad-hoc structural fix is more error-prone than a small `Drop`-impl guard struct. This is now a pattern in the codebase (`PopGuard` in `algorithms.rs`).
- **Determinism contracts are easy to overstate.** The diagrams module doc initially read like a global determinism guarantee. The actual guarantee is narrow: `render_mermaid(diagram_result)` is byte-stable for a fixed input. The BFS that produces the input is not. Stating the boundary explicitly is worth more than the words it adds.
- **`pub(crate)` storage fields beat parallel getter methods.** When sibling modules need read access, lifting field visibility is one line; threading a getter API is dozens. As long as the storage type's invariants don't depend on encapsulation (and the `Graph`'s don't — every mutation goes through the public mutators), the visibility lift is the right answer. External consumers still see only the public methods; only intra-crate code crosses the field boundary.
- **`cargo doc --workspace --no-deps` is a useful low-overhead gate.** Caught real doc-link drift in 2.7. Adding it to the structural verification set for every phase costs ~1 second of CI time and documents intent. The phase doc already lists it; the project should keep it.
- **Quality-scanner Major findings stay worth fixing inline; Minor often defers cleanly.** Phase 1 established this; Phase 2 confirmed. Of 14 quality-scanner findings across 7 tasks (6 Major, 8 Minor), the 6 Majors all became inline fixes (genuine correctness or API hygiene). 6 of 8 Minors became inline fixes; the 2 deferred Minors were both in transitional code (parse-test bin's symlink/lookup gaps in Phase 1; the implementation-detail comment in 2.5's stub `parse_file` already from Phase 1.5). The deferral reasoning was the same both times: the code is about to be replaced.

## Impact on Subsequent Phases

- **Phase 3 (MCP server, parallel discovery, persistence)** picks up:
  - `pub use parking_lot::RwLock` — `ServerInner` wraps `RwLock<Graph>` directly using this re-export.
  - The full Graph query API including `search`/`callers`/`callees`/`symbol_summary`/`coupling`/`incoming_coupling`/`class_hierarchy`/`detect_cycles`/the three `diagram_*` methods. MCP tool handlers should be thin parameter-marshalling wrappers — all the logic is in the engine.
  - `DiagramResult::render_mermaid` — `generate_mermaid` MCP handler is a one-liner over the result.
  - **Brief mode** is a Phase 3 handler-layer concern. The engine returns full `Symbol` objects; handlers project to a brief view (drop `signature`, `column`, `end_line`) when the request asks for it. This is consistent with the LLMOptimization design.
  - **Wire-format snapshot tests (`cargo insta`)** will lock in JSON shapes from Phase 2's types: `Symbol` with `language`, `SearchResult { symbols, total }`, `DiagramResult { center, edges }`, `HierarchyNode { name, bases, derived }`, `CallChain { symbol_id, file, line, depth }`. Any subsequent change is a deliberate snapshot rebaseline.
  - **`LanguagePlugin::resolve_call` / `resolve_include`** — Phase 1 left these as stubs returning None. Phase 3 fills in `default_scope_aware_resolve` (matching Go's `same file > same parent > same namespace > global` heuristic) and `default_basename_resolve`. The graph engine's edges have `from`/`to` strings (raw, unresolved); Phase 3's discovery layer is responsible for resolving them via the registered language plugin's hooks before edges land in the graph.

- **Phase 4 (cutover)** — Removes the Go tree (`internal/`, `cmd/`, `go.mod`, `go.sum`). The Rust workspace at this point is feature-complete for C++. The Phase 1 transitional `codegraph-parse-test` (synchronous walker) gets superseded; Phase 3's parallel `ignore::WalkBuilder` discovery becomes the only walker.

- **Phases 5/6/7 (Rust/Go/Python parsers)** — Each language plugin overrides `resolve_call` / `resolve_include` for its semantics:
  - Rust: use-tree resolution (`use foo::bar::baz` brings `baz` into scope).
  - Go: package-path resolution (`pkg.FuncName` resolves via the import map).
  - Python: dotted import paths plus `__init__.py` semantics.
  - The default scope-aware heuristic (Phase 3) is the C++ behavior. Each subsequent language plugin layers on top.

- **Plan README's `testdata/cpp` and `fmtlib/fmt` numbers** remain stale (per Phase 1 debrief). Phase 1 corrected them in the phase doc; the README still cites the original figures. Phase 3 should normalize once the parallel walker is the canonical entry point.

## Skill Opportunities

- **What you did repeatedly:** Run the Go reference binary on the same input, capture its output, then `diff` against the Rust binary's output to confirm parity.
  - **Where it belongs:** A `Makefile` recipe (carry-forward from the Phase 1 debrief — flagged but not yet implemented). E.g., `make parity-check DIR=testdata/cpp`.
  - **Why a skill:** Phase 5/6/7 will all need this for their dogfooding gates. Memorizing the two `tail -50` paths plus a `diff` invocation is friction; a one-line `make` invocation is not. Phase 2 didn't trigger it because Phase 2 is graph-engine work with no parse-test entry point — but Phase 3 onwards will.
  - **Rough shape:** Inputs: a directory. Outputs: zero on parity, non-zero with the diff printed. Implementation: temp files, `go run ./cmd/parse-test $DIR > /tmp/go-output.txt`, `cargo run -p codegraph-parse-test -- $DIR > /tmp/rust-output.txt`, `diff` them.

- **What you did repeatedly:** Run `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo doc --workspace --no-deps` as the structural verification gate after every commit.
  - **Where it belongs:** A `Makefile` recipe like `make verify` that runs all four. Already ~50% there: `make rust-build`, `rust-test`, `rust-lint`, `rust-fmt-check` exist; just need a `rust-verify` target that chains them plus `cargo doc`.
  - **Why a skill:** Quality-scanner-driven workflow runs this gate ~3-5x per task. Each invocation is 4 commands. A single-command gate cuts the sequence and makes "before requesting review" a one-liner.
  - **Rough shape:** Pure `Makefile` target; depends on the existing recipes. Can run in parallel via `make -j4` if the recipes are independent.

- **What you did repeatedly:** Identify "vacuous test" anti-patterns where a test passes equally under buggy and fixed code (`cpp_cast_does_not_produce_call_edge` in 1.5, `diagram_inheritance_default_depth_is_two` in 2.5). Quality-scanner caught both — pattern is recognizable.
  - **Where it belongs:** A bullet in `planner:quality-scanner`'s prompt: "For any regression-protection test, verify it would fail under the buggy code by inspecting whether the buggy and correct paths produce visibly different output for the test fixture. If the assertion is structural-invariant (always-true by construction) or the fixture doesn't exercise the differential, flag it."
  - **Why a skill:** Two of two regression-test misses in Phase 1+2 had the same shape. Hard to spot without explicit prompting. Already a quality-scanner finding pattern; codifying it in the agent prompt makes it consistent across reviews.
  - **Rough shape:** Prompt convention; not code. Add a sentence to the quality-scanner agent prompt's testing lens.

- **What you did repeatedly:** Decide whether a Minor finding is worth fixing inline vs deferring. The criterion that emerged: "the code is about to be replaced" justifies deferral; everything else gets fixed inline.
  - **Where it belongs:** Documentation in the `/implement` skill's "process review findings" section. Currently that section says "Non-critical findings → collect and present to user after the wave completes" — true but doesn't surface the deferral criterion.
  - **Why a skill:** Two phases in, this decision happens 3-4 times per phase. The criterion is consistent enough to write down. Saves 30 seconds of deliberation per finding and produces consistent dispositions across phases.
  - **Rough shape:** Documentation update; one paragraph in `shared/orchestration.md` or the `/implement` command body.

- **What you did repeatedly:** Look up the canonical Go reference shape (line ranges in `internal/graph/*.go`, `internal/parser/*.go`) before each Rust port task to embed in the implementer brief.
  - **Where it belongs:** Could be a small `make ref-show TASK=<file>:<lines>` recipe, but really this is an artifact of the porting nature of Phases 1-2 and won't recur in Phases 3+ (where the Rust port stops mirroring Go and starts implementing rmcp-specific shapes).
  - **Why a skill:** Probably not worth a skill — the friction is real but localized to porting phases, and Phase 3 starts deviating from Go (rmcp tool handlers don't have a Go equivalent, the watch loop uses `notify-debouncer-full` instead of Go's `fsnotify`, etc.).
  - **Verdict:** No action.
