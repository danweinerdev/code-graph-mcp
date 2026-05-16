//! Tree-sitter query patterns for Python symbol extraction.
//!
//! Validated against tree-sitter-python v0.25.0 — `PythonParser::new()`
//! returning `Ok(_)` is the gate that proves every query string compiles.
//!
//! These query strings and the `PythonParser::new()` compile gate are
//! wired into per-extractor methods on `PythonParser` (definitions,
//! calls, imports, inheritance).
//!
//! ## Python-specific node-kind notes (tree-sitter-python 0.25.0)
//!
//! - **Calls use the `call` node kind**, NOT `call_expression` (which is
//!   what every other tree-sitter grammar in this workspace uses). Getting
//!   this wrong is the textbook Python tree-sitter footgun.
//! - **`async def` parses as `function_definition`**, not as a separate
//!   `async_function_definition` node. The leading `async` keyword is a
//!   sibling token of the `function_definition` (in the `decorated_definition`
//!   wrapper or directly as part of the `function_definition`'s leading
//!   children). The definition-extraction fixture confirms this; the
//!   query targets `function_definition` and trusts the grammar.
//! - **`@decorator` wraps the inner definition** in a `decorated_definition`
//!   node whose `definition` field is the `class_definition` /
//!   `function_definition`. tree-sitter queries match the entire tree, so
//!   matching the inner node directly is enough — decorator presence does
//!   NOT block extraction.
//! - **`from foo.bar import baz`** parses as `import_from_statement` with
//!   a `module_name` field of kind `dotted_name` (or `relative_import` for
//!   `from . import x`). The dependency edge points at the *module*, not at
//!   the imported symbol — `extract_imports` (7.4) reads the `module_name`
//!   field, NOT the `name` field.
//! - **`class D(B):`** parses as `class_definition` with a `superclasses`
//!   field of kind `argument_list`. Each base appears as a child of the
//!   argument_list — typically `identifier` for bare names, `attribute`
//!   for qualified bases like `module.Base`, and `keyword_argument` for
//!   `metaclass=Meta` style kwargs (which are NOT bases and are filtered
//!   out by the inheritance extractor).

/// Definition queries: top-level `function_definition` and
/// `class_definition`. The function-vs-method distinction is computed at
/// extraction time (a `function_definition` whose ancestor chain
/// contains a `class_definition` is a Method; otherwise it is a Function),
/// so we do not try to encode that distinction in the query string.
///
/// `async def` parses as `function_definition` in tree-sitter-python 0.25,
/// so a single query covers both sync and async forms.
///
/// Decorated definitions are reached via the same query: tree-sitter
/// queries search the whole tree, so a `function_definition` nested inside
/// `decorated_definition` matches without needing a separate pattern.
pub(crate) const DEFINITION_QUERIES: &str = r#"
; Function or async function: def f(...): ... / async def f(...): ...
; Method-vs-function disambiguation happens at extraction time by walking
; the ancestor chain for a `class_definition`.
(function_definition
  name: (identifier) @func.name) @func.def

; Class: class Foo: ... / class Foo(Base): ... / class Foo(A, B, metaclass=M): ...
; Base extraction (the `superclasses` argument_list) lives in
; INHERITANCE_QUERIES so kind dispatch and base-name capture stay in
; separate query strings — matches the C++/Rust/Go split between
; DEFINITION_QUERIES and INHERITANCE_QUERIES.
(class_definition
  name: (identifier) @class.name) @class.def
"#;

/// Call queries: direct calls (`foo()`) and attribute-call calls
/// (`obj.method()`, `module.func()`, chained `a.b().c()`).
///
/// **Python's tree-sitter uses node kind `call`, NOT `call_expression`.**
/// This is the most common mistake when authoring Python queries from
/// muscle memory of the C++/Rust/Go grammars.
///
/// Per-pattern behavior:
/// - `(call function: (identifier))` — direct call: `foo()`. The captured
///   identifier IS the callee name.
/// - `(call function: (attribute attribute: (identifier)))` — attribute
///   call: `obj.method()`, `module.func()`, chained `a.b().c()`. We
///   capture the *trailing* identifier (the field name `attribute`), so
///   for `obj.method()` we record `method`, and for chained `a.b().c()`
///   each chain link is its own `call` node and contributes its own edge
///   (one for `b`, one for `c`). This mirrors the Go selector-expression
///   handling in `code-graph-lang-go::queries::CALL_QUERIES`.
///
/// Constructor calls (`MyClass()`) are direct calls with `MyClass` as the
/// identifier — the extractor records `to = "MyClass"`. The agent is
/// expected to interpret class-named call edges as construction.
///
/// `super()` is also a direct call (To = "super"). Built-in calls (`print`,
/// `len`) likewise. Calls inside list comprehensions, lambdas, and default
/// arguments parse as ordinary `call` nodes nested under their enclosing
/// statement, so the queries match them naturally; the `from` of each edge
/// is computed by [`crate::helpers::enclosing_function_id`] at extraction
/// time.
///
/// Consistent with the C++/Rust/Go plugins, we deliberately bind only
/// `@call.name` here rather than a dead `@call.expr`-style outer capture.
/// The enclosing `call` node is reachable via
/// `find_enclosing_kind(.., "call")` at extraction time when the call
/// extractor needs to re-anchor the line.
pub(crate) const CALL_QUERIES: &str = r#"
; Direct call: foo() / MyClass() / super() / print(...)
(call
  function: (identifier) @call.name)

