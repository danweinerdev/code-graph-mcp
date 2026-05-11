# Testdata Java Project — Expected Parse Results

The Java parser (`code-graph-lang-java`) must produce these exact counts
when each `.java` file under `testdata/java/` is parsed in isolation and
the results are aggregated. The corpus test
`crates/code-graph-lang-java/tests/corpus.rs` asserts every total in
this file; if you change a fixture, update both.

## Totals

### Symbols by Kind (TOTAL = 57)

| Kind      | Count |
|-----------|------:|
| Function  |     3 |
| Method    |    25 |
| Class     |    23 |
| Interface |     5 |
| Enum      |     1 |

`Function = 3` covers the three default-interface-method bodies per
Decision 11 — `IGreeter::greet` (default), `IGreeter::banner` (static),
and `IExtended::doBoth` (default) — all of which extract as `Function`
(no parent) matching Rust's trait-default-method rule. Every other
method-shaped declaration in this corpus is a `Method` with a
class/interface/enum/record parent.

`Class = 23` counts records (Decision 6 — records extract as `Class`,
not as a new `SymbolKind::Record`). The Java fixture's records are
`Records` (top-level in `edge_cases/Records.java`) and `User` (nested
inside `Handlers.java`). Sealed types' `permits` declarations contribute
no new symbols.

There are no `Struct` symbols — Java has no struct construct. There are
no `Typedef` symbols either — Java has type aliases via `import`
but not via a dedicated declaration form.

### Edges by Kind (TOTAL = 46)

| Kind     | Count |
|----------|------:|
| Calls    |    29 |
| Includes |     8 |
| Inherits |     9 |

`Inherits = 9` covers single (`Beta extends Alpha`), multiple-base
(`Gamma extends Alpha implements IMixin` → 2 edges), interface
implementation (`Service implements IService`, plus `Circle implements
Shape`, `Square implements Shape`), interface-extends-interface
(`IExtended extends IMixin, IService` → 2 edges), and generic
(`Box<T> extends BoxBase<T>` → 1 edge with `from = "Box<T>"`,
`to = "BoxBase<T>"`) inheritance forms. Per Decision 2, both `extends`
and `implements` produce the same `Inherits` edge kind. Per Decision 9,
generic parameter text is preserved verbatim in both `from` and `to`.
Sealed types' `permits` clauses contribute zero `Inherits` edges per
Decision 6.

## Per-file Breakdown

### `Program.java`

| Name | Kind   | Namespace | Parent  |
|------|--------|-----------|---------|
| Program | Class  |           |         |
| main    | Method |           | Program |
| run     | Method |           | Program |

- 3 symbols (1 Class, 2 Methods)
- 10 edges:
  - 4 `Includes`: `java.util.ArrayList`, `java.util.List`,
    `java.util.*` (wildcard preserved verbatim — Decision 7),
    `java.lang.Math.abs` (static-import — the `static` modifier is
    dropped, the field name folds into the dotted path)
  - 6 `Calls`: `Program::main -> ArrayList` (constructor call —
    `new ArrayList<>()` records the bare type name as the target),
    `Program::main -> add`, `Program::main -> size`, `Program::main ->
    run`, `Program::main -> abs`, `Program::run -> println`
  - 0 `Inherits` (Program has no extends/implements)

