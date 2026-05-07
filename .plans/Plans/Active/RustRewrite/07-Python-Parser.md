---
title: "Python Language Parser"
type: phase
plan: RustRewrite
phase: 7
status: in-progress
created: 2026-04-28
updated: 2026-05-05
deliverable: "codegraph-lang-python crate parsing .py and .pyi files with class/method/decorator handling, both import forms, multi-base inheritance, and the Python-specific call node type; registered in the main binary; testdata/python/ + real-world validation; the four-language MCP is complete and the RustRewrite plan moves to Plans/Complete/"
tasks:
  - id: "7.1"
    title: "codegraph-lang-python crate scaffold + queries.rs + Cargo.toml dependencies"
    status: complete
    verification: "`tree-sitter-python = \"=0.25.0\"` added to workspace `[workspace.dependencies]` (strict `=` pin matching the Phase 1 C++ convention); `crates/codegraph-lang-python/Cargo.toml` `[dependencies]` populated (tree-sitter, tree-sitter-python, streaming-iterator, codegraph-core, codegraph-lang, thiserror, anyhow) and `[dev-dependencies]` populated (rstest, pretty_assertions, insta); `cargo build -p codegraph-lang-python` green before any parser code lands; `PythonParser::new() -> anyhow::Result<PythonParser>` compiles all queries against tree-sitter-python 0.25 without error; `extensions()` returns `[\".py\", \".pyi\"]` (both extensions dispatch to the same parser; `.pyi` stub files use the same grammar — no separate query path); `id()` returns `Language::Python` (already defined in `crates/codegraph-core/src/lib.rs:29` from Phase 1); **object-safety + id() verified by a single `#[test] fn python_parser_is_object_safe_via_box_dyn() { let p: Box<dyn LanguagePlugin> = Box::new(PythonParser::new().unwrap()); assert_eq!(p.id(), Language::Python); }` matching the Phase 1 C++ test at `crates/codegraph-lang-cpp/src/lib.rs:542-545` exactly**; `PythonParser` does NOT override `resolve_call` or `resolve_include` — `resolve_call` accepts the default scope-aware heuristic (Python's dynamic typing makes any static call resolution inherently noisy; the default heuristic is the documented contract); `resolve_include` accepts the default basename match against the FileIndex, which is a no-op for Python module paths because `from foo.bar import baz` records `foo.bar` as a dotted module path, not a filesystem path; query categories: definitions (function_definition, class_definition — tree-sitter-python 0.25 wraps `async def` as a `function_definition` node, NOT a separate `async_function_definition`, so no extra query is needed; confirmed by fixture), calls (`call` — Python's tree-sitter uses 'call' not 'call_expression'), imports (import_statement with dotted_name, import_from_statement with module_name), inheritance (class_definition with superclasses argument_list); helpers find_enclosing_class, extract_module_path, walk_decorator_unwrap unit-tested"
  - id: "7.2"
    title: "Definition extraction with method-vs-function context, decorator transparency, async methods"
    status: complete
    depends_on: ["7.1"]
    verification: "function_definition without enclosing class_definition → Kind=Function; function_definition with enclosing class_definition → Kind=Method, parent=class name; async def methods (`class Server: async def handle(self): ...`) produce Kind=Method with parent=class — confirmed by fixture; class_definition → Kind=Class; @decorator wrapping a function/class produces decorated_definition wrapping the inner node — queries match the inner node directly because tree-sitter queries search the entire tree, so decorator presence does not block extraction (verified by a fixture with `@property def x(self)` producing a Method symbol); __init__, __str__, __repr__ are ordinary methods (no special handling); nested classes (class inside class) handled — outer class is the parent for the inner class; namespace stays empty for Python (Python's module concept is captured in the file path itself, not in a namespace tag); `.pyi` stub files extract symbols identically to `.py` files (function_definition with `...` body still parses as a function_definition; corpus tests assert this); tests cover each case"
  - id: "7.3"
    title: "Call site extraction"
    status: complete
    depends_on: ["7.1"]
    verification: "Python's tree-sitter uses node kind 'call' (NOT 'call_expression'); call > function: identifier → direct call (foo()); call > function: attribute > attribute: identifier → method/attribute call (obj.method()); chained calls (a.b().c()) produce 2 edges; constructor calls (MyClass()) treated as direct calls (To=MyClass) — these naturally produce edges that look like 'calls to a class', which the agent can interpret as construction; super() calls captured; built-in calls (print, len) captured; calls inside list comprehensions, lambdas, default arguments — all captured with the enclosing function as From; tests for each pattern"
  - id: "7.4"
    title: "Import extraction with module path semantics"
    status: complete
    depends_on: ["7.1"]
    verification: "import foo → 1 edge with To='foo'; import foo.bar → 1 edge with To='foo.bar'; import foo as f → 1 edge with To='foo' (alias dropped, path preserved — same rule as Go); from foo import bar → 1 edge with To='foo' (the module path, NOT 'bar' — the imported symbol name is not the dependency); from foo.bar import baz → 1 edge with To='foo.bar'; from . import utils → 1 edge with To='.utils' (relative import preserved as written); from typing import List → 1 edge with To='typing'; from __future__ import annotations → 1 edge with To='__future__' (dunder module name handled correctly); each edge has Kind=Includes; for `from . import utils` the default `resolve_include` returns None (no filesystem path is matched), confirming relative imports are stored verbatim and never accidentally resolved; tests cover every form including the relative form, the from-form-vs-module-path distinction, and the dunder __future__ case"
  - id: "7.5"
    title: "Inheritance extraction"
    status: complete
    depends_on: ["7.1"]
    verification: "class D(B) → 1 inherits edge from D to B; class D(A, B) → 2 inherits edges (multiple inheritance, common in Python); class D(module.Base) → 1 inherits edge from D to 'module.Base' (qualified base preserved); class C: (no parens) → 0 inherits edges; class C(metaclass=Meta) → metaclass keyword arg ignored (not a base); ABC inheritance (`class C(ABC)`) treated like any other base; tests for each form"
  - id: "7.6"
    title: "testdata/python + corpus tests + real-world dogfood + watch-mode reindex regression"
    status: complete
    depends_on: ["7.2", "7.3", "7.4", "7.5"]
    verification: "testdata/python/ project covers: classes with __init__/__str__/__repr__, @property/@staticmethod/@classmethod decorators, single + multiple + qualified inheritance, ABC with @abstractmethod (extracted as ordinary methods), async def methods inside classes, generators (yield), context managers, type hints, all import forms (incl. `from __future__ import annotations`); a `stubs.pyi` file demonstrates `.pyi` stub extraction; MANIFEST.md documents expected symbols and edges with separate counts for the `.pyi` file; corpus tests cover every definition form, every call pattern, every import form, every inheritance form, and edge cases (empty file, comments-only file, syntax error file → parser skips error nodes gracefully, deeply nested classes, method-with-same-name-as-free-function, *args/**kwargs in signature, generator function, property decorator, `.pyi` stub file); parse-test testdata/python matches MANIFEST; **watch-mode reindex regression** (in `crates/codegraph-tools/tests/watch_python_reindex.rs`): start watch on a temp directory containing `models.py` with `class Alpha:`, `class Beta(Alpha):`, and `class Delta: def use_beta(self): Beta()`; modify `models.py`: remove `Beta` and `Delta.use_beta`, add `class Gamma(Alpha):`; after debounce, assert `get_file_symbols` shows `Alpha` + `Gamma`, no `Beta` or `Delta.use_beta`; assert `get_class_hierarchy` for `Alpha` shows `Gamma` as derived (not `Beta`); assert no dangling Inherits edge from `Beta` and no dangling Calls edge from `Delta.use_beta` to `Beta` — both kinds of edges go through `Graph::prune_dangling_edges`; real-world dogfood: clone `github.com/psf/requests` (well-known mid-sized Python library) to `/tmp/requests` at a pinned tag (v2.32.3), run `parse-test /tmp/requests/src/requests`, expect 0 crashes, 0 warnings, approximate symbol count between 400 and 1000 recorded as `testdata/python/requests-baseline.txt` (one line: `symbols: N`); follow-up test asserts the recorded count stays within ±10% as a regression gate"
  - id: "7.7"
    title: "Register parser, four-language integration, cross-language collision regression, snapshots, documentation"
    status: planned
    depends_on: ["7.6"]
    verification: "main.rs registers PythonParser using the shipped Box+context pattern (mirroring the C++/Rust/Go blocks at `main.rs:20-23`); full four-language analyze on a directory containing .cpp + .rs + .go + .py — extends the `testdata/mixed/` fixture (created in 5.6, extended in 6.6) by adding `foo.py` defining `def helper(): pass`; analyze indexes all four; search without language filter returns from all four; with each `language=` filter returns only that language's match; **4-way cross-language collision regression**: `init` exists in C++, Go, and Python (`def init(): ...` at module scope) — assert `search_symbols` returns three entries, and `get_callers` against any one does NOT return the others' callers (verifying the `(Language, name)`-keyed SymbolIndex isolation from Phase 3 at `crates/codegraph-lang/src/lib.rs:116`); wire-format snapshot tests extended with Python responses (cargo insta accept on the new fixtures); README + CLAUDE.md final update — all four languages listed in supported-languages table (C++ `.cpp/.cc/.h/.hpp`, Rust `.rs`, Go `.go`, Python `.py/.pyi`); update `crates/code-graph-mcp/src/main.rs` module-level doc comment to reflect all four languages are now live; Python-specific limitations documented: call resolution especially noisy due to dynamic typing; decorators transparent for definition extraction but @abstractmethod not flagged as a separate kind; type hints not extracted as edges; conditional imports (`if TYPE_CHECKING: import ...`) NOT extracted because tree-sitter sees them as block-level statements inside an `if_statement`, not as top-level `import_statement` nodes"
  - id: "7.8"
    title: "Structural verification + plan close-out"
    status: planned
    depends_on: ["7.7"]
    verification: "`make release` (host-target only) succeeds and produces a four-language-capable binary; `cargo fmt --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean across all crates including codegraph-lang-python; `cargo test --workspace` green — every Phase 1-7 test passes; `cargo audit` clean; no new `unsafe` (workspace `unsafe_code = \"forbid\"`); no `#[allow(clippy::...)]` suppressions; **plan close-out (in this exact order)**: (1) write Phase 7 debrief to `notes/07-Python-Parser.md` while the plan is still in `Plans/Active/` (so the debrief captures the actual implementation experience, not a retrospective); (2) update plan README `phases[7].status` to `complete` and the README top-level `status` to `complete`; (3) `git mv .plans/Plans/Active/RustRewrite .plans/Plans/Complete/RustRewrite`; (4) commit with subject `[RustRewrite/Phase 7] plan close-out — RustRewrite complete`; the four-language MCP is shipped and the SharedDaemon plan (in `Designs/SharedDaemon/`, status `draft`) is unblocked and ready for `/planner:plan`"
