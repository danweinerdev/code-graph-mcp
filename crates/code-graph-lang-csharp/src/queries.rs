//! Tree-sitter query patterns for C# symbol extraction.
//!
//! Validated against tree-sitter-c-sharp v0.23.5 — `CSharpParser::new()`
//! returning `Ok(_)` is the gate that proves every query string compiles.
//!
//! Phase status: Phase 2.2 fills in [`DEFINITION_QUERIES`]; the remaining
//! three constants stay empty until 2.3/2.4/2.5. Empty query strings compile
//! to a no-op `Query` against any grammar, so the structural smoke test in
//! `lib.rs` passes against the empty set.
//!
//! ## C#-specific node-kind notes (tree-sitter-c-sharp 0.23.5)
//!
//! - **Calls** use the `invocation_expression` node kind (verified during
//!   Phase 2.2 grammar-probing); constructor calls use
//!   `object_creation_expression`. Phase 2.3 fills [`CALL_QUERIES`].
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
//! - **Default interface methods** (Decision 11) are detected by presence
//!   of a `body:` field on a `method_declaration` whose ancestor chain
//!   contains an `interface_declaration`. The body field can be either
//!   `(block ...)` or `(arrow_expression_clause ...)` (`int Foo() => 42`)
//!   — both forms count as "has body" and produce a Function symbol;
//!   abstract interface methods (no body field at all) produce no symbol.
//! - **`enum_declaration`** wraps its members in
//!   `(enum_member_declaration_list (enum_member_declaration ...))`. The
//!   member declarations are NOT extracted as symbols — only the enum
//!   type itself surfaces, matching the C++/Rust/Go/Python convention
//!   for enum constants and the analog of Java Decision 12.

/// Definition query: classes, structs, interfaces, enums, methods,
/// constructors, and local functions. Each top-level pattern uses a
/// dedicated capture name so the extractor can dispatch on capture name
/// alone (mirroring the C++/Rust/Go/Python plugins).
///
/// Per-pattern behavior:
///
/// - `class.name` from `class_declaration` → [`SymbolKind::Class`]. The
///   `partial` modifier is NOT inspected at extraction time per Decision 3
///   — each `partial class Foo {}` declaration produces its own Class
///   symbol; merging happens at hierarchy-walk time via the bare-name
///   `from`-field rule.
/// - `struct.name` from `struct_declaration` → [`SymbolKind::Struct`].
/// - `interface.name` from `interface_declaration` → [`SymbolKind::Interface`].
/// - `enum.name` from `enum_declaration` → [`SymbolKind::Enum`]. Enum
///   members (`enum_member_declaration` children of the
///   `enum_member_declaration_list` body) are intentionally NOT matched —
///   only the enum type surfaces (Decision 12 analog for C#).
/// - `method.name` from `method_declaration` → [`SymbolKind::Method`] or
///   [`SymbolKind::Function`]. The classification depends on the enclosing
///   scope, computed at extraction time:
///     * Inside `class_declaration` / `struct_declaration` →
///       [`SymbolKind::Method`] with parent = enclosing type name.
///     * Inside `interface_declaration` AND with a body present →
///       [`SymbolKind::Function`] (no parent), per Decision 11. The
///       body-presence check happens at extraction time by inspecting the
///       `body:` field on the `method_declaration` node — the query matches
///       all interface methods (with or without bodies) and the extractor
///       drops abstract ones (no body) as forward declarations.
///     * Inside `interface_declaration` AND with no body → no symbol
///       (forward-declaration rule, mirroring C++/Rust/Go).
/// - `ctor.name` from `constructor_declaration` → [`SymbolKind::Method`]
///   with parent = enclosing class/struct name. The captured name is the
///   class/struct identifier itself (C# constructor syntax).
/// - `local.name` from `local_function_statement` → [`SymbolKind::Function`]
///   with no parent. Local functions are nested inside method bodies and
///   are not members of their enclosing type — treating them as Function
///   (no parent) matches the Python/Go conventions for nested function-
///   shaped declarations.
///
/// **Records intentionally NOT matched.** `record_declaration` is a
/// distinct node kind in tree-sitter-c-sharp 0.23.5; this task's verification
/// scope explicitly enumerates the seven supported declarations, and
/// records are not among them. Adding record support is a follow-up.
pub(crate) const DEFINITION_QUERIES: &str = r#"
; class Foo {} / partial class Foo {} / public class Foo<T> {}
(class_declaration
  name: (identifier) @class.name) @class.def

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

/// Query for `invocation_expression` and `object_creation_expression`
/// (constructor calls). Filled in Phase 2.3.
pub(crate) const CALL_QUERIES: &str = "";

/// Query for `using_directive` in all forms (plain, `using static`,
/// alias, `global using`). Filled in Phase 2.4.
pub(crate) const IMPORT_QUERIES: &str = "";

/// Query for `base_list` on classes, structs, and interfaces. C# does not
/// syntactically distinguish class extension from interface implementation
/// in the base list — both produce `Inherits` edges per Decision 2. Filled
/// in Phase 2.5.
pub(crate) const INHERITANCE_QUERIES: &str = "";