; Attribute call: obj.method() / module.func() / a.b().c() (one match per link)
(call
  function: (attribute
    attribute: (identifier) @call.name))
"#;

/// Import queries: `import foo`, `import foo.bar`, `import foo as f`, and
/// `from foo import bar`, `from foo.bar import baz`, `from . import x`,
/// `from foo import (a, b)`, `from foo import *`, plus the special
/// `from __future__ import annotations` form.
///
/// **Field reference (tree-sitter-python 0.25.0):**
/// - `import_statement` → `name: (dotted_name | aliased_import)+` (multiple
///   names possible: `import a, b`). For `aliased_import` the underlying
///   path is in the `name` field of the alias node; the alias name itself
///   is in `alias`. We drop the alias and record the path — same rule as
///   the Go and Rust plugins.
/// - `import_from_statement` → `module_name: (dotted_name | relative_import)`
///   carries the module path; the `name` field carries the imported
///   symbol(s) but is NOT the dependency target (the *module* is what we
///   depend on, not the symbol). The import extractor reads `module_name`
///   and ignores the `name` field by design.
/// - `future_import_statement` → tree-sitter-python parses
///   `from __future__ import annotations` as a **distinct node kind**,
///   NOT as an `import_from_statement`. The grammar special-cases the
///   dunder module because `__future__` imports have unique compile-time
///   semantics. The node has a `name:` field carrying the imported
///   feature(s) but no `module_name:` field — the module is implicit.
///   We match it directly and synthesize the dependency target as the
///   string `"__future__"` (no field text needed).
///
/// We capture only the path nodes — `@import.module` from
/// `import_statement`, `@import.from_module` /
/// `@import.from_module_relative` from `import_from_statement`, and
/// `@import.future` from `future_import_statement`. The import extractor
/// walks each capture to recover the dotted path (handling `dotted_name`
/// directly, `relative_import` for `from . import x` style imports —
/// preserved verbatim, including leading dots — and the
/// `future_import_statement` form by emitting a fixed `__future__` edge).
///
/// We do NOT bind a dead `@import.stmt` capture on the outer statement —
/// the line is recovered by walking up to the enclosing import-statement
/// node at extraction time.
pub(crate) const IMPORT_QUERIES: &str = r#"
; import foo / import foo.bar / import foo as f / import a, b
; The `name` field is the dotted_name path or an aliased_import wrapping a
; dotted_name. The import extractor walks each capture to extract the
; path text (alias dropped).
(import_statement
  name: (dotted_name) @import.module)

; import foo as f — aliased form; capture the inner name (path), drop alias.
(import_statement
  name: (aliased_import
    name: (dotted_name) @import.module))

; from foo import bar / from foo.bar import baz — `module_name` is the
; dependency target; the `name` field (the imported symbols) is NOT.
(import_from_statement
  module_name: (dotted_name) @import.from_module)

; from . import utils / from .. import x — relative import. The import
; extractor records the relative_import text verbatim (e.g. `.utils` if a
; dotted_name follows
; the dots, or combines the dots with the imported name(s) for the
; dots-only form `from . import utils`). Captured here as a separate
; capture name so the extractor branches on relative-vs-absolute form.
(import_from_statement
  module_name: (relative_import) @import.from_module_relative)

; from __future__ import annotations — tree-sitter-python uses a distinct
; `future_import_statement` node kind. The synthetic capture lets the
; extractor emit a single edge with `to = "__future__"` regardless of
; which feature(s) are imported.
(future_import_statement) @import.future
"#;

/// Inheritance queries: `class_definition` with a `superclasses` argument
/// list. We capture both `identifier` bases (`class D(B):`) and `attribute`
/// bases (`class D(module.Base):`); `keyword_argument` children of the
/// argument_list (e.g. `metaclass=Meta`) are NOT captured — they parse as
/// keyword arguments, not as base classes, and the `superclasses` argument
/// list is the only place metaclass kwargs appear in tree-sitter-python.
///
/// The inheritance extractor reads each capture's text directly (for
/// `identifier`) or via a small descend (for `attribute` — joining `object` and
/// `attribute` fields with `.`) to form the `to` of each `Inherits` edge.
/// Multiple bases produce multiple matches naturally (one capture per base
/// in the argument_list).
///
/// `class C:` (no parens, no superclasses field) produces zero matches —
/// nothing to capture — so the inheritance extractor emits zero edges by
/// construction.
pub(crate) const INHERITANCE_QUERIES: &str = r#"
; class D(B): / class D(A, B): — bare-identifier bases.
(class_definition
  name: (identifier) @class.name
  superclasses: (argument_list
    (identifier) @base.name))

; class D(module.Base): — qualified bases.
(class_definition
  name: (identifier) @class.name
  superclasses: (argument_list
    (attribute) @base.attr))
"#;
