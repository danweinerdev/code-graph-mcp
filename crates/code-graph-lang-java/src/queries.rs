//! Tree-sitter query patterns for Java symbol extraction.
//!
//! Validated against tree-sitter-java v0.23.5 — `JavaParser::new()`
//! returning `Ok(_)` is the gate that proves every query string compiles.
//!
//! [`DEFINITION_QUERIES`], [`CALL_QUERIES`], [`IMPORT_QUERIES`], and
//! [`INHERITANCE_QUERIES`] are all populated and live — the Java plugin's
//! query surface is complete.
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
//!   the same bug the C# parser fixed in commit `0cf200b`).
//! - **Sealed types**' `permits` clause appears as a `permits:` field on
//!   the type declaration. Per Decision 6 the clause is ignored — no
//!   inheritance edges are produced for it (inheritance extraction only
//!   matches `superclass`/`super_interfaces`/`extends_interfaces`).
//! - **Methods** use `method_declaration` with `name: (identifier)`. The
//!   `body:` field is optional — abstract methods (interface forward
//!   declarations and enum-level abstract methods) lack the field
//!   entirely. The extractor uses body presence as the
//!   forward-declaration discriminator, mirroring the C# plugin.
//! - **Constructors** use `constructor_declaration` with
//!   `name: (identifier)`. The captured name matches the enclosing class
//!   identifier (Java constructor syntax — like C#).
//! - **Default and static interface methods** (Decision 11) are
//!   classified at extraction time by **presence of the `body:` field**
//!   on `method_declaration` — the same discriminator the C# plugin
//!   uses. Any `method_declaration` inside an `interface_declaration`
//!   that has a body (regardless of `default`, `static`, or Java-9+
//!   `private` modifier) extracts as `Function` (no parent); any method
//!   without a body is a forward declaration and is skipped. The body-
//!   presence rule subsumes the modifier check cleanly and covers the
//!   Java-9+ private interface method case the brief did not enumerate.
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
//! - **`import_declaration`** has NO fields (`fields: {}` in
//!   tree-sitter-java 0.23.5's `node-types.json`) and exactly three
//!   possible named-child kinds: `identifier` (single-segment imports
//!   like `import Foo;`), `scoped_identifier` (dotted-path imports —
//!   the most common form), and `asterisk` (wildcard — appears as a
//!   sibling of the path, separated by an anonymous `.` token, in
//!   `import com.foo.*;` and `import static com.foo.Bar.*;`). The
//!   `static` modifier is an anonymous keyword child (kind `"static"`,
//!   `is_named() == false`) and is automatically excluded by any
//!   named-children walk — no special filter is needed in the extractor.
//!   For static-field imports (`import static com.foo.Bar.X;`) the
//!   field name (`X`) folds INTO the `scoped_identifier`'s text, so the
//!   captured path is `com.foo.Bar.X` directly — no reconstruction
//!   required for the non-wildcard static form. Wildcard forms
//!   reconstruct as `<path>.*` at extraction time by appending `.*` to
//!   the named path child's text when a sibling `asterisk` is present.

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