---

# Phase 7: Python Language Parser

## Overview

Add Python language support — priority 4 per the user's ordering, completing the four-language MCP. Python's grammar is simple but has two distinctive challenges: (1) call nodes are `call`, not `call_expression`; (2) imports come in two forms (`import` and `from ... import`), and the dependency edge points at the *module*, not the imported symbol. This phase replaces the original `Plans/PythonParser/` (status: superseded).

After this phase ships green, `code-graph-mcp` is feature-complete for the user's stated four-language scope. The plan moves from `Plans/Active/` → `Plans/Complete/` and the next planned body of work is the `SharedDaemon` design (in `Designs/SharedDaemon/`, status `draft`, deferred — replaces the per-session stdio model with a long-running multi-tenant daemon).

This doc was reviewed against the as-shipped state of phases 1-4 on 2026-04-30 and re-reviewed after task-split refactoring (see `notes/04-Watch-Cross-Compile-Cutover.md`). Updates incorporate: the actual `LanguageRegistry::register(Box<dyn LanguagePlugin>)` signature with anyhow `.context(...)` wrappers, the `Graph::prune_dangling_edges` invariant established in Phase 4.2, the no-cross-compile build path from Phase 4.3, the explicit `id() → Language::Python` registration requirement, the `#[test]` form of the object-safety check (avoids the `-D warnings` clippy gate), `.pyi` stub-file coverage, async-method coverage inside classes, `from __future__ import annotations` handling, and a 7.6/7.7/7.8 task split that lifts the plan close-out into a standalone final task.

