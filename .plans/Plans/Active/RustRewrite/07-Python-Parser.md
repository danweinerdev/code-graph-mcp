---
title: "Python Language Parser"
type: phase
plan: RustRewrite
phase: 7
status: planned
created: 2026-04-28
updated: 2026-04-28
deliverable: "codegraph-lang-python crate parsing .py and .pyi files with class/method/decorator handling, both import forms, multi-base inheritance, and the Python-specific call node type; registered in the main binary; testdata/python/ + real-world validation; the four-language MCP is complete"
tasks:
  - id: "7.1"
    title: "codegraph-lang-python crate scaffold + queries.rs"
    status: planned
    verification: "PythonParser::new() compiles all queries against tree-sitter-python 0.25 without error; Extensions() returns [.py, .pyi]; query categories: definitions (function_definition, class_definition), calls (call — Python's tree-sitter uses 'call' not 'call_expression'), imports (import_statement with dotted_name, import_from_statement with module_name), inheritance (class_definition with superclasses argument_list); helpers find_enclosing_class, extract_module_path, walk_decorator_unwrap unit-tested; compile-time interface check"
  - id: "7.2"
    title: "Definition extraction with method-vs-function context and decorator transparency"
    status: planned
    depends_on: ["7.1"]
    verification: "function_definition without enclosing class_definition → Kind=Function; function_definition with enclosing class_definition → Kind=Method, parent=class name; class_definition → Kind=Class; @decorator wrapping a function/class produces decorated_definition wrapping the inner node — queries match the inner node directly because tree-sitter queries search the entire tree, so decorator presence does not block extraction (verified by a fixture with `@property def x(self)` producing a Method symbol); __init__, __str__, __repr__ are ordinary methods (no special handling); nested classes (class inside class) handled — outer class is the parent for the inner class; namespace stays empty for Python (Python's module concept is captured in the file path itself, not in a namespace tag); tests cover each case"
  - id: "7.3"
    title: "Call site extraction"
    status: planned
    depends_on: ["7.1"]
    verification: "Python's tree-sitter uses node kind 'call' (NOT 'call_expression'); call > function: identifier → direct call (foo()); call > function: attribute > attribute: identifier → method/attribute call (obj.method()); chained calls (a.b().c()) produce 2 edges; constructor calls (MyClass()) treated as direct calls (To=MyClass) — these naturally produce edges that look like 'calls to a class', which the agent can interpret as construction; super() calls captured; built-in calls (print, len) captured; calls inside list comprehensions, lambdas, default arguments — all captured with the enclosing function as From; tests for each pattern"
  - id: "7.4"
    title: "Import extraction with module path semantics"
    status: planned
    depends_on: ["7.1"]
    verification: "import foo → 1 edge with To='foo'; import foo.bar → 1 edge with To='foo.bar'; import foo as f → 1 edge with To='foo' (alias dropped, path preserved — same rule as Go); from foo import bar → 1 edge with To='foo' (the module path, NOT 'bar' — the imported symbol name is not the dependency); from foo.bar import baz → 1 edge with To='foo.bar'; from . import utils → 1 edge with To='.utils' (relative import preserved); from typing import List → 1 edge with To='typing'; each edge has Kind=Includes; tests cover every form including the relative form and the from-form-vs-module-path distinction"
  - id: "7.5"
    title: "Inheritance extraction"
    status: planned
    depends_on: ["7.1"]
    verification: "class D(B) → 1 inherits edge from D to B; class D(A, B) → 2 inherits edges (multiple inheritance, common in Python); class D(module.Base) → 1 inherits edge from D to 'module.Base' (qualified base preserved); class C: (no parens) → 0 inherits edges; class C(metaclass=Meta) → metaclass keyword arg ignored (not a base); ABC inheritance (`class C(ABC)`) treated like any other base; tests for each form"
  - id: "7.6"
    title: "testdata/python + corpus tests + real-world validation + register + structural verification"
    status: planned
    depends_on: ["7.2", "7.3", "7.4", "7.5"]
    verification: "testdata/python/ project covers: classes with __init__/__str__/__repr__, @property/@staticmethod/@classmethod decorators, single + multiple + qualified inheritance, ABC with @abstractmethod (extracted as ordinary methods), async def, generators (yield), context managers, type hints, all import forms; MANIFEST.md documents expected symbols and edges; corpus tests cover every definition form, every call pattern, every import form, every inheritance form, and edge cases (empty file, comments-only file, syntax error file, deeply nested classes, method-with-same-name-as-free-function, *args/**kwargs in signature, generator function, property decorator); parse-test testdata/python matches MANIFEST; real-world dogfood: clone a small open-source Python library (e.g., 'requests', 'click', 'attrs', or similar) to /tmp; parse-test produces 0 crashes, 0 warnings, sensible output (spot-check 20+ symbols); main.rs registers PythonParser; full mixed-language analyze (.cpp + .rs + .go + .py) indexes all four — the original cross-language collision test now exercises 4-way isolation; README + CLAUDE.md updated to list all four supported languages with .py/.pyi extensions and Python-specific limitations (call resolution dynamic-typing-noisy, decorators transparent for definition extraction but @abstractmethod not flagged, type hints not extracted as edges); cargo fmt clean; cargo clippy clean; cargo test --workspace green — the rewrite is complete"
