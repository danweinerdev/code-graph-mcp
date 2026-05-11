---
title: "C# Plugin"
type: phase
plan: CSharpJavaSupport
phase: 2
status: complete
created: 2026-05-08
updated: 2026-05-11
deliverable: "A working `code-graph-lang-csharp` crate that parses .cs files via tree-sitter-c-sharp, emits Function/Method/Class/Struct/Enum/Interface symbols, Calls/Includes/Inherits edges, and ships a corpus + watch-mode reindex regression + dogfood baseline (efcore submodule). Plugin is NOT YET registered in the binary — that lands in Phase 4."
tasks:
  - id: "2.1"
    title: "Scaffold code-graph-lang-csharp crate"
    status: complete
    verification: "`crates/code-graph-lang-csharp/` exists with the same shape as `crates/code-graph-lang-python/` (Cargo.toml, src/lib.rs, src/queries.rs, src/helpers.rs, tests/corpus.rs). Cargo.toml's name is `code-graph-lang-csharp`, depends on `code-graph-core`, `code-graph-lang`, `tree-sitter`, `tree-sitter-c-sharp`, `streaming-iterator`, `thiserror`, `anyhow`. `CSharpParser::new()` returns `Result<Self, _>` and constructs a tree-sitter parser with the C# language. The plugin's `id()` returns `Language::CSharp`. The plugin's `extensions()` returns `&[\".cs\"]`. Object-safety + id() smoke test mirrors the canonical form at `crates/code-graph-lang-cpp/src/lib.rs:542-545`: `let p: Box<dyn LanguagePlugin> = Box::new(CSharpParser::new().unwrap()); assert_eq!(p.id(), Language::CSharp);` (the canonical form asserts only `id()` — extensions are tested implicitly through the corpus tests in 2.6). The crate is added to `Cargo.toml`'s `[workspace.members]`. `cargo build -p code-graph-lang-csharp` succeeds. `cargo test -p code-graph-lang-csharp` runs the smoke test. `cargo clippy -p code-graph-lang-csharp --all-targets -- -D warnings` clean."
  - id: "2.2"
    title: "C# definition extraction"
    status: complete
    depends_on: ["2.1"]
    verification: "tree-sitter queries in `queries.rs` produce Symbol records for: free functions, methods inside classes (parent = enclosing class), classes (incl. partial), structs, interfaces, enums (members not extracted as symbols — Decision 12 analog for C#). Partial classes (Decision 3): `partial class Foo {}` in two files produces TWO Class symbols both named `Foo`, distinguishable by `Symbol.path` and `Symbol.line`. Default interface methods (Decision 11, C# 8+): C# does NOT use a `default` keyword on interface methods — a default interface method is detected by the presence of a method body (`{ ... }`) inside an `interface_declaration`, in contrast to abstract methods which end with `;`. Example: `interface I { void Foo() { /* body */ } }` extracts Foo as `Function` (no parent), NOT `Method`; `interface I { void Bar(); }` (no body, abstract) does NOT produce a Symbol record (forward-declaration rule, mirroring the four shipped plugins). Extension methods (Decision 5): `static class Ext { static int Count(this string s) {...} }` extracts Count as Method with parent `Ext` (syntactic), NOT `string`. Inline tests in `src/lib.rs` cover each rule. Anti-regression test for partial-class case: pin that two `partial class Foo` declarations across two files yield exactly two Class symbols. `cargo test -p code-graph-lang-csharp` passes; `cargo clippy -p code-graph-lang-csharp --all-targets -- -D warnings` clean."
  - id: "2.3"
    title: "C# call extraction"
    status: complete
    depends_on: ["2.2"]
    verification: "Call edges produced for: direct (`Foo()`), member-access (`obj.Foo()`), chained (`a.B().C()` → 2 edges, one per chain link), invocation through `?.` (null-conditional, `obj?.Foo()`), invocation inside lambda (`x => Foo(x)`), invocation inside LINQ expression (`from x in ys select Foo(x)`), constructor calls via `new Foo()` (recorded as a call to `Foo`, agent interprets as construction — same rule Python uses for `MyClass()`). Inline tests in `src/lib.rs` cover each pattern. `cargo test -p code-graph-lang-csharp` passes."
  - id: "2.4"
    title: "C# import extraction"
    status: complete
    depends_on: ["2.2"]
    verification: "Import (Includes) edges produced for: `using System;` → `to = \"System\"`; `using System.Collections.Generic;` → `to = \"System.Collections.Generic\"`; `using static System.Console;` → `to = \"System.Console\"` (the `static` modifier is dropped); `using FooAlias = Some.Long.Type.Name;` → `to = \"Some.Long.Type.Name\"` (alias dropped, path preserved — same rule as Python `import foo as f`); `global using System.Linq;` (C# 10+) → `to = \"System.Linq\"` (the `global` modifier is dropped). Per Decision 7, all imports record the dotted path verbatim — no resolution against build metadata. Anti-regression test: an import inside a namespace block (`namespace Foo { using Bar; ... }`) is captured (the import query must walk into namespace bodies, not just the top of the file). `cargo test -p code-graph-lang-csharp` passes."
  - id: "2.5"
    title: "C# inheritance extraction"
    status: complete
    depends_on: ["2.2"]
    verification: "Inherits edges produced for: `class Foo : Bar` → 1 edge with `from = \"Foo\"`, `to = \"Bar\"`; `class Foo : Bar, IBaz, IQux` → 3 edges (one per base); `class Foo<T> : Bar<T>` → 1 edge with `from = \"Foo<T>\"`, `to = \"Bar<T>\"` (Decision 9 — generic params preserved verbatim, matching Rust's rule, NOT Go's strip-rule). The `from` field uses bare class name (no path:Outer::Inner form) per the contract established in Phase 1 and reaffirmed in Phase 5 of RustRewrite — see `crates/code-graph-graph/src/algorithms.rs` (the `get_class_hierarchy` walker is the consumer of this contract; cite it explicitly in the task brief). Both class extension and interface implementation produce `Inherits` (Decision 2 — no separate `Implements` edge kind). Inline test asserts both forms produce edges with the same edge kind. `cargo test -p code-graph-lang-csharp` passes."
  - id: "2.6"
    title: "C# testdata + corpus + watch + efcore dogfood"
    status: complete
    depends_on: ["2.3", "2.4", "2.5"]
    verification: "`testdata/csharp/` exists with realistic fixtures (mirroring `testdata/python/` shape): `MANIFEST.md` documenting expected pinned counts, sample multi-class files, and `edge_cases/` containing `empty.cs` (zero bytes — 0 symbols, 0 crashes), `comments_only.cs`, `broken.cs` (syntax-error file — pin the recovered-symbol count discovered at write time, mirroring the Phase 7 `broken.py` discovery; tree-sitter recovers more than expected — RUN AND RECORD, do not assume zero), `nested_classes.cs` (2-level nested class with parent assertion), and `partial_class_a.cs` + `partial_class_b.cs` (two partial declarations of the same Class). `tests/corpus.rs` walks `testdata/csharp/` and asserts pinned aggregate counts against `MANIFEST.md`. New file `crates/code-graph-tools/tests/watch_csharp_reindex.rs` covers `Graph::prune_dangling_edges` for both Inherits and Calls when symbols are removed; specifically pins the partial-class lifecycle (start with two partials of `Foo`, remove one file, confirm one Class symbol remains and the orphaned methods are pruned). The watch test follows the diagnostic-sentinel pattern (CLAUDE.md test conventions) — assert a low-stakes baseline first before the discriminator. `external/efcore` git submodule added (`.gitmodules` entry, pinned to a v8.x LTS tag selected at implementation time, `git submodule add --depth 1`). `Makefile` comment block at lines 64-68 has `efcore` added to the named submodules list (the comment lists submodules by name, not by count — \"all six\"/\"all eight\" count language lives in CLAUDE.md and is updated in Phase 4.4). `testdata/csharp/efcore-baseline.txt` written with `symbols: <recorded>`, `tag: <pinned>`, `commit: <SHA>` (use `expected baseline: TBD; populated on first dogfood run, gated at ±10%` in the task brief — DO NOT pre-guess a numeric range). New dogfood test in `tests/corpus.rs` mirrors `crates/code-graph-lang-rust/tests/corpus.rs:529` exactly (auto-skip via `eprintln!` + `return` when `external/efcore/src/EFCore` is absent — verify the directory layout at the pinned SHA; do NOT panic; do NOT use `#[ignore]`). `cargo test -p code-graph-lang-csharp` passes (skips dogfood when submodule absent); `cargo test -p code-graph-tools --test watch_csharp_reindex` passes."
  - id: "2.7"
    title: "C# structural verification"
    status: complete
    depends_on: ["2.6"]
    verification: "All four structural gates pass against the C# crate: `cargo test -p code-graph-lang-csharp`, `cargo clippy -p code-graph-lang-csharp --all-targets -- -D warnings` (zero warnings), `cargo fmt --all --check` (clean), `cargo audit` (no new advisories from the tree-sitter-c-sharp dep). Workspace `unsafe_code = \"forbid\"` lint holds (zero `unsafe` blocks introduced — same as the four shipped plugins). No `.snap.new` pending snapshots in the working tree (`make snapshot-clean` passes — relevant if any inline snapshots were added during 2.2–2.6). Phase 2 close-out: write the debrief at `notes/02-CSharp-Plugin.md` capturing actual baseline numbers from 2.6, any tree-sitter-c-sharp grammar surprises encountered (matches the broken.py discovery from Phase 7), and any quality-scanner findings carried-forward to Phase 4."
