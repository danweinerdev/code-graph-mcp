//! Tree-sitter query patterns for Rust symbol extraction.
//!
//! Validated against tree-sitter-rust v0.24.2 — `RustParser::new()`
//! returning `Ok(_)` is the gate that proves every query string compiles.
//!
//! These query strings are wired into the `extract_*` loops on
//! `RustParser`.

/// Definition queries: function items (free + impl methods), structs, enums,
/// traits, type aliases, modules, impl-block methods.
///
/// **Deliberate exclusion:** `macro_rules_definition` is intentionally NOT
/// matched. `macro_rules!` definitions are not extracted as symbols; only
/// invocations produce call edges. This matches the documented
/// Rust-plugin limitation.
pub(crate) const DEFINITION_QUERIES: &str = r#"
; Free functions and impl methods both surface as `function_item` whose
; `name` is an `identifier`. The distinction (Function vs Method) is
; resolved at extraction time by walking up to look for an enclosing
; `impl_item` — see helpers::find_enclosing_impl, which implements
; the disambiguation; for `impl Trait for Type { fn m() }` the parent is
; Type, not Trait.
(function_item
  name: (identifier) @func.name) @func.def

; Structs: tuple, named-field, and unit forms all share the struct_item
; node with a `type_identifier` name field.
(struct_item
  name: (type_identifier) @struct.name) @struct.def

; Enums (covers all variant kinds: unit, tuple, struct).
(enum_item
  name: (type_identifier) @enum.name) @enum.def

; Traits.
(trait_item
  name: (type_identifier) @trait.name) @trait.def

; Type aliases: `type Foo = Bar;`
(type_item
  name: (type_identifier) @type.name) @type.def

; Modules: `mod foo { ... }` and `mod foo;`
; The definition extractor walks ancestors to compute Symbol.namespace
; as `a::b::c`.
(mod_item
  name: (identifier) @mod.name) @mod.def
"#;

/// Call queries: direct (free function) calls, method calls via field
/// expressions, scoped calls (`ns::foo()`, `Type::assoc()`), and macro
/// invocations.
pub(crate) const CALL_QUERIES: &str = r#"
; Direct call: foo()
(call_expression
  function: (identifier) @call.name) @call.expr

; Method call: obj.foo() — surfaces via field_expression.
(call_expression
  function: (field_expression
    field: (field_identifier) @call.name)) @call.expr

; Scoped call: ns::foo() or Type::assoc()
(call_expression
  function: (scoped_identifier) @call.qname) @call.expr

; Turbofish: foo::<T>() — `generic_function` wraps either a plain
; identifier or a scoped_identifier.
(call_expression
  function: (generic_function
    function: (identifier) @call.name)) @call.expr

(call_expression
  function: (generic_function
    function: (scoped_identifier) @call.qname)) @call.expr

; Macro invocation: foo!() / println!("..."). The leading identifier
; (without the `!`) is the macro name. Captured here for call
; edges — `macro_rules!` definitions are NOT captured (see
; DEFINITION_QUERIES).
(macro_invocation
  macro: (identifier) @call.name) @call.expr

(macro_invocation
  macro: (scoped_identifier) @call.qname) @call.expr
"#;

/// Use-declaration and `extern crate` queries.
///
/// `use` paths are parsed via tree-sitter's `use_declaration > use_*` family
/// of nodes; full expansion (grouped imports, wildcards, aliases, `self`) is
/// done by [`crate::helpers::split_use_path`]. Here we capture
/// the top-level `use_declaration` and the `argument` field that holds the
/// use-tree; the recursive walker handles the rest.
///
/// `extern_crate_declaration` is the legacy `extern crate <name>;` form,
/// captured for completeness — produces an `Includes` edge to the crate
/// name.
pub(crate) const USE_QUERIES: &str = r#"
; Top-level use declaration. The `argument` field is the use-tree
; (identifier, scoped_identifier, scoped_use_list, use_list, use_wildcard,
; use_as_clause, or self). split_use_path walks it.
(use_declaration
  argument: (_) @use.tree) @use.decl

; extern crate alloc;  →  Edge { to: "alloc", kind: Includes }
(extern_crate_declaration
  name: (identifier) @extern.name) @extern.decl
"#;

/// Inheritance queries: `impl Trait for Type` blocks. Captures both the
/// `type` field (the implementing type) and the `trait` field (the trait
/// being implemented). Inherent impls (`impl Type { ... }`, no trait field)
/// are also matched here but the inheritance extractor only emits an
/// inherits edge when the trait field is present.
pub(crate) const INHERITANCE_QUERIES: &str = r#"
(impl_item
  trait: (_) @impl.trait
  type: (_) @impl.type) @impl.def
"#;
