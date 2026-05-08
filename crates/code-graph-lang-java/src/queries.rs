//! Tree-sitter query patterns for Java symbol extraction.
//!
//! Validated against tree-sitter-java v0.23.5 — `JavaParser::new()`
//! returning `Ok(_)` is the gate that proves every query string compiles.
//!
//! Phase status: Phase 3.1 ships these as empty placeholders. Tasks
//! 3.2/3.3/3.4/3.5 fill in the real query bodies and wire them into
//! per-extractor methods on `JavaParser` (definitions, calls, imports,
//! inheritance).
//!
//! Naming follows the established `*_QUERIES` convention shared with
//! the C++/Rust/Go/Python plugins (plural form, `pub(crate)`), even
//! though the 3.1 brief sketched `DEFINITION_QUERY` (singular). The
//! convention wins so future readers grepping across plugins find
//! consistent symbol names.

/// Definition queries: `class_declaration`, `interface_declaration`,
/// `enum_declaration`, `record_declaration` (Java 14+),
/// `method_declaration`, `constructor_declaration`. Filled in 3.2.
pub(crate) const DEFINITION_QUERIES: &str = "";

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
