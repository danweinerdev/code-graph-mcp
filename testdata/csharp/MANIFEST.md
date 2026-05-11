# Testdata C# Project — Expected Parse Results

The C# parser (`code-graph-lang-csharp`) must produce these exact counts
when each `.cs` file under `testdata/csharp/` is parsed in isolation and
the results are aggregated. The corpus test
`crates/code-graph-lang-csharp/tests/corpus.rs` asserts every total in
this file; if you change a fixture, update both.

## Totals

### Symbols by Kind (TOTAL = 41)

| Kind      | Count |
|-----------|------:|
| Function  |     1 |
| Method    |    19 |
| Class     |    18 |
| Interface |     3 |

`Function = 1` is the default interface method `IGreeter::Greet` —
per Decision 11's C# follow-up, default interface methods extract as
`Function` (no parent), NOT `Method`, matching Rust's trait-default-
method rule. Every other named method-shaped declaration in this
corpus is a `Method` with a class/struct/record parent. There are no
`Struct` or `Enum` symbols in this corpus (the edge cases don't need
them — struct/enum extraction is covered by the inline tests in
`src/lib.rs`).

### Edges by Kind (TOTAL = 22)

| Kind     | Count |
|----------|------:|
| Calls    |    10 |
| Includes |     7 |
| Inherits |     5 |

`Inherits = 5` covers single (`Beta : Alpha`), multiple-base
(`Gamma : Alpha, IMixin` → 2 edges), interface implementation
(`Service : IService`), and generic (`Box<T> : BoxBase<T>` → 1 edge
with `from = "Box<T>", to = "BoxBase<T>"`) inheritance forms. Per
Decision 2, both class extension and interface implementation
produce the same `Inherits` edge kind. Per Decision 9, generic
parameter text is preserved verbatim in both `from` and `to`.

## Per-file Breakdown

### `Program.cs`

| Name | Kind   | Line | Namespace | Parent  |
|------|--------|-----:|-----------|---------|
| Program | Class  |    9 | App       |         |
| Main    | Method |   11 | App       | Program |
| Run     | Method |   18 | App       | Program |

- 3 symbols (1 Class, 2 Methods)
- 7 edges:
  - 3 `Includes`: `System`, `System.Collections.Generic`, `Models`
  - 4 `Calls`: `Program::Main -> Service` (constructor call —
    `new Service()` records the type name as the target), `Program::Main
    -> Handle`, `Program::Main -> Run`, `Program::Run -> WriteLine`

### `Models.cs`

| Name        | Kind      | Line | Namespace | Parent  |
|-------------|-----------|-----:|-----------|---------|
| IMixin      | Interface |    9 | Models    |         |
| IService    | Interface |   14 | Models    |         |
| Alpha       | Class     |   19 | Models    |         |
| Alpha::Alpha| Method    |   21 | Models    | Alpha   |
| Alpha::M    | Method    |   22 | Models    | Alpha   |
| Beta        | Class     |   25 | Models    |         |
| Beta::Beta  | Method    |   27 | Models    | Beta    |
| Beta::M     | Method    |   28 | Models    | Beta    |
| Gamma       | Class     |   31 | Models    |         |
| Gamma::Mix  | Method    |   33 | Models    | Gamma   |
| Service     | Class     |   36 | Models    |         |
| Service::Handle | Method |  38 | Models    | Service |
| Box         | Class     |   41 | Models    |         |
| BoxBase     | Class     |   46 | Models    |         |

- 14 symbols (2 Interfaces, 6 Classes, 6 Methods)
- 9 edges:
  - 1 `Includes`: `System`
  - 3 `Calls`: `Alpha::M -> WriteLine`, `Beta::M -> M` (the
    `base.M()` call records the method name as target), `Gamma::Mix
    -> M` (call to inherited method)
  - 5 `Inherits`:
    - `Beta -> Alpha` (single inheritance)
    - `Gamma -> Alpha`, `Gamma -> IMixin` (multiple-base; Decision 2)
    - `Service -> IService` (interface implementation; Decision 2)
    - `Box<T> -> BoxBase<T>` (generic; Decision 9 preserves the
      generic parameter text verbatim in both `from` and `to`)
- `Alpha` and `BoxBase` are top-level classes with no bases — they
  contribute zero `Inherits` edges.