## 7.1: codegraph-lang-python crate scaffold + queries.rs + Cargo.toml dependencies

### Subtasks
- [x] Add `tree-sitter-python = "=0.25.0"` to workspace `Cargo.toml` `[workspace.dependencies]` (strict `=` pin matching the Phase 1 C++ convention)
- [x] `crates/codegraph-lang-python/Cargo.toml`:
  - `[dependencies]`: tree-sitter, tree-sitter-python (workspace = true), streaming-iterator, codegraph-core, codegraph-lang, thiserror, anyhow
  - `[dev-dependencies]`: rstest, pretty_assertions, insta
- [x] **Compile gate:** `cargo build -p codegraph-lang-python` succeeds (empty crate, deps resolve) before any parser code is written
- [x] `PythonParser` with cached Query objects (definitions, calls, imports, inheritance)
- [x] `PythonParser::new() -> anyhow::Result<PythonParser>`
- [x] `extensions()` returns `[".py", ".pyi"]` — both extensions dispatch to the same parser; `.pyi` stub files use the same grammar
- [x] `id()` returns `Language::Python` — already defined in `crates/codegraph-core/src/lib.rs:29`; no codegraph-core change needed
- [x] **Default trait methods:** `PythonParser` does NOT override `resolve_call` or `resolve_include`. Rationale: Python's dynamic typing means *no* static call resolution is fully accurate; the default scope-aware heuristic is the documented contract. Default `resolve_include` is a no-op for Python module paths because `from foo.bar import baz` records `foo.bar` as the dotted module path, not a filesystem path.
- [x] `queries.rs`:
  - `DEFINITION_QUERIES`: function_definition, class_definition. **Note:** tree-sitter-python 0.25 wraps `async def` as a `function_definition` node (not a separate `async_function_definition`), so no extra query is needed — confirm by fixture in 7.2.
  - `CALL_QUERIES`: `(call function: (identifier))` and `(call function: (attribute attribute: (identifier)))` — note `call`, not `call_expression`
  - `IMPORT_QUERIES`: import_statement and import_from_statement
  - `INHERITANCE_QUERIES`: `(class_definition superclasses: (argument_list (identifier) @base) ...)` plus dotted_name variant for qualified bases