Note: Java symbols carry an empty `namespace` field by convention —
`package app;` is recorded only via the import paths into and out of
the file, not on individual symbols. The C# plugin uses the `namespace
Foo { ... }` block as the namespace; the Java plugin treats Java's
file-level `package` declaration differently. This is an intentional
plugin-level convention (see `crates/code-graph-lang-java/src/lib.rs`'s
`namespace_is_empty_for_java_symbols` test).

### `Models.java`

| Name           | Kind      | Parent  |
|----------------|-----------|---------|
| Models         | Class     |         |
| IMixin         | Interface | Models  |
| IService       | Interface | Models  |
| IExtended      | Interface | Models  |
| doBoth         | Function  |         |
| Alpha          | Class     | Models  |
| Alpha::Alpha   | Method    | Alpha   |
| Alpha::m       | Method    | Alpha   |
| Beta           | Class     | Models  |
| Beta::Beta     | Method    | Beta    |
| Beta::m        | Method    | Beta    |
| Gamma          | Class     | Models  |
| Gamma::mix     | Method    | Gamma   |
| Service        | Class     | Models  |
| Service::handle| Method    | Service |
| Box            | Class     | Models  |
| BoxBase        | Class     | Models  |

- 17 symbols (3 Interfaces, 1 Function, 7 Classes, 6 Methods)
- 14 edges:
  - 1 `Includes`: `java.util.List`
  - 6 `Calls`: `doBoth -> mix`, `doBoth -> handle` (calls from the
    default interface method body — `from` is the bare `doBoth` because
    Function symbols have no parent), `Alpha::m -> println`,
    `Beta::Beta -> super` (constructor chaining `super()`),
    `Beta::m -> m`, `Gamma::mix -> m`
  - 7 `Inherits`:
    - `IExtended -> IMixin`, `IExtended -> IService`
      (interface-extends-interface; Decision 2)
    - `Beta -> Alpha` (single inheritance)
    - `Gamma -> Alpha`, `Gamma -> IMixin` (multiple-base; Decision 2)
    - `Service -> IService` (interface implementation; Decision 2)
    - `Box<T> -> BoxBase<T>` (generic; Decision 9 preserves the
      generic parameter text verbatim in both `from` and `to`)
- `Alpha` and `BoxBase` are top-level classes (parented to the
  file's outer class `Models` via the public-nested-class shape;
  they have no bases) — they contribute zero `Inherits` edges.
- The `Inherits` edge from `Box<T>` uses the bare-class-name-with-
  generics convention required by the `get_class_hierarchy` walker
  in `crates/code-graph-graph/src/algorithms.rs`.

### `Handlers.java`

| Name                  | Kind      | Parent   |
|-----------------------|-----------|----------|
| Handlers              | Class     |          |
| IGreeter              | Interface | Handlers |
| greet                 | Function  |          |
| banner                | Function  |          |
| Shape                 | Interface | Handlers |
| Circle                | Class     | Handlers |
| Square                | Class     | Handlers |
| User                  | Class     | Handlers |
| User::display         | Method    | User     |
| Hub                   | Class     | Handlers |
| Hub::dispatch         | Method    | Hub      |
| Hub::process          | Method    | Hub      |

- 12 symbols (2 Interfaces, 2 Functions, 5 Classes, 3 Methods)
- 6 edges:
  - 1 `Includes`: `java.lang.Math.max` (the static-import modifier is
    dropped; the field name folds into the dotted path)
  - 3 `Calls`: `greet -> required` (default-interface-method body
    calls the abstract sibling), `Hub::dispatch -> process` (intra-class
    static call), `Hub::dispatch -> max` (the static-import target)
  - 2 `Inherits`: `Circle -> Shape`, `Square -> Shape`
- `IGreeter::required` (abstract — no body) produces **no** symbol
  (forward-declaration rule, mirroring Rust and the four shipped plugins).
- `IGreeter::greet` (default interface method, has body) extracts
  as `Function` per Decision 11. Its parent is empty — NOT `IGreeter`.
- `IGreeter::banner` (static interface method, has body) also
  extracts as `Function` per Decision 11. Body presence is the
  discriminator that subsumes the `default`/`static`/Java-9+-private
  modifier check.
- `User` (a `record User(String name)`) extracts as `Class` per
  Decision 6. The record's `display()` method extracts as `Method`
  with parent `User`.
- `Shape` is a `sealed` interface; the `permits Circle, Square`
  clause is ignored per Decision 6 — it produces zero edges and the
  symbol still has kind `Interface`.

### `edge_cases/Empty.java`

| Name  | Kind  | Parent |
|-------|-------|--------|
| Empty | Class |        |

- 1 symbol (1 Class), 0 edges
- Java requires at least a top-level class for the file to parse —
  there is no zero-symbol equivalent to C#'s 0-byte `empty.cs`. The
  empty class body still produces a `Class` symbol with 0 methods.
- Parser must not panic on a class with an empty body.

### `edge_cases/CommentsOnly.java`

| Name         | Kind  | Parent |
|--------------|-------|--------|
| CommentsOnly | Class |        |

- 1 symbol (1 Class), 0 edges
- Single-line `//`, multi-line `/* ... */`, and javadoc `/** ... */`
  comments surrounding and inside an otherwise-empty class shell.
