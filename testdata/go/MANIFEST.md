# Testdata Go Project — Expected Parse Results

The Go parser (`codegraph-lang-go`) must produce these exact counts when
each `.go` file under `testdata/go/` is parsed in isolation and the
results are aggregated. The corpus test
`crates/codegraph-lang-go/tests/corpus.rs` asserts every total in this
file; if you change a fixture, update both.

## Totals

### Symbols by Kind (TOTAL = 42)

| Kind      | Count |
|-----------|------:|
| Function  |    14 |
| Method    |    12 |
| Struct    |     7 |
| Interface |     6 |
| Typedef   |     3 |

### Edges by Kind (TOTAL = 41)

| Kind     | Count |
|----------|------:|
| Calls    |    30 |
| Includes |    11 |
| Inherits |     0 |

`Inherits = 0` is a CRITICAL invariant: Go interfaces are structurally
typed (no syntactic `implements` clause) and embedded struct/interface
fields are structural composition, not inheritance. The Go parser is
expected to emit ZERO `Inherits` edges across the entire corpus — even
when a concrete type satisfies an interface (e.g. `*Server` satisfies
`Runner`, `*User` satisfies `Named`) and even when a struct embeds
another struct (e.g. `User` embeds `Profile` and `*sync.Mutex`). The
corpus test pins this with an explicit assertion.

## Per-file Breakdown

### `empty.go`

```go
package main
```

- 0 symbols (the `package_clause` is consumed without emitting a Symbol;
  the package name lives only in `Symbol.namespace` on other symbols)
- 0 edges (no imports, no calls)

### `main.go`

| Name | Kind     | Line | Namespace | Parent |
|------|----------|-----:|-----------|--------|
| init | Function |   49 | main      |        |
| main | Function |   59 | main      |        |

- 2 symbols (2 Functions)
- 9 edges:
  - 5 `Includes`: `fmt`, `code-graph-go-corpus/server`,
    `code-graph-go-corpus/utils` (aliased — alias `umath` dropped),
    `code-graph-go-corpus/models` (dot-imported — `.` dropped),
    `image/png` (blank — `_` dropped). All five share the line of the
    grouped `import (` block (line 33), per the line-anchoring rule.
  - 4 `Calls`: `init -> Add` (resolved as `Add` from the aliased
    selector `umath.Add`), `main -> New`, `main -> Run`, `main -> Println`

### `models/deps.go`

| Name        | Kind      | Line | Namespace | Parent  |
|-------------|-----------|-----:|-----------|---------|
| Reader      | Interface |   24 | models    |         |
| ReadWriter  | Interface |   32 | models    |         |
| Node        | Struct    |   38 | models    |         |
| Node::ID    | Method    |   43 | models    | Node    |
| Cluster     | Struct    |   49 | models    |         |
| Cluster::Add| Method    |   58 | models    | Cluster |
| _           | Function  |   66 | models    |         |

- 7 symbols (2 Interfaces, 2 Structs, 2 Methods, 1 Function)
- 1 edge:
  - 1 `Calls`: `Cluster::Add -> append` (Go builtin)
- The blank-identifier function `func _() {}` is extracted as Function
  with name `"_"`. Anti-regression for "blank identifier function name"
  edge case from the Phase 6.5 brief.
- `ReadWriter` embeds `Reader` (interface-embedding-interface). The
  embedded `Reader` line in the body is a `type_elem` with a
  `type_identifier`, NOT a `method_elem` — the definition queries match
  neither, so no extra Symbol falls out for the embed and no edge is
  produced.
- `Cluster.Endpoints` is an anonymous-struct slice field. The anonymous
  struct's inner fields are NOT emitted as nested Symbols.

### `models/repo.go`

| Name           | Kind      | Line | Namespace | Parent |
|----------------|-----------|-----:|-----------|--------|
| Closer         | Interface |   48 | models    |        |
| Repo           | Interface |   54 | models    |        |
| Memo           | Struct    |   63 | models    |        |
| Memo::Get      | Method    |   74 | models    | Memo   |
| Memo::Hits     | Method    |   82 | models    | Memo   |
| KV             | Struct    |   88 | models    |        |
| KV::Set        | Method    |   94 | models    | KV     |
| KV::Lookup     | Method    |  104 | models    | KV     |
| Map            | Function  |  112 | models    |        |
| Filter         | Function  |  122 | models    |        |

