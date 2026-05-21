# Testdata Rust Project — Expected Parse Results

The Rust parser (`codegraph-lang-rust`) must produce these exact counts when
each `.rs` file under `src/` is parsed in isolation and the results are
aggregated. The corpus test
`crates/codegraph-lang-rust/tests/corpus.rs` asserts every total in this
file; if you change a fixture, update both.

## Totals

### Symbols by Kind (TOTAL = 41)

| Kind     | Count |
|----------|------:|
| Function |     9 |
| Method   |    14 |
| Struct   |     9 |
| Enum     |     3 |
| Trait    |     3 |
| Typedef  |     3 |

Post-Task-1.4 totals. The 1.4 reshaping moves `traits.rs::default_greet`
from `Function` → `Method`/parent=`Greet`, and extracts two abstract
trait method signatures that previously produced no symbols
(`Greet::greet` and `Compute::compute`, both `Method`/parent=trait).
Net: `Function` -1, `Method` +3, totals +2.

### Edges by Kind (TOTAL = 45)

| Kind     | Count |
|----------|------:|
| Calls    |    22 |
| Includes |    17 |
| Inherits |     6 |

`Includes` totals 17, split as 11 from `use`/`extern crate` declarations
(unchanged) + 6 provisional mod-decl edges (4 in `lib.rs`, 2 in
`mod_only.rs`). Provisional means the edge's `to` is a bare modname token
that resolves to no indexed file under the default basename matcher and
so is dropped at edge-resolution time; the parser still emits these edges
unconditionally because whole-graph resolution to concrete sibling files
is a separate post-parse pass.

## Per-file Breakdown

### `src/empty.rs`

Empty file.
- 0 symbols
- 0 edges

### `src/mod_only.rs`

```rust
pub mod a;
pub mod b;
```

- 0 symbols (mod_items are namespace anchors, not Symbol records)
- 2 edges (`Includes` to bare `a` and `b` — one per external `mod` decl):
  - The `to` is the bare modname token (a provisional placeholder).
  - The default basename matcher does not promote these to indexed
    files, so they drop at edge-resolution time. Whole-graph resolution
    of the bare modname to a concrete sibling file is a separate
    post-parse pass.

### `src/lib.rs`

| Name     | Kind    | Line | Namespace | Parent |
|----------|---------|-----:|-----------|--------|
| Pair     | Typedef |   17 |           |        |
| Result2  | Typedef |   18 |           |        |

- 2 symbols (2 Typedefs)
- 4 edges (`Includes` to bare `errors`, `models`, `traits`, `utils` —
  one per external `pub mod foo;` declaration). These are provisional
  bare-modname targets; they drop at edge-resolution time under the
  default basename matcher (whole-graph mod→file resolution is a
  separate post-parse pass).

### `src/main.rs`

| Name | Kind     | Line | Namespace | Parent |
|------|----------|-----:|-----------|--------|
| main | Function |   22 |           |        |

- 1 symbol (1 Function)
- 16 edges:
  - 9 `Includes` (1 from `extern crate alloc;` + 8 from the `use` declarations,
    counting expanded paths: `…::errors::AppError`, `…::models`, `…::models::Vec2`,
    `…::traits::*`, `…::utils`, `std::collections::HashMap`, `std::io`,
    `std::io::Read`)
  - 7 `Calls`: `Vec2::new`, `HashMap::<String, i32>::new`, `Ok` (twice — the
    `Ok(())` in main appears in two `let _: ... = Ok(())` bindings),
    `utils_mod::add`, `println` (macro), `models::nested_helper`

### `src/models.rs`

| Name              | Kind     | Line | Namespace | Parent |
|-------------------|----------|-----:|-----------|--------|
| Vec2              | Struct   |   19 |           |        |
| Vec2::new         | Method   |   25 |           | Vec2   |
| Vec2::magnitude_squared | Method | 29 |        | Vec2   |
| RGB               | Struct   |   35 |           |        |
| Marker            | Struct   |   38 |           |        |
| Shape             | Enum     |   40 |           |        |
| Status            | Enum     |   47 |           |        |
| Inner             | Struct   |   56 | nested    |        |
| nested_helper     | Function |   60 | nested    |        |

