# Testdata Python Project — Expected Parse Results

The Python parser (`codegraph-lang-python`) must produce these exact
counts when each `.py` and `.pyi` file under `testdata/python/` is parsed
in isolation and the results are aggregated. The corpus test
`crates/codegraph-lang-python/tests/corpus.rs` asserts every total in
this file; if you change a fixture, update both.

## Totals

### Symbols by Kind (TOTAL = 45)

| Kind     | Count |
|----------|------:|
| Function |    13 |
| Method   |    18 |
| Class    |    14 |

### Edges by Kind (TOTAL = 27)

| Kind     | Count |
|----------|------:|
| Calls    |    14 |
| Includes |     9 |
| Inherits |     4 |

`Inherits = 4` covers single (`Beta(Alpha)`), multiple
(`Gamma(Alpha, Mixin)` → 2 edges), and qualified (`Delta(abc.ABC)`)
inheritance forms. The dataclass-style `WithSlots` (no parens, no
bases) and `Mixin`/`Alpha` (no bases) contribute zero `Inherits` edges
— `class C:` has no `superclasses` field at all.

## Per-file Breakdown

### `__init__.py`

| Name | Kind | Line | Namespace | Parent |
|------|------|-----:|-----------|--------|

- 0 symbols (the package's public surface is built from re-exports —
  re-exports are `import` statements, not `def`/`class`)
- 2 edges:
  - 2 `Includes`: `.models`, `.handlers` (relative imports preserved
    as written; default `resolve_include` is a no-op for the leading
    dot)
- `__all__ = [...]` is a module-level assignment (not a `def` or
  `class`), so it produces no symbol record.

### `app.py`

| Name | Kind     | Line | Namespace | Parent |
|------|----------|-----:|-----------|--------|
| run  | Function |   15 |           |        |
| main | Function |   22 |           |        |

- 2 symbols (2 Functions; namespace stays empty for Python — the
  module concept is captured in the file path itself)
- 8 edges:
  - 4 `Includes`: `__future__` (the dunder module is treated like any
    other module), `.utils`, `.handlers`, `.models` (relative imports
    preserved verbatim)
  - 4 `Calls`: `run -> Alpha` (constructor call — `MyClass()` parses
    as `(call function: (identifier))` and produces `to=MyClass`),
    `run -> handle`, `run -> kw` (attribute call — `utils.kw(...)`
    matches the `attribute attribute: (identifier)` query),
    `main -> run`

### `models.py`

| Name              | Kind   | Line | Namespace | Parent    |
|-------------------|--------|-----:|-----------|-----------|
| Mixin             | Class  |   12 |           |           |
| Mixin::mixed      | Method |   13 |           | Mixin     |
| Alpha             | Class  |   17 |           |           |
| Alpha::__init__   | Method |   18 |           | Alpha     |
| Alpha::__str__    | Method |   21 |           | Alpha     |
| Alpha::__repr__   | Method |   24 |           | Alpha     |
| Beta              | Class  |   28 |           |           |
| Beta::__init__    | Method |   29 |           | Beta      |
| Gamma             | Class  |   34 |           |           |
| Gamma::combine    | Method |   37 |           | Gamma     |
| Delta             | Class  |   41 |           |           |
| Delta::required   | Method |   45 |           | Delta     |
| WithSlots         | Class  |   49 |           |           |
| WithSlots::__init__ | Method | 55 |           | WithSlots |

- 14 symbols (6 Classes, 8 Methods)
- 10 edges:
  - 1 `Includes`: `abc`
  - 5 `Calls`: `Alpha::__repr__ -> repr` (builtin),
    `Beta::__init__ -> super` (super() call),
    `Beta::__init__ -> __init__` (chained from `super().__init__(...)`),
    `Gamma::combine -> mixed` (attribute call),
    `Gamma::combine -> str` (builtin)
  - 4 `Inherits`: `Beta -> Alpha`, `Gamma -> Alpha`, `Gamma -> Mixin`
    (multiple inheritance), `Delta -> abc.ABC` (qualified base — the
    dotted text is preserved verbatim as the `to` field)
- The `@abc.abstractmethod` decorator on `Delta::required` is
  transparent for definition extraction; `required` is an ordinary
  Method symbol with no separate flag for abstractness.
- `__slots__ = ("x",)` is a class-level assignment, not a `def` or
  `class` — it produces no symbol and no edge.

### `handlers.py`

