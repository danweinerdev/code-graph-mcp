---
title: "Java Plugin"
type: phase
plan: CSharpJavaSupport
phase: 3
status: complete
created: 2026-05-08
updated: 2026-05-11
deliverable: "A working `code-graph-lang-java` crate that parses .java files via tree-sitter-java, emits Function/Method/Class/Enum/Interface symbols (no Struct — Java has no struct construct; records fold into Class per Decision 6), Calls/Includes/Inherits edges, and ships a corpus + watch-mode reindex regression + dogfood baseline (commons-lang submodule). Plugin is NOT YET registered in the binary — that lands in Phase 4."
tasks:
  - id: "3.1"
    title: "Scaffold code-graph-lang-java crate"
    status: complete
    verification: "`crates/code-graph-lang-java/` exists with the same shape as `crates/code-graph-lang-python/` (Cargo.toml, src/lib.rs, src/queries.rs, src/helpers.rs, tests/corpus.rs). Cargo.toml's name is `code-graph-lang-java`, depends on `code-graph-core`, `code-graph-lang`, `tree-sitter`, `tree-sitter-java`, `streaming-iterator`, `thiserror`, `anyhow`. `JavaParser::new()` returns `Result<Self, _>` and constructs a tree-sitter parser with the Java language. The plugin's `id()` returns `Language::Java`. The plugin's `extensions()` returns `&[\".java\"]`. Object-safety + id() smoke test mirrors the canonical form at `crates/code-graph-lang-cpp/src/lib.rs:542-545` — asserts only `id()` (extensions are tested implicitly through the corpus tests in 3.6). The crate is added to `Cargo.toml`'s `[workspace.members]`. `cargo build -p code-graph-lang-java` succeeds. `cargo test -p code-graph-lang-java` runs the smoke test. `cargo clippy -p code-graph-lang-java --all-targets -- -D warnings` clean."
  - id: "3.2"
    title: "Java definition extraction"
    status: complete
    depends_on: ["3.1"]
    verification: "tree-sitter queries produce Symbol records for: methods inside classes (parent = enclosing class), classes, interfaces, enums (Decision 12: enum constants are NOT extracted as symbols; enum methods extract as `Method` with parent = enum type — e.g., `Planet`), records as ordinary `Class` (Decision 6 — `SymbolKind::Record` not added; auto-generated members not extracted; `permits` clauses ignored). Anonymous classes (Decision 4): `new Runnable() { void run() {...} }` does NOT emit a Class symbol; the inner method takes the enclosing **named entity's parent** as parent. If the enclosing method is `OuterClass::handle`, the anonymous's `run` method has parent `OuterClass`. Pin the anti-regression: two anonymous classes inside the same enclosing method that both define `run` produce two `OuterClass::run` symbols (Decision 4 documented limitation). Default interface methods (Decision 11): `interface I { default void doFoo() {...} }` extracts doFoo as `Function` (no parent), NOT `Method` — same rule as Rust trait default methods (cite `crates/code-graph-lang-rust/src/lib.rs`). Static interface methods follow the same rule. Enum-with-method-bodies fixture covers `enum Planet { EARTH { double surfaceGravity() {...} } ... }` — emits `Method` records on `Planet`, no synthetic `Planet$EARTH` parent. Inline tests in `src/lib.rs` cover each rule. `cargo test -p code-graph-lang-java` passes."
  - id: "3.3"
    title: "Java call extraction"
    status: complete
    depends_on: ["3.2"]
    verification: "Call edges produced for: direct (`foo()`), member-access (`obj.foo()`), chained (`a.b().c()` → 2 edges, one per chain link), method reference (`String::length` — recorded as a call to `length` if the grammar treats method references as `method_reference` invocations, OR documented as a known limitation if not), invocation inside lambda (`x -> foo(x)`), invocation inside anonymous class method bodies (the call's `from` is the enclosing named entity per Decision 4). Inline tests cover each pattern. `cargo test -p code-graph-lang-java` passes."
  - id: "3.4"
    title: "Java import extraction"
    status: complete
    depends_on: ["3.2"]
    verification: "Import (Includes) edges produced for: `import com.foo.Bar;` → `to = \"com.foo.Bar\"`; `import com.foo.*;` → `to = \"com.foo.*\"` (wildcard preserved verbatim, matching the Rust plugin's `use foo::*` rule); `import static com.foo.Bar.STATIC_FIELD;` → `to = \"com.foo.Bar.STATIC_FIELD\"` (treats static-import target as the dotted path; `static` modifier dropped — same rule as C# `using static`). Per Decision 7, all imports record the dotted path verbatim — no resolution against pom.xml or build.gradle. Anti-regression: a backtick-style or unusual-grammar import does NOT crash the parser. `cargo test -p code-graph-lang-java` passes."
  - id: "3.5"
    title: "Java inheritance extraction"
    status: complete
    depends_on: ["3.2"]
    verification: "Inherits edges produced for: `class Foo extends Bar` → 1 edge (`from = \"Foo\"`, `to = \"Bar\"`); `class Foo implements IBaz, IQux` → 2 edges; `class Foo extends Bar implements IBaz, IQux` → 3 edges total; `interface I extends J, K` → 2 edges; `class Foo<T extends Comparable<T>> extends Bar<T>` → 1 edge with `from = \"Foo<T>\"`, `to = \"Bar<T>\"` (Decision 9 — generic params preserved verbatim). Both `extends` and `implements` produce the same `EdgeKind::Inherits` (Decision 2). The `from` field uses bare class name per the contract established in Phase 1 of RustRewrite — cite `crates/code-graph-graph/src/algorithms.rs`. Sealed types' `permits` clauses are ignored (Decision 6). Inline test asserts that `class Foo extends Bar implements IBaz` produces 2 Inherits edges, both with `EdgeKind::Inherits`. `cargo test -p code-graph-lang-java` passes."
  - id: "3.6"
    title: "Java testdata + corpus + watch + commons-lang dogfood"
    status: complete
    depends_on: ["3.3", "3.4", "3.5"]
    verification: "`testdata/java/` exists with realistic fixtures (mirroring `testdata/python/` shape): `MANIFEST.md` documenting expected pinned counts, sample multi-class files, and `edge_cases/` containing `Empty.java` (empty class body — assert 1 Class symbol, 0 method symbols), `CommentsOnly.java`, `Broken.java` (syntax-error file — pin the recovered-symbol count discovered at write time, RUN AND RECORD; do not assume zero), `NestedClasses.java` (2-level nested class with parent assertion), `AnonymousInside.java` (method containing two anonymous classes both with a `run` method — pins the Decision 4 collision behavior), `Records.java` (a `record User(String name)` declaration — assert one Class symbol, no synthetic accessor methods), `EnumWithMethods.java` (the Planet fixture from Decision 12). `tests/corpus.rs` walks `testdata/java/` and asserts pinned aggregate counts against `MANIFEST.md`. New file `crates/code-graph-tools/tests/watch_java_reindex.rs` covers `Graph::prune_dangling_edges` for both Inherits and Calls when symbols are removed. The watch test follows the diagnostic-sentinel pattern (CLAUDE.md test conventions). `external/commons-lang` git submodule added (`.gitmodules` entry, pinned to a `LANG_3_X_X` tag selected at implementation time, `git submodule add --depth 1`). `Makefile` comment block at lines 64-68 has `commons-lang` added to the named submodules list (the comment lists by name; the count language in CLAUDE.md is updated in Phase 4.4). `testdata/java/commons-lang-baseline.txt` written with `symbols: <recorded>`, `tag: <pinned>`, `commit: <SHA>` (use `expected baseline: TBD; populated on first dogfood run, gated at ±10%` in the task brief — DO NOT pre-guess a numeric range). New dogfood test in `tests/corpus.rs` mirrors `crates/code-graph-lang-rust/tests/corpus.rs:529` (auto-skip via `eprintln!` + `return` when `external/commons-lang/src/main/java` is absent — verify the directory layout at the pinned SHA; do NOT panic; do NOT use `#[ignore]`). `cargo test -p code-graph-lang-java` passes (skips dogfood when submodule absent); `cargo test -p code-graph-tools --test watch_java_reindex` passes."
  - id: "3.7"
    title: "Java structural verification"
    status: complete
    depends_on: ["3.6"]
    verification: "All four structural gates pass against the Java crate: `cargo test -p code-graph-lang-java`, `cargo clippy -p code-graph-lang-java --all-targets -- -D warnings` (zero warnings), `cargo fmt --all --check` (clean), `cargo audit` (no new advisories from the tree-sitter-java dep). Workspace `unsafe_code = \"forbid\"` lint holds. No `.snap.new` pending snapshots in the working tree (`make snapshot-clean` passes). Phase 3 close-out: write the debrief at `notes/03-Java-Plugin.md` capturing actual baseline numbers from 3.6, any tree-sitter-java grammar surprises encountered, and any quality-scanner findings carried-forward to Phase 4."