- 9 symbols (4 Structs, 2 Enums, 2 Methods, 1 Function)
- 0 edges (no `use`, no `extern crate`, no calls inside any body, and
  the `pub mod nested { … }` declaration is INLINE — its body lives in
  the same file, so the mod-decl extractor suppresses the self-edge at
  emission time. Only *external* `mod foo;` declarations contribute
  provisional mod-decl Includes edges.)
- The `nested` mod contributes namespace `nested` to `Inner` and `nested_helper`
  but is NOT itself emitted as a Symbol.

### `src/traits.rs`

| Name             | Kind     | Line | Namespace | Parent  |
|------------------|----------|-----:|-----------|---------|
| Greet            | Trait    |   43 |           |         |
| Greet::greet     | Method   |   44 |           | Greet   |
| Greet::default_greet | Method | 46 |           | Greet   |
| Compute          | Trait    |   51 |           |         |
| Compute::compute | Method   |   52 |           | Compute |
| Sized2           | Trait    |   58 |           |         |
| Greeter          | Struct   |   60 |           |         |
| Greeter::run_async | Method |   65 |           | Greeter |
| Greeter::do_unsafe | Method |   72 |           | Greeter |
| Greeter::greet   | Method   |   84 |           | Greeter |
| EmptyImpl        | Struct   |   91 |           |         |
| Foo              | Struct   |   95 |           |         |
| Bar              | Struct   |   96 |           |         |
| Foo<T>::compute  | Method   |   99 |           | Foo<T>  |
| Bar<T>::compute  | Method   |  112 |           | Bar<T>  |

- 15 symbols (3 Traits, 4 Structs, 8 Methods)
- 7 edges:
  - 1 `Includes`: `std::fmt::Display`
  - 3 `Calls`: `Greet::default_greet -> String::from`,
    `Greeter::greet -> format`, `Foo<T>::compute -> format`
  - 3 `Inherits`: `Greeter -> Greet`, `Foo<T> -> Compute`, `Bar<T> -> Compute`
- The `Sized2` trait has no impl in the fixture and produces no inheritance
  edge — that's deliberate.
- The `impl EmptyImpl {}` block has no methods and produces 0 method symbols
  AND 0 inheritance edges (the inheritance query requires a `trait` field,
  which an inherent impl lacks).
- **Post-Task-1.4 trait-method classification:** abstract trait method
  signatures (`Greet::greet`, `Compute::compute`) are now extracted as
  `Method` symbols with parent = trait name. The default method
  `Greet::default_greet` (with a body) is also now `Method`/parent=`Greet`
  (pre-1.4 it was `Function` with empty parent). Trait identity rides on
  the parent field.
- The Rust parser also emits `Inherits` edges for trait supertrait
  bounds (`pub trait Sub: Super { … }` → one `Inherits` edge from `Sub`
  to each nameable bound). None of the traits in this fixture currently
  declare supertrait bounds, so the aggregate `Inherits` count is
  unchanged.

### `src/utils.rs`

| Name           | Kind     | Line | Namespace | Parent |
|----------------|----------|-----:|-----------|--------|
| Op             | Typedef  |   19 |           |        |
| add            | Function |   21 |           |        |
| mul            | Function |   25 |           |        |
| with_closure   | Function |   43 |           |        |
| cfg_gated_fn   | Function |   55 |           |        |
| unsafe_op      | Function |   61 |           |        |

- 6 symbols (1 Typedef, 5 Functions)
- 2 edges:
  - 2 `Calls`: `with_closure -> add`, `with_closure -> f` (closure call by
    binding name)
- **Anti-regression: `macro_rules! my_macro { ... }` MUST yield zero
  Symbol records.** The corpus test asserts that no Symbol with `name ==
  "my_macro"` is emitted from `utils.rs`. Macro definitions are
  intentionally excluded from extraction; only macro *invocations* produce
  Calls edges.

### `src/errors.rs`

| Name              | Kind     | Line | Namespace | Parent     |
|-------------------|----------|-----:|-----------|------------|
| AppError          | Enum     |   23 |           |            |
| AppError::message | Method   |   29 |           | AppError   |
| AppError::fmt     | Method   |   38 |           | AppError   |
| IoErrorWrapper    | Struct   |   44 |           |            |
| AppError::from #1 | Method   |   47 |           | AppError   |
| AppError::from #2 | Method   |   53 |           | AppError   |
| read_or_fail      | Function |   58 |           |            |
| q_propagation     | Function |   67 |           |            |

