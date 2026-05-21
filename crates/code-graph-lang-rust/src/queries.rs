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
; Free functions, impl methods, and trait default methods all surface as
; `function_item` whose `name` is an `identifier`. The classification
; (Function vs Method, and the parent string) is resolved at extraction
; time by helpers::find_nearest_def_ancestor — a single composite walk
; that returns whichever of `impl_item` or `trait_item` it hits first.
; For `impl Trait for Type { fn m() }` the impl branch fires and the
; parent is Type (never Trait); for `trait T { fn m(&self) {} }` the
; trait branch fires and the parent is T.
(function_item
  name: (identifier) @func.name) @func.def

; Abstract trait method declarations (`fn f(&self);` with NO body) parse
; as `function_signature_item`, not `function_item`. We capture them so
; the definition extractor can emit a Method symbol when their nearest
; definition ancestor is a `trait_item` — see
; helpers::find_nearest_def_ancestor. Bare signatures OUTSIDE any
; trait (e.g. inside `extern "C"` blocks) remain excluded from the
; symbol set: the extractor gates emission on the trait-ancestor branch
; of the dispatch, preserving the cross-language "forward declarations
; excluded" invariant everywhere except trait method declarations.
(function_signature_item
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

; Modules: `mod foo { ... }` and `mod foo;`. Captured here as a
; namespace anchor only — no Symbol is emitted (the extractor's
; `mod.name` branch is intentionally empty). Inline-mod ancestor walks
; populate `Symbol.namespace` (`a::b::c`) on symbols *inside* a `mod`
; block via `resolve_mod_namespace`. File-level `Includes` edges for
; external `mod foo;` declarations are emitted by the separate
; `MOD_DECL_QUERIES` / `extract_mod_decls` path.
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

/// Supertrait-bound query: `trait Sub: Super1 + Super2 { … }`.
///
/// Matches every `trait_item` whose `bounds` field is set (i.e. the
/// sub-trait declares `: Super1 + … + SuperN`). Captures:
///
/// - `@sub.name` — the sub-trait's name (the `type_identifier` in the
///   `name` field of `trait_item`). One per match.
/// - `@sub.bound` — each individual bound node inside the `trait_bounds`
///   list. The `(_)` child wildcard fires once per `+`-separated bound,
///   so a trait with N supertrait bounds produces N matches sharing the
///   same `@sub.name` but each carrying a distinct `@sub.bound`.
///
/// The captured `@sub.bound` node kind drives downstream filtering — the
/// `trait_bounds` rule (per the tree-sitter-rust grammar) admits three
/// alternation arms:
///
/// 1. `_type` (the type supertype) — the bound is a nameable trait when
///    the concrete kind is `type_identifier` (`Super`),
///    `scoped_type_identifier` (`module::Super`), or `generic_type`
///    (`Super<T>`). One bound under `_type` is a `removed_trait_bound`
///    (`?Sized`, `?Send`, …) — that variant carries the same `_type`
///    supertype umbrella but represents an opt-OUT marker, not a real
///    supertrait, so it must NOT emit an `Inherits` edge.
/// 2. `higher_ranked_trait_bound` — `for<'a> Foo<'a>`. We dig into the
///    inner `type` field for the actual nameable bound (typically a
///    `generic_type` like `Foo<'a>` → base name `Foo`).
/// 3. `lifetime` — `'a`, `'static`. Lifetimes are not traits and must NOT
///    emit an `Inherits` edge.
///
/// Filtering of (1, `?Sized`) / (3, lifetimes) happens in the extractor
/// loop (`extract_inheritance`'s supertrait branch), not in the query —
/// the query stays simple and the filter rules live next to the edge
/// emission they govern.
///
/// This query is deliberately separate from [`INHERITANCE_QUERIES`]: that
/// query matches `impl_item` and the extractor's `impl` loop accumulates
/// a `(type, trait)` pair within one match. A `trait_item` with N
/// supertrait bounds yields N `(sub, super)` pairs — one match per bound
/// — which the existing accumulate-then-emit shape cannot express. Each
/// query is paired with its own dedicated loop so neither shape leaks
/// into the other.
pub(crate) const SUPERTRAIT_QUERY: &str = r#"
(trait_item
  name: (type_identifier) @sub.name
  bounds: (trait_bounds
    (_) @sub.bound))
"#;

/// Module-declaration query, feeding [`crate::RustParser::extract_mod_decls`].
///
/// Captures every `mod_item` along with its `name` identifier. The
/// extractor uses the captured `mod_item`'s presence/absence of a `body`
/// field (set when the source is `mod foo { … }`, absent when the source
/// is `mod foo;`) to decide whether to emit an `Includes` edge:
///
/// - External `mod foo;` (no body) → emit one `Includes` edge with
///   `from` = the declaring file path, `to` = the bare modname token, and
///   `line` = the `mod_item`'s start row (1-indexed). The bare token is
///   provisional; whole-graph resolution to a concrete child file lives
///   in [`crate::RustParser::post_index`].
/// - Inline `mod foo { … }` (has body) → no edge: the contents live in
///   the same file, so a self-edge would be a no-op at best and noise at
///   worst.
///
/// The same `mod_item` nodes are also matched by `DEFINITION_QUERIES`
/// (the `@mod.def`/`@mod.name` pair there is **namespace-anchor only** —
/// no Symbol is emitted; `resolve_mod_namespace` walks inline-mod
/// ancestors of inner symbols to populate `Symbol.namespace`). This query
/// stays independent of that one so the namespace-anchor mechanism in
/// `extract_definitions` is unaffected by edge-emission changes here.
pub(crate) const MOD_DECL_QUERIES: &str = r#"
(mod_item
  name: (identifier) @mod.name) @mod.decl
"#;