/// Call query: every form of `method_invocation` (direct, member-access,
/// chained, generic), plus `object_creation_expression` for `new T()`
/// constructor calls, `explicit_constructor_invocation` for `this(...)`
/// / `super(...)` chaining inside constructor bodies, and `method_reference`
/// for identifier-on-RHS form (`String::length`, `obj::method`, `this::doIt`,
/// `super::doIt`). All forms share the single capture name `call.name`
/// so the extractor dispatches uniformly (mirroring the C# plugin's
/// `call.name` convention).
///
/// Per-pattern shape (verified against tree-sitter-java 0.23.5 via
/// scratch-crate probe):
///
/// - `(method_invocation name: (identifier))` — covers direct (`foo()`),
///   member-access (`obj.foo()`), and generic (`obj.<T>foo()`) call forms
///   in a single pattern. The `name:` field on `method_invocation` always
///   points at the rightmost bare identifier; the `object:` (receiver)
///   and `type_arguments:` fields are part of the node but not captured.
///   Chained calls (`a.b().c()`) parse as nested `method_invocation`s —
///   the outer query matches both levels independently, producing one
///   edge per chain link (`b` and `c`).
/// - `(object_creation_expression type: (type_identifier))` — bare
///   constructor (`new Foo()`). Records `to = "Foo"`.
/// - `(object_creation_expression type: (generic_type (type_identifier)))` —
///   generic constructor (`new ArrayList<Integer>()`). Captures only the
///   bare type_identifier inside the `generic_type`, dropping
///   `type_arguments`.
/// - `(object_creation_expression type: (scoped_type_identifier (type_identifier) @x .))` —
///   qualified constructor (`new java.util.ArrayList()`). The anchor `.`
///   pins the capture to the rightmost `type_identifier` of the
///   `scoped_type_identifier` chain (e.g., `ArrayList`, not `java` or
///   `util`).
/// - `(object_creation_expression type: (generic_type (scoped_type_identifier (type_identifier) @x .)))` —
///   qualified-and-generic constructor (`new java.util.ArrayList<Integer>()`).
///   Captures the rightmost bare name (`ArrayList`), dropping both the
///   namespace chain and the type arguments.
/// - `(explicit_constructor_invocation constructor: _ @call.name)` —
///   constructor chaining (`this(...)` / `super(...)` inside a
///   constructor body). The `constructor:` field is the bare `this` or
///   `super` keyword node — its `kind()` is the literal string `"this"`
///   or `"super"`, and its text is the same. Recorded with
///   `to = "this"` or `to = "super"`: these ARE genuine constructor
///   invocations and SHOULD produce call edges.
/// - `(method_reference "::" (identifier) @call.name)` — method reference
///   with an identifier on the RHS. The `"::"` anchor matches the
///   `::` token; the captured `identifier` is the right-hand-side name
///   (`String::length` → `length`, `obj::method` → `method`,
///   `this::doIt` → `doIt`, `super::doIt` → `doIt`).
///
/// Patterns NOT matched (intentional or documented limitations):
/// - `method_reference` with a `new` keyword on the RHS (constructor
///   references, e.g. `ArrayList::new`, `Foo::new`). The grammar lays out
///   `method_reference` as `(method_reference <lhs> :: <rhs>)`, where
///   `<rhs>` is the `new` keyword token for constructor references — its
///   node kind is `"new"`, not `"identifier"`. The pattern
///   `(method_reference "::" (identifier) @call.name)` cleanly skips
///   these. Documented as a known limitation rather than over-engineering
///   a second capture pattern: an agent asking "what does this code
///   construct via a method reference?" gets no edge; the same agent
///   asking "what `new T()` calls exist?" gets the `object_creation`
///   edges. The asymmetry mirrors the brief's "discover-and-document is
///   acceptable; over-engineering an unusable query is not."
/// - Cast expressions (`(String) o`) parse as `cast_expression`, NOT
///   `method_invocation` or `object_creation_expression`. No filter
///   needed (unlike C++ where casts appear as call_expression and require
///   `is_cpp_cast` filtering).
/// - `instanceof`, `synchronized`, `switch_expression`, and the rest of
///   Java's keyword-led syntax each parse as dedicated nodes
///   (`instanceof_expression`, `synchronized_statement`,
///   `switch_expression`, etc.) and do NOT trigger spurious call edges.
/// - `array_creation_expression` (`new int[10]`) is a distinct node from
///   `object_creation_expression` — it does NOT match. Array allocations
///   correctly produce zero call edges.
/// - **No Java analog to C#'s `nameof` filter is required.** Java's
///   `String::class` (`.class` literal) parses as `class_literal`, not as
///   a method invocation. There is no syntactic-look-alike-but-not-a-call
///   construct in Java analogous to `nameof(X)`. Annotations
///   (`@Override`, `@Deprecated`, `@HttpGet(...)`) parse as
///   `marker_annotation` / `annotation` nodes and are not
///   `method_invocation`. The probe confirmed no spurious-call sources;
///   no callee-name filter is wired in `extract_calls`.
pub(crate) const CALL_QUERIES: &str = r#"
; Direct, member-access, chained, and generic call: foo() / obj.foo() / a.b().c() / obj.<T>foo()
; Chained calls produce two matches because the grammar nests
; method_invocation on the `object:` field; each link runs through this
; pattern independently.
(method_invocation
  name: (identifier) @call.name)

