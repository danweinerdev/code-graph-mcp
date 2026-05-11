//! Tree-sitter query patterns for C# symbol extraction.
//!
//! Validated against tree-sitter-c-sharp v0.23.5 — `CSharpParser::new()`
//! returning `Ok(_)` is the gate that proves every query string compiles.
//!
//! Phase status: Phase 2.2 filled [`DEFINITION_QUERIES`]; Phase 2.3
//! filled [`CALL_QUERIES`]; Phase 2.4 fills [`IMPORT_QUERIES`].
//! [`INHERITANCE_QUERIES`] stays empty until 2.5. Empty query strings
//! compile to a no-op `Query` against any grammar, so the structural
//! smoke test in `lib.rs` passes against the empty set for the
//! still-empty queries.
//!
//! ## C#-specific node-kind notes (tree-sitter-c-sharp 0.23.5)
//!
//! - **Calls** use the `invocation_expression` node kind for all four
//!   forms (direct, member-access, null-conditional, generic); each form
//!   has a different shape on the `function:` field. Constructor calls
//!   use `object_creation_expression` with a `type:` field that may be a
//!   bare `identifier`, a `qualified_name`, or a `generic_name`. Phase
//!   2.3's [`CALL_QUERIES`] documents each shape with its own pattern.
//!   Cast expressions (`(Foo)x`) parse as `cast_expression`, NOT
//!   `invocation_expression`, so no C++-style cast-filter is needed;
//!   `typeof`/`sizeof`/`default`/`checked` similarly have dedicated
//!   expression node kinds and do not trigger spurious call edges.
//! - **`namespace_declaration`** wraps its members in a `body:
//!   (declaration_list ...)` field; nested namespaces (`namespace Outer {
//!   namespace Inner { ... } }`) walk via repeated `namespace_declaration`
//!   ancestors. Dotted names (`namespace A.B.C`) parse with the `name:`
//!   field as a `qualified_name` chain.
//! - **`file_scoped_namespace_declaration`** is a sibling of subsequent
//!   declarations, NOT their ancestor — the namespace ends at end-of-file
//!   semantically, but tree-sitter expresses it as a top-level declaration
//!   with no body. `extract_definitions` checks for the file-scoped form
//!   at compilation_unit level when no `namespace_declaration` ancestor is
//!   found.
//! - **Default interface methods** (per Decision 11's C# follow-up) are
//!   detected by presence of a `body:` field on a `method_declaration`
//!   whose ancestor chain contains an `interface_declaration`. The body
//!   field can be either `(block ...)` or `(arrow_expression_clause ...)`
//!   (`int Foo() => 42`) — both forms count as "has body" and produce a
//!   Function symbol; abstract interface methods (no body field at all)
//!   produce no symbol.
//! - **`enum_declaration`** wraps its members in
//!   `(enum_member_declaration_list (enum_member_declaration ...))`. The
//!   member declarations are NOT extracted as symbols — only the enum
//!   type itself surfaces, matching the C++/Rust/Go/Python convention
//!   for enum constants and the analog of Java Decision 12.