- Only the class shell extracts; comments produce no methods or fields.

### `edge_cases/Broken.java`

| Name     | Kind   | Parent   |
|----------|--------|----------|
| Broken   | Class  |          |
| bar      | Method | Broken   |
| good     | Method | Broken   |
| AlsoGood | Class  |          |
| run      | Method | AlsoGood |

- 5 symbols (2 Classes, 3 Methods), 0 edges
- The malformed `public void bar(` (opening paren immediately followed
  by `{` instead of a parameter list) produces ERROR nodes in
  tree-sitter's parse. **Unlike the C# analog**, tree-sitter-java
  0.23.5 STILL extracts the `bar` method — the recovery is aggressive
  enough that the `method_declaration` node matches the definition
  query despite the malformed parameter list. The enclosing `Broken`
  class, the sibling `good` method, the post-error `AlsoGood` class,
  and its `run` method all extract cleanly.
- The recovered count is **what tree-sitter-java recovers**, NOT
  zero. **Run and record.** Mirrors the Phase 7 `broken.py` discovery.
- **Critical anti-regression:** the parser must not panic and must
  not abort the file when ERROR nodes appear mid-stream.

### `edge_cases/NestedClasses.java`

| Name          | Kind   | Parent        |
|---------------|--------|---------------|
| NestedClasses | Class  |               |
| Outer         | Class  | NestedClasses |
| Inner         | Class  | Outer         |
| leaf          | Method | Inner         |

- 4 symbols (3 Classes, 1 Method), 0 edges
- **Parent contract:** the inner class records the *immediate*
  enclosing outer class (`Outer`) as its parent — NOT a dotted path
  (`NestedClasses.Outer`). Mirrors the Python/Rust/C++/C# nested-class
  convention. The leaf method records `Inner` for the same reason.

### `edge_cases/AnonymousInside.java`

| Name            | Kind   | Parent          |
|-----------------|--------|-----------------|
| AnonymousInside | Class  |                 |
| handle          | Method | AnonymousInside |
| run             | Method | AnonymousInside |
| run             | Method | AnonymousInside |

- 4 symbols (1 Class, 3 Methods), 6 edges
- **Decision 4** anti-regression. Anonymous classes
  (`new Runnable() { ... }`) emit NO Class symbol. The two anonymous
  `run()` methods both take `AnonymousInside` as parent (the
  enclosing NAMED entity), producing two symbols with **identical
  symbol IDs**. `Symbol.line` is the only disambiguator at query
  time — documented as a known limitation in the crate docstring.
- The 6 Calls edges are:
  - 2 × `AnonymousInside::handle -> Runnable` (the two `new Runnable()`
    constructor calls)
  - 2 × `AnonymousInside::run -> println` (the body of each anonymous
    `run` method)
  - 2 × `AnonymousInside::handle -> run` (the `first.run()` and
    `second.run()` calls)

### `edge_cases/Records.java`

| Name     | Kind   | Parent  |
|----------|--------|---------|
| Records  | Class  |         |
| greeting | Method | Records |
| nextAge  | Method | Records |

- 3 symbols (1 Class, 2 Methods), 0 edges
- **Decision 6:** `record Records(String name, int age)` extracts as
  `Class` — NOT a new `SymbolKind::Record`. Methods inside the record
  body extract as `Method` with parent = the record name. **No orphan
  `Function` symbols leak** — this is the C# 2.2 records-leak bug
  analog that the C# plugin fixed in commit `0cf200b`.
- Record components (`name`, `age`) are `formal_parameter` nodes,
  not `method_declaration` — they're correctly invisible. The
  auto-generated accessor methods (`name()`, `age()`), `equals`,
  `hashCode`, `toString` are NOT extracted because they don't appear
  in source.

### `edge_cases/EnumWithMethods.java`

| Name             | Kind   | Parent          |
|------------------|--------|-----------------|
| EnumWithMethods  | Enum   |                 |
| surfaceGravity   | Method | EnumWithMethods |
| surfaceGravity   | Method | EnumWithMethods |
| describe         | Method | EnumWithMethods |

