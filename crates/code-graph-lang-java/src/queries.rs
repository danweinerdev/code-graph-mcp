//! Tree-sitter query patterns for Java symbol extraction.
//!
//! Validated against tree-sitter-java v0.23.5 — `JavaParser::new()`
//! returning `Ok(_)` is the gate that proves every query string compiles.
//!
//! Phase status: Phase 3.2 fills [`DEFINITION_QUERIES`]; the remaining
//! three constants stay empty until 3.3/3.4/3.5. Empty query strings
//! compile to a no-op `Query` against any grammar, so the structural
//! smoke test in `lib.rs` passes against the empty set.
//!
//! Naming follows the established `*_QUERIES` convention shared with
//! the C++/Rust/Go/Python/C# plugins (plural form, `pub(crate)`).
//!
//! ## Java-specific node-kind notes (tree-sitter-java 0.23.5)
//!
//! - **Top-level types** use `class_declaration`, `interface_declaration`,
//!   `enum_declaration`, and `record_declaration` (Java 14+). All four
//!   carry their identifier in the `name:` field.
//! - **`record_declaration`** wraps its body in a `class_body` node — the
//!   same node kind classes use. Methods inside record bodies surface as
//!   ordinary `method_declaration` children. Per Decision 6 records
//!   extract as `Class`; the `enclosing_type_name` helper recognises
//!   `record_declaration` as a type ancestor so methods inside records
//!   record the record name as parent (NOT as orphan Function symbols —
//!   the same bug C# task 2.2 had to fix in commit `0cf200b`).
//! - **Sealed types**' `permits` clause appears as a `permits:` field on
//!   the type declaration. Per Decision 6 the clause is ignored — no
//!   inheritance edges are produced for it (3.5 will only match
//!   `superclass`/`super_interfaces`/`extends_interfaces`).
//! - **Methods** use `method_declaration` with `name: (identifier)`. The
//!   `body:` field is optional — abstract methods (interface forward
//!   declarations and enum-level abstract methods) lack the field
//!   entirely. The extractor uses body presence as the
//!   forward-declaration discriminator, mirroring the C# plugin.
//! - **Constructors** use `constructor_declaration` with
//!   `name: (identifier)`. The captured name matches the enclosing class
//!   identifier (Java constructor syntax — like C#).
//! - **Default and static interface methods** (Decision 11) are detected
//!   at extraction time by walking the `modifiers` child of the
//!   `method_declaration` node and matching keyword tokens of kind
//!   `"default"` or `"static"`. Modifier kinds in tree-sitter-java 0.23.5
//!   are anonymous keyword tokens whose `kind()` is the literal modifier
//!   text (e.g. a `default` keyword has `kind() == "default"`).
//! - **Anonymous classes** (Decision 4) parse as `object_creation_expression`
//!   with an unnamed `class_body` child appearing AFTER the `argument_list`.
//!   Methods inside the anonymous body are ordinary `method_declaration`
//!   children of that `class_body`. The extractor walks past
//!   `object_creation_expression` boundaries when computing the enclosing
//!   named entity so anonymous-class methods inherit the OUTER named
//!   entity's parent (e.g., `OuterClass`) rather than synthesising an
//!   `Anonymous$1` parent or losing the parent entirely.
//! - **Enum constants with method bodies** (Decision 12) — `enum Planet {
//!   EARTH { double surfaceGravity() {...} }, ... }` — parse as
//!   `enum_constant > class_body > method_declaration`. The extractor
//!   walks past the `enum_constant`/`class_body` boundary the same way it
//!   walks past anonymous classes, so per-constant methods record the
//!   ENUM TYPE (`Planet`) as parent rather than a synthesised
//!   `Planet$EARTH`. Enum-level methods (after the `;`) appear under
//!   `enum_body_declarations > method_declaration` and resolve to the
//!   same enum-type parent via the same walk.
//! - **Enum constants themselves** (`enum_constant` nodes — `EARTH`,
//!   `MARS`, etc.) are NOT extracted as symbols (Decision 12). Only the
//!   enum type and any methods declared inside it surface.