---

# Phase 3: Java Plugin

## Overview

Build the `code-graph-lang-java` plugin crate from scaffold to dogfood-baseline. This phase runs in **parallel with Phase 2** (C# plugin) — neither depends on the other; both depend on Phase 1.

Tasks 3.2–3.6 are sequential within this phase: each adds a new `extract_X` call to `parse_to_filegraph`, so parallel dispatch within Phase 3 produces merge conflicts on the shared entry point.

**Prerequisite:** Run `/planner:refresh-brief CSharpJavaSupport/03-Java-Plugin.md` before dispatching `/planner:implement` for any task in this phase.

**Carry-forward:** `/planner:carry-forward` prior task's quality-scanner findings into the next task's brief at every task boundary in this phase.

## 3.1: Scaffold code-graph-lang-java crate

### Subtasks

- [x] Create `crates/code-graph-lang-java/` with the standard shape mirroring `crates/code-graph-lang-python/` (queries.rs uses `pub(crate) const DEFINITION_QUERIES` etc. matching shipped-plugin convention)
- [x] Add `crates/code-graph-lang-java` to `[workspace.members]` (alphabetical order between go and python)
- [x] Object-safety + id() smoke test passes (`parser_is_object_safe_and_id_returns_java`)
- [x] Build/test/clippy gates pass for `code-graph-lang-java`
- [x] `parse_to_filegraph` stub-comment `_tree`/`root` mismatch corrected in follow-up commit `c1e5130` so the 3.2 implementer doesn't stumble on the rebind step

### Notes

Same scaffold shape as 2.1. Helper consolidation already done; no pre-phase consolidation needed.

## 3.2: Java definition extraction

### Subtasks

- [x] Filled `DEFINITION_QUERIES` with 6 patterns: class, interface, enum, record, method, ctor
- [x] Implemented `extract_definitions` walking query matches and producing Symbol records
- [x] Decision 6 (records): record extracts as Class; methods inside records extract as Method with parent = record name (records-leak anti-regression mirrored from C# 2.2's `0cf200b` fix)
- [x] Decision 4 (anonymous classes): no Class symbol emitted; methods inside anonymous bodies take enclosing named entity as parent (helpers walk past `object_creation_expression` boundaries); collision case (two anonymous in same method, both with `run`) produces two symbols disambiguated by line
- [x] Decision 11 (default interface methods): body-presence discriminator (refinement of brief — subsumes `default`, `static`, AND Java-9+ `private` interface methods cleanly); abstract bodyless methods drop as forward declarations. Doc minors fixed in follow-up `b6f45ab` plus a private-interface-method test pinning the Java-9+ case
- [x] Decision 12 (enum methods): enum-level AND per-constant methods extract with parent = enum type; enum constants don't extract; enum-level abstract methods filtered as forward decls
- [x] Decision 6 (sealed types): `permits` clause ignored; sealed interface extracts as ordinary Interface
- [x] 28 inline tests passing (27 from `4342b36` + 1 from `b6f45ab` for Java-9+ private case)

### Notes

Decision 4 documented limitation: two anonymous classes inside the same enclosing method that both define a `run` method produce two `OuterClass::run` symbols with the same ID — the file path + line number disambiguate at query time. The fixture `AnonymousInside.java` pins this case.

## 3.3: Java call extraction

### Subtasks

- [ ] Write `CALL_QUERY` covering `method_invocation` (direct, member-access, chained), `object_creation_expression` (constructor calls), invocation inside `lambda_expression` body, invocation inside anonymous class method bodies, invocation inside enum constant method bodies
- [ ] Implement `extract_calls` resolving the enclosing function/method via `find_enclosing_kind`
- [ ] Method references (`String::length`, `obj::method`) — record as a `Calls` edge to the right-hand identifier IF tree-sitter-java's grammar exposes a clean node for the referenced name. If the grammar makes this awkward, document as a known limitation in the phase 4 CLAUDE.md update (matching Python's heuristic-resolution disclaimers).
- [ ] Inline tests cover each pattern

### Notes

The method-reference handling is the one place this phase leaves room for grammar reality to dictate the answer. Discover-and-document is acceptable; over-engineering an unusable query is not.

## 3.4: Java import extraction

### Subtasks

- [ ] Write `IMPORT_QUERY` covering `import_declaration` in all forms (plain, wildcard, static)
- [ ] Implement `extract_imports` recording the dotted path verbatim per Decision 7
- [ ] Strip the `static` modifier (`import static com.foo.Bar.X` → `to = "com.foo.Bar.X"`); preserve the wildcard verbatim (`import com.foo.*` → `to = "com.foo.*"`)
- [ ] Inline tests cover each form

### Notes

Java imports record dotted-path verbatim (no resolution against `pom.xml` or `build.gradle`).

## 3.5: Java inheritance extraction

### Subtasks

- [ ] Write `INHERITANCE_QUERY` for `superclass` (extends) and `super_interfaces` (implements) on classes, and `extends_interfaces` on interfaces
- [ ] Implement `extract_inheritance` producing one `Inherits` edge per base
- [ ] Use the bare class name as the `from` field (cite `crates/code-graph-graph/src/algorithms.rs`)
- [ ] Generic types preserve generic parameter text verbatim (Decision 9): `class Foo<T extends Comparable<T>> extends Bar<T>` → `from = "Foo<T>"`, `to = "Bar<T>"`. Wildcard generics (`List<? extends Number>`) appear in signatures only, not in the structured symbol record
- [ ] Sealed types' `permits` clauses do NOT produce edges (Decision 6)
- [ ] Inline tests: single `extends`, multiple `implements`, both combined, generic with bound

### Notes

Decision 2 trade-off: `class Foo extends Bar implements IBaz, IQux` produces 3 edges, all `EdgeKind::Inherits`. Agents disambiguate via the target symbol's kind (Class vs Interface).

## 3.6: Java testdata + corpus + watch + commons-lang dogfood

### Subtasks

- [ ] Create `testdata/java/` with realistic multi-class fixtures
- [ ] After creating fixtures, run `git check-ignore -v testdata/java/**` to confirm no files are silently gitignored (per CLAUDE.md test conventions — the `.code-graph.toml` trap class). If any fixture is excluded, use `git add -f`. `git status` must show every fixture as staged before commit.
- [ ] Create `testdata/java/edge_cases/`:
  - `Empty.java` (empty class body — 1 Class symbol, 0 method symbols)
  - `CommentsOnly.java`
  - `Broken.java` (syntax-error file — **run and record** the recovered symbol count; do not assume zero)
  - `NestedClasses.java` (2-level nested class — pin parent assertion)
  - `AnonymousInside.java` (method containing two anonymous classes, both with `run` — pins Decision 4 collision)
  - `Records.java` (a `record User(String name)` — assert one Class symbol)
  - `EnumWithMethods.java` (the Planet fixture from Decision 12 — abstract method + per-constant body)
  - `MethodReferences.java` (covers `String::length`-style calls; documents whatever the grammar produces)
- [ ] `testdata/java/MANIFEST.md` documenting pinned aggregate counts (use `expected: TBD; populated on first run`)
- [ ] `crates/code-graph-lang-java/tests/corpus.rs` walks `testdata/java/` and asserts the MANIFEST counts
- [ ] Create `crates/code-graph-tools/tests/watch_java_reindex.rs`:
  - Sentinel + discriminator pattern
  - Cover Inherits-edge pruning (`class Foo extends Bar`, remove file, edge pruned)
  - Cover Calls-edge pruning
- [ ] Add `external/commons-lang` git submodule:
  - Add to `.gitmodules` with `[submodule "external/commons-lang"]`, url `https://github.com/apache/commons-lang.git`
  - `git submodule add --depth 1 -b <pinned-tag> https://github.com/apache/commons-lang.git external/commons-lang`
  - **Verify the walked subdirectory layout** at the pinned SHA — design says `src/main/java`
- [ ] Add dogfood test to `crates/code-graph-lang-java/tests/corpus.rs` mirroring `crates/code-graph-lang-rust/tests/corpus.rs:529`:
  - Resolve `external/commons-lang/src/main/java` via `env!("CARGO_MANIFEST_DIR")`
  - `eprintln!` + `return` when directory absent
  - Read baseline from `testdata/java/commons-lang-baseline.txt`; panic if baseline file missing
  - Assert symbol count within ±10% of baseline
- [ ] Write `testdata/java/commons-lang-baseline.txt` with `symbols: <recorded>`, `tag: <pinned>`, `commit: <SHA>`
- [ ] Update `Makefile` comment block at lines 64-68 — add `commons-lang` to the named list of submodules (the comment lists by name, not by count).
- [ ] Update CLAUDE.md's "Optional: dogfood-baseline submodules" table to add the Java row (the Build-section `init all six (shallow clones)` count update is bundled into Phase 4.4)

### Notes

If 2.6 has already shipped when 3.6 lands, the Makefile docstring update is editing the post-2.6 state (growing 7 → 8). If 2.6 and 3.6 happen to land in the same merge window, both modifications go in one commit.

## 3.7: Java structural verification

### Subtasks

- [ ] Run all four structural gates against the Java crate:
  - `cargo test -p code-graph-lang-java`
  - `cargo clippy -p code-graph-lang-java --all-targets -- -D warnings`
  - `cargo fmt --all --check`
  - `cargo audit`
- [ ] Verify workspace `unsafe_code = "forbid"` lint holds
- [ ] Run `make snapshot-clean`
- [ ] Write Phase 3 debrief at `notes/03-Java-Plugin.md`:
  - Actual baseline numbers from 3.6 (commons-lang symbol count, pinned tag, walked-subdir layout)
  - Tree-sitter-java grammar surprises (any node-name discoveries that diverged from the design — especially around method references, default methods, sealed types)
  - Quality-scanner findings to carry forward to Phase 4
  - Skill opportunities for the planner plugin

### Notes

Run this task only after 3.6 is fully merged.

## Acceptance Criteria

- [ ] `crates/code-graph-lang-java/` exists with the canonical scaffold and all four extractors implemented
- [ ] All inline tests pass for definitions, calls, imports, inheritance — including the records, anonymous classes, default methods, and enum-methods anti-regressions
- [ ] `testdata/java/` + `crates/code-graph-lang-java/tests/corpus.rs` regression locks aggregate symbol counts
- [ ] `crates/code-graph-tools/tests/watch_java_reindex.rs` covers Inherits + Calls pruning
- [ ] `external/commons-lang` submodule pinned; `testdata/java/commons-lang-baseline.txt` recorded; dogfood test auto-skips when submodule absent
- [ ] Phase 3 debrief written at `notes/03-Java-Plugin.md`
- [ ] All structural gates pass: `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, `cargo audit`, `make snapshot-clean`