| Name                 | Kind     | Line | Namespace | Parent  |
|----------------------|----------|-----:|-----------|---------|
| Service              | Class    |   11 |           |         |
| Service::__init__    | Method   |   12 |           | Service |
| Service::value       | Method   |   16 |           | Service |
| Service::factory     | Method   |   20 |           | Service |
| Service::from_name   | Method   |   24 |           | Service |
| Service::handle      | Method   |   27 |           | Service |
| fetch                | Function |   32 |           |         |
| make_handler         | Function |   38 |           |         |
| inner                | Function |   44 |           |         |
| handle               | Function |   50 |           |         |

- 10 symbols (1 Class, 5 Methods, 4 Functions)
- 6 edges:
  - 1 `Includes`: `asyncio`
  - 5 `Calls`: `Service::factory -> Service` (constructor call to the
    enclosing class — produces `to=Service`),
    `Service::from_name -> cls` (calling the conventional `cls`
    parameter — captured as a direct call to the parameter name),
    `Service::handle -> sleep` (attribute call on `asyncio.sleep`),
    `fetch -> sleep` (same — module-level free async function),
    `handle -> factory` (attribute call on `Service.factory()`)
- `@property`, `@staticmethod`, `@classmethod`, and `async def` are
  all transparent for definition extraction — every Service method is
  Kind=Method, parent=Service, regardless of decorator or async-ness.
- The closure `inner` inside `make_handler` is extracted as a separate
  Function symbol (parent empty — Python nested functions don't
  record an enclosing-function parent, only an enclosing-class one).
- `Service::handle` returning `payload` does NOT produce a call edge
  — bare references that aren't applied with `()` are not calls.

### `utils.py`

| Name | Kind     | Line | Namespace | Parent |
|------|----------|-----:|-----------|--------|
| gen  | Function |   12 |           |        |
| kw   | Function |   18 |           |        |
| add  | Function |   24 |           |        |

- 3 symbols (3 Functions; the `Result = Dict[str, int]` type alias is
  a module-level assignment and produces no symbol)
- 1 edge:
  - 1 `Includes`: `typing`
- `gen` (a generator with `yield`) is extracted as an ordinary
  Function — there is no separate generator kind. The signature
  captures `def gen():` (truncated at the body opener).
- `kw(*args, **kwargs)` preserves the variadic markers in the
  captured signature text.

### `stubs.pyi`

| Name              | Kind     | Line | Namespace | Parent   |
|-------------------|----------|-----:|-----------|----------|
| foo               | Function |    5 |           |          |
| Stub              | Class    |    7 |           |          |
| Stub::m           | Method   |    8 |           | Stub     |
| Stub::n           | Method   |    9 |           | Stub     |
| Protocol          | Class    |   11 |           |          |
| Protocol::required| Method   |   12 |           | Protocol |

- 6 symbols (1 Function, 2 Classes, 3 Methods) — **identical extraction
  behavior to `.py`**: `def f() -> T: ...` (a stub with `...` body)
  parses as `function_definition` whose body is a single
  `expression_statement` containing `...`. The same query path applies.
- 0 edges (no imports, no calls, no inheritance — pure stubs)
- This file is the load-bearing contract that `.pyi` files dispatch to
  the same parser as `.py` files.

### `edge_cases/empty.py`

- 0 bytes — completely empty file
- 0 symbols, 0 edges
- Parser must not panic on a zero-byte input

### `edge_cases/comments_only.py`

- Only `#`-prefixed lines, no declarations
- 0 symbols, 0 edges

### `edge_cases/broken.py`

| Name | Kind     | Line | Namespace | Parent |
|------|----------|-----:|-----------|--------|
| foo  | Function |    4 |           |        |
| good | Function |    7 |           |        |

- 2 symbols (2 Functions)
- 0 edges
- Despite the malformed `def foo(:` signature, tree-sitter's error
  recovery still produces a `function_definition` node for `foo` (with
  ERROR children inside the parameter list), and the parser emits a
  Symbol for it. The well-formed `good` function below the error site
  parses cleanly. **Critical anti-regression:** the parser must not
  panic or skip the rest of the file when an ERROR node appears
  mid-stream.

### `edge_cases/nested.py`

| Name              | Kind   | Line | Namespace | Parent  |
|-------------------|--------|-----:|-----------|---------|
| Outer             | Class  |    5 |           |         |
| Outer::Mid        | Class  |    6 |           | Outer   |
| Mid::Inner        | Class  |    7 |           | Mid     |
| Inner::Deepest    | Class  |    8 |           | Inner   |
| Deepest::leaf     | Method |    9 |           | Deepest |