---

# Phase 2: C# Plugin

## Overview

Build the `code-graph-lang-csharp` plugin crate from scaffold to dogfood-baseline. This phase runs in **parallel with Phase 3** (Java plugin) — neither depends on the other; both depend on Phase 1.

Tasks 2.2–2.6 are sequential within this phase: each adds a new `extract_X` call to `parse_to_filegraph`, so parallel dispatch within Phase 2 produces merge conflicts on the shared entry point. The Phase 5/6/7 debriefs from RustRewrite all re-derived this rule.

**Prerequisite:** Run `/planner:refresh-brief CSharpJavaSupport/02-CSharp-Plugin.md` before dispatching `/planner:implement` for any task in this phase. Phase 5, 6, and 7 of RustRewrite each shipped with zero cross-cutting fixes *because* of the refresh — the refresh-brief catches contracts where the codebase reality has drifted from the design's prose.

**Carry-forward:** `/planner:carry-forward` prior task's quality-scanner findings into the next task's brief at every task boundary in this phase. Validated 5× in Phase 7.

## 2.1: Scaffold code-graph-lang-csharp crate

### Subtasks

- [x] Create `crates/code-graph-lang-csharp/` with the standard shape mirroring `crates/code-graph-lang-python/`
- [x] Add `crates/code-graph-lang-csharp` to `[workspace.members]` (alphabetical order between cpp and go)
- [x] Object-safety + id() smoke test passes (`parser_is_object_safe_and_id_returns_csharp`); query constants renamed to plural `DEFINITION_QUERIES` etc. with `pub(crate)` visibility (corrected from initial brief in follow-up commit `c1e5130`)
- [x] Build/test/clippy gates pass for `code-graph-lang-csharp`