- [x] Helpers in `helpers.rs`:
  - `find_enclosing_class(node)` — walks up to class_definition
  - `extract_module_path(import_node, content)` — produces the dotted module path string
- [x] **Object-safety + id() test** in `#[cfg(test)] mod tests` (mirrors `crates/codegraph-lang-cpp/src/lib.rs:542-545` exactly):
  ```rust
  #[test]
  fn python_parser_is_object_safe_via_box_dyn() {
      let p: Box<dyn LanguagePlugin> = Box::new(PythonParser::new().unwrap());
      assert_eq!(p.id(), Language::Python);
  }
  ```
- [x] **Bundled consolidation:** `truncate_signature` extracted to `crates/codegraph-lang/src/helpers.rs`; cpp/rust/go plugins re-export via `pub use codegraph_lang::helpers::truncate_signature;`. Eliminates the 4-copy state Phase 6 debrief flagged.

## 7.2: Definition extraction with decorator transparency and async methods

### Subtasks
- [x] `function_definition` with no enclosing class_definition → Kind=Function, no parent
- [x] `function_definition` enclosed in class_definition → Kind=Method, parent = class name
- [x] `class_definition` → Kind=Class
- [x] **async def in class:** `class Server: async def handle(self): ...` produces a Method with parent=Server (tree-sitter-python 0.25 represents `async def` as `function_definition`, so the same query path applies — confirmed by fixture)
- [x] Decorated definitions: tree-sitter wraps them in `decorated_definition` but the queries match the inner `function_definition` / `class_definition` nodes directly (this is how tree-sitter queries work — they search the whole tree)
- [x] Nested classes: a class defined inside another class produces a Class symbol whose parent is the outer class
- [x] `__init__`, `__str__`, `__repr__`, `__call__` are ordinary methods — no special handling
- [x] Namespace stays empty for Python; the file path encodes the module location
- [x] `.pyi` stub files extract symbols identically to `.py` files — `def foo(x: int) -> str: ...` (a stub with `...` body) still parses as `function_definition` and produces a Function symbol
- [x] Tests:
  - `def foo(): ...` → Function
  - `class C: def m(self): ...` → C is Class, m is Method with parent=C
  - `class Server: async def handle(self): ...` → handle is Method with parent=Server (async-method-in-class fixture)
  - `@property def x(self): ...` → Method (decorator transparent)
  - `@staticmethod def s(): ...` → Method
  - `class A: class B: pass` → both Classes; B's parent=A
  - `.pyi` stub: `def foo(x: int) -> str: ...` → Function (same as `.py` equivalent)