- 10 symbols (2 Interfaces, 2 Structs, 4 Methods, 2 Functions)
- 11 edges:
  - 1 `Includes`: `code-graph-go-corpus/utils`
  - 10 `Calls`: `Memo::Get -> Add`, `Memo::Hits -> Get`,
    `KV::Set -> make`, `KV::Lookup -> ok`, `Map -> make`, `Map -> len`,
    `Map -> append`, `Map -> f`, `Filter -> pred`, `Filter -> append`
- `Repo` embeds `Closer` (interface-embedding-interface). The embedded
  interface produces no Symbol.
- `Memo` has both a value-receiver method (`Get`) and a pointer-receiver
  method (`Hits`) — exercises both branches of `extract_receiver_type`
  against the same parent.
- `Map` and `Filter` are Go 1.18+ generic functions; their signatures
  truncate at the body opener with the type-parameter lists intact.
- `Memo`'s `Cache` field is an anonymous struct value. The anonymous
  struct's inner fields are NOT emitted as nested Symbols.

### `models/user.go`

| Name        | Kind      | Line | Namespace | Parent |
|-------------|-----------|-----:|-----------|--------|
| Named       | Interface |   28 | models    |        |
| Profile     | Struct    |   34 | models    |        |
| User        | Struct    |   40 | models    |        |
| User::Name  | Method    |   49 | models    | User   |
| User::Close | Method    |   56 | models    | User   |

- 5 symbols (1 Interface, 2 Structs, 2 Methods)
- 2 edges:
  - 1 `Includes`: `sync`
  - 1 `Calls`: `User::Close -> Unlock` (deferred selector call)
- `User` embeds `Profile` and `*sync.Mutex` (both anonymous fields).
  Both embeds MUST produce zero Inherits edges and zero Symbol records
  — this is the load-bearing anti-regression for "embedded struct
  fields produce no Inherits edge."
- `User` structurally satisfies the `Named` interface (it has a
  `Name() string` method); the parser does NOT emit any `Inherits`
  edge for the implicit relationship.

### `server/handler.go`

| Name    | Kind     | Line | Namespace | Parent |
|---------|----------|-----:|-----------|--------|
| handle  | Function |   40 | server    |        |
| withLog | Function |   49 | server    |        |

- 2 symbols (2 Functions)
- 5 edges:
  - 1 `Includes`: `fmt`
  - 4 `Calls`:
    - `handler.go -> Println` — package-level closure assigned to
      `var Logger`. The call's enclosing-fn walk reaches the source
      file root with no `function_declaration` / `method_declaration`
      ancestor, so `from` falls back to the bare file path. Mirrors the
      C++ lambda-at-global-scope rule. CRITICAL anti-regression at the
      corpus level.
    - `handle -> Println`
    - `withLog -> Println` (call inside the inner `func_literal`
      returned by withLog — closure-transparent walk attributes the
      call to `withLog`, NOT to the inner literal)
    - `withLog -> inner` (parameter-name invocation inside the
      returned closure)

### `server/server.go`

| Name             | Kind      | Line | Namespace | Parent |
|------------------|-----------|-----:|-----------|--------|
| Runner           | Interface |   40 | server    |        |
| Server           | Struct    |   46 | server    |        |
| New              | Function  |   53 | server    |        |
| Server::Run      | Method    |   65 | server    | Server |
| Server::Stop     | Method    |   78 | server    | Server |
| Server::Status   | Method    |   88 | server    | Server |
| Server::cleanup  | Method    |   95 | server    | Server |

- 7 symbols (1 Interface, 1 Struct, 1 Function, 4 Methods)
- 8 edges:
  - 2 `Includes`: `fmt`, `code-graph-go-corpus/utils`
  - 6 `Calls`: `New -> Println`, `Server::Run -> handle` (goroutine),
    `Server::Run -> Greet` (package-qualified `utils.Greet`),
    `Server::Stop -> cleanup` (deferred selector call),
    `Server::Stop -> Println`, `Server::Status -> len`
- `Server` has both pointer-receiver methods (`Run`, `Status`,
  `cleanup`) and a value-receiver method (`Stop`) — exercising both
  branches of `extract_receiver_type` against the same parent.
- `*Server` structurally satisfies `Runner`; the parser does NOT emit
  any `Inherits` edge.

### `utils/helpers.go`

| Name    | Kind     | Line | Namespace | Parent |
|---------|----------|-----:|-----------|--------|
| Op      | Typedef  |   27 | utils     |        |
| Handler | Typedef  |   32 | utils     |        |
| Count   | Typedef  |   36 | utils     |        |
| Add     | Function |   39 | utils     |        |
| Mul     | Function |   44 | utils     |        |
| Sub     | Function |   49 | utils     |        |
| Apply   | Function |   54 | utils     |        |
| Greet   | Function |   60 | utils     |        |
| init    | Function |   67 | utils     |        |