### Notes

This task is intentionally narrow — no queries, no extractor logic. Just enough scaffold to make 2.2–2.5 land cleanly. Helper consolidation already happened in RustRewrite's 7.1 and 7.7 phases — both `truncate_signature` and `find_enclosing_kind` are already in `code-graph-lang::helpers`. No pre-phase consolidation needed.

## 2.2: C# definition extraction

### Subtasks

- [x] Filled `DEFINITION_QUERIES` with 8 patterns (added `record_declaration` after a quality-scan finding — see fix-up commit `0cf200b`): class, struct, interface, enum, record, method, ctor, local function
- [x] Implemented `extract_definitions` walking query matches and producing Symbol records
- [x] Decision 3 (partial classes): two `partial class Foo` declarations across files yield two Class symbols (anti-regression test pinned)
- [x] Decision 11 (default interface methods): body-presence discriminator (works for both `block` and `arrow_expression_clause` body forms); abstract methods drop as forward-declarations
- [x] Decision 5 (extension methods): `this` modifier does NOT remap the parent; method extracts under the enclosing static class
- [x] Decision 6 follow-up (records): records extract as Class, methods inside records extract as Method with parent = record name (NOT orphan Function — fixed in `0cf200b`)
- [x] 28 inline tests passing (initial 25 in `f4581fa` + 2 records tests + 1 doc-fix follow-up): kind dispatch, partial classes, default interface methods (block + arrow), extension methods, namespace resolution, records, nested classes, line/end_line, signature truncation

### Notes

The C# `partial class` detection requires checking the modifier list on the class declaration; the symbol record itself does NOT need a `partial: true` flag (Decision 3 closed-question note). Agents disambiguate via file path.

## 2.3: C# call extraction

### Subtasks