---

# Phase 7: Python Language Parser

## Overview

Add Python language support — priority 4 per the user's ordering, completing the four-language MCP. Python's grammar is simple but has two distinctive challenges: (1) call nodes are `call`, not `call_expression`; (2) imports come in two forms (`import` and `from ... import`), and the dependency edge points at the *module*, not the imported symbol. This phase replaces the original `Plans/PythonParser/` (status: superseded).

After this phase ships green, `code-graph-mcp` is feature-complete for the user's stated four-language scope.

## 7.1: codegraph-lang-python crate scaffold + queries.rs

### Subtasks
- [ ] Crate `crates/codegraph-lang-python` with `tree-sitter-python = "0.25"`
- [ ] `PythonParser` with cached Query objects (definitions, calls, imports, inheritance)
- [ ] `Extensions()` returns `[".py", ".pyi"]`
- [ ] `queries.rs`:
  - `DEFINITION_QUERIES`: function_definition, class_definition
  - `CALL_QUERIES`: `(call function: (identifier))` and `(call function: (attribute attribute: (identifier)))` — note `call`, not `call_expression`
  - `IMPORT_QUERIES`: import_statement and import_from_statement
  - `INHERITANCE_QUERIES`: `(class_definition superclasses: (argument_list (identifier) @base) ...)` plus dotted_name variant for qualified bases
- [ ] Helpers in `helpers.rs`:
  - `find_enclosing_class(node)` — walks up to class_definition
  - `extract_module_path(import_node, content)` — produces the dotted module path string
- [ ] Compile-time interface check

## 7.2: Definition extraction

### Subtasks
- [ ] `function_definition` with no enclosing class_definition → Kind=Function, no parent
- [ ] `function_definition` enclosed in class_definition → Kind=Method, parent = class name
- [ ] `class_definition` → Kind=Class
- [ ] Decorated definitions: tree-sitter wraps them in `decorated_definition` but the queries match the inner `function_definition` / `class_definition` nodes directly (this is how tree-sitter queries work — they search the whole tree)
- [ ] Nested classes: a class defined inside another class produces a Class symbol whose parent is the outer class
- [ ] `__init__`, `__str__`, `__repr__`, `__call__` are ordinary methods — no special handling
- [ ] Namespace stays empty for Python; the file path encodes the module location
- [ ] Tests:
  - `def foo(): ...` → Function
  - `class C: def m(self): ...` → C is Class, m is Method with parent=C
  - `@property def x(self): ...` → Method (decorator transparent)
  - `@staticmethod def s(): ...` → Method
  - `class A: class B: pass` → both Classes; B's parent=A

## 7.3: Call site extraction

### Subtasks
- [ ] `extract_calls`:
  - `(call function: (identifier))` → direct call (To = identifier text)
  - `(call function: (attribute attribute: (identifier)))` → method/attribute call (To = the attribute identifier)