- 4 symbols (1 Enum, 3 Methods), 2 edges
- **Decision 12 fixture** (the Planet shape). enum-level methods AND
  per-constant method bodies both extract as `Method` with parent =
  the enum type. There is NO synthetic `EnumWithMethods$EARTH` parent.
- The two `surfaceGravity` methods are the per-constant bodies on
  `EARTH` and `MARS`. They share the same name and parent — only
  `Symbol.line` disambiguates them, the same rule as Decision 4's
  anonymous-class collision.
- Enum-level `abstract double surfaceGravity();` (no body) is
  filtered as a forward declaration and produces ZERO symbols (it's
  not the third `surfaceGravity` in the count).
- Enum constants themselves (EARTH, MARS) are NOT extracted as
  symbols per Decision 12.
- The 2 Calls edges are inside `describe()`: `name()` and
  `surfaceGravity()`.

### `edge_cases/MethodReferences.java`

| Name             | Kind   | Parent           |
|------------------|--------|------------------|
| MethodReferences | Class  |                  |
| len              | Method | MethodReferences |
| run              | Method | MethodReferences |

- 3 symbols (1 Class, 2 Methods), 8 edges
- 2 `Includes`: `java.util.function.Function`,
  `java.util.function.Supplier`
- 6 `Calls`:
  - `len -> length` (body call `s.length()` inside `len()`)
  - `run -> length` (method reference `String::length` — Phase 3.3
    records the RHS as the callee)
  - `run -> len` (method reference `this::len`)
  - `run -> apply` ×2 (the `a.apply(...)` and `b.apply(...)` calls)
  - `run -> get` (the `c.get()` call)
- **Phase 3.3 documented limitation:** the constructor reference
  `MethodReferences::new` produces NO Calls edge. The query only
  matches method-reference forms with an identifier on the RHS — the
  `new` keyword on the RHS is intentionally not matched, mirroring
  Python's `__init__` non-extraction rule.

## Key Validation Points

- **Default interface methods are Functions, not Methods**
  (Decision 11; cross-language contract with Rust's
  trait-default-method rule). Body-presence is the discriminator
  that subsumes `default`/`static`/Java-9+-private modifier checks.
- **Records extract as Class**, methods inside records extract as
  Method with parent = record name (Decision 6 — no records-leak
  bug).
- **Anonymous classes emit no Class symbol** — inner methods take
  the outer NAMED entity's parent (Decision 4). Two anonymous
  classes inside the same method that both define `run()` produce
  two symbols with identical IDs, disambiguated by line.
- **Enum methods (enum-level + per-constant) extract with parent =
  the enum type** — NOT a synthetic `Planet$EARTH` (Decision 12).
- **Generic parameter text preserved verbatim** in `Inherits` edge
  endpoints (Decision 9, Rust precedent).
- **Both class extension (`extends`) and interface implementation
  (`implements`)** produce `Inherits` (Decision 2; no separate
  `Implements` edge kind). Interface-extends-interface uses the
  same edge kind.
- **`import static` and `import com.foo.*`** are dropped/preserved
  per Decision 7: `static` modifier is dropped; wildcard preserved
  verbatim.
- **Recovered-symbol count for `Broken.java` = 5**, not zero —
  tree-sitter-java 0.23.5 recovers more aggressively than the C#
  parser, picking up the malformed `bar` method as well as the
  surrounding clean declarations.
- **Nested classes record the immediate enclosing class**, not a
  dotted path.
- **Sealed interfaces extract as ordinary `Interface`**; the
  `permits` clause is ignored (Decision 6).
- **Constructor references (`Type::new`) produce no Calls edges**
  (Phase 3.3 documented limitation).

## Conditional / scope-walking forms NOT in this corpus

- The watch-mode reindex test (`crates/code-graph-tools/tests/
  watch_java_reindex.rs`) covers Inherits-edge pruning, Calls-edge
  pruning, and the Decision 4 anonymous-class lifecycle discriminator
  (add/remove of files containing anonymous-class call edges). This
  corpus pins the steady-state extraction; the watch test pins the
  dynamic graph-merge invariants.
- Struct symbols do not exist in Java (no `struct` keyword); no
  fixture covers them.
- `Typedef` symbols do not exist in this corpus — Java's only
  type-alias-like form is `import`, which produces `Includes` edges
  (not symbols).