- [ ] Write `CALL_QUERY` covering `invocation_expression` in all forms (direct, member-access, chained, null-conditional, lambda body, LINQ-expression body) and `object_creation_expression` (for `new Foo()` constructor calls)
- [ ] Implement `extract_calls` resolving the enclosing function/method via `find_enclosing_kind`
- [ ] Inline tests cover each pattern (direct, member-access, chained-2-link, null-conditional, lambda-internal, LINQ-internal, constructor)

### Notes

Decision 5 documented limitation: extension method calls (`myString.CountWords()`) resolve via the standard scope-aware heuristic — the resolver may attribute to the syntactic `Extensions::CountWords` (correct) or to a same-named method on `string` (incorrect, if one exists). This is the same imperfection class as C++'s overloaded-function resolution.

## 2.4: C# import extraction

### Subtasks

- [ ] Write `IMPORT_QUERY` covering `using_directive` in all forms (plain, static, alias, global)
- [ ] Implement `extract_imports` recording the dotted path verbatim per Decision 7
- [ ] Strip the `static` modifier (`using static X.Y` → `to = "X.Y"`); strip the `global` modifier (`global using X` → `to = "X"`); drop the alias name but keep the target path (`using A = X.Y` → `to = "X.Y"`)
- [ ] Anti-regression: `using` directives nested inside a `namespace { ... }` block must be captured

### Notes

C# imports do NOT participate in build-system resolution. The `Includes` edge's `to` field is the dotted namespace name as written; the default `resolve_include` returns `None` for non-filesystem paths, mirroring Python.

## 2.5: C# inheritance extraction

### Subtasks

- [ ] Write `INHERITANCE_QUERY` for `base_list` on classes, structs, and interfaces (C# does not syntactically distinguish class extension from interface implementation in the base-list)
- [ ] Implement `extract_inheritance` producing one `Inherits` edge per base
- [ ] Use the bare class name as the `from` field (cite `crates/code-graph-graph/src/algorithms.rs` — the `get_class_hierarchy` walker is the consumer of this contract; the brief established this in Phase 1 and Phase 5)
- [ ] Generic types preserve generic parameter text verbatim (Decision 9, Rust precedent — `Foo<T> : Bar<T>` → `from = "Foo<T>"`, `to = "Bar<T>"`)
- [ ] Inline test: `class Foo : Bar, IBaz, IQux` produces 3 `Inherits` edges, all with the same `EdgeKind::Inherits` (no separate `Implements` kind — Decision 2)

### Notes

Decision 2 trade-off: `get_class_hierarchy("Foo")` returns the union of class extension and interface implementation. Agents disambiguate post-hoc from the target symbol's kind (Class vs Interface).

## 2.6: C# testdata + corpus + watch + efcore dogfood

### Subtasks

- [ ] Create `testdata/csharp/` with realistic multi-class fixtures
- [ ] After creating fixtures, run `git check-ignore -v testdata/csharp/**` to confirm no files are silently gitignored (per CLAUDE.md test conventions — the `.code-graph.toml` trap class). If any fixture is excluded, use `git add -f`. `git status` must show every fixture as staged before commit.
- [ ] Create `testdata/csharp/edge_cases/`:
  - `empty.cs` (zero bytes — assert 0 symbols, 0 crashes)
  - `comments_only.cs`
  - `broken.cs` (syntax-error file — **run and record** the recovered symbol count; do not assume zero. Tree-sitter recovers more than expected per the Phase 7 `broken.py` discovery)
  - `nested_classes.cs` (2-level nested class — pin parent assertion that the inner class records the immediate enclosing outer class as parent)
  - `partial_class_a.cs` + `partial_class_b.cs` (two `partial class Foo` declarations)
  - `method_name_collides_with_free_function.cs` (a method `Foo` and a free function `Foo` — distinct symbols)
- [ ] `testdata/csharp/MANIFEST.md` documenting pinned aggregate counts (use `expected: TBD; populated on first run`)
- [ ] `crates/code-graph-lang-csharp/tests/corpus.rs` walks `testdata/csharp/` and asserts the MANIFEST counts
- [ ] Create `crates/code-graph-tools/tests/watch_csharp_reindex.rs`:
  - Sentinel + discriminator pattern (CLAUDE.md test conventions): assert a no-partial fixture extracts as a baseline; assert the partial-class lifecycle as the discriminator
  - Cover Inherits-edge pruning (start with `class Foo : Bar`, remove the file, confirm Inherits edge is pruned)
  - Cover Calls-edge pruning (similar)
- [ ] Add `external/efcore` git submodule:
  - Add to `.gitmodules` with `[submodule "external/efcore"]`, url `https://github.com/dotnet/efcore.git`
  - `git submodule add --depth 1 -b <pinned-tag> https://github.com/dotnet/efcore.git external/efcore` (pinned tag chosen at implementation time — a v8.x LTS tag)
  - **Verify the walked subdirectory layout** at the pinned SHA — design says `src/EFCore` but actual layout may differ
- [ ] Add dogfood test to `crates/code-graph-lang-csharp/tests/corpus.rs` mirroring `crates/code-graph-lang-rust/tests/corpus.rs:529` exactly:
  - Resolve `external/efcore/<walked-subdir>` via `env!("CARGO_MANIFEST_DIR")`
  - `eprintln!` + `return` when directory absent (NOT panic; NOT `#[ignore]`)
  - Read baseline from `testdata/csharp/efcore-baseline.txt`; panic if baseline file missing
  - Assert symbol count within ±10% of baseline
- [ ] Write `testdata/csharp/efcore-baseline.txt` with `symbols: <recorded>`, `tag: <pinned>`, `commit: <SHA>` headers (use `expected baseline: TBD; populated on first run` placeholder; record actual numbers when first dogfood run completes)
- [ ] Update `Makefile` comment block at lines 64-68 — the comment lists submodules by name (`logrus, requests, ripgrep, fmt, curl, abseil-cpp`), NOT by count. Add `efcore` to the named list. (The "all six" / "all eight" count language lives in CLAUDE.md, not the Makefile.)
- [ ] Update CLAUDE.md's "Optional: dogfood-baseline submodules" table to add the C# row (commons-lang lands in 3.6 as the 8th row). The Build-section reference `make submodules # init all six (shallow clones)` will be updated to `init all eight` in Phase 4.4 (don't update it incrementally to "all seven" here — Phase 4 batches the count update)