- The `Inherits` edge from `Box<T>` uses the bare-class-name-with-
  generics convention required by the `get_class_hierarchy` walker
  in `crates/code-graph-graph/src/algorithms.rs`.

### `Handlers.cs`

| Name        | Kind     | Line | Namespace | Parent    |
|-------------|----------|-----:|-----------|-----------|
| IGreeter    | Interface|   10 | Handlers  |           |
| Greet       | Function |   17 | Handlers  |           |
| User        | Class    |   20 | Handlers  |           |
| User::Display | Method |   22 | Handlers  | User      |
| StringExt   | Class    |   25 | Handlers  |           |
| StringExt::CountWords | Method | 29 | Handlers | StringExt |
| Hub         | Class    |   35 | Handlers  |           |
| Hub::Dispatch | Method |   37 | Handlers  | Hub       |
| Hub::Process | Method  |   45 | Handlers  | Hub       |

- 9 symbols (1 Interface, 1 Function, 3 Classes, 4 Methods)
- 6 edges:
  - 3 `Includes`: `System.Math` (the `using static` modifier is
    dropped from the path per Decision 7), `System.Collections.
    Generic.List<string>` (the `using A = X.Y` alias is dropped;
    the target path is preserved), `System.Linq` (the `global
    using` modifier is dropped)
  - 3 `Calls`: `Greet -> Required` (default-interface-method body
    calls the abstract sibling), `StringExt::CountWords -> Abs`
    (extension method body calls a using-static target),
    `Hub::Dispatch -> Process` (intra-class static call)
- `IGreeter::Required` (abstract — no body) produces **no** symbol
  (forward-declaration rule, mirroring Rust and the four shipped
  plugins).
- `IGreeter::Greet` (default interface method, has body) extracts
  as `Function` per Decision 11. Its parent is empty — NOT
  `IGreeter`.
- `User` (a `record User(string Name)`) extracts as `Class`
  (Decision 6 analog for C#). The record's `Display` method
  extracts as `Method` with parent `User`.
- `StringExt::CountWords` is an extension method (`this string s`
  parameter). Decision 5: the syntactic parent (`StringExt`) is
  used, NOT the extended type (`string`).
- Records' auto-generated members (the implicit `Name` property,
  `Equals`, `GetHashCode`, `ToString`, etc.) are NOT extracted —
  they don't appear in source.

### `edge_cases/empty.cs`

- 0 bytes — completely empty file
- 0 symbols, 0 edges
- Parser must not panic on a zero-byte input.

### `edge_cases/comments_only.cs`

- Single-line `//`, multi-line `/* ... */`, and XML doc `///` forms
- 0 symbols, 0 edges

### `edge_cases/broken.cs`

| Name      | Kind   | Line | Namespace | Parent   |
|-----------|--------|-----:|-----------|----------|
| Foo       | Class  |    6 | Bad       |          |
| Good      | Method |   12 | Bad       | Foo      |
| AlsoGood  | Class  |   15 | Bad       |          |
| Run       | Method |   17 | Bad       | AlsoGood |

- 4 symbols (2 Classes, 2 Methods)
- 0 edges
- The malformed method `Bar(` (opening paren immediately followed
  by `{` instead of a parameter list) produces ERROR nodes in
  tree-sitter's parse. The recovered count is **what tree-sitter
  recovers**, NOT zero. tree-sitter-c-sharp 0.23.5 silently drops
  the `Bar` method (its `method_declaration` node never matches
  the definition query because of the ERROR) but still recognises
  `Foo` (the enclosing class) and the subsequent `Good` method,
  and the entire `AlsoGood` class declaration parses cleanly.
- **Critical anti-regression:** the parser must not panic and
  must not abort the file when ERROR nodes appear mid-stream.

### `edge_cases/nested_classes.cs`

| Name  | Kind   | Line | Namespace | Parent |
|-------|--------|-----:|-----------|--------|
| Outer | Class  |    6 | Nested    |        |
| Inner | Class  |    8 | Nested    | Outer  |
| Leaf  | Method |   10 | Nested    | Inner  |

