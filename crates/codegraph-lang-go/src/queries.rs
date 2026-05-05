//! Tree-sitter query patterns for Go symbol extraction.
//!
//! Validated against tree-sitter-go v0.25.0 — `GoParser::new()` returning
//! `Ok(_)` is the gate that proves every query string compiles.
//!
//! Phase status: Phase 6.1 ships these query strings. Phase 6.2/6.3/6.4 wire
//! them into `extract_*` loops on `GoParser`.

/// Definition queries: free functions (`function_declaration`), methods on
/// receivers (`method_declaration`), and named type declarations
/// (`type_spec` / `type_alias`) — covering struct, interface, and other
/// named-type forms.
///
/// **Field reference (tree-sitter-go 0.25.0):**
/// - `function_declaration` → `name: identifier`, optional `type_parameters`,
///   `parameters`, optional `result`, optional `body`. Generic functions
///   (Go 1.18+, `func Map[T any](...)`) carry the `type_parameters` field.
/// - `method_declaration` → `receiver: parameter_list`, `name: field_identifier`,
///   `parameters`, optional `result`, optional `body`. The receiver is always a
///   `parameter_list` containing one `parameter_declaration`.
/// - `type_spec` → `name: type_identifier`, `type: _type` (e.g. `struct_type`,
///   `interface_type`, or any other type form). This is the classic
///   `type Foo struct { ... }` shape.
/// - `type_alias` → `name: type_identifier`, `type: _type`. This is the Go 1.9+
///   alias form `type ID = string`. (Distinct AST node from `type_spec`.)
/// - `package_clause` → has a single named child of kind `package_identifier`
///   (no fields). Phase 6.2 walks this to populate `Symbol.namespace`.
pub(crate) const DEFINITION_QUERIES: &str = r#"
; Free function: func foo() { ... }
(function_declaration
  name: (identifier) @func.name) @func.def

; Method with receiver: func (s *Server) M() { ... } or func (s Server) M() { ... }
; The receiver type is extracted from the receiver field by helpers::extract_receiver_type
; in Phase 6.2 (handles both pointer_type and value type_identifier forms).
(method_declaration
  receiver: (parameter_list) @method.receiver
  name: (field_identifier) @method.name) @method.def

; Named type declarations (struct, interface, or other named type bodies).
; The body kind (struct_type, interface_type, or anything else) is dispatched
; on at extraction time in Phase 6.2 to produce Struct / Interface / Typedef
; symbol kinds respectively.
(type_spec
  name: (type_identifier) @type.name
  type: (_) @type.body) @type.def

; Type aliases: `type ID = string` (Go 1.9+) — separate AST node from type_spec.
(type_alias
  name: (type_identifier) @alias.name
  type: (_) @alias.body) @alias.def

; Package clause: `package foo` — captured so Phase 6.2 can populate
; Symbol.namespace from it. Note: package_clause has no named fields; the
; package_identifier is a direct named child.
(package_clause
  (package_identifier) @package.name) @package.def
"#;

/// Call queries: direct calls (`foo()`) and selector-expression calls
/// (`obj.Method()`, `pkg.Func()`, chained `a.B().C()`).
///
/// **Field reference (tree-sitter-go 0.25.0):**
/// - `call_expression` → `function: _expression`, `arguments: argument_list`.
///   The `function` field can be an `identifier` (direct), a
///   `selector_expression` (method or package-qualified), or various other
///   expression forms.
/// - `selector_expression` → `operand: _expression`, `field: field_identifier`.
///   For both `obj.Method()` and `fmt.Println()` the captured `field` is the
///   trailing identifier — the receiver/package name is in `operand`.
///
/// `go` and `defer` statements wrap a `call_expression` directly, so the same
/// queries naturally cover `go foo()` and `defer conn.Close()`.
///
/// Only `@call.name` is captured — `extract_calls` consumes that single
/// capture and re-anchors the line by walking up to the enclosing
/// `call_expression` via [`crate::find_enclosing_kind`]. We deliberately do
/// not bind a `@call.expr` capture on the outer `call_expression`: it would
/// be emitted on every match but never read, so it is dead weight.
pub(crate) const CALL_QUERIES: &str = r#"
; Direct call: foo()
(call_expression
  function: (identifier) @call.name)

; Method or package-qualified call: obj.Method() / fmt.Println() / a.B().C()
; Each chain link is its own call_expression with its own selector_expression,
; so chained calls naturally produce one edge per link.
(call_expression
  function: (selector_expression
    field: (field_identifier) @call.name))
"#;

/// Import queries: `import_spec` carrying an `interpreted_string_literal`
/// path. Covers single-line `import "fmt"`, grouped `import ( "fmt"; "os" )`,
/// aliased `import f "fmt"`, dot `import . "testing"`, and blank
/// `import _ "image/png"` forms — they all parse as `import_spec` with the
/// same `path` field.
///
/// Quote stripping happens at extraction time in Phase 6.4
/// (`interpreted_string_literal` text includes the surrounding `"`s).
///
/// `raw_string_literal` (backtick-quoted import paths) is a valid grammar
/// alternative for the `path` field but is not idiomatic Go and is not
/// produced by `gofmt`; capturing only `interpreted_string_literal` matches
/// the documented brief. The Phase 6.5 anti-regression test
/// `backtick_import_produces_no_includes_edge` pins this behavior.
///
/// Only `@import.path` is captured. `extract_imports` consumes the path
/// capture and re-anchors the line by walking up to the enclosing
/// `import_declaration` via [`crate::find_enclosing_kind`]. We deliberately
/// do not bind a `@import.spec` capture on the outer `import_spec`: it
/// would be emitted on every match but never read, so it is dead weight —
/// same rationale that removed `@call.expr` from `CALL_QUERIES` in 6.4.
pub(crate) const IMPORT_QUERIES: &str = r#"
(import_spec
  path: (interpreted_string_literal) @import.path)
"#;