- 5 symbols (4 Classes, 1 Method)
- 0 edges
- **Parent contract:** each nested class records the *immediate*
  enclosing class as its parent (bare name) — NOT a dotted path
  (`Outer.Mid.Inner.Deepest`). The leaf method records `Deepest` as
  its parent for the same reason. This matches the C++/Rust nested-
  class convention (`find_enclosing_class` walks one level up).

### `edge_cases/collide.py`

| Name        | Kind     | Line | Namespace | Parent |
|-------------|----------|-----:|-----------|--------|
| Adder       | Class    |    4 |           |        |
| Adder::add  | Method   |    5 |           | Adder  |
| add         | Function |    9 |           |        |

- 3 symbols (1 Class, 1 Method, 1 Function)
- 0 edges
- The method `Adder::add` and the free function `add` coexist with
  distinct symbol IDs (`<path>:Adder::add` vs `<path>:add`). Anti-
  regression for the SymbolIndex's parent-disambiguated keying.

## Key Validation Points

- **`from __future__ import annotations`** — `__future__` is treated
  like any other module; one `Includes` edge with `to="__future__"`.
- **Relative imports preserved verbatim.** `from . import utils` →
  `to=".utils"`; `from .models import Alpha` → `to=".models"`. The
  default `resolve_include` returns None for these (no FileIndex
  basename match), so the wire format records the dotted path with
  the leading dot intact.
- **`from X import Y` records the module, not the name.** `from .models
  import Alpha` produces `to=".models"`, not `to="Alpha"`. The agent
  reads "this file depends on the .models module" — which is the
  semantic relationship, not the imported name.
- **Decorators are transparent.** `@property`, `@staticmethod`,
  `@classmethod`, `@abc.abstractmethod` all wrap a `function_definition`
  in a `decorated_definition` node, but the queries match the inner
  node directly — every decorated def becomes a Method (or Function)
  with no special flag.
- **`async def` parses as `function_definition`** in tree-sitter-python
  0.25 — there is no separate `async_function_definition` node. The
  same query path covers both sync and async forms.
- **`.pyi` stub files extract identically to `.py`.** Six symbols out
  of `stubs.pyi`, mirroring what `def foo(x: int) -> str: pass`,
  `class Stub: def m(self): pass`, etc. would produce in a `.py` file.
- **Multiple inheritance.** `class Gamma(Alpha, Mixin)` produces 2
  `Inherits` edges; `class Delta(abc.ABC)` produces 1 `Inherits` edge
  with `to="abc.ABC"` (the dotted text, not the resolved class).
- **Dataclass-style `__slots__`** is a module/class-level assignment,
  not a `def`/`class` — produces no symbol and no edge.
- **Empty file (0 bytes).** Parser produces zero symbols, zero edges,
  no panic.
- **Comments-only file.** Same — zero symbols, zero edges.
- **Syntax error file.** `def foo(:` produces ERROR nodes in the
  parameter list, but `function_definition` for `foo` is still
  recovered; subsequent definitions parse normally. Parser does not
  panic and does not abort the file.
- **Deeply nested classes (4 levels).** Each class records its
  immediate enclosing class as parent (bare name), not a dotted path.
- **Method `add` and free function `add` coexist.** The parent-
  disambiguated symbol IDs prevent collision.
- **Generator function (`yield`).** Extracted as Function — no special
  generator kind.
- **Variadic `*args/**kwargs`.** Preserved in the captured signature
  text (truncate_signature only drops from the body opener `:` onwards).
- **Module-level fallback for calls.** The `__init__.py` package is
  pure imports — no calls; the closest `from`-fallback case is the
  closure inside `make_handler` (whose body call site falls back to
  `make_handler` as the enclosing function). No bare-file-path
  fallbacks are exercised by this corpus because every `.py` file's
  module-level scope contains only declarations and imports, not
  function calls.

## Conditional Imports — NOT Extracted

`if TYPE_CHECKING: import expensive_module` is intentionally NOT
captured as an `Includes` edge — the import lives inside an
`if_statement > block` rather than at module top level. The
`extract_imports` guard restricts matches to module-top-level
`import_statement` / `import_from_statement` / `future_import_statement`
nodes; conditional imports fall outside that scope. None of the
fixtures in this corpus exercise the conditional-import path; the
unit tests in `crates/codegraph-lang-python/src/lib.rs` cover that
behavior directly.