### Notes

The `expected baseline: TBD` convention is mandatory per PLANNER_IMPROVEMENTS.md Tier 3 #3. The Phase 7 retro found that `requests` came in at 284 vs the brief's 400-1000 estimate; pre-guessing baselines is anti-pattern.

The watch-mode test's diagnostic-sentinel pattern is documented in CLAUDE.md test conventions — the sentinel's failure message names the most likely root cause (timing, IO, file-write race) so the failure mode is self-diagnosing.

## 2.7: C# structural verification

### Subtasks

- [ ] Run all four structural gates against the C# crate:
  - `cargo test -p code-graph-lang-csharp` (full crate test suite)
  - `cargo clippy -p code-graph-lang-csharp --all-targets -- -D warnings` (zero warnings)
  - `cargo fmt --all --check` (clean)
  - `cargo audit` (no new advisories)
- [ ] Verify workspace `unsafe_code = "forbid"` lint holds (no `unsafe` blocks)
- [ ] Run `make snapshot-clean` (no `.snap.new` pending)
- [ ] Write Phase 2 debrief at `notes/02-CSharp-Plugin.md`:
  - Actual baseline numbers from 2.6 (efcore symbol count, pinned tag, walked-subdir layout)
  - Tree-sitter-c-sharp grammar surprises (any node-name discoveries that diverged from the design)
  - Quality-scanner findings to carry forward to Phase 4
  - Skill opportunities for the planner plugin (mirroring Phase 7 debrief format)

### Notes

Run this task only after 2.6 is fully merged. `cargo audit` may flag new advisories from `tree-sitter-c-sharp`'s transitive deps — investigate before suppressing.

## Acceptance Criteria

- [ ] `crates/code-graph-lang-csharp/` exists with the canonical scaffold and all four extractors implemented
- [ ] All inline tests pass for definitions, calls, imports, inheritance
- [ ] `testdata/csharp/` + `crates/code-graph-lang-csharp/tests/corpus.rs` regression locks aggregate symbol counts
- [ ] `crates/code-graph-tools/tests/watch_csharp_reindex.rs` covers Inherits + Calls pruning, with the partial-class lifecycle as the load-bearing discriminator
- [ ] `external/efcore` submodule pinned; `testdata/csharp/efcore-baseline.txt` recorded; dogfood test auto-skips when submodule absent
- [ ] Phase 2 debrief written at `notes/02-CSharp-Plugin.md`
- [ ] All structural gates pass: `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, `cargo audit`, `make snapshot-clean`