## 7.3: Call site extraction

### Subtasks
- [x] `extract_calls`:
  - `(call function: (identifier))` → direct call (To = identifier text)
  - `(call function: (attribute attribute: (identifier)))` → method/attribute call (To = the attribute identifier)
- [x] Enclosing function: walk up to `function_definition`; if found, From = `path:funcName` or `path:Parent::funcName`; module-level fallback (no enclosing `function_definition`) → From = bare file path
- [x] Constructor calls: `MyClass()` is just `(call function: (identifier))` matching the direct-call query — produces an edge To=MyClass; this naturally captures construction (the agent can interpret it)
- [x] super() calls captured as direct calls (To=super)
- [x] Calls inside list/dict/set comprehensions, lambdas, default arguments all produce edges with the enclosing top-level function as From
- [x] Tests for each pattern

## 7.4: Import extraction

### Subtasks
- [ ] `extract_imports`:
  - `import foo` → `(import_statement name: (dotted_name))` → To='foo'
  - `import foo.bar` → To='foo.bar'
  - `import foo as f` → To='foo' (alias dropped via the `aliased_import` wrapper — pull from `name:` field)
  - `from foo import bar` → `(import_from_statement module_name: (dotted_name))` → To='foo' (NOT 'bar')
  - `from foo.bar import baz` → To='foo.bar'
  - `from . import utils` → To='.utils' (relative imports preserved as written)
  - `from typing import List, Dict` → 1 edge with To='typing' (the module is the dependency, not each imported name)
  - `from __future__ import annotations` → To='__future__' (dunder module name handled correctly)
- [ ] **Default `resolve_include` no-op test:** for a fixture file `src/app.py` containing `from . import utils`, assert `get_dependencies("src/app.py")` returns the list `[".utils"]` (literal, not resolved against the FileIndex) — confirms the default basename resolver returns None for relative-import paths and the wire format records them verbatim
- [ ] Tests for every form including the relative form, the from-form-vs-module-path distinction, the dunder __future__ case, and the resolve_include no-op confirmation

### Notes
The "from-form points at the module, not the imported name" rule comes from the original `Plans/PythonParser/01-Parser-Core.md` task 1.6 notes and is preserved verbatim. An agent searching for "what does this file depend on?" wants modules, not names — and the imported name is already in scope after the import statement.

**Conditional imports are NOT extracted.** Patterns like:
```python
if TYPE_CHECKING:
    import expensive_module
```
are wrapped in `if_statement > block` rather than appearing at module scope as `import_statement` nodes. Tree-sitter's query matching does walk the whole tree, but the existing `IMPORT_QUERIES` deliberately don't enter `block` nodes inside conditionals — so no edges are emitted for these. This is documented as a known limitation in CLAUDE.md (added in Task 7.7).