/// Definition query: classes, records, structs, interfaces, enums,
/// methods, constructors, and local functions. Each top-level pattern
/// uses a dedicated capture name so the extractor can dispatch on
/// capture name alone (mirroring the C++/Rust/Go/Python plugins).
///
/// Per-pattern behavior:
///
/// - `class.name` from `class_declaration` → [`SymbolKind::Class`]. The
///   `partial` modifier is NOT inspected at extraction time per Decision 3
///   — each `partial class Foo {}` declaration produces its own Class
///   symbol; merging happens at hierarchy-walk time via the bare-name
///   `from`-field rule.
/// - `record.name` from `record_declaration` → [`SymbolKind::Class`].
///   Parent computed identically to `class.name`. tree-sitter-c-sharp
///   0.23.5 produces a single `record_declaration` node for all record
///   forms — `record User(string n)`, `record class User(string n)`,
///   and `record struct Pt(int x, int y)` all parse to the same node
///   kind. Both class-records and struct-records dispatch to `Class`
///   per Decision 11's C# follow-up (Java Decision 6 analog: records
///   are ordinary class symbols regardless of value-type semantics).
/// - `struct.name` from `struct_declaration` → [`SymbolKind::Struct`].
/// - `interface.name` from `interface_declaration` → [`SymbolKind::Interface`].
/// - `enum.name` from `enum_declaration` → [`SymbolKind::Enum`]. Enum
///   members (`enum_member_declaration` children of the
///   `enum_member_declaration_list` body) are intentionally NOT matched —
///   only the enum type surfaces (Decision 12 analog for C#).
/// - `method.name` from `method_declaration` → [`SymbolKind::Method`] or
///   [`SymbolKind::Function`]. The classification depends on the enclosing
///   scope, computed at extraction time:
///     * Inside `class_declaration` / `struct_declaration` /
///       `record_declaration` → [`SymbolKind::Method`] with parent =
///       enclosing type name.
///     * Inside `interface_declaration` AND with a body present →
///       [`SymbolKind::Function`] (no parent), per Decision 11. The
///       body-presence check happens at extraction time by inspecting the
///       `body:` field on the `method_declaration` node — the query matches
///       all interface methods (with or without bodies) and the extractor
///       drops abstract ones (no body) as forward declarations.
///     * Inside `interface_declaration` AND with no body → no symbol
///       (forward-declaration rule, mirroring C++/Rust/Go).
/// - `ctor.name` from `constructor_declaration` → [`SymbolKind::Method`]
///   with parent = enclosing class/struct/record name. The captured name
///   is the class/struct/record identifier itself (C# constructor
///   syntax).
/// - `local.name` from `local_function_statement` → [`SymbolKind::Function`]
///   with no parent. Local functions are nested inside method bodies and
///   are not members of their enclosing type — treating them as Function
///   (no parent) matches the Python/Go conventions for nested function-
///   shaped declarations.
pub(crate) const DEFINITION_QUERIES: &str = r#"
; class Foo {} / partial class Foo {} / public class Foo<T> {}
(class_declaration
  name: (identifier) @class.name) @class.def

; record User(string n) {} / record class User(string n) {} / record struct Pt(int x, int y) {}
; All three forms produce a single record_declaration node in
; tree-sitter-c-sharp 0.23.5; the extractor maps every form to Class.
(record_declaration
  name: (identifier) @record.name) @record.def

; struct Pt {}
(struct_declaration
  name: (identifier) @struct.name) @struct.def

; interface IFoo {}
(interface_declaration
  name: (identifier) @interface.name) @interface.def

; enum Status { Active, Inactive }
; Members are reached via the body's enum_member_declaration_list children
; but are NOT captured — only the enum type itself surfaces.
(enum_declaration
  name: (identifier) @enum.name) @enum.def

; void Foo() { ... } / static int Bar() => 42 / void Baz();
; Method-vs-Function dispatch and the interface-default-method body check
; happen at extraction time.
(method_declaration
  name: (identifier) @method.name) @method.def