- [ ] Enclosing function: walk up to `function_definition`; if found, From = `path:funcName` or `path:Parent::funcName`
- [ ] Constructor calls: `MyClass()` is just `(call function: (identifier))` matching the direct-call query — produces an edge To=MyClass; this naturally captures construction (the agent can interpret it)
- [ ] super() calls captured as direct calls (To=super)
- [ ] Calls inside list/dict/set comprehensions, lambdas, default arguments all produce edges with the enclosing top-level function as From
- [ ] Tests for each pattern

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
- [ ] Tests for every form

### Notes
The "from-form points at the module, not the imported name" rule comes from the original `Plans/PythonParser/01-Parser-Core.md` task 1.6 notes and is preserved verbatim. An agent searching for "what does this file depend on?" wants modules, not names — and the imported name is already in scope after the import statement.

## 7.5: Inheritance extraction

### Subtasks
- [ ] `(class_definition superclasses: (argument_list ...))` — walk children for identifiers and dotted_names
- [ ] Each base produces an Edge { from: derived class name, to: base name, kind: Inherits }
- [ ] Multiple inheritance handled: `class D(A, B, C)` → 3 edges
- [ ] Qualified bases: `class D(module.Base)` → 1 edge with to='module.Base'
- [ ] No base (`class C:`) → 0 inheritance edges
- [ ] Keyword arguments in superclasses (`class C(metaclass=Meta)`) — these appear as `keyword_argument` nodes inside the argument_list and are ignored (not bases)
- [ ] Tests for each form including ABC inheritance

## 7.6: testdata/python + corpus tests + real-world validation + register + structural verification

### Subtasks
- [ ] `testdata/python/` package:
  - `__init__.py` — package init, public exports
  - `app.py` — main module with calls into other modules
  - `models.py` — classes with `__init__`/`__str__`/`__repr__`, dataclasses, ABC + @abstractmethod, single and multiple inheritance
  - `handlers.py` — decorated functions (@property, @staticmethod, @classmethod), closures, async def
  - `utils.py` — free functions, type aliases via `Type = ...`, generators (yield)
  - `MANIFEST.md` — expected symbols and edges
- [ ] Corpus tests covering every form + edge cases (empty file, comments-only, syntax error file → parser skips error nodes gracefully, deeply nested classes, method same name as free function in another file, *args/**kwargs, async fn, generator, property decorator)
- [ ] `parse-test testdata/python` matches MANIFEST
- [ ] Real-world dogfood: clone `github.com/psf/requests` (a well-known mid-sized Python library) to `/tmp/requests` at a pinned tag (e.g. v2.32.3), run `parse-test /tmp/requests/src/requests`, expect 0 crashes, 0 warnings, and an approximate symbol count between 400 and 1000 — record the actual numbers as the regression baseline in a committed fixture file
- [ ] Register PythonParser in `main.rs`
- [ ] Full four-language integration test: directory with .cpp + .rs + .go + .py — analyze indexes all; search without language filter returns from all four; with each language filter returns only that language; cross-language collision regression now exercises 4-way isolation (a name shared across all four languages stays separated)
- [ ] Wire-format snapshot tests extended with Python responses
- [ ] README + CLAUDE.md final update:
  - All four languages listed in supported-languages table
  - Python-specific limitations: call resolution especially noisy due to dynamic typing; decorators are transparent for definition extraction but @abstractmethod is not flagged as a separate kind; type hints not extracted as edges
- [ ] `cargo fmt --check` clean
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo test --workspace` green — every Phase 1-7 test passes
- [ ] **The rewrite is complete.**

## Acceptance Criteria
- [ ] PythonParser implements LanguagePlugin
- [ ] All extraction patterns working: definitions with decorator transparency, both call patterns (note `call` not `call_expression`), all import forms with module-not-name-dependency rule, multiple/qualified inheritance
- [ ] testdata/python passes; real-world Python project parses cleanly
- [ ] Four-language mixed indexing works
- [ ] 4-way cross-language collision regression passes
- [ ] All Phase 1-7 tests green
- [ ] Lint and format gates clean
- [ ] Documentation lists all four languages