- 9 symbols (3 Typedefs, 6 Functions)
- 5 edges:
  - 1 `Includes`: `fmt`
  - 4 `Calls`: `Apply -> op`, `Greet -> Println`, `init -> Add`,
    `init -> Mul`
- `Op` is a `type_alias` (Go 1.9+ `type X = T` form); `Handler` and
  `Count` are `type_spec`s with non-struct/non-interface bodies. All
  three produce `Typedef` symbols — exercises both AST-node forms
  feeding the Typedef branch of `extract_definitions`.
- `init` is extracted as an ordinary Function; the corpus also has
  a second `init` (in `main.go`, namespace `main`). Both coexist as
  distinct Symbol records — the package namespace disambiguates them.

## Namespace Map

| Namespace | Files Contributing                                  | Symbol Count |
|-----------|-----------------------------------------------------|-------------:|
| `main`    | `main.go`                                           |            2 |
| `models`  | `models/deps.go`, `models/repo.go`, `models/user.go`|           22 |
| `server`  | `server/handler.go`, `server/server.go`             |            9 |
| `utils`   | `utils/helpers.go`                                  |            9 |

`empty.go` declares `package main` but contributes 0 symbols, so it
shows up under no namespace bucket.

## Key Validation Points

- **Empty file (only `package` clause)** — `empty.go` parses without
  error and contributes 0 symbols / 0 edges. The package_clause itself
  is consumed silently; only declarations produce Symbol records.
- **Interface implementation produces no edge.** `*Server` satisfies
  `Runner`, `*User` satisfies `Named`, `Memo`/`*KV` satisfy nothing
  named in this corpus — and ZERO `Inherits` edges are emitted. Go
  interfaces are structurally typed.
- **Embedded struct fields produce no `Inherits` edge.** `User` embeds
  `Profile` and `*sync.Mutex`; neither contributes a Symbol or an edge.
- **Interface embedding interface.** `ReadWriter` embeds `Reader` and
  `Repo` embeds `Closer`. The embedded interface line is a `type_elem`
  with a `type_identifier`, not a `method_elem`; both produce 0 extra
  Symbols and 0 edges.
- **Anonymous struct field.** `Memo.Cache` and `Cluster.Endpoints` are
  anonymous structs (no type name); their inner fields are not emitted
  as nested Symbols (the type_spec query requires a `name` field, which
  anonymous structs lack).
- **Blank-identifier function (`func _() {}`).** Go accepts this, and
  tree-sitter-go parses it as a `function_declaration` with
  `name=identifier("_")`. The extractor produces a Function symbol with
  `name="_"`. Pinned by the `models/deps.go` fixture.
- **All import forms.** The grouped `import (...)` in `main.go`
  exercises every form `Phase 6.4` supports — single, aliased
  (`umath "code-graph-go-corpus/utils"`), dot
  (`. "code-graph-go-corpus/models"`), blank (`_ "image/png"`) — plus
  a plain `"fmt"`. Every spec produces one `Includes` edge with the
  module path captured (alias / `.` / `_` are dropped).
- **Backtick-quoted import paths produce zero `Includes` edges.**
  `IMPORT_QUERIES` only matches `interpreted_string_literal`. Pinned
  by `backtick_import_produces_no_includes_edge` in
  `crates/codegraph-lang-go/src/lib.rs`.
- **Pointer + value receivers on the same type.** `Server` has both
  pointer-receiver methods (Run / Status / cleanup) and a value-receiver
  method (Stop). `Memo` has both Get (value) and Hits (pointer).
- **Goroutines and `defer`.** `Server.Run` uses `go handle(...)` and
  `Server.Stop` uses `defer s.cleanup()`. Both produce the expected
  Calls edges naturally because the child of `go_statement` /
  `defer_statement` is a `call_expression` already matched by the
  query.
- **Package-level closure fallback.** `var Logger = func(...) {...}` in
  `server/handler.go` produces a Calls edge whose `from` is the bare
  file path (no enclosing function/method).
- **Closure inside a function body.** `withLog`'s inner `func_literal`
  has two body calls that both attribute to `withLog` — closures are
  transparent in the parent walk when an enclosing function exists.
- **Generic functions (Go 1.18+).** `Map[T any, U any](...)` and
  `Filter[T any](...)` parse without error; the signatures truncate at
  the body opener with the type-parameter lists intact.
- **`init()` function.** `main.go::init` and `utils/helpers.go::init`
  coexist as distinct Symbol records, namespaced by their respective
  packages.