- 8 symbols (1 Enum, 1 Struct, 4 Methods, 2 Functions)
- 14 edges:
  - 1 `Includes`: `std::fmt`
  - 10 `Calls`: `AppError::fmt -> write`, `AppError::from -> AppError::Io`
    (twice, one per `From` impl), `AppError::from -> format` (in the
    `From<std::io::Error>` impl), `read_or_fail -> Ok`,
    `read_or_fail -> Err`, `read_or_fail -> AppError::Parse`,
    `read_or_fail -> String::from`, `q_propagation -> read_or_fail`,
    `q_propagation -> Ok`
  - 3 `Inherits`: `AppError -> fmt::Display`,
    `AppError -> From<std::io::Error>`,
    `AppError -> From<IoErrorWrapper>`
- Both `from` methods land at `AppError::from` (parent disambiguation
  selects the `type` field from each `impl Trait for Type` block — `From`
  is the trait, `AppError` is the type, so parent = `AppError`). The
  trait identity is recorded only on the `Inherits` edge, never on the
  method symbol.

## Namespace Map

| Namespace | Files Contributing | Symbols |
|-----------|--------------------|---------|
| (empty)   | all                | most    |
| `nested`  | `models.rs`        | `Inner`, `nested_helper` |

## Key Validation Points

- **`macro_rules!` definitions yield zero symbols** (`utils.rs::my_macro`).
  Asserted explicitly in the corpus test.
- **`#[derive(...)]`** appears as `attribute_item`, not `macro_invocation`,
  so it does NOT produce Calls edges. Confirmed: 0 Calls edges in
  `models.rs` despite multiple `#[derive(Debug, Clone, ...)]` attributes.
- **Empty file** (`empty.rs`) parses without error and contributes 0 symbols
  / 0 edges.
- **Mod-only file** (`mod_only.rs`) parses without error and contributes 0
  symbols (mod_items are namespace anchors, not Symbol records). Each
  external `mod foo;` declaration produces one provisional `Includes`
  edge to the bare modname token; under the default basename matcher,
  those edges drop at edge-resolution time, so the *surviving* dependency
  set is unchanged. Whole-graph mod→file resolution is a separate
  post-parse pass.
- **`unsafe { ... }` block** does not interfere with symbol extraction —
  `utils.rs::unsafe_op` and `traits.rs::Greeter::do_unsafe` both produce
  the expected single function/method symbol.
- **`extern crate alloc;`** produces a single `Includes` edge to `alloc`
  (in `main.rs`).
- **`#[cfg(...)]` attribute** on `utils.rs::cfg_gated_fn` does not stop
  the function from being extracted; the parser does not evaluate cfg
  predicates.
- **Nested mods** (`models::nested`) populate `Symbol.namespace` but do
  not themselves produce Symbol records.
- **Inherent impl with no methods** (`impl EmptyImpl {}` in `traits.rs`)
  produces 0 method symbols and 0 inheritance edges.
- **Trait with default methods** (`Greet::default_greet`): post-Task-1.4
  the default method produces a `Method` symbol with parent =
  `Greet` (the enclosing trait name). Pre-1.4 it was a Function with
  empty parent.
- **Trait abstract method signatures** (`Greet::greet`,
  `Compute::compute`): post-Task-1.4 these produce `Method` symbols with
  parent = trait name. This is the documented Rust-trait-scoped exception
  to the "forward declarations excluded" invariant; bare
  `function_signature_item`s outside any trait (e.g. inside
  `extern "C"` blocks) remain excluded.
- **Trait-impl method parent disambiguation**: every method inside an
  `impl Trait for Type {}` block has parent = `Type`, never `Trait`.
  Asserted both in the lib's unit tests and via this corpus's
  `errors.rs::AppError::from` (parent `AppError`, never `From`) and
  `traits.rs::Greeter::greet` (parent `Greeter`, never `Greet`).
- **Generic impls** record the type-field text verbatim including
  generics (`Foo<T>`, `Bar<T>`), which is why methods inside them carry
  parents like `Foo<T>` rather than `Foo`. The Inherits edges'
  `from` field follows the same rule.
- **`fn from()` collisions** across two `From` impls live in the same
  `FileGraph` as two separate Symbol records with identical names and
  parents. The graph layer's `SymbolIndex` resolves the (Language, name)
  collision deterministically downstream; the parser's job is to surface
  every definition, which it does.