; Constructor with bare type: new Foo()
(object_creation_expression
  type: (type_identifier) @call.name)

; Constructor with generic type: new ArrayList<Integer>()
(object_creation_expression
  type: (generic_type
    (type_identifier) @call.name))

; Constructor with qualified type: new java.util.ArrayList()
; The anchor `.` pins the capture to the rightmost type_identifier of
; the scoped_type_identifier chain.
(object_creation_expression
  type: (scoped_type_identifier
    (type_identifier) @call.name .))

; Constructor with qualified-and-generic type: new java.util.ArrayList<Integer>()
(object_creation_expression
  type: (generic_type
    (scoped_type_identifier
      (type_identifier) @call.name .)))

; this(...) / super(...) inside constructor bodies. The `constructor:`
; field is the `this` or `super` keyword node; its text equals its kind.
(explicit_constructor_invocation
  constructor: _ @call.name)

; Method reference with identifier on RHS: String::length / obj::method /
; this::doIt / super::doIt. Constructor references (Type::new) deliberately
; skipped — see Patterns NOT matched in the module doc.
(method_reference
  "::"
  (identifier) @call.name)
"#;

/// Query for `import_declaration` in all forms (plain, single-segment,
/// wildcard, static, and static wildcard).
///
/// All five Java import forms parse to a single `import_declaration` node
/// in tree-sitter-java 0.23.5 — there is NO separate
/// `static_import_declaration` node. The `static` keyword is an anonymous
/// (non-named) child of kind `"static"`; only the path nodes
/// (`identifier` / `scoped_identifier`) and the `asterisk` are named
/// children. The grammar's `node-types.json` confirms the three named
/// child kinds: `asterisk`, `identifier`, `scoped_identifier`.
///
/// Per-form shape (verified against tree-sitter-java 0.23.5 via the
/// scratch-crate probe at `/tmp/java-probe`):
///
/// - `import com.foo.Bar;` → `(import_declaration (scoped_identifier ...))`.
///   Single `scoped_identifier` named child whose text is the dotted
///   path `com.foo.Bar`.
/// - `import Foo;` → `(import_declaration (identifier))`. Single-segment
///   imports use a bare `identifier` node (no `scoped_identifier`).
/// - `import com.foo.*;` → `(import_declaration (scoped_identifier ...)
///   (asterisk))`. The `scoped_identifier` text is `com.foo` (NOT
///   `com.foo.*`); the `asterisk` is a separate named sibling. The `.`
///   between them is an anonymous keyword child. The extractor
///   reconstructs `com.foo.*` by appending `.*` to the path text when an
///   `asterisk` sibling is present.
/// - `import static com.foo.Bar.STATIC_FIELD;` →
///   `(import_declaration (scoped_identifier ...))`. The `static`
///   keyword is anonymous; the `scoped_identifier`'s text already
///   contains the FULL path including the field name
///   (`com.foo.Bar.STATIC_FIELD`). No reconstruction needed beyond
///   accepting the path text verbatim.
/// - `import static com.foo.Bar.*;` →
///   `(import_declaration (scoped_identifier ...) (asterisk))`.
///   Combination of the static and wildcard forms: `static` is dropped,
///   `scoped_identifier` text is `com.foo.Bar`, and the `asterisk`
///   triggers the `.*` reconstruction → `com.foo.Bar.*`.
///
/// **Single-capture strategy.** We capture the `import_declaration` node
/// itself and recover the path text at extraction time. Encoding the
/// path-with-or-without-asterisk reconstruction in the query would
/// require two patterns plus predicate logic; one capture name + a
/// small Rust-side walk over the directive's named children covers all
/// five forms cleanly. Mirrors the C# plugin's `using_directive`
/// single-capture approach.
///
/// Per Decision 7: the path is recorded verbatim — no resolution
/// against build metadata (`pom.xml`, `build.gradle`). Wildcards are
/// preserved verbatim (`import com.foo.*;` records `to = "com.foo.*"`,
/// matching the Rust plugin's `use foo::*` rule). Static-import targets
/// record the full path including the field name (`import static
/// com.foo.Bar.X;` records `to = "com.foo.Bar.X"`); the `static`
/// modifier is dropped. The default `resolve_include` returns `None`
/// for these dotted strings — they are not filesystem paths — so the
/// wire format records the package path verbatim and the engine never
/// accidentally resolves it.
pub(crate) const IMPORT_QUERIES: &str = r#"
; import com.foo.Bar; / import Foo; / import com.foo.*; /
; import static com.foo.Bar.STATIC_FIELD; / import static com.foo.Bar.*;
; tree-sitter-java 0.23.5 produces a single `import_declaration` node
; for every form; the `static` keyword and the `.` between the path
; and the asterisk are anonymous children. The extractor recovers the
; dotted path from the named identifier / scoped_identifier child and
; appends `.*` when an `asterisk` sibling is present.
(import_declaration) @import.dir
"#;