## 7.5: Inheritance extraction

### Subtasks
- [x] `(class_definition superclasses: (argument_list ...))` — walk children for identifiers and dotted_names
- [x] Each base produces an Edge { from: derived class name, to: base name, kind: Inherits }
- [x] Multiple inheritance handled: `class D(A, B, C)` → 3 edges
- [x] Qualified bases: `class D(module.Base)` → 1 edge with to='module.Base'
- [x] No base (`class C:`) → 0 inheritance edges
- [x] Keyword arguments in superclasses (`class C(metaclass=Meta)`) — these appear as `keyword_argument` nodes inside the argument_list and are ignored (not bases)
- [x] Tests for each form including ABC inheritance

## 7.6: testdata/python + corpus tests + real-world dogfood + watch-mode reindex regression

### Subtasks
- [x] `testdata/python/` package:
  - `__init__.py` — package init, public exports
  - `app.py` — main module with calls into other modules, includes `from __future__ import annotations`
  - `models.py` — classes with `__init__`/`__str__`/`__repr__`, dataclasses, ABC + @abstractmethod, single and multiple inheritance
  - `handlers.py` — decorated functions (@property, @staticmethod, @classmethod), closures, async def free functions AND async def methods inside a class
  - `utils.py` — free functions, type aliases via `Type = ...`, generators (yield)
  - `stubs.pyi` — representative stub-only declarations: `def foo(x: int) -> str: ...`, a class stub with method stubs, a stubbed protocol with abstract methods (covers the .pyi extension dispatch path)
  - `MANIFEST.md` — expected symbols and edges; explicit row for `stubs.pyi` showing `.pyi` symbols are extracted identically to `.py`
