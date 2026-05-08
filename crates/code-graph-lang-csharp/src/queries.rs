//! Tree-sitter query patterns for C# symbol extraction.
//!
//! Validated against tree-sitter-c-sharp v0.23.5 — `CSharpParser::new()`
//! returning `Ok(_)` is the gate that proves every query string compiles.
//!
//! Phase status: Phase 2.1 ships these as empty placeholders; Phases
//! 2.2/2.3/2.4/2.5 fill them in (definitions, calls, imports, inheritance).
//! Empty query strings compile to a no-op `Query` against any grammar, so
//! the structural smoke test in `lib.rs` passes against the empty set.

/// Query for class/struct/interface/enum/method/constructor/free-function
/// definitions. Filled in Phase 2.2.
pub const DEFINITION_QUERY: &str = "";

/// Query for `invocation_expression` and `object_creation_expression`
/// (constructor calls). Filled in Phase 2.3.
pub const CALL_QUERY: &str = "";

/// Query for `using_directive` in all forms (plain, `using static`,
/// alias, `global using`). Filled in Phase 2.4.
pub const IMPORT_QUERY: &str = "";

/// Query for `base_list` on classes, structs, and interfaces. C# does not
/// syntactically distinguish class extension from interface implementation
/// in the base list — both produce `Inherits` edges per Decision 2. Filled
/// in Phase 2.5.
pub const INHERITANCE_QUERY: &str = "";