/// Definition query: classes, interfaces, enums, records, methods, and
/// constructors. Each top-level pattern uses a dedicated capture name so
/// the extractor can dispatch on capture name alone (mirroring the
/// C++/Rust/Go/Python/C# plugins).
///
/// Per-pattern behavior:
///
/// - `class.name` from `class_declaration` → [`SymbolKind::Class`]. Parent
///   is the immediate enclosing class/interface/enum/record (or empty for
///   top-level classes; nested types record the immediate outer type).
/// - `interface.name` from `interface_declaration` → [`SymbolKind::Interface`].
/// - `enum.name` from `enum_declaration` → [`SymbolKind::Enum`]. Enum
///   members (`enum_constant` children of the `enum_body`) are
///   intentionally NOT matched (Decision 12) — only the enum type and
///   any declared methods surface.
/// - `record.name` from `record_declaration` → [`SymbolKind::Class`] per
///   Decision 6. The record's component list (the parameters appearing in
///   the declaration syntax `record User(String name)`) parses as
///   `formal_parameters > formal_parameter` and does NOT match
///   `method_declaration` — record components are correctly invisible.
///   Auto-generated members (`name()` accessor, `equals`, `hashCode`,
///   `toString`) are extracted ONLY if they appear in source (synthetic
///   members are not visible to tree-sitter).
/// - `method.name` from `method_declaration` → [`SymbolKind::Method`] or
///   [`SymbolKind::Function`]. The classification depends on the enclosing
///   scope, computed at extraction time:
///     * Inside `class_declaration` / `enum_declaration` /
///       `record_declaration` → [`SymbolKind::Method`] with parent =
///       enclosing type name. The walk skips past
///       `object_creation_expression` boundaries so anonymous-class
///       methods inherit the OUTER named entity's parent (Decision 4),
///       and skips past `enum_constant` boundaries so per-constant
///       methods inherit the enum-type parent (Decision 12).
///     * Inside `interface_declaration` AND with a body → [`SymbolKind::Function`]
///       (no parent), per Decision 11. Body presence is the discriminator
///       (mirroring C#'s rule); both `default void Foo() {...}` and
///       `static void Bar() {...}` qualify, as does any future
///       Java-9+ private interface method with a body.
///     * Inside `interface_declaration` AND with no body → no symbol
///       (forward-declaration rule, mirroring C++/Rust/Go/C#).
///     * Enum-level abstract methods (`abstract double surfaceGravity();`
///       directly inside `enum_body_declarations`, with no body) are
///       skipped under the same forward-declaration rule.
/// - `ctor.name` from `constructor_declaration` → [`SymbolKind::Method`]
///   with parent = enclosing class/record name. The captured name is the
///   class/record identifier itself (Java constructor syntax).
pub(crate) const DEFINITION_QUERIES: &str = r#"
; class Foo {} / public class Foo<T> extends Bar {}
(class_declaration
  name: (identifier) @class.name) @class.def

; interface I {} / public sealed interface Shape permits Circle, Square {}
(interface_declaration
  name: (identifier) @interface.name) @interface.def

; enum Status { Active, Inactive } / enum Planet { EARTH { ... }; abstract ... }
; Enum constants are NOT captured here — only the enum type itself.
(enum_declaration
  name: (identifier) @enum.name) @enum.def

; record User(String name) {} (Java 14+)
; The component list (`(String name)`) parses as formal_parameters and
; does not match method_declaration, so record components stay invisible.
(record_declaration
  name: (identifier) @record.name) @record.def

; void foo() {} / default void doFoo() {} / static int bar() { return 0; }
; Method-vs-Function dispatch and the interface-default-method body check
; happen at extraction time.
(method_declaration
  name: (identifier) @method.name) @method.def

; public Foo() {}
; The captured name is the class/record identifier (Java constructor syntax).
(constructor_declaration
  name: (identifier) @ctor.name) @ctor.def
"#;

/// Call queries: `method_invocation` (direct, member-access, chained),
/// `object_creation_expression`, invocations inside lambdas, anonymous
/// classes, and enum-constant bodies. Filled in 3.3.
pub(crate) const CALL_QUERIES: &str = "";

/// Import queries: `import_declaration` in plain, wildcard, and static
/// forms. Filled in 3.4.
pub(crate) const IMPORT_QUERIES: &str = "";

/// Inheritance queries: `superclass` (extends) and `super_interfaces`
/// (implements) on classes; `extends_interfaces` on interfaces. Sealed
/// types' `permits` clauses intentionally NOT matched. Filled in 3.5.
pub(crate) const INHERITANCE_QUERIES: &str = "";