/// Query for `extends` (`superclass`) and `implements` (`super_interfaces`)
/// clauses on classes/records/enums, plus `extends` (`extends_interfaces`)
/// clauses on interfaces. Java syntactically distinguishes `extends`
/// (single-superclass) from `implements` (multi-interface), but Decision 2
/// folds both into the same [`EdgeKind::Inherits`] — there is no separate
/// `Implements` edge kind. Sealed types' `permits` clauses are intentionally
/// NOT matched (Decision 6).
///
/// Per-form shape (verified against tree-sitter-java 0.23.5 via the
/// scratch-crate probe at `/tmp/java-inherit-probe`):
///
/// - `class Foo extends Bar { }` → `(class_declaration ... superclass:
///   (superclass (type_identifier)) ...)`. The `superclass:` field's
///   ONLY named child is the base (no `type_list` wrapper); Java allows
///   exactly one superclass.
/// - `class Foo extends Bar implements IBaz, IQux { }` → adds a sibling
///   `interfaces: (super_interfaces (type_list (type_identifier)
///   (type_identifier)))`. The `interfaces:` field's named child is a
///   `super_interfaces` NODE; the multi-base list lives inside a
///   `type_list` named child.
/// - `class Foo<T> extends Bar<T> { }` → the base is a `generic_type`
///   under `superclass`; `utf8_text` is `"Bar<T>"` (generic argument list
///   preserved verbatim per Decision 9).
/// - `class Foo<T extends Comparable<T>> extends Bar<T> { }` — the
///   `extends Comparable<T>` is a CONSTRAINT inside the
///   `type_parameters:` field's `(type_parameter ... (type_bound ...))`
///   sub-tree, NOT a sibling of `superclass`. The query never sees
///   `Comparable<T>` through that path. Pinned by
///   `generic_class_with_extends_constraint_does_not_pollute_to_field`.
/// - `class Foo extends Ns.Bar { }` → base is a `scoped_type_identifier`;
///   `utf8_text` is `"Ns.Bar"`.
/// - `interface I extends J, K { }` → `(interface_declaration ...
///   (extends_interfaces (type_list (type_identifier) (type_identifier)))
///   ...)`. The `extends_interfaces` node is an UNNAMED-FIELD child of
///   `interface_declaration` (no `interfaces:` field, no `superclass:`
///   field — interfaces use a different node kind for their extends
///   clause).
/// - `record User(String name) implements Foo { }` → same shape as
///   classes: `(record_declaration ... interfaces: (super_interfaces
///   (type_list (type_identifier))) ...)`. Records can ONLY implement
///   interfaces — `record User(...) extends Base { }` is a syntax error
///   that tree-sitter recovers from as an ERROR node, producing zero
///   inheritance matches for the malformed clause.
/// - `enum Color implements Comparable<Color> { }` → same shape:
///   `(enum_declaration ... interfaces: (super_interfaces (type_list
///   (generic_type ...))) ...)`. Enums can ONLY implement interfaces
///   (they cannot extend a superclass — they implicitly extend `Enum`
///   and tree-sitter rejects an explicit `extends`).
/// - `sealed interface Shape permits Circle, Square { }` → the
///   `permits:` field is a SIBLING of `extends_interfaces`, NOT a child
///   of it. Decision 6 mandates that `permits` produces no edges; the
///   query simply doesn't reach `permits` nodes (no matching pattern).
/// - `class Foo { }` → no `superclass`, no `super_interfaces`, no
///   `extends_interfaces`; the query produces zero matches.
///
/// **Capture strategy.** Each base produces one `@inherit.base` capture
/// paired with the enclosing declaration's `@inherit.def`. We use the
/// single-child shape for `superclass` (no `type_list` wrapper) and the
/// wildcard `(type_list (_) @inherit.base)` for the multi-base
/// `super_interfaces` / `extends_interfaces` shapes — mirroring the
/// C# plugin's `(base_list (_) @inherit.base)` pattern, where `(_)`
/// matches `type_identifier`, `generic_type`, `scoped_type_identifier`,
/// or any future grammar variant uniformly.
///
/// Per Decision 2: no edge-kind distinction between `extends` and
/// `implements`. The agent disambiguates from the target Symbol's kind
/// (`Class` vs `Interface`) at query time.
///
/// Per Decision 9: the `from` field in the emitted `Inherits` edge is
/// the bare type name including generic parameter text verbatim
/// (`Foo` for `class Foo`, `Foo<T>` for `class Foo<T>`). The contract
/// is consumed by `Graph::class_hierarchy` in
/// `crates/code-graph-graph/src/algorithms.rs` — the walker looks up
/// classes by `Symbol.name` (bare name), not by `symbol_id`. The
/// extractor in `lib.rs::extract_inheritance` composes the enclosing
/// type's name + adjacent `type_parameters` text to satisfy this
/// contract; cite the C# 2.5 precedent in
/// `crates/code-graph-lang-csharp/src/lib.rs::enclosing_type_name_with_generics`.
pub(crate) const INHERITANCE_QUERIES: &str = r#"
; class Foo extends Bar { }  /  record User(...) extends ? — n/a (records can't extend)
; The `superclass` field holds a single named child (no type_list wrapper);
; Java allows at most one superclass.
(class_declaration
  superclass: (superclass (_) @inherit.base)) @inherit.def

; class Foo implements IBaz, IQux { } / record User(...) implements Foo { } /
; enum Color implements Comparable<Color> { }
; All three declaration kinds use the same `interfaces: (super_interfaces
; (type_list ...))` shape per the 3.5 probe.
(class_declaration
  interfaces: (super_interfaces (type_list (_) @inherit.base))) @inherit.def

(record_declaration
  interfaces: (super_interfaces (type_list (_) @inherit.base))) @inherit.def

(enum_declaration
  interfaces: (super_interfaces (type_list (_) @inherit.base))) @inherit.def

; interface I extends J, K { }
; `extends_interfaces` is an unnamed-field child of interface_declaration
; (no field name); its `type_list` child holds the bases.
(interface_declaration
  (extends_interfaces (type_list (_) @inherit.base))) @inherit.def
"#;