- 3 symbols (2 Classes, 1 Method)
- 0 edges
- **Parent contract:** the inner class records the *immediate*
  enclosing outer class (`Outer`) as its parent — NOT a dotted
  path (`Nested.Outer`). Mirrors the Python/Rust/C++ nested-class
  convention. The leaf method records `Inner` for the same reason.

### `edge_cases/partial_class_a.cs`

| Name | Kind   | Line | Namespace | Parent |
|------|--------|-----:|-----------|--------|
| Foo  | Class  |    8 | Partials  |        |
| A    | Method |   10 | Partials  | Foo    |

- 2 symbols (1 Class, 1 Method)
- 0 edges

### `edge_cases/partial_class_b.cs`

| Name | Kind   | Line | Namespace | Parent |
|------|--------|-----:|-----------|--------|
| Foo  | Class  |    7 | Partials  |        |
| B    | Method |    9 | Partials  | Foo    |

- 2 symbols (1 Class, 1 Method)
- 0 edges
- **Partial-class contract (Decision 3):** the corpus contains TWO
  `Class` symbols named `Foo`, one per file. Both share the
  namespace `Partials` but their `Symbol.path` differs, so the
  symbol IDs are distinct (`partial_class_a.cs:Foo` vs
  `partial_class_b.cs:Foo`). Methods extracted from each partial
  carry the bare-name parent `Foo`; the merge-by-bare-name
  contract in `Graph::class_hierarchy` (per Phase 1, Phase 5, and
  reaffirmed in Phase 2.5) handles the cross-file lookup.

### `edge_cases/method_name_collides_with_free_function.cs`

| Name     | Kind   | Line | Namespace | Parent        |
|----------|--------|-----:|-----------|---------------|
| Container | Class  |   12 | Collide   |               |
| Foo (1)  | Method |   14 | Collide   | Container     |
| FreeFunctions | Class | 17 | Collide   |               |
| Foo (2)  | Method |   19 | Collide   | FreeFunctions |

- 4 symbols (2 Classes, 2 Methods)
- 0 edges
- Two methods named `Foo` coexist with distinct symbol IDs because
  their `parent` strings differ (`Container` vs `FreeFunctions`).
  Anti-regression for the SymbolIndex's parent-disambiguated
  keying. C# does not have module-level free functions like
  Python's `def add():`; the static-method-on-a-static-class form
  is the closest idiomatic equivalent and is what's used here.

## Key Validation Points

- **Default interface methods are Functions, not Methods**
  (Decision 11's C# follow-up; cross-language contract with Rust's
  trait-default-method rule).
- **Records extract as Class**, methods inside records extract as
  Method with parent = record name (Decision 6 analog for C#).
- **Extension methods keep their syntactic parent** (the enclosing
  static class), NOT the extended type (Decision 5).
- **`using` modifiers are dropped** from the dotted path:
  `static`, alias (`=`), and `global` all collapse to the same
  path-verbatim form (Decision 7).
- **Generic parameter text preserved verbatim** in `Inherits` edge
  endpoints (Decision 9, Rust precedent).
- **Both class extension and interface implementation** produce
  `Inherits` (Decision 2; no separate `Implements` edge kind).
- **Partial classes produce one Class symbol per declaration**;
  the file path disambiguates at query time (Decision 3).
- **Recovered-symbol count for `broken.cs` = 4**, not zero
  (Phase 7 `broken.py` discovery — run and record).
- **Nested classes record the immediate enclosing class**, not a
  dotted path.
- **Empty file (0 bytes)** produces zero symbols, zero edges, no
  panic.
- **Comments-only file** produces zero symbols, zero edges.

## Conditional / scope-walking forms NOT in this corpus

- The watch-mode reindex test (`crates/code-graph-tools/tests/
  watch_csharp_reindex.rs`) covers add/remove of partial-class
  declarations across files (Decision 3 lifecycle), Inherits-edge
  pruning, and Calls-edge pruning. This corpus pins the
  steady-state extraction; the watch test pins the dynamic
  graph-merge invariants.
- Struct and Enum symbols are exercised by the inline tests in
  `crates/code-graph-lang-csharp/src/lib.rs`; the corpus
  intentionally keeps the fixture set focused on the cross-file
  cases (partial classes, namespace organization, default
  interface methods, records, extension methods) that don't
  exercise cleanly in a single inline test buffer.