; public Foo() { ... }
; The captured name is the class/struct identifier (C# constructor syntax).
(constructor_declaration
  name: (identifier) @ctor.name) @ctor.def

; void Inner() { ... } inside a method body.
(local_function_statement
  name: (identifier) @local.name) @local.def
"#;

/// Call query: every form of `invocation_expression` (direct, member-
/// access, null-conditional, generic) plus `object_creation_expression`
/// for constructor calls. All forms share the single capture name
/// `call.name` so the extractor dispatches uniformly (mirroring the
/// Python plugin's `call.name` convention).
///
/// Per-pattern shape (verified against tree-sitter-c-sharp 0.23.5 via
/// scratch-crate probe):
///
/// - `(invocation_expression function: (identifier))` — direct call
///   (`Foo()`). The `function:` field is a bare identifier; capture it.
///   Also covers chained-link inner calls and calls inside lambda /
///   LINQ-select / property-getter / field-initializer bodies, since
///   each inner call is its own `invocation_expression`.
/// - `(invocation_expression function: (member_access_expression
///   name: (identifier)))` — member-access call (`obj.Foo()`,
///   `this.Foo()`, `base.Foo()`, namespace-qualified
///   `System.Console.WriteLine()`). Capture the rightmost `name:` field
///   only. Chained calls (`a.B().C()`) produce two invocation_expression
///   matches (one per chain link); each runs through this pattern (or
///   the direct-call pattern for the leaf).
/// - `(invocation_expression function: (conditional_access_expression
///   (member_binding_expression name: (identifier))))` — null-conditional
///   call (`obj?.Foo()`). The `member_binding_expression` is a distinct
///   node kind from `member_access_expression`; the `name:` field on it
///   is the callee identifier.
/// - `(invocation_expression function: (generic_name (identifier)))` —
///   generic call (`Foo<int>()`). Capture only the inner identifier so
///   the recorded `to` is the bare name (`Foo`), NOT `Foo<int>`.
/// - `(object_creation_expression type: (identifier))` — constructor with
///   bare type (`new Foo()`). Per Decision-5-style convention: the edge
///   records `to = "Foo"`; the agent interprets the edge as
///   construction.
/// - `(object_creation_expression type: (qualified_name name:
///   (identifier)))` — constructor with namespace-qualified type
///   (`new System.Foo()`). Capture the rightmost name (`Foo`).
/// - `(object_creation_expression type: (generic_name (identifier)))` —
///   constructor with generic type (`new List<int>()`). Capture the bare
///   inner name (`List`), dropping the type-argument list.
/// - `(object_creation_expression type: (qualified_name name:
///   (generic_name (identifier))))` — constructor with qualified-and-
///   generic type (`new System.Collections.Generic.List<int>()`). Captures
///   the rightmost bare name (`List`).
///
/// Patterns NOT matched (intentional, per the C# grammar):
/// - `cast_expression` (`(Foo)x`) — distinct node kind, never an
///   `invocation_expression`. No filter needed (unlike C++ where casts
///   appear as call_expression and require `is_cpp_cast` filtering).
/// - `typeof(T)`, `sizeof(T)`, `default(T)`, `checked(expr)`,
///   `unchecked(expr)` — each parses as a dedicated expression node
///   (`typeof_expression`, etc.), NOT `invocation_expression`. No filter
///   needed.
/// - `nameof(X)` IS an `invocation_expression function: (identifier)`
///   in tree-sitter-c-sharp 0.23.5 (the grammar treats `nameof` as an
///   ordinary call), so the query DOES match it. The extractor in
///   `lib.rs::extract_calls` filters these matches (a one-line callee-
///   name check) — `nameof` is a compile-time name operator, not a
///   method call, so producing a `Calls` edge to `nameof` would
///   pollute the call graph for every method that uses `nameof` for
///   logging/reflection. Same precedent as the C++ plugin filtering
///   `static_cast` etc.
pub(crate) const CALL_QUERIES: &str = r#"
; Direct call: Foo()
(invocation_expression
  function: (identifier) @call.name)

; Member-access call: obj.Foo() / this.Foo() / base.Foo() / Ns.Type.Method()
; Capture only the rightmost `name:` field — the leftmost subtree
; (the receiver expression chain) carries the rest of the syntactic chain
; but is not the callee identifier.
(invocation_expression
  function: (member_access_expression
    name: (identifier) @call.name))

; Null-conditional call: obj?.Foo()
; The conditional_access_expression's right side is a
; member_binding_expression, not a member_access_expression.
(invocation_expression
  function: (conditional_access_expression
    (member_binding_expression
      name: (identifier) @call.name)))

; Generic call: Foo<int>()
; Capture only the inner identifier so `to` is `Foo` (not `Foo<int>`).
(invocation_expression
  function: (generic_name
    (identifier) @call.name))

; Constructor: new Foo()
(object_creation_expression
  type: (identifier) @call.name)

; Constructor with qualified type: new System.Foo()
(object_creation_expression
  type: (qualified_name
    name: (identifier) @call.name))

; Constructor with generic type: new List<int>()
(object_creation_expression
  type: (generic_name
    (identifier) @call.name))

; Constructor with qualified generic type: new System.Collections.Generic.List<int>()
(object_creation_expression
  type: (qualified_name
    name: (generic_name
      (identifier) @call.name)))
"#;

/// Query for `using_directive` in all forms (plain, `using static`,
/// alias, `global using`, and any combination of those modifiers).
///
/// All five C# `using` forms parse to a single `using_directive` node in
/// tree-sitter-c-sharp 0.23.5 — there is NO separate
/// `global_using_directive` / `static_using_directive` node. The
/// modifier keywords (`global`, `static`) and the alias `=` token are
/// anonymous (non-named) children; only the path nodes and the alias
/// name are named, named children.
///
/// Per-form shape (verified against tree-sitter-c-sharp 0.23.5 via
/// scratch-crate probe):
///
/// - `using System;` → `(using_directive (identifier))` — single
///   `identifier` named child holding the path.
/// - `using System.Collections.Generic;` → `(using_directive
///   (qualified_name ...))` — single `qualified_name` named child
///   holding the dotted path verbatim.
/// - `using static System.Console;` → `(using_directive
///   (qualified_name ...))` — anonymous `static` keyword child precedes
///   the path. Same named-child shape as the dotted form.
/// - `using FooAlias = Some.Long.Type.Name;` → `(using_directive
///   name: (identifier) (qualified_name ...))` — has a `name:` field
///   holding the alias identifier AND a separate unnamed-field named
///   child holding the target path. The `=` token is anonymous.
/// - `global using System.Linq;` → `(using_directive
///   (qualified_name ...))` — anonymous `global` keyword child
///   precedes `using`. Same named-child shape as plain dotted form.
///
/// Combination forms (`global using static X.Y;`, `global using A =
/// X.Y;`) parse to the same `using_directive` node and follow the same
/// rules. The `using` keyword inside a namespace block (`namespace Foo
/// { using Bar; ... }`) ALSO parses as `using_directive` — the query
/// matches it because tree-sitter queries walk the entire tree by
/// default.
///
/// **Single-capture strategy.** We capture the `using_directive` node
/// itself and recover the path text at extraction time. Encoding the
/// path-vs-alias logic in the query would require two patterns with a
/// `!name` negation predicate to distinguish the alias-present from the
/// alias-absent case; tree-sitter-c-sharp 0.23.5 does not assign a
/// stable field name to the path in either form (the path is an
/// anonymous named child in both), so the extractor's identifier /
/// qualified_name traversal is the load-bearing path extraction
/// regardless. One capture name + Rust-side extraction is the simplest
/// design that covers all five forms without query duplication.
///
/// Per Decision 7: the path is recorded verbatim — no resolution
/// against build metadata. `using static System.Console;` records
/// `to = "System.Console"`, NOT `to = "static System.Console"`;
/// `using FooAlias = Some.Long.Type.Name;` records
/// `to = "Some.Long.Type.Name"`, NOT `to = "FooAlias"`. The default
/// `resolve_include` returns `None` for these dotted strings — they are
/// not filesystem paths — so the wire format records the namespace
/// path verbatim and the engine never accidentally resolves it.
pub(crate) const IMPORT_QUERIES: &str = r#"
; using System; / using System.X.Y; / using static System.Console; /
; using FooAlias = Some.Long.Type.Name; / global using System.Linq; /
; global using static System.Math; / global using A = X.Y; /
; and any combination of those modifiers. tree-sitter-c-sharp 0.23.5
; produces a single `using_directive` node for every form; modifier
; keywords (`global`, `static`) and the alias `=` are anonymous
; children. The extractor recovers the dotted path from the named
; identifier / qualified_name children at extraction time.
(using_directive) @using.dir
"#;

/// Query for `base_list` on classes, structs, and interfaces. C# does not
/// syntactically distinguish class extension from interface implementation
/// in the base list — both produce `Inherits` edges per Decision 2. Filled
/// in Phase 2.5.
pub(crate) const INHERITANCE_QUERIES: &str = "";