- [x] Corpus tests covering every form + edge cases (empty file, comments-only, syntax error file → parser skips error nodes gracefully, deeply nested classes, method same name as free function in another file, *args/**kwargs, async fn, generator, property decorator, `.pyi` stub file)
- [x] `parse-test testdata/python` matches MANIFEST
- [x] **Watch-mode reindex regression** — new test in `crates/codegraph-tools/tests/watch_python_reindex.rs` (covers BOTH inheritance-edge and call-edge pruning):
  - Spawn watch on a temp directory containing `models.py` with `class Alpha:`, `class Beta(Alpha):`, and `class Delta: def use_beta(self): Beta()`
  - Modify `models.py`: remove `Beta` and `Delta.use_beta`, add `class Gamma(Alpha):`
  - After debounce, assert `get_file_symbols` shows `Alpha` + `Gamma`, no `Beta` or `Delta.use_beta`
  - Assert `get_class_hierarchy` for `Alpha` shows `Gamma` as derived (not `Beta`)
  - Assert no dangling `Inherits` edge from `Beta` and no dangling `Calls` edge from `Delta.use_beta` to `Beta` — exercises both edge kinds through `Graph::prune_dangling_edges`
- [x] **Real-world dogfood:** clone `github.com/psf/requests` (well-known mid-sized Python library) to `/tmp/requests` at a pinned tag (v2.32.3), run `parse-test /tmp/requests/src/requests`, expect 0 crashes, 0 warnings, approximate symbol count between 400 and 1000 — record the actual count in `testdata/python/requests-baseline.txt` (one line: `symbols: N`); follow-up test asserts the recorded count stays within ±10% as a regression gate

## 7.7: Register parser, four-language integration, snapshots, documentation

### Subtasks
- [ ] Register PythonParser in `crates/code-graph-mcp/src/main.rs` using the shipped Box+context pattern (mirrors C++/Rust/Go blocks):
  ```rust
  .register(Box::new(
      codegraph_lang_python::PythonParser::new()
          .context("initialize Python language plugin")?,
  ))
  .context("register Python language plugin")?;
  ```
- [ ] **Four-language fixture:** extend `testdata/mixed/` (created in 5.6 with `foo.cpp` + `foo.rs`, extended in 6.6 with `foo.go`) by adding `foo.py` defining `def helper(): pass`; the four files share the anchor name `helper`
- [ ] Full four-language integration test: `analyze_codebase` on `testdata/mixed/` indexes all four; `search_symbols` for `helper` without language filter returns four entries; with each `language=` filter returns only that language's match
- [ ] **4-way cross-language collision regression:** `init` exists in C++ (`void init()`), Go (`func init()`), and Python (`def init(): pass` at module scope). Assert `search_symbols` returns three entries; assert `get_callers` against any one does NOT return the others' callers — verifying the `(Language, name)`-keyed `SymbolIndex` isolation from Phase 3 at `crates/codegraph-lang/src/lib.rs:116` extends cleanly to four languages
- [ ] Wire-format snapshot tests extended with Python-specific responses (`cargo insta accept` on the new fixtures)
- [ ] README + CLAUDE.md final update:
  - All four languages listed in the supported-languages table (C++ `.cpp/.cc/.h/.hpp`, Rust `.rs`, Go `.go`, Python `.py/.pyi`)
  - Update `crates/code-graph-mcp/src/main.rs` module-level doc comment (currently "C++ only — Phases 5/6/7 add Rust, Go, Python") to reflect all four languages are now live
  - Python-specific limitations:
    - Call resolution especially noisy due to dynamic typing
    - Decorators are transparent for definition extraction but `@abstractmethod` is not flagged as a separate kind
    - Type hints not extracted as edges
    - Conditional imports (`if TYPE_CHECKING: import ...`) NOT extracted because tree-sitter sees them as block-level statements inside an `if_statement`, not as top-level `import_statement` nodes

## 7.8: Structural verification + plan close-out

### Subtasks
- [ ] `make release` (host-target only) succeeds and produces a four-language-capable binary
- [ ] `cargo fmt --check` clean
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo test --workspace` green — every Phase 1-7 test passes
- [ ] `cargo audit` clean (no new advisories)
- [ ] No new `unsafe` blocks; no `#[allow(clippy::...)]` suppressions
- [ ] **Plan close-out (in this exact order — debrief MUST be written while the plan is still in `Plans/Active/`):**
  1. Write Phase 7 debrief to `notes/07-Python-Parser.md` — captures the actual implementation experience, decisions made, deviations, lessons learned, and the SharedDaemon handoff context
  2. Update `Plans/Active/RustRewrite/README.md`:
     - `phases[7].status` → `complete`
     - top-level `status:` → `complete`
  3. `git mv .plans/Plans/Active/RustRewrite .plans/Plans/Complete/RustRewrite`
  4. Commit with subject `[RustRewrite/Phase 7] plan close-out — RustRewrite complete` and a body listing all four newly-supported languages and pointing at SharedDaemon as the next planned work
- [ ] **The rewrite is complete.** SharedDaemon plan (`Designs/SharedDaemon/`, status `draft`) is unblocked; next step is `/planner:plan` against that design

## Acceptance Criteria
- [ ] PythonParser implements LanguagePlugin (object-safety + id() test passes)
- [ ] All extraction patterns working: definitions with decorator transparency AND async methods inside classes, both call patterns (note `call` not `call_expression`), all import forms with module-not-name-dependency rule (incl. `__future__`), multiple/qualified inheritance
- [ ] `.pyi` stub files indexed identically to `.py` (covered by `stubs.pyi` fixture)
- [ ] testdata/python passes; real-world Python project (requests@v2.32.3) parses cleanly within recorded baseline
- [ ] Four-language mixed indexing works
- [ ] 4-way cross-language collision regression passes (C++/Go/Python `init` stay isolated)
- [ ] Watch-mode reindex regression passes — exercises both inheritance-edge AND call-edge pruning through `Graph::prune_dangling_edges`
- [ ] All Phase 1-7 tests pass; lint, format, audit gates clean
- [ ] Documentation lists all four languages; conditional-imports limitation documented
- [ ] Phase 7 debrief written
- [ ] Plan moved from `Plans/Active/` to `Plans/Complete/` via `git mv`; plan README status flipped to `complete`
- [ ] SharedDaemon plan is unblocked and ready for `/planner:plan`
