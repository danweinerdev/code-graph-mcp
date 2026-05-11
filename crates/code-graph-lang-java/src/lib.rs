//! Java language plugin for code-graph-mcp.
//!
//! Uses tree-sitter (via the `tree-sitter` and `tree-sitter-java` crates)
//! to extract symbols, calls, import edges, and inheritance edges from
//! `.java` source files.
//!
//! # Phase status
//!
//! Phase 3.1 shipped the crate scaffold: dependency wiring, empty query
//! string constants in [`queries`] that compile against tree-sitter-java
//! 0.23.5, the [`JavaParser`] struct with cached `Query` objects, and
//! the [`LanguagePlugin`] impl.
//!
//! Phase 3.2 wires `extract_definitions` (classes, interfaces, enums,
//! records, methods, constructors) and switches `parse_to_filegraph`
//! from the empty-graph stub to a real tree-sitter-driven parse.
//! Anonymous classes (Decision 4), records (Decision 6), default
//! interface methods (Decision 11), and enum-with-method-bodies
//! (Decision 12) are covered with inline tests.
//!
//! Phase 3.3 wires `extract_calls` covering `method_invocation` (direct,
//! member-access, chained, generic), `object_creation_expression`
//! (constructor calls in bare / generic / qualified / qualified-generic
//! forms), `explicit_constructor_invocation` (`this(...)` and `super(...)`
//! constructor chaining), and `method_reference` (identifier-on-RHS form:
//! `String::length`, `obj::method`, `this::doIt`, `super::doIt`).
//! Constructor references (`Type::new`) are deliberately not matched —
//! see `queries::CALL_QUERIES` for rationale.
//!
//! Phase 3.4 wires `extract_imports`, producing [`EdgeKind::Includes`]
//! edges for `import_declaration` in all five forms — plain
//! (`import com.foo.Bar;`), single-segment (`import Foo;`), wildcard
//! (`import com.foo.*;` → `to = "com.foo.*"`), static
//! (`import static com.foo.Bar.X;` → `to = "com.foo.Bar.X"`, `static`
//! modifier dropped), and the combination static-wildcard
//! (`import static com.foo.Bar.*;` → `to = "com.foo.Bar.*"`). All forms
//! record the dotted path verbatim per Decision 7; no resolution
//! against build metadata (`pom.xml`, `build.gradle`).
//!
//! Phase 3.5 wires `extract_inheritance`, producing
//! [`EdgeKind::Inherits`] edges for `superclass` (extends) and
//! `super_interfaces` (implements) on `class_declaration` /
//! `record_declaration` / `enum_declaration`, plus
//! `extends_interfaces` on `interface_declaration`. Per Decision 2,
//! `extends` and `implements` produce the same edge kind — agents
//! disambiguate via the target Symbol's kind. Per Decision 9, generic
//! parameter text is preserved verbatim in both the `from` field
//! (`Foo<T>` for `class Foo<T>`) and the `to` field (`Bar<T>` for
//! `extends Bar<T>`). Sealed types' `permits` clauses are intentionally
//! ignored per Decision 6.
//!
//! # Default trait methods
//!
//! `JavaParser` does NOT override [`LanguagePlugin::resolve_call`] or
//! [`LanguagePlugin::resolve_include`].
//!
//! - `resolve_call`: the default scope-aware heuristic (same file > same
//!   parent > same namespace > global) is the documented contract,
//!   mirroring the four shipped plugins. Method overloading and dynamic
//!   dispatch produce the same imperfection class as C++ overloaded-
//!   function resolution (Decision 10).
//! - `resolve_include`: Java imports (`import com.foo.Bar`) are dotted
//!   package paths, not filesystem paths — the default basename-match
//!   resolver returns `None` for them, mirroring the Python/C# plugins'
//!   approach to dotted module-path imports.
//!
//! # Java-specific notes
//!
//! - **Default and static interface methods** (Decision 11) are
//!   distinguished from abstract ones by **presence of a method body**,
//!   not by the `default`/`static` keyword alone. `interface I { default
//!   void Foo() {...} }`, `interface I { static void Bar() {...} }`, and
//!   any Java-9+ private interface method with a body all produce a
//!   [`SymbolKind::Function`] symbol with no parent (matching the Rust
//!   trait-default-method contract). `interface I { void Bar(); }` (no
//!   body, no `default`/`static`) is a forward declaration and produces
//!   no Symbol record. Confirmed against tree-sitter-java 0.23.5: the
//!   discriminator is the `body:` field on `method_declaration`. Abstract
//!   methods have no `body:` field at all.
//! - **Anonymous classes** (Decision 4) emit no Class symbol — the
//!   `object_creation_expression { class_body { ... } }` shape is
//!   transparent. Methods inside the anonymous body extract with the
//!   ENCLOSING NAMED ENTITY's parent: walking up the AST past
//!   `object_creation_expression` boundaries until a
//!   `class_declaration`/`interface_declaration`/`enum_declaration`/
//!   `record_declaration` is found. Documented limitation: two
//!   anonymous classes inside the same enclosing method that both define
//!   the same method name produce two symbols with the same ID — the
//!   `Symbol.line` disambiguates at query time.
//! - **Records** (Decision 6) extract as [`SymbolKind::Class`] —
//!   `SymbolKind::Record` is intentionally not added. The record's
//!   component list (`(String name, int age)`) parses as
//!   `formal_parameters > formal_parameter`, NOT as
//!   `method_declaration`, so record components are correctly invisible.
//!   Auto-generated members (`name()` accessor, `equals`, `hashCode`,
//!   `toString`) are extracted ONLY if they appear in source — synthetic
//!   members are correctly invisible to tree-sitter. Methods declared
//!   inside a record body record the record name as parent (NOT as
//!   orphan Function symbols — the same bug C# task 2.2 had to fix in
//!   commit `0cf200b`).
//! - **Enum methods** (Decision 12) extract as [`SymbolKind::Method`]
//!   with parent = enum type name. Both enum-level methods (`enum Planet
//!   { ...; abstract double surfaceGravity(); }`) and per-constant
//!   methods (`EARTH { double surfaceGravity() {...} }`) produce
//!   methods on the enum type — NOT on a synthetic `Planet$EARTH`
//!   parent. Enum constants themselves (the `EARTH`, `MARS`, ...
//!   `enum_constant` nodes) are NOT extracted as symbols.
//! - **Sealed types** (Decision 6): `sealed interface Shape permits
//!   Circle, Square` extracts as ordinary [`SymbolKind::Interface`].
//!   The `permits` clause is ignored (no edges produced).

pub(crate) mod helpers;
pub(crate) mod queries;

use std::path::Path;

use code_graph_core::{Edge, EdgeKind, FileGraph, Language, Symbol, SymbolKind};
use code_graph_lang::helpers::{find_enclosing_kind, truncate_signature};
use code_graph_lang::{LanguagePlugin, ParseError};
use streaming_iterator::StreamingIterator;
use tree_sitter::{
    Language as TsLanguage, Node, Parser as TsParser, Query, QueryCursor, Tree as TsTree,
};

use crate::queries::{CALL_QUERIES, DEFINITION_QUERIES, IMPORT_QUERIES, INHERITANCE_QUERIES};

/// File extensions the Java parser claims. Java has a single canonical
/// source extension; `.jav` is technically accepted by some legacy
/// toolchains but not produced by any modern build system, so we only
/// claim `.java`.
pub const EXTENSIONS: &[&str] = &[".java"];

/// Java source-file parser. Holds the tree-sitter `Language` and the
/// four pre-compiled queries used to drive symbol/edge extraction in
/// Phases 3.2-3.5.
///
/// Construct with [`JavaParser::new`]; share across threads (queries
/// are `Send + Sync`).
pub struct JavaParser {
    /// Compiled Java grammar. Held so per-call [`tree_sitter::Parser`]
    /// instances built inside `parse_file` can attach to it without
    /// rebuilding the `LanguageFn`.
    language: TsLanguage,
    /// Compiled definition query (live in 3.2 — drives
    /// [`Self::extract_definitions`]).
    def_query: Query,
    /// Compiled call query (live in 3.3 — drives [`Self::extract_calls`]).
    call_query: Query,
    /// Compiled import query (live in 3.4 — drives
    /// [`Self::extract_imports`]).
    import_query: Query,
    /// Compiled inheritance query (live in 3.5 — drives
    /// [`Self::extract_inheritance`]).
    inheritance_query: Query,
}

impl JavaParser {
    /// Build a new parser, compiling all four tree-sitter queries against
    /// the pinned tree-sitter-java grammar. Returns an [`anyhow::Error`]
    /// (wrapping the query compiler's message) if any query fails to
    /// compile against the pinned grammar version.
    ///
    /// Successful return is the gate that proves every query string in
    /// [`queries`] parses against tree-sitter-java 0.23.5. Phase 3.2
    /// filled [`DEFINITION_QUERIES`]; Phase 3.3 filled [`CALL_QUERIES`];
    /// Phase 3.4 filled [`IMPORT_QUERIES`]; Phase 3.5 filled
    /// [`INHERITANCE_QUERIES`]. All four query strings are live as of
    /// 3.5.
    pub fn new() -> anyhow::Result<Self> {
        let language: TsLanguage = tree_sitter_java::LANGUAGE.into();

        let def_query = Query::new(&language, DEFINITION_QUERIES)
            .map_err(|e| anyhow::anyhow!("definition query: {e}"))?;
        let call_query =
            Query::new(&language, CALL_QUERIES).map_err(|e| anyhow::anyhow!("call query: {e}"))?;
        let import_query = Query::new(&language, IMPORT_QUERIES)
            .map_err(|e| anyhow::anyhow!("import query: {e}"))?;
        let inheritance_query = Query::new(&language, INHERITANCE_QUERIES)
            .map_err(|e| anyhow::anyhow!("inheritance query: {e}"))?;

        Ok(Self {
            language,
            def_query,
            call_query,
            import_query,
            inheritance_query,
        })
    }

    /// File extensions handled by this plugin. Exposed as an associated
    /// function so the trait implementation and external callers (e.g.
    /// CLI argument parsing) share the single source of truth.
    pub fn extensions() -> &'static [&'static str] {
        EXTENSIONS
    }

    /// Parse `content` (UTF-8 bytes) as Java and produce a [`FileGraph`].
    /// Internal entry point for [`Self::parse_file`] (the trait method);
    /// kept crate-private so the public surface stays the trait method
    /// while each per-extractor method can be tested via `parse_file`
    /// without exposing it. Mirrors the Python/C# plugins'
    /// `parse_to_filegraph` indirection.
    ///
    /// Phase 3.2 wired `extract_definitions` into the pipeline; Phase 3.3
    /// wired `extract_calls`; Phase 3.4 wired `extract_imports`; Phase
    /// 3.5 wires `extract_inheritance`. All four extractors are live —
    /// `parse_file` produces Symbol records, `Calls` edges, `Includes`
    /// edges, and `Inherits` edges from a single tree-sitter parse.
    fn parse_to_filegraph(&self, path: &Path, content: &[u8]) -> Result<FileGraph, ParseError> {
        let tree = parse_tree(&self.language, content)?;
        let root = tree.root_node();
        let path_str = path.to_string_lossy().into_owned();

        let mut fg = FileGraph {
            path: path_str.clone(),
            language: Language::Java,
            symbols: Vec::new(),
            edges: Vec::new(),
        };

        self.extract_definitions(root, content, &path_str, &mut fg);
        self.extract_calls(root, content, &path_str, &mut fg);
        self.extract_imports(root, content, &path_str, &mut fg);
        self.extract_inheritance(root, content, &path_str, &mut fg);

        Ok(fg)
    }

    /// Run the definition query and produce symbols. Mirrors the C++/Rust/
    /// Go/Python/C# plugins' capture-name dispatch: each capture name from
    /// `DEFINITION_QUERIES` maps to a small branch that builds the right
    /// [`Symbol`].
    ///
    /// Per-capture-name behavior:
    ///
    /// - `class.name` from `class_declaration` → [`SymbolKind::Class`].
    ///   Parent is the immediate enclosing class/interface/enum/record
    ///   (or empty for top-level classes; nested classes record the
    ///   immediate outer type).
    /// - `interface.name` from `interface_declaration` →
    ///   [`SymbolKind::Interface`]. Sealed interfaces (`sealed interface
    ///   Shape permits Circle, Square`) extract as ordinary `Interface`;
    ///   the `permits` clause is ignored per Decision 6.
    /// - `enum.name` from `enum_declaration` → [`SymbolKind::Enum`]. Enum
    ///   constants (`enum_constant` children of the `enum_body`) are NOT
    ///   extracted (Decision 12).
    /// - `record.name` from `record_declaration` → [`SymbolKind::Class`]
    ///   per Decision 6. `SymbolKind::Record` is intentionally not added.
    ///   Methods inside the record body extract as `Method` with parent
    ///   = record name (the C# 2.2 records-leak bug — see crate
    ///   docstring).
    /// - `method.name` from `method_declaration` → Method or Function
    ///   depending on enclosing scope:
    ///     * Inside `interface_declaration` with a `body:` field present →
    ///       [`SymbolKind::Function`], no parent (per Decision 11 —
    ///       default/static interface methods extract as Function,
    ///       matching Rust trait default methods). Body presence is the
    ///       discriminator; both `default` and `static` modifiers (and
    ///       Java-9+ private interface methods with bodies) qualify.
    ///     * Inside `interface_declaration` with no `body:` field →
    ///       skipped (forward-declaration rule, no Symbol record).
    ///     * Inside an `enum_body_declarations` with no `body:` field →
    ///       skipped under the same forward-declaration rule (covers
    ///       enum-level `abstract double surfaceGravity();`).
    ///     * Inside `class_declaration` / `enum_declaration` /
    ///       `record_declaration` → [`SymbolKind::Method`] with parent =
    ///       enclosing named-type name. Anonymous-class methods take the
    ///       OUTER named entity's parent (Decision 4); enum-constant
    ///       per-instance methods take the enum-type parent (Decision 12)
    ///       — see [`enclosing_named_type_name`].
    ///     * No enclosing class/interface/enum/record →
    ///       [`SymbolKind::Function`] with no parent (defensive:
    ///       shouldn't happen in well-formed Java but the extractor
    ///       doesn't assume well-formedness).
    /// - `ctor.name` from `constructor_declaration` →
    ///   [`SymbolKind::Method`] with parent = enclosing class/record
    ///   name (defensive Function fallback if no enclosing type is
    ///   found; not reachable in well-formed Java). The captured name
    ///   *is* the type identifier (Java constructor syntax — the
    ///   constructor's name matches its enclosing type's name).
    ///
    /// Captures consumed without emitting a Symbol:
    /// - `class.def` / `interface.def` / `enum.def` / `record.def` /
    ///   `method.def` / `ctor.def`: structural anchors used by the
    ///   queries to bind captures to the same definition. The
    ///   `name`-arms above already resolve the enclosing definition via
    ///   `find_enclosing_kind`.
    fn extract_definitions(&self, root: Node<'_>, content: &[u8], path: &str, fg: &mut FileGraph) {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.def_query, root, content);
        let cap_names = self.def_query.capture_names();

        while let Some(m) = matches.next() {
            for capture in m.captures {
                let cap_node = capture.node;
                if cap_node.has_error() {
                    continue;
                }
                let cap_name = capture_name_for_index(cap_names, capture.index);
                let text = cap_node.utf8_text(content).unwrap_or("");
                if text.is_empty() {
                    continue;
                }

                match cap_name {
                    "class.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "class_declaration")
                        else {
                            continue;
                        };
                        let parent = enclosing_type_name(def_node, content);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Class,
                            path,
                            def_node,
                            content,
                            parent,
                        ));
                    }

                    "interface.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "interface_declaration")
                        else {
                            continue;
                        };
                        let parent = enclosing_type_name(def_node, content);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Interface,
                            path,
                            def_node,
                            content,
                            parent,
                        ));
                    }

                    "enum.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "enum_declaration")
                        else {
                            continue;
                        };
                        let parent = enclosing_type_name(def_node, content);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Enum,
                            path,
                            def_node,
                            content,
                            parent,
                        ));
                    }

                    "record.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "record_declaration")
                        else {
                            continue;
                        };
                        let parent = enclosing_type_name(def_node, content);
                        // Decision 6: records extract as ordinary Class
                        // — `SymbolKind::Record` is intentionally not
                        // added. The `enclosing_type_name` helper
                        // recognises `record_declaration` as a type
                        // ancestor so methods inside record bodies
                        // record the record name as parent (NOT as
                        // orphan Function symbols — see the C# 2.2
                        // records-leak bug).
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Class,
                            path,
                            def_node,
                            content,
                            parent,
                        ));
                    }

                    "method.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "method_declaration")
                        else {
                            continue;
                        };

                        let has_body = def_node.child_by_field_name("body").is_some();

                        // Decision 11: inside an interface_declaration,
                        // a method with a body extracts as Function (no
                        // parent) — matching the Rust trait-default-
                        // method contract. A method without a body is an
                        // abstract declaration and produces no Symbol
                        // (forward-declaration rule, mirroring
                        // C++/Rust/Go/C#). Body presence is the
                        // discriminator (the `default`, `static`, and
                        // Java-9+ private cases all yield bodies; the
                        // unmodified abstract case yields no body).
                        //
                        // The walk uses `enclosing_named_type_kind`,
                        // which honours Decision 4 (anonymous-class
                        // boundaries are transparent) and Decision 12
                        // (enum_constant boundaries are transparent) by
                        // walking up to the first named-type ancestor.
                        let enclosing_kind = enclosing_named_type_kind(def_node);

                        if matches!(enclosing_kind, Some("interface_declaration")) {
                            if !has_body {
                                continue;
                            }
                            fg.symbols.push(make_symbol(
                                text,
                                SymbolKind::Function,
                                path,
                                def_node,
                                content,
                                String::new(),
                            ));
                            continue;
                        }

                        // Forward-declaration filter for non-interface
                        // contexts: enum-level abstract methods (`abstract
                        // double surfaceGravity();` directly inside
                        // `enum_body_declarations`) have no body and are
                        // skipped, matching the C++/Rust/Go/C# rule that
                        // declarations without bodies produce no Symbol.
                        // Class-level abstract methods (`abstract void
                        // Foo();` inside an abstract class) follow the
                        // same rule.
                        if !has_body {
                            continue;
                        }

                        // Outside an interface: classify as Method when
                        // an enclosing named type exists; otherwise fall
                        // back to Function (defensive — well-formed Java
                        // can't have a method outside a type, but the
                        // extractor stays robust to recovery from syntax
                        // errors).
                        let parent = enclosing_named_type_name(def_node, content);
                        let kind = if parent.is_empty() {
                            SymbolKind::Function
                        } else {
                            SymbolKind::Method
                        };
                        fg.symbols
                            .push(make_symbol(text, kind, path, def_node, content, parent));
                    }

                    "ctor.name" => {
                        let Some(def_node) =
                            find_enclosing_kind(cap_node, "constructor_declaration")
                        else {
                            continue;
                        };
                        let parent = enclosing_named_type_name(def_node, content);
                        let kind = if parent.is_empty() {
                            SymbolKind::Function
                        } else {
                            SymbolKind::Method
                        };
                        fg.symbols
                            .push(make_symbol(text, kind, path, def_node, content, parent));
                    }

                    // `*.def` captures are structural anchors — the `name`
                    // arms above resolved the enclosing definition node
                    // via `find_enclosing_kind`.
                    _ => {}
                }
            }
        }
    }

    /// Run the call query and produce [`EdgeKind::Calls`] edges. Mirrors
    /// the C++/Rust/Go/Python/C# plugins' `extract_calls`: each capture
    /// is a callee identifier, the line is anchored at the enclosing
    /// `method_invocation` / `object_creation_expression` /
    /// `explicit_constructor_invocation` / `method_reference` so the
    /// reported line tracks the call-site (not an inner identifier on a
    /// chain-continuation line), and the `from` field is built by
    /// [`enclosing_function_id`] so it lines up exactly with the
    /// `symbol_id()` shape produced by [`Self::extract_definitions`].
    ///
    /// Per-capture-name behavior (single capture name `call.name` shared
    /// across all seven query patterns):
    ///
    /// - Direct, member-access, chained, and generic calls — `foo()`,
    ///   `obj.foo()`, `a.b().c()`, `obj.<T>foo()` — captured by the
    ///   single `method_invocation name: (identifier)` pattern. Chained
    ///   calls produce one edge per link (the grammar nests
    ///   `method_invocation`s on the `object:` field).
    /// - Constructor calls (`new Foo()`, `new ArrayList<Integer>()`,
    ///   `new java.util.ArrayList<>()`) → `to = "Foo"` / `to = "ArrayList"`.
    ///   The query captures only the bare rightmost type_identifier;
    ///   generics and qualifier chains are stripped.
    /// - `this(...)` / `super(...)` constructor chaining → `to = "this"`
    ///   / `to = "super"`. These ARE genuine constructor invocations
    ///   (Java syntactically requires them in the first statement of a
    ///   constructor body when used) and produce call edges per the
    ///   design brief — no filter is applied. Agents disambiguate from
    ///   ordinary method calls via the literal `"this"` / `"super"`
    ///   callee name, which is not a valid Java identifier in a normal
    ///   call position.
    /// - Method references with identifier on RHS (`String::length`,
    ///   `obj::method`, `this::doIt`, `super::doIt`) → `to = <RHS name>`.
    ///   Constructor references (`Type::new`) are NOT matched by the
    ///   query — see `queries::CALL_QUERIES` for the documented
    ///   limitation.
    ///
    /// **Lambda transparency** is implemented in [`enclosing_function_id`]
    /// — the enclosing-function walk does not stop at `lambda_expression`
    /// nodes, so calls inside `() -> foo()` report the enclosing
    /// method/constructor as the `from` field, not the lambda. Mirrors
    /// Python `lambda`, Go `func_literal`, and C# `lambda_expression`
    /// transparency.
    ///
    /// **Decision 4 transparency** (anonymous classes) is also
    /// implemented in [`enclosing_function_id`] — `object_creation_expression`
    /// boundaries are transparent when computing a method's **parent**
    /// prefix, not when finding which method-shaped ancestor owns a
    /// call. For `new Runnable() { void run() { foo(); } }`, the call
    /// to `foo()` is directly inside `run` — `run` is a
    /// `method_declaration` and stops the walk normally, so the call's
    /// `from` is `<path>:C::run` (where `C` is the outer named class).
    /// The transparency is what makes `run`'s parent be `C` rather
    /// than a synthesized `Anonymous$1` — that's what 3.2's
    /// `extract_definitions` established and 3.3 inherits. The result:
    /// `run`'s call edges naturally carry `C::run` as `from`, threading
    /// the anonymous boundary cleanly without a synthetic parent.
    ///
    /// **Decision 12 transparency** (enum-constant method bodies) is also
    /// implemented in [`enclosing_function_id`] — the walk passes through
    /// `enum_constant` boundaries when no `method_declaration` ancestor
    /// is found beneath the enum_constant, so a call inside a per-constant
    /// method body resolves to the enum-type parent. (In well-formed Java,
    /// the per-constant method body is itself a `method_declaration`, so
    /// the walk stops there normally; the enum_constant transparency is
    /// load-bearing only for calls directly inside an enum_constant's
    /// `class_body` outside any method — a degenerate case but pinned by
    /// the same walk-past rule the 3.2 helpers established.)
    ///
    /// **Callee filtering.** Java has no syntactic-look-alike-but-not-a-
    /// call construct analogous to C#'s `nameof(X)`: casts parse as
    /// `cast_expression`, `instanceof` as `instanceof_expression`,
    /// `synchronized` as `synchronized_statement`, `switch` as
    /// `switch_expression`, `class` literals as `class_literal`,
    /// annotations as `marker_annotation`/`annotation`, and array
    /// allocations as `array_creation_expression` (NOT
    /// `object_creation_expression`). The probe found no Java callee-
    /// name filter worth wiring; the extractor records every match.
    fn extract_calls(&self, root: Node<'_>, content: &[u8], path: &str, fg: &mut FileGraph) {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.call_query, root, content);
        let cap_names = self.call_query.capture_names();

        while let Some(m) = matches.next() {
            for capture in m.captures {
                let cap_node = capture.node;
                if cap_node.has_error() {
                    continue;
                }
                let cap_name = capture_name_for_index(cap_names, capture.index);
                if cap_name != "call.name" {
                    continue;
                }

                // For most captures the text is the callee identifier.
                // For `explicit_constructor_invocation` the captured node
                // is a keyword (`this` or `super`); its text equals its
                // kind. The unified text extraction works in both cases.
                let callee = cap_node.utf8_text(content).unwrap_or("");
                if callee.is_empty() {
                    continue;
                }

                // Anchor the line at the enclosing call/object-creation/
                // ctor-chain/method-ref expression so the reported line
                // tracks the call site. For chained or multi-line calls
                // the inner identifier can land on a continuation line;
                // the outer call node's start_position is the
                // semantically-correct anchor. Falls back to the capture
                // node when no enclosing call ancestor is found
                // (defensive — the query patterns guarantee one).
                let call_node = find_enclosing_kind(cap_node, "method_invocation")
                    .or_else(|| find_enclosing_kind(cap_node, "object_creation_expression"))
                    .or_else(|| find_enclosing_kind(cap_node, "explicit_constructor_invocation"))
                    .or_else(|| find_enclosing_kind(cap_node, "method_reference"))
                    .unwrap_or(cap_node);
                let from = enclosing_function_id(cap_node, content, path);

                fg.edges.push(Edge {
                    from,
                    to: callee.to_owned(),
                    kind: EdgeKind::Calls,
                    file: path.to_owned(),
                    line: call_node.start_position().row as u32 + 1,
                });
            }
        }
    }

    /// Run the import query and produce [`EdgeKind::Includes`] edges.
    /// Mirrors the C++/Rust/Go/Python/C# plugins' `extract_imports`: the
    /// query's single capture (`import.dir`) yields each `import_declaration`
    /// node; the [`import_declaration_path`] helper recovers the dotted
    /// path text (reconstructing wildcards as `<path>.*`); the resulting
    /// edge records the path verbatim per Decision 7.
    ///
    /// The `static` modifier on static imports is automatically dropped
    /// because tree-sitter-java parses it as an anonymous keyword child
    /// (`kind() == "static"`, `is_named() == false`); the
    /// [`import_declaration_path`] walk visits only named children.
    /// Wildcards reconstruct as `<path>.*` by appending `.*` to the
    /// path text when an `asterisk` sibling is present — matching the
    /// Rust plugin's `use foo::*` rule for path-with-glob imports.
    ///
    /// The edge's `from` is the file path (NOT a symbol ID), `kind` is
    /// `Includes`, and `line` is anchored at the `import_declaration`
    /// node's row (1-indexed). The `to` is the recovered path. Skips
    /// captures whose node has a parse error so partial extraction
    /// stays consistent with the other extractors' contracts.
    fn extract_imports(&self, root: Node<'_>, content: &[u8], path: &str, fg: &mut FileGraph) {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.import_query, root, content);
        let cap_names = self.import_query.capture_names();

        while let Some(m) = matches.next() {
            for capture in m.captures {
                let cap_node = capture.node;
                if cap_node.has_error() {
                    continue;
                }
                let cap_name = capture_name_for_index(cap_names, capture.index);
                if cap_name != "import.dir" {
                    continue;
                }

                let Some(path_text) = import_declaration_path(cap_node, content) else {
                    continue;
                };
                if path_text.is_empty() {
                    continue;
                }

                fg.edges.push(Edge {
                    from: path.to_owned(),
                    to: path_text,
                    kind: EdgeKind::Includes,
                    file: path.to_owned(),
                    line: cap_node.start_position().row as u32 + 1,
                });
            }
        }
    }

    /// Run the inheritance query and produce [`EdgeKind::Inherits`]
    /// edges, one per base in each extends/implements clause. Mirrors
    /// the C++/Rust/Go/Python/C# plugins' `extract_inheritance` for the
    /// bare-name `from`-field contract (cite Phase 1 / Phase 5 of
    /// RustRewrite and the C# 2.5 precedent at
    /// `crates/code-graph-lang-csharp/src/lib.rs::extract_inheritance`).
    ///
    /// **Per-match shape.** The query returns one match *per base*:
    ///
    /// - `superclass: (superclass (_) @inherit.base)` on
    ///   `class_declaration` — exactly one match per class (Java allows
    ///   exactly one superclass).
    /// - `interfaces: (super_interfaces (type_list (_) @inherit.base))` on
    ///   `class_declaration` / `record_declaration` / `enum_declaration` —
    ///   one match per implemented interface in the list.
    /// - `(extends_interfaces (type_list (_) @inherit.base))` on
    ///   `interface_declaration` — one match per extended interface.
    ///
    /// `class Foo extends Bar implements IBaz, IQux { }` produces three
    /// matches (one for `Bar` via `superclass`, two for `IBaz` / `IQux`
    /// via `super_interfaces`). Each carries the same `@inherit.def`
    /// (the enclosing declaration) and a distinct `@inherit.base`. The
    /// `inherit.base` capture is a single named child of the `superclass`
    /// or `type_list` wrapper and can be any of:
    ///
    /// - `type_identifier` — bare type name (`Bar`, `IBaz`)
    /// - `generic_type` — generic type (`Bar<T>`,
    ///   `Comparable<Color>`)
    /// - `scoped_type_identifier` — dotted type name (`Ns.Bar`,
    ///   `java.util.List`)
    ///
    /// For every form the captured node's `utf8_text` is the verbatim
    /// source text — generic argument lists and dotted qualifications
    /// survive into the `to` field as written (Decision 9 — generic
    /// params preserved verbatim).
    ///
    /// **`from`-field composition.** The `from` field is the bare
    /// enclosing type name + the type-parameter list text (if any). For
    /// `class Foo` → `from = "Foo"`; for `class Foo<T>` →
    /// `from = "Foo<T>"`. See [`enclosing_type_name_with_generics`] for
    /// the full rule and a documented known asymmetry with `Symbol.name`
    /// (mirrors C# 2.5 and Rust).
    ///
    /// **Decision 2 — no edge-kind distinction.** All bases — whether
    /// reached through `superclass` (`extends`), `super_interfaces`
    /// (`implements`), or `extends_interfaces` (interface-extends-
    /// interface) — produce the same [`EdgeKind::Inherits`]. The agent
    /// disambiguates from the target Symbol's `kind` at query time.
    ///
    /// **Decision 6 — `permits` ignored.** Sealed types'
    /// `permits: (permits (type_list ...))` field is NOT matched by any
    /// query pattern. No `Inherits` edges are produced for it.
    ///
    /// **Constraint isolation.** Generic constraints inside the
    /// `type_parameters:` clause (e.g. `class Foo<T extends Comparable<T>>`)
    /// live inside `(type_parameter ... (type_bound ...))` — a sibling
    /// of `superclass`, NOT a child. The query never sees constraint
    /// types through the inheritance path. Pinned by
    /// `generic_class_with_extends_constraint_does_not_pollute_to_field`.
    ///
    /// **Edge shape per match:**
    /// - `from = bare_name + type_parameters` (e.g. `Foo<T>`)
    /// - `to = base_node.utf8_text` (verbatim — `Bar<T>`, `Ns.Bar`, etc.)
    /// - `kind = EdgeKind::Inherits`
    /// - `file = path`
    /// - `line = enclosing declaration's start_position().row + 1`
    ///   (i.e., the class/interface/enum/record declaration line, not
    ///   the base node's line — keeps the edge anchored at where the
    ///   inheritance relationship is *declared*).
    fn extract_inheritance(&self, root: Node<'_>, content: &[u8], path: &str, fg: &mut FileGraph) {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.inheritance_query, root, content);
        let cap_names = self.inheritance_query.capture_names();

        while let Some(m) = matches.next() {
            let mut def_node: Option<Node<'_>> = None;
            let mut base_node: Option<Node<'_>> = None;

            for capture in m.captures {
                let cap_node = capture.node;
                if cap_node.has_error() {
                    continue;
                }
                let cap_name = capture_name_for_index(cap_names, capture.index);
                match cap_name {
                    "inherit.def" => def_node = Some(cap_node),
                    "inherit.base" => base_node = Some(cap_node),
                    // Defensive catch-all — guards against future grammar
                    // revisions that might introduce additional capture
                    // names under the same query patterns.
                    _ => {}
                }
            }

            let (Some(def), Some(base)) = (def_node, base_node) else {
                continue;
            };

            let from = enclosing_type_name_with_generics(def, content);
            if from.is_empty() {
                continue;
            }

            let to = base.utf8_text(content).unwrap_or("").to_owned();
            if to.is_empty() {
                continue;
            }

            fg.edges.push(Edge {
                from,
                to,
                kind: EdgeKind::Inherits,
                file: path.to_owned(),
                line: def.start_position().row as u32 + 1,
            });
        }
    }
}

impl LanguagePlugin for JavaParser {
    fn id(&self) -> Language {
        Language::Java
    }

    fn extensions(&self) -> &'static [&'static str] {
        EXTENSIONS
    }

    /// Parse `content` (UTF-8 bytes) as Java and produce a [`FileGraph`].
    /// All four extractors (definitions, calls, imports, inheritance)
    /// are live as of Phase 3.5.
    fn parse_file(&self, path: &Path, content: &[u8]) -> Result<FileGraph, ParseError> {
        self.parse_to_filegraph(path, content)
    }

    // resolve_call and resolve_include intentionally NOT overridden — see
    // the crate-level docstring for the rationale (default heuristic
    // matches the C++/Rust/Go/Python/C# plugins; default basename resolver
    // is a no-op for Java's dotted package `import` paths, which is the
    // intended behavior).

    fn close(&self) {}
}

/// Build a tree-sitter [`TsTree`] for `content` against the Java grammar.
/// The caller-supplied [`TsLanguage`] is borrowed; the returned tree owns
/// its AST. Returns [`ParseError::Parse`] if `set_language` fails or if
/// tree-sitter declines to produce a tree (e.g. on cancellation). Mirrors
/// `parse_tree` in the C++/Rust/Go/Python/C# plugins byte-for-byte modulo
/// the language identity.
fn parse_tree(language: &TsLanguage, content: &[u8]) -> Result<TsTree, ParseError> {
    let mut parser = TsParser::new();
    parser
        .set_language(language)
        .map_err(|e| ParseError::Parse(format!("set_language: {e}")))?;
    parser
        .parse(content, None)
        .ok_or_else(|| ParseError::Parse("tree-sitter parse failed".to_owned()))
}

/// Look up a capture name by index. Returns `""` (empty) on out-of-range
/// indices, matching the C++/Rust/Go/Python/C# plugins' silent fallback.
fn capture_name_for_index<'a>(cap_names: &[&'a str], index: u32) -> &'a str {
    cap_names.get(index as usize).copied().unwrap_or("")
}

/// Return the immediate enclosing class/interface/enum/record name for a
/// **type declaration** node (used to populate the `parent` field for
/// nested types). Walks ancestors starting from `def_node.parent()` so a
/// top-level type returns `""` (not its own name); a nested type records
/// the outer type as parent.
///
/// Includes `record_declaration` so that records nested inside other
/// types are recognised, and so that types nested inside a record body
/// record the record name as parent. Without `record_declaration` here,
/// nested types inside records would orphan to top-level, mirroring the
/// C# 2.2 records-leak bug.
fn enclosing_type_name(def_node: Node<'_>, content: &[u8]) -> String {
    let mut current = def_node.parent();
    while let Some(n) = current {
        if matches!(
            n.kind(),
            "class_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "record_declaration"
        ) {
            if let Some(name_node) = n.child_by_field_name("name") {
                return name_node.utf8_text(content).unwrap_or("").to_owned();
            }
            return String::new();
        }
        current = n.parent();
    }
    String::new()
}

/// Return the bare enclosing-type name *with* the generic parameter
/// list text appended (when present), suitable for the `from` field of
/// an `Inherits` edge. For `class Foo` returns `"Foo"`; for
/// `class Foo<T>` returns `"Foo<T>"`; for
/// `interface I<T, U>` returns `"I<T, U>"`.
///
/// `def_node` must be one of `class_declaration`, `interface_declaration`,
/// `enum_declaration`, or `record_declaration`. Other node kinds return
/// the empty string defensively.
///
/// Decision 9 (generic parameter text preserved verbatim) — and the
/// Phase 1 / Phase 5 bare-name `from`-field rule — are both enforced
/// here. The result is the bare type name, EXCEPT for generic types
/// where the `type_parameters` text is appended verbatim.
///
/// **Known asymmetry with `Symbol.name`** (matches the Rust plugin's
/// pre-existing behavior and the C# 2.5 precedent at
/// `crates/code-graph-lang-csharp/src/lib.rs::enclosing_type_name_with_generics` —
/// accepted as a documented limitation): `extract_definitions` stores
/// `Symbol.name` as the bare identifier (`"Foo"` for `class Foo<T>`),
/// but the `from` field of an `Inherits` edge produced here is the
/// generics-preserving form (`"Foo<T>"`). `Graph::class_hierarchy` at
/// `crates/code-graph-graph/src/algorithms.rs` looks up symbols by
/// `Symbol.name` then walks `adj.get(name)` — for a generic class
/// queried as `class_hierarchy("Foo")` the symbol is found but the
/// adjacency lookup misses (edges are keyed under `"Foo<T>"`).
/// Generic-class hierarchy walks are effectively unsupported by the
/// graph layer in its current form. Same limitation exists in the
/// Rust and C# plugins; the trade-off is documented in Phase 4.4's
/// CLAUDE.md "Java Parser Limitations" section.
///
/// In tree-sitter-java 0.23.5 the generic parameter list is the named
/// child of kind `type_parameters` (reached via the `type_parameters:`
/// field on every declaration kind that admits generics —
/// `class_declaration`, `interface_declaration`, `record_declaration`).
/// `enum_declaration` does not accept generics in Java; the field is
/// simply absent and the function returns the bare name.
fn enclosing_type_name_with_generics(def_node: Node<'_>, content: &[u8]) -> String {
    let name_node = match def_node.child_by_field_name("name") {
        Some(n) => n,
        None => return String::new(),
    };
    let bare = name_node.utf8_text(content).unwrap_or("").to_owned();
    if bare.is_empty() {
        return bare;
    }

    let mut cursor = def_node.walk();
    for child in def_node.children(&mut cursor) {
        if child.kind() == "type_parameters" {
            let tp = child.utf8_text(content).unwrap_or("");
            if !tp.is_empty() {
                return format!("{bare}{tp}");
            }
        }
    }

    bare
}

/// Return the kind of the **first enclosing named-type ancestor** of
/// `def_node`, walking past anonymous-class
/// (`object_creation_expression`) and enum-constant (`enum_constant`)
/// boundaries.
///
/// This is the routine that implements Decision 4 (anonymous-class
/// transparency) and Decision 12 (enum-constant transparency). The walk
/// IGNORES `object_creation_expression` and `enum_constant` ancestors
/// when looking for the named-type kind, so a method inside an anonymous
/// class reports the kind of the OUTER named type, and a method inside an
/// `EARTH { ... }` enum-constant body reports `enum_declaration` (the
/// enum type, not a synthesised `Planet$EARTH`).
///
/// Returns `None` when no named-type ancestor exists.
fn enclosing_named_type_kind(def_node: Node<'_>) -> Option<&'static str> {
    let mut current = def_node.parent();
    while let Some(n) = current {
        match n.kind() {
            "class_declaration" => return Some("class_declaration"),
            "interface_declaration" => return Some("interface_declaration"),
            "enum_declaration" => return Some("enum_declaration"),
            "record_declaration" => return Some("record_declaration"),
            // `object_creation_expression` and `enum_constant` are
            // transparent — the walk continues past them. This is the
            // load-bearing Decision-4 / Decision-12 behavior.
            _ => {}
        }
        current = n.parent();
    }
    None
}

/// Return the **name** of the first enclosing named-type ancestor, with
/// the same anonymous-class / enum-constant transparency as
/// [`enclosing_named_type_kind`]. Used to populate the `parent` field on
/// methods and constructors.
///
/// Returns `""` when no named-type ancestor exists (defensive — well-
/// formed Java methods always live inside a type).
fn enclosing_named_type_name(def_node: Node<'_>, content: &[u8]) -> String {
    let mut current = def_node.parent();
    while let Some(n) = current {
        if matches!(
            n.kind(),
            "class_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "record_declaration"
        ) {
            if let Some(name_node) = n.child_by_field_name("name") {
                return name_node.utf8_text(content).unwrap_or("").to_owned();
            }
            return String::new();
        }
        current = n.parent();
    }
    String::new()
}

/// Build a `path:fn_name` or `path:Parent::fn_name` symbol-ID anchor for
/// the function/method/constructor enclosing `node`. Mirrors the
/// C++/Rust/Go/Python/C# plugins' `enclosing_function_id` and matches
/// the [`code_graph_core::symbol_id`] shape produced by Phase 3.2's
/// definition extractor so call edges' `from` fields line up exactly
/// with definition IDs.
///
/// Lives in `lib.rs` rather than `helpers.rs` because the walk is
/// tightly coupled to the same Decision 4 / Decision 12 transparency
/// rules implemented by [`enclosing_named_type_kind`] and
/// [`enclosing_named_type_name`]. Keeping all three together makes the
/// "anonymous classes and enum_constant boundaries are transparent"
/// contract obvious at a single read.
///
/// Behavior:
/// - **`method_declaration`** with an enclosing named-type
///   (`class_declaration` / `enum_declaration` / `record_declaration`)
///   → returns `<path>:<TypeName>::<method>`. Uses
///   [`enclosing_named_type_name`] so anonymous-class
///   (`object_creation_expression`) and enum-constant (`enum_constant`)
///   boundaries are transparent — matches the 3.2 parent-resolution
///   rule.
/// - **`method_declaration`** inside an `interface_declaration` (default,
///   `static`, or Java-9+ `private` interface method, all detected by
///   `body:` presence) → returns `<path>:<method>` (no parent). Matches
///   Decision 11: the symbol kind is `Function`, and the symbol ID has
///   no parent prefix. Methods without bodies in an interface yield no
///   Symbol record (3.2's forward-declaration rule), so any call
///   lexically inside such a node is unreachable in practice — the walk
///   still reports the `<path>:<method>` form for robustness.
/// - **`constructor_declaration`** → returns `<path>:<TypeName>::<ctor>`.
///   The constructor's name matches its enclosing type's name (Java
///   constructor syntax). Uses the same
///   [`enclosing_named_type_name`] walk for consistency with how
///   `extract_definitions` resolves the constructor's `parent` field.
/// - **No enclosing function-shaped declaration** (call at file scope,
///   in a field initializer, in an annotation argument, etc.) → returns
///   `path` (the bare file path). Matches the C++/Rust/Go/Python/C#
///   top-level-call rule.
///
/// **Lambda transparency:** `lambda_expression` is NOT a function-shaped
/// declaration in this walk — calls inside `() -> foo()` walk past the
/// lambda and report the enclosing method/constructor as the `from`.
/// Mirrors the Python `lambda` and Go `func_literal` rules.
///
/// **Anonymous-class transparency (Decision 4):**
/// `object_creation_expression` is NOT a function-shaped declaration in
/// this walk. A call inside `new Runnable() { void run() { foo(); } }`
/// has `run` (an inner `method_declaration`) as the immediate enclosing
/// function — but the `run` symbol's PARENT, resolved via
/// [`enclosing_named_type_name`], walks past the
/// `object_creation_expression` to the outer named type. So the `from`
/// field for the `foo()` call is `<path>:<OuterClass>::run` (matching
/// the symbol ID produced by 3.2's `extract_definitions`). Two
/// anonymous classes in the same method that both define `run` produce
/// two collisions by design (documented limitation in 3.2's
/// `two_anonymous_classes_in_same_method_both_define_run_collide_by_design`
/// test).
///
/// **Enum-constant transparency (Decision 12):** `enum_constant` is NOT
/// a function-shaped declaration in this walk. The transparency is
/// load-bearing only when the enclosing named-type lookup would
/// otherwise stop at an `enum_constant` boundary — see
/// [`enclosing_named_type_name`] for the details.
fn enclosing_function_id(node: Node<'_>, content: &[u8], path: &str) -> String {
    let mut current = Some(node);
    while let Some(n) = current {
        match n.kind() {
            "method_declaration" => {
                let name = n
                    .child_by_field_name("name")
                    .and_then(|nm| nm.utf8_text(content).ok())
                    .unwrap_or("");
                if name.is_empty() {
                    return path.to_owned();
                }
                // A method directly inside an interface with a body is a
                // default/static/private interface method — 3.2's
                // extract_definitions records it as Function with no
                // parent (Decision 11), so the call's `from` must omit
                // the parent too. Body-less abstract methods produce no
                // symbol at all but the walk reports the `<path>:<name>`
                // form for robustness (matches the C# precedent).
                if find_enclosing_kind(n, "interface_declaration").is_some() {
                    return format!("{path}:{name}");
                }
                // Otherwise, prefer the nearest enclosing named type as
                // the parent. The walk uses
                // [`enclosing_named_type_name`] so anonymous-class and
                // enum-constant boundaries are transparent (Decisions 4
                // and 12 — the parent must match the symbol ID 3.2
                // produced for this method). Falls back to the bare
                // `<path>:<name>` form if no enclosing type is found
                // (defensive — shouldn't happen in well-formed Java).
                let parent = enclosing_named_type_name(n, content);
                if parent.is_empty() {
                    return format!("{path}:{name}");
                }
                return format!("{path}:{parent}::{name}");
            }
            "constructor_declaration" => {
                let name = n
                    .child_by_field_name("name")
                    .and_then(|nm| nm.utf8_text(content).ok())
                    .unwrap_or("");
                if name.is_empty() {
                    return path.to_owned();
                }
                let parent = enclosing_named_type_name(n, content);
                if parent.is_empty() {
                    return format!("{path}:{name}");
                }
                return format!("{path}:{parent}::{name}");
            }
            _ => {}
        }
        current = n.parent();
    }
    path.to_owned()
}

/// Return the dotted package path written in an `import_declaration`
/// node, or `None` if the directive has no recoverable path (defensive
/// — well-formed Java always has at least one named path child).
///
/// Rules (per the tree-sitter-java 0.23.5 probe at Phase 3.4):
/// - **Plain** (`import com.foo.Bar;`): the path is the single named
///   `scoped_identifier` child; its text is the verbatim dotted path
///   (`com.foo.Bar`).
/// - **Single-segment** (`import Foo;`): the path is a bare `identifier`
///   named child; its text is the single segment (`Foo`).
/// - **Wildcard** (`import com.foo.*;`): the named children are
///   `scoped_identifier` (text `com.foo`) AND `asterisk` (text `*`),
///   with an anonymous `.` keyword between them. Reconstruct the path
///   as `<scoped_identifier_text>.*`.
/// - **Static** (`import static com.foo.Bar.STATIC_FIELD;`): the
///   `static` keyword is an anonymous child and is automatically
///   skipped by the named-children walk. The `scoped_identifier`'s
///   text already includes the field name (`com.foo.Bar.STATIC_FIELD`)
///   — no reconstruction beyond accepting the verbatim text.
/// - **Static wildcard** (`import static com.foo.Bar.*;`): combination
///   of the static and wildcard cases — `static` is dropped (anonymous),
///   the `scoped_identifier` text is `com.foo.Bar`, and the `asterisk`
///   sibling triggers `.*` reconstruction → `com.foo.Bar.*`.
///
/// The returned text is the verbatim dotted path (with the trailing
/// `.*` for wildcards). Per Decision 7 the `static` modifier never
/// appears in the returned text; wildcards are preserved verbatim.
fn import_declaration_path(directive: Node<'_>, content: &[u8]) -> Option<String> {
    let mut path_text: Option<String> = None;
    let mut has_asterisk = false;

    let mut cursor = directive.walk();
    for child in directive.children(&mut cursor) {
        if !child.is_named() {
            // The `static` keyword (kind `"static"`) and the `.` between
            // path and asterisk (kind `"."`) are anonymous children and
            // are correctly skipped here. The opening `import` keyword
            // and trailing `;` are likewise anonymous.
            continue;
        }
        match child.kind() {
            // First (and, per the grammar, only) path-shaped child
            // wins. `node-types.json` guarantees at most one
            // `identifier` / `scoped_identifier` named child.
            "identifier" | "scoped_identifier" if path_text.is_none() => {
                path_text = Some(child.utf8_text(content).unwrap_or("").to_owned());
            }
            "asterisk" => {
                has_asterisk = true;
            }
            _ => {
                // Defensive: any future grammar-evolution children fall
                // through here without panicking. The probe confirmed
                // exactly three possible named child kinds in
                // tree-sitter-java 0.23.5 (identifier, scoped_identifier,
                // asterisk); this arm exists so a grammar bump doesn't
                // silently bypass the named-children contract.
            }
        }
    }

    let mut path = path_text?;
    if path.is_empty() {
        return None;
    }
    if has_asterisk {
        path.push_str(".*");
    }
    Some(path)
}

/// Build a [`Symbol`] from a definition node. Centralises the row/column/
/// signature math so each branch in `extract_definitions` stays small.
/// Mirrors the C++/Rust/Go/Python/C# plugins' `make_symbol`.
///
/// Java has no syntactic `namespace` construct (the closest analog —
/// the `package` declaration — applies file-wide and is captured by the
/// import / inheritance subsystems rather than carried as a per-symbol
/// field). The `namespace` field is therefore left empty here, mirroring
/// the Python plugin's `module_path = ""` default for un-namespaced
/// declarations.
fn make_symbol(
    name: &str,
    kind: SymbolKind,
    path: &str,
    def_node: Node<'_>,
    content: &[u8],
    parent: String,
) -> Symbol {
    let start = def_node.start_position();
    let end = def_node.end_position();
    Symbol {
        name: name.to_owned(),
        kind,
        file: path.to_owned(),
        line: start.row as u32 + 1,
        column: start.column as u32,
        end_line: end.row as u32 + 1,
        signature: truncate_signature(def_node.utf8_text(content).unwrap_or("")),
        namespace: String::new(),
        parent,
        language: Language::Java,
    }
}

#[cfg(test)]
mod tests {
    //! Phase 3.1 structural smoke tests + Phase 3.2 definition-extraction
    //! coverage + Phase 3.3 call-extraction coverage + Phase 3.4
    //! import-extraction coverage + Phase 3.5 inheritance-extraction
    //! coverage. All four extractors are live.
    use super::*;
    use code_graph_core::symbol_id;
    use code_graph_lang::LanguagePlugin;

    // ----------------------------------------------------------------
    // Phase 3.1 — structural smoke tests
    // ----------------------------------------------------------------

    #[test]
    fn parser_is_object_safe_and_id_returns_java() {
        let p: Box<dyn LanguagePlugin> = Box::new(JavaParser::new().unwrap());
        assert_eq!(p.id(), Language::Java);
    }

    // ----------------------------------------------------------------
    // Phase 3.2 — definition extraction
    // ----------------------------------------------------------------

    /// Parse `src` against `JavaParser` at a synthetic absolute path.
    /// Used by every Phase 3.2 behavioral test below.
    fn parse(src: &str) -> FileGraph {
        parse_at(src, "/tmp/Test.java")
    }

    /// Parse `src` against `JavaParser` at a caller-chosen path.
    fn parse_at(src: &str, path: &str) -> FileGraph {
        let p = JavaParser::new().unwrap();
        p.parse_file(Path::new(path), src.as_bytes())
            .expect("parse_file must succeed")
    }

    /// Find the (first) symbol with `name`, panicking with a helpful
    /// message if absent. Tests use this when they expect exactly one.
    fn sym<'a>(fg: &'a FileGraph, name: &str) -> &'a Symbol {
        fg.symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| {
                panic!(
                    "expected symbol named {name:?}; got: {:?}",
                    fg.symbols
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                )
            })
    }

    #[test]
    fn parse_file_returns_correct_path_and_language() {
        let fg = parse("");
        assert_eq!(fg.path, "/tmp/Test.java");
        assert_eq!(fg.language, Language::Java);
    }

    #[test]
    fn empty_file_produces_no_symbols() {
        let fg = parse("");
        assert!(fg.symbols.is_empty(), "got: {:?}", fg.symbols);
        assert!(fg.edges.is_empty(), "got: {:?}", fg.edges);
    }

    #[test]
    fn top_level_class_produces_class_symbol_no_parent() {
        let fg = parse("class Foo { }");
        assert_eq!(fg.symbols.len(), 1, "got: {:?}", fg.symbols);
        let s = sym(&fg, "Foo");
        assert_eq!(s.kind, SymbolKind::Class);
        assert!(s.parent.is_empty(), "top-level class must have no parent");
    }

    #[test]
    fn interface_produces_interface_kind() {
        let fg = parse("interface IFoo { }");
        let s = sym(&fg, "IFoo");
        assert_eq!(s.kind, SymbolKind::Interface);
    }

    #[test]
    fn enum_produces_enum_kind_and_constants_are_not_extracted() {
        let fg = parse(
            r#"
enum Status { ACTIVE, INACTIVE, PENDING }
"#,
        );
        // Exactly one Symbol — the enum type. The enum constants
        // (ACTIVE/INACTIVE/PENDING) are NOT extracted as symbols
        // (Decision 12).
        assert_eq!(
            fg.symbols.len(),
            1,
            "enum constants must not produce symbols: got {:?}",
            fg.symbols
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
        );
        let s = sym(&fg, "Status");
        assert_eq!(s.kind, SymbolKind::Enum);
    }

    #[test]
    fn method_in_class_produces_method_kind_with_class_parent() {
        let fg = parse(
            r#"
class Foo {
    public void bar() { }
}
"#,
        );
        let m = sym(&fg, "bar");
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.parent, "Foo");
    }

    #[test]
    fn constructor_records_class_as_parent() {
        let fg = parse(
            r#"
class Foo {
    public Foo() { }
}
"#,
        );
        // Class + constructor (named `Foo` too).
        let ctor = fg
            .symbols
            .iter()
            .find(|s| s.name == "Foo" && s.kind == SymbolKind::Method)
            .unwrap_or_else(|| {
                panic!(
                    "expected a Method named Foo (constructor); got {:?}",
                    fg.symbols
                )
            });
        assert_eq!(ctor.parent, "Foo");
    }

    #[test]
    fn nested_class_records_outer_class_as_parent() {
        let fg = parse(
            r#"
class Outer {
    class Inner { }
}
"#,
        );
        let outer = sym(&fg, "Outer");
        assert!(outer.parent.is_empty(), "outer class must have no parent");

        let inner = sym(&fg, "Inner");
        assert_eq!(inner.kind, SymbolKind::Class);
        assert_eq!(inner.parent, "Outer", "nested class must record outer");
    }

    #[test]
    fn class_implementing_interface_extracts_both_kinds() {
        let fg = parse(
            r#"
interface IFoo { }
class Foo implements IFoo { void bar() { } }
"#,
        );
        let i = sym(&fg, "IFoo");
        assert_eq!(i.kind, SymbolKind::Interface);
        let c = sym(&fg, "Foo");
        assert_eq!(c.kind, SymbolKind::Class);
        let m = sym(&fg, "bar");
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.parent, "Foo");
    }

    // ---- Decision 11: default interface methods --------------------

    #[test]
    fn default_interface_method_extracts_as_function_no_parent() {
        // `default void doFoo() { ... }` inside an interface
        // (Decision 11) — method body present → extracts as Function,
        // NOT Method; parent is empty (matches Rust trait-default-
        // method rule).
        let fg = parse(
            r#"
interface I {
    default void doFoo() { return; }
}
"#,
        );
        let s = sym(&fg, "doFoo");
        assert_eq!(
            s.kind,
            SymbolKind::Function,
            "default interface method must extract as Function (not Method)"
        );
        assert!(
            s.parent.is_empty(),
            "default interface method must have empty parent"
        );
    }

    #[test]
    fn static_interface_method_extracts_as_function_no_parent() {
        // `static void doBar() { ... }` inside an interface — same rule
        // as `default`. Body present → Function, no parent.
        let fg = parse(
            r#"
interface I {
    static void doBar() { return; }
}
"#,
        );
        let s = sym(&fg, "doBar");
        assert_eq!(
            s.kind,
            SymbolKind::Function,
            "static interface method must extract as Function (not Method)"
        );
        assert!(s.parent.is_empty());
    }

    #[test]
    fn private_interface_method_with_body_extracts_as_function_no_parent() {
        // Java 9+ allows `private` methods on interfaces (no `default`
        // or `static` modifier). The body-presence discriminator covers
        // this case: the method has a body, so it extracts as Function
        // (no parent) just like `default`/`static`. Pins the claim from
        // queries.rs / lib.rs docs that body-presence subsumes the
        // modifier check cleanly across all three Java-9+ method kinds.
        let fg = parse(
            r#"
interface I {
    private void helper() { return; }
}
"#,
        );
        let s = sym(&fg, "helper");
        assert_eq!(
            s.kind,
            SymbolKind::Function,
            "private interface method with body must extract as Function (not Method)"
        );
        assert!(
            s.parent.is_empty(),
            "private interface method with body must have empty parent"
        );
    }

    #[test]
    fn abstract_interface_method_produces_no_symbol() {
        // `void Bar();` inside an interface (no body) — forward
        // declaration; produces no Symbol record (mirroring
        // C++/Rust/Go/C#).
        let fg = parse(
            r#"
interface I {
    void Bar();
}
"#,
        );
        // Only the interface type itself surfaces.
        assert_eq!(
            fg.symbols.len(),
            1,
            "abstract interface methods must not produce a Symbol; got: {:?}",
            fg.symbols
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
        );
        let i = sym(&fg, "I");
        assert_eq!(i.kind, SymbolKind::Interface);
    }

    #[test]
    fn interface_with_mixed_methods_extracts_only_the_body_having_one() {
        // Discriminator: same interface holds an abstract method and a
        // default method. Only the default method produces a Symbol;
        // the abstract one is dropped. Load-bearing anti-regression
        // for Decision 11.
        let fg = parse(
            r#"
interface I {
    default void hasBody() { return; }
    void noBody();
}
"#,
        );
        // Interface + hasBody method = 2 symbols total; noBody is
        // filtered.
        assert_eq!(
            fg.symbols.len(),
            2,
            "expected interface + hasBody; got: {:?}",
            fg.symbols
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
        );
        let s = sym(&fg, "hasBody");
        assert_eq!(s.kind, SymbolKind::Function);
        assert!(fg.symbols.iter().all(|sy| sy.name != "noBody"));
    }

    // ---- Decision 4: anonymous classes -----------------------------

    #[test]
    fn anonymous_class_method_takes_outer_named_entity_as_parent() {
        // `new Runnable() { void run() {...} }` inside `Outer.go()` —
        // per Decision 4, `run`'s parent is the OUTER named entity
        // (the class `Outer`), NOT `Runnable` (which is just a type
        // reference) and NOT a synthetic `Anonymous$1`.
        let fg = parse(
            r#"
public class Outer {
    public void go() {
        Runnable r = new Runnable() {
            public void run() {
                System.out.println("hi");
            }
        };
    }
}
"#,
        );
        let run = sym(&fg, "run");
        assert_eq!(run.kind, SymbolKind::Method);
        assert_eq!(
            run.parent, "Outer",
            "anonymous-class method must take outer named entity's parent (Outer); \
             not Runnable (a type reference) and not a synthesised Anonymous$1"
        );
    }

    #[test]
    fn two_anonymous_classes_in_same_method_both_define_run_collide_by_design() {
        // Decision 4 documented limitation: two anonymous classes
        // inside the same enclosing method, both with a `run` method,
        // produce two `Outer::run` symbols with the same name. They
        // are distinguishable by line number (file path + line tuple
        // disambiguates at query time).
        let fg = parse(
            r#"
public class Anchor {
    public void go() {
        Runnable a = new Runnable() {
            public void run() { System.out.println("a"); }
        };
        Runnable b = new Runnable() {
            public void run() { System.out.println("b"); }
        };
    }
}
"#,
        );
        let runs: Vec<&Symbol> = fg
            .symbols
            .iter()
            .filter(|s| s.name == "run" && s.kind == SymbolKind::Method)
            .collect();
        assert_eq!(
            runs.len(),
            2,
            "two anonymous classes with `run` must produce two Symbol records; got: {:?}",
            runs
        );
        // Both share the same parent (`Anchor`) and name (`run`); only
        // the line number disambiguates.
        for r in &runs {
            assert_eq!(r.parent, "Anchor");
            assert_eq!(r.name, "run");
        }
        assert_ne!(
            runs[0].line, runs[1].line,
            "the two anonymous-class `run` methods must live on different lines"
        );
    }

    // ---- Decision 6: records ---------------------------------------

    #[test]
    fn record_declaration_extracts_as_class() {
        // `record User(String name)` extracts as Class — Decision 6.
        // `SymbolKind::Record` is intentionally not added.
        let fg = parse("public record User(String name) {}");
        let user = sym(&fg, "User");
        assert_eq!(
            user.kind,
            SymbolKind::Class,
            "record extracts as Class per Decision 6"
        );
    }

    #[test]
    fn record_components_do_not_produce_method_or_function_symbols() {
        // The component list `(String name, int age)` parses as
        // `formal_parameters > formal_parameter`, NOT
        // `method_declaration` — record components must be invisible to
        // the definition extractor.
        let fg = parse(
            r#"
public record User(String name, int age) {}
"#,
        );
        // Only one symbol: the record itself.
        assert_eq!(
            fg.symbols.len(),
            1,
            "record components must not surface as symbols; got: {:?}",
            fg.symbols
                .iter()
                .map(|s| (s.name.as_str(), s.kind))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn methods_inside_record_body_extract_as_method_with_record_parent() {
        // Load-bearing records-leak anti-regression (mirrors C# 2.2's
        // `0cf200b` fix). Without `record_declaration` in
        // `enclosing_named_type_name`, methods inside a record orphan
        // as `Function(no parent)` rather than `Method(parent=User)`.
        let fg = parse(
            r#"
public record User(String name) {
    public boolean isAdmin() { return name.equals("admin"); }
    public String greeting() { return "hi " + name; }
}
"#,
        );
        // 1 record + 2 methods = 3 symbols.
        assert_eq!(
            fg.symbols.len(),
            3,
            "expected 1 record + 2 methods; got: {:?}",
            fg.symbols
                .iter()
                .map(|s| (s.name.as_str(), s.kind))
                .collect::<Vec<_>>()
        );

        let user = sym(&fg, "User");
        assert_eq!(user.kind, SymbolKind::Class);

        let is_admin = sym(&fg, "isAdmin");
        assert_eq!(
            is_admin.kind,
            SymbolKind::Method,
            "method inside record must be Method, not orphan Function"
        );
        assert_eq!(
            is_admin.parent, "User",
            "method inside record must record record name as parent"
        );

        let greeting = sym(&fg, "greeting");
        assert_eq!(greeting.kind, SymbolKind::Method);
        assert_eq!(greeting.parent, "User");
    }

    // ---- Decision 12: enum methods ---------------------------------

    #[test]
    fn enum_level_method_records_enum_type_as_parent() {
        // Methods declared at the enum level (after the `;` separator)
        // extract as Method with parent = enum type name.
        let fg = parse(
            r#"
public enum Planet {
    EARTH, MARS;
    public static Planet first() { return EARTH; }
}
"#,
        );
        let m = sym(&fg, "first");
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.parent, "Planet");
    }

    #[test]
    fn enum_constant_per_instance_method_records_enum_type_not_synthetic_parent() {
        // `EARTH { double surfaceGravity() {...} }` — the per-constant
        // method body lives in `enum_constant > class_body >
        // method_declaration`. Per Decision 12 the parent is the enum
        // type (`Planet`), NOT a synthesised `Planet$EARTH`. The
        // `enum_constant` boundary is transparent in
        // `enclosing_named_type_name`, mirroring the
        // anonymous-class transparency for Decision 4.
        let fg = parse(
            r#"
public enum Planet {
    EARTH {
        @Override
        double surfaceGravity() { return 9.8; }
    },
    MARS {
        @Override
        double surfaceGravity() { return 3.7; }
    };
    abstract double surfaceGravity();
}
"#,
        );
        // Two per-constant `surfaceGravity` methods (both with parent =
        // `Planet`); the enum-level abstract `surfaceGravity()` has no
        // body and is filtered as a forward declaration.
        let methods: Vec<&Symbol> = fg
            .symbols
            .iter()
            .filter(|s| s.name == "surfaceGravity")
            .collect();
        assert_eq!(
            methods.len(),
            2,
            "expected exactly 2 per-constant surfaceGravity methods (abstract decl filtered); \
             got: {:?}",
            methods
        );
        for m in &methods {
            assert_eq!(m.kind, SymbolKind::Method);
            assert_eq!(
                m.parent, "Planet",
                "per-constant method parent must be enum type, not Planet$EARTH or similar"
            );
        }
    }

    #[test]
    fn enum_abstract_method_at_enum_level_is_filtered() {
        // `abstract double surfaceGravity();` directly inside
        // `enum_body_declarations` (no body) — forward declaration;
        // produces no Symbol record. Pinned separately to make the
        // forward-declaration filter visible.
        let fg = parse(
            r#"
public enum Planet {
    EARTH, MARS;
    abstract double surfaceGravity();
}
"#,
        );
        // Only the enum type surfaces; the abstract method is filtered.
        assert!(
            fg.symbols.iter().all(|s| s.name != "surfaceGravity"),
            "abstract enum-level method must not produce a Symbol; got: {:?}",
            fg.symbols
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
        );
        let p = sym(&fg, "Planet");
        assert_eq!(p.kind, SymbolKind::Enum);
    }

    // ---- Sealed types ---------------------------------------------

    #[test]
    fn sealed_interface_extracts_as_interface_permits_clause_ignored() {
        // `sealed interface Shape permits Circle, Square` — Decision 6
        // says the `permits` clause is ignored. The interface still
        // extracts cleanly; no edges/symbols come from `permits`.
        let fg = parse(
            r#"
public sealed interface Shape permits Circle, Square {}
public final class Circle implements Shape {}
public final class Square implements Shape {}
"#,
        );
        let s = sym(&fg, "Shape");
        assert_eq!(s.kind, SymbolKind::Interface);
        // Circle and Square are extracted as their own classes — but
        // NOT through any `permits` mechanism; they're top-level
        // declarations.
        let c = sym(&fg, "Circle");
        assert_eq!(c.kind, SymbolKind::Class);
        let sq = sym(&fg, "Square");
        assert_eq!(sq.kind, SymbolKind::Class);
    }

    // ---- Symbol shape sanity --------------------------------------

    #[test]
    fn line_and_end_line_are_one_indexed_and_populated() {
        let fg = parse(
            r#"
class Foo {
    public void bar() {
        return;
    }
}
"#,
        );
        let foo = sym(&fg, "Foo");
        assert!(foo.line >= 1, "line is 1-indexed");
        assert!(foo.end_line >= foo.line);

        let bar = sym(&fg, "bar");
        assert!(bar.line >= 1);
        assert!(bar.end_line >= bar.line);
    }

    #[test]
    fn signature_truncates_at_method_body() {
        let fg = parse(
            r#"
class Foo {
    public int bar() {
        return 42;
    }
}
"#,
        );
        let bar = sym(&fg, "bar");
        // truncate_signature drops the body — `{` is a hard cutoff.
        assert!(
            !bar.signature.contains('{'),
            "signature should drop body: got {:?}",
            bar.signature
        );
        // Whatever survives must still mention the method name.
        assert!(
            bar.signature.contains("bar"),
            "signature should preserve method name; got {:?}",
            bar.signature
        );
    }

    #[test]
    fn symbol_id_for_method_uses_parent_form() {
        // Sanity that the extracted method's parent flows through into
        // `symbol_id` correctly — `path:Class::method` shape.
        let fg = parse_at(
            r#"
class Foo {
    public void bar() { }
}
"#,
            "/abs/Foo.java",
        );
        let bar = sym(&fg, "bar");
        assert_eq!(symbol_id(bar), "/abs/Foo.java:Foo::bar");
    }

    #[test]
    fn namespace_is_empty_for_java_symbols() {
        // Java has no per-symbol `namespace` field analog to C#
        // `namespace` declarations — package declarations apply
        // file-wide and are surfaced via imports/inheritance, not the
        // symbol record. Document the contract with an explicit test.
        let fg = parse("class Foo { void bar() {} }");
        let foo = sym(&fg, "Foo");
        assert!(foo.namespace.is_empty(), "expected empty namespace");
        let bar = sym(&fg, "bar");
        assert!(bar.namespace.is_empty(), "expected empty namespace");
    }

    // ----------------------------------------------------------------
    // Phase 3.3 — call extraction
    // ----------------------------------------------------------------

    /// Filter to just the `Calls` edges of `fg` (drops Includes edges
    /// from 3.4 and the Inherits edges 3.5 will add). Mirrors the C#
    /// plugin's test helper.
    fn calls(fg: &FileGraph) -> Vec<&Edge> {
        fg.edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .collect()
    }

    /// Assert that exactly one Calls edge with `from = expected_from`
    /// and `to = expected_to` exists. Panics with a helpful message
    /// listing every Calls edge if not. Mirrors the C# plugin's
    /// `assert_one_call`.
    fn assert_one_call(fg: &FileGraph, expected_from: &str, expected_to: &str) {
        let edges = calls(fg);
        let matched: Vec<&&Edge> = edges
            .iter()
            .filter(|e| e.from == expected_from && e.to == expected_to)
            .collect();
        assert_eq!(
            matched.len(),
            1,
            "expected exactly one Calls edge from={:?} to={:?}; got: {:?}",
            expected_from,
            expected_to,
            edges
                .iter()
                .map(|e| (e.from.as_str(), e.to.as_str()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn direct_call_inside_method_records_calls_edge() {
        let fg = parse_at("class C { void m() { foo(); } }", "/p/Test.java");
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_one_call(&fg, "/p/Test.java:C::m", "foo");
    }

    #[test]
    fn member_access_call_records_rightmost_name_only() {
        // `obj.foo()` → `to = "foo"`. The receiver `obj` is part of the
        // syntactic chain but is not the callee identifier.
        let fg = parse_at("class C { void m() { obj.foo(); } }", "/p/Test.java");
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_one_call(&fg, "/p/Test.java:C::m", "foo");
    }

    #[test]
    fn chained_call_produces_one_edge_per_chain_link() {
        // `a.b().c()` — tree-sitter parses two nested method_invocation
        // nodes (the inner `a.b()` is the `object:` of the outer
        // `_.c()`); the query matches both. Expect 2 edges (to `b` and
        // to `c`), each from `<path>:C::m`.
        let fg = parse_at("class C { void m() { a.b().c(); } }", "/p/Test.java");
        let edges = calls(&fg);
        assert_eq!(edges.len(), 2, "got: {:?}", edges);
        assert_one_call(&fg, "/p/Test.java:C::m", "b");
        assert_one_call(&fg, "/p/Test.java:C::m", "c");
    }

    #[test]
    fn constructor_call_records_type_as_callee() {
        // `new Foo()` → `to = "Foo"`. Matches the C#/Python convention
        // for constructor calls: the agent interprets the edge as
        // construction; the recorded callee is the bare type name.
        let fg = parse_at(
            "class C { void m() { var x = new Foo(); } }",
            "/p/Test.java",
        );
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_one_call(&fg, "/p/Test.java:C::m", "Foo");
    }

    #[test]
    fn generic_call_records_bare_name_not_typed_form() {
        // `obj.<Integer>foo()` → `to = "foo"`, NOT `foo<Integer>`. The
        // `name:` field on method_invocation is the bare identifier; the
        // `type_arguments:` field is a sibling that the query does not
        // capture.
        let fg = parse_at(
            "class C { <T> void m() { this.<Integer>foo(); } }",
            "/p/Test.java",
        );
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_one_call(&fg, "/p/Test.java:C::m", "foo");
        // Pin the bare-name invariant — if a future refactor regresses
        // to capturing the typed form, the substring check catches it.
        assert!(
            !edges[0].to.contains('<'),
            "to must be bare name; got {:?}",
            edges[0].to
        );
    }

    #[test]
    fn generic_constructor_records_bare_type_name() {
        // `new ArrayList<Integer>()` → `to = "ArrayList"`. The query
        // captures only the type_identifier inside generic_type.
        let fg = parse_at(
            "class C { void m() { var x = new ArrayList<Integer>(); } }",
            "/p/Test.java",
        );
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_one_call(&fg, "/p/Test.java:C::m", "ArrayList");
        assert!(
            !edges[0].to.contains('<'),
            "to must be bare name; got {:?}",
            edges[0].to
        );
    }

    #[test]
    fn qualified_constructor_records_rightmost_type_name() {
        // `new java.util.ArrayList()` → `to = "ArrayList"`. The
        // scoped_type_identifier query anchors on the rightmost
        // type_identifier.
        let fg = parse_at(
            "class C { void m() { var x = new java.util.ArrayList(); } }",
            "/p/Test.java",
        );
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_one_call(&fg, "/p/Test.java:C::m", "ArrayList");
        assert!(
            !edges[0].to.contains('.'),
            "to must be bare name; got {:?}",
            edges[0].to
        );
    }

    #[test]
    fn qualified_generic_constructor_records_rightmost_bare_type_name() {
        // `new java.util.ArrayList<Integer>()` → `to = "ArrayList"`.
        // The pattern walks generic_type → scoped_type_identifier and
        // anchors on the rightmost type_identifier.
        let fg = parse_at(
            "class C { void m() { var x = new java.util.ArrayList<Integer>(); } }",
            "/p/Test.java",
        );
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_one_call(&fg, "/p/Test.java:C::m", "ArrayList");
        assert!(
            !edges[0].to.contains('.') && !edges[0].to.contains('<'),
            "to must be bare name; got {:?}",
            edges[0].to
        );
    }

    #[test]
    fn this_constructor_call_records_to_this() {
        // `this(...)` inside a constructor body produces an
        // explicit_constructor_invocation node. Per the design brief,
        // these ARE genuine constructor invocations and SHOULD record
        // edges with `to = "this"`. Agents disambiguate from ordinary
        // method calls via the literal `"this"` callee name, which
        // cannot appear as an identifier in a normal call position.
        let fg = parse_at("class C { C(int x) { this(); } C() {} }", "/p/Test.java");
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_one_call(&fg, "/p/Test.java:C::C", "this");
    }

    #[test]
    fn super_constructor_call_records_to_super() {
        // `super(42)` inside a constructor body — same rule as
        // `this(...)`. The `superclass` clause does NOT produce a Calls
        // edge (3.5 will produce an Inherits edge for it instead); only
        // the `super(...)` invocation itself counts as a call.
        let fg = parse_at("class C extends B { C() { super(42); } }", "/p/Test.java");
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_one_call(&fg, "/p/Test.java:C::C", "super");
    }

    #[test]
    fn call_inside_lambda_body_records_enclosing_method() {
        // Lambdas are transparent: a call inside `() -> foo()` reports
        // the enclosing method as the `from`, not the lambda. Matches
        // the C#/Python/Go convention.
        let fg = parse_at(
            "class C { void m() { Runnable r = () -> foo(); } }",
            "/p/Test.java",
        );
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_one_call(&fg, "/p/Test.java:C::m", "foo");
    }

    #[test]
    fn call_inside_anonymous_class_method_records_outer_class_parent() {
        // Decision 4 transparency: the anonymous `new Runnable() { void
        // run() { foo(); } }` does NOT produce a Class symbol. The
        // `run` method DOES produce a Symbol — its parent is the
        // enclosing NAMED type (the outer `C`), NOT the (non-existent)
        // anonymous Class. The 3.2 test
        // `method_inside_anonymous_class_records_outer_named_entity_as_parent`
        // pins the symbol shape.
        //
        // For the call edge: `foo()` lives inside `run`, which is itself
        // a method_declaration. So `enclosing_function_id` stops at
        // `run` and reports `<path>:C::run` as the `from` — matching
        // the symbol ID 3.2 produced for the anonymous `run` method.
        // The anonymous-class boundary is transparent for parent
        // resolution, not for function-shape resolution.
        //
        // The `new Runnable()` itself is also a constructor call edge
        // (`from = C::m`, `to = "Runnable"`) — anonymous-class creation
        // syntax IS a `new T()` from the agent's perspective. This test
        // pins both edges to make the dual behavior explicit.
        let fg = parse_at(
            r#"class C { void m() { new Runnable() { public void run() { foo(); } }; } }"#,
            "/p/Test.java",
        );
        let edges = calls(&fg);
        assert_eq!(edges.len(), 2, "got: {:?}", edges);
        // (a) Anonymous-class creation records a constructor-style call
        //     edge to the base type. The base-type symbol is in JRE
        //     and won't resolve at query time, but the edge IS
        //     recorded — matching the `object_creation_expression`
        //     contract.
        assert_one_call(&fg, "/p/Test.java:C::m", "Runnable");
        // (b) The call inside `run` resolves to `<path>:C::run` via
        //     Decision 4 parent transparency.
        assert_one_call(&fg, "/p/Test.java:C::run", "foo");
    }

    #[test]
    fn call_inside_enum_constant_method_body_records_enum_type_parent() {
        // Decision 12 transparency: per-constant method bodies extract
        // as `Method` with parent = enum type name (e.g., `Planet`,
        // NOT a synthesised `Planet$EARTH`). The call edge's `from`
        // follows the same rule: a call inside `EARTH { void f() {
        // foo(); } }` reports `<path>:Planet::f`, NOT a synthesised
        // parent. The 3.2 test
        // `per_constant_enum_method_records_enum_type_as_parent` pins
        // the symbol shape.
        let fg = parse_at("enum P { E { void f() { foo(); } } }", "/p/Test.java");
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        // The call's enclosing function is `f`; `f`'s parent is the
        // enum type `P` via the Decision 12 transparency walk.
        assert_one_call(&fg, "/p/Test.java:P::f", "foo");
    }

    #[test]
    fn method_reference_with_identifier_rhs_records_rhs_as_callee() {
        // `String::length` → `to = "length"`. The grammar parses this
        // as `method_reference (identifier "String") :: (identifier
        // "length")`; the query captures only the RHS identifier.
        let fg = parse_at(
            "class C { void m() { java.util.function.Function<String, Integer> f = String::length; } }",
            "/p/Test.java",
        );
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_one_call(&fg, "/p/Test.java:C::m", "length");
    }

    #[test]
    fn this_method_reference_records_rhs_as_callee() {
        // `this::doIt` → `to = "doIt"`. The LHS is a `this` keyword
        // node, not an identifier; the query anchors past `"::"` and
        // captures the identifier RHS, so this works.
        let fg = parse_at(
            r#"
class C {
    void doIt(String s) {}
    void m() {
        java.util.function.Consumer<String> c = this::doIt;
    }
}
"#,
            "/p/Test.java",
        );
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_one_call(&fg, "/p/Test.java:C::m", "doIt");
    }

    #[test]
    fn constructor_method_reference_records_no_call_edge() {
        // `Type::new` constructor references — the RHS is the `new`
        // keyword token (kind = "new"), NOT an identifier. The query
        // pattern `(method_reference "::" (identifier) @x)` cleanly
        // skips them. This is a documented limitation: an agent asking
        // "what does this code construct via a method reference?" gets
        // no edge from this pattern; the same agent asking "what
        // `new T()` direct calls exist?" gets `object_creation_expression`
        // edges. Pin the no-edge behavior so a future refactor that
        // unintentionally captures these would fail.
        let fg = parse_at(
            r#"
class C {
    void m() {
        java.util.function.Supplier<java.util.ArrayList> s = java.util.ArrayList::new;
    }
}
"#,
            "/p/Test.java",
        );
        let edges = calls(&fg);
        assert!(
            edges.is_empty(),
            "constructor method reference (Type::new) must NOT produce a Calls edge; got: {:?}",
            edges
                .iter()
                .map(|e| (e.from.as_str(), e.to.as_str()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn cast_expression_produces_no_call_edge() {
        // `(String) o` parses as `cast_expression`, not as a method
        // invocation. No filter is needed; the query never matches it.
        // Mirrors the C# `cast_expression_does_not_produce_call_edge`
        // pin.
        let fg = parse_at(
            "class C { void m(Object o) { String s = (String) o; } }",
            "/p/Test.java",
        );
        let edges = calls(&fg);
        assert!(
            edges.is_empty(),
            "cast expression must not produce Calls edges; got: {:?}",
            edges
                .iter()
                .map(|e| (e.from.as_str(), e.to.as_str()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn instanceof_and_synchronized_produce_no_call_edges() {
        // `o instanceof String` parses as `instanceof_expression`;
        // `synchronized (x) { ... }` parses as `synchronized_statement`.
        // Neither is a method_invocation; the query never matches them.
        let fg = parse_at(
            r#"
class C {
    void m(Object o) {
        boolean b = o instanceof String;
        synchronized (this) {}
    }
}
"#,
            "/p/Test.java",
        );
        let edges = calls(&fg);
        assert!(
            edges.is_empty(),
            "instanceof/synchronized must not produce Calls edges; got: {:?}",
            edges
                .iter()
                .map(|e| (e.from.as_str(), e.to.as_str()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn array_creation_produces_no_call_edge() {
        // `new int[10]` parses as `array_creation_expression`, which is
        // a distinct node from `object_creation_expression`. The
        // constructor-call patterns in CALL_QUERIES never match it.
        let fg = parse_at(
            "class C { void m() { int[] a = new int[10]; } }",
            "/p/Test.java",
        );
        let edges = calls(&fg);
        assert!(
            edges.is_empty(),
            "array creation must not produce Calls edges; got: {:?}",
            edges
                .iter()
                .map(|e| (e.from.as_str(), e.to.as_str()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn annotations_produce_no_call_edges() {
        // `@Override`, `@Deprecated`, etc. parse as `marker_annotation`
        // / `annotation` nodes — not method_invocation. They are
        // transparent for call extraction (per Decision 8 — annotations
        // are metadata, not behavior).
        let fg = parse_at(
            r#"
@Deprecated
class C {
    @Override
    void m() {}
}
"#,
            "/p/Test.java",
        );
        let edges = calls(&fg);
        assert!(
            edges.is_empty(),
            "annotations must not produce Calls edges; got: {:?}",
            edges
                .iter()
                .map(|e| (e.from.as_str(), e.to.as_str()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn call_inside_constructor_records_constructor_symbol_as_from() {
        // The from-field for a call inside a constructor is
        // `<path>:Class::Class` (the constructor's name matches its
        // class — Java constructor syntax). Pins that constructor calls
        // (3.2's `ctor` capture) and ordinary method calls (3.2's
        // `method` capture) route through the same enclosing-function
        // rule.
        let fg = parse_at("class C { public C() { Init(); } }", "/p/Test.java");
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_one_call(&fg, "/p/Test.java:C::C", "Init");
    }

    #[test]
    fn call_in_default_interface_method_omits_parent() {
        // Decision 11: default interface methods extract as Function
        // (no parent). The call's `from` follows: `<path>:doFoo`, NOT
        // `<path>:I::doFoo`. Mirrors the C# pin in
        // `call_in_default_interface_method_omits_parent`.
        let fg = parse_at(
            "interface I { default void doFoo() { helper(); } }",
            "/p/Test.java",
        );
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_one_call(&fg, "/p/Test.java:doFoo", "helper");
    }

    #[test]
    fn call_edge_carries_file_and_line() {
        // Sanity: edge.file and edge.line populate as expected (file =
        // the path, line >= 1 for the 1-indexed call-site row). Don't
        // pin a precise row (whitespace fragility); just assert the
        // math is populated.
        let fg = parse_at(
            r#"
class C {
    void m() {
        foo();
    }
}
"#,
            "/p/Test.java",
        );
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].file, "/p/Test.java");
        assert!(edges[0].line >= 1);
    }

    #[test]
    fn empty_file_produces_no_call_edges() {
        let fg = parse("");
        let edges = calls(&fg);
        assert!(edges.is_empty(), "got: {:?}", edges);
    }

    #[test]
    fn call_from_field_lines_up_with_symbol_id() {
        // Concrete invariant: for every call edge whose `from` matches
        // a definition symbol's `symbol_id`, the relationship is
        // recoverable end-to-end. This is the contract that lets
        // `get_callers`/`get_callees` work without name guessing.
        let fg = parse_at(
            r#"
class C {
    void caller() { callee(); }
    void callee() {}
}
"#,
            "/p/Test.java",
        );
        let caller = sym(&fg, "caller");
        let edges = calls(&fg);
        let from = symbol_id(caller);
        let matched: Vec<&&Edge> = edges
            .iter()
            .filter(|e| e.from == from && e.to == "callee")
            .collect();
        assert_eq!(
            matched.len(),
            1,
            "expected the call edge's `from` to equal symbol_id(caller); got: from={:?}; edges={:?}",
            from,
            edges
                .iter()
                .map(|e| (e.from.as_str(), e.to.as_str()))
                .collect::<Vec<_>>()
        );
    }

    // ----------------------------------------------------------------
    // Phase 3.4 — import extraction
    // ----------------------------------------------------------------

    /// Filter to just the `Includes` edges of `fg`. Mirrors the C#
    /// plugin's `includes` test helper.
    fn includes(fg: &FileGraph) -> Vec<&Edge> {
        fg.edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Includes)
            .collect()
    }

    #[test]
    fn plain_import_records_includes_edge() {
        // The shipped contract: a plain `import com.foo.Bar;` records
        // one Includes edge with `to = "com.foo.Bar"` and
        // `kind = EdgeKind::Includes`. Mirrors C# 2.4's
        // `plain_using_records_includes_edge`.
        let fg = parse_at("import com.foo.Bar;\n", "/p/Test.java");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_eq!(edges[0].to, "com.foo.Bar");
        assert_eq!(edges[0].kind, EdgeKind::Includes);
    }

    #[test]
    fn dotted_import_preserves_full_path() {
        // A longer dotted chain survives verbatim — no segments dropped,
        // no segments swapped. The `scoped_identifier` text IS the
        // verbatim dotted path.
        let fg = parse_at("import com.foo.bar.baz.Qux;\n", "/p/Test.java");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_eq!(
            edges[0].to, "com.foo.bar.baz.Qux",
            "dotted path must be preserved verbatim"
        );
    }

    #[test]
    fn wildcard_import_preserves_asterisk() {
        // `import com.foo.*;` → `to = "com.foo.*"`. The trailing `.*` is
        // reconstructed at extraction time (tree-sitter parses the path
        // and the asterisk as separate named children); the wire-format
        // contract is that wildcards survive verbatim, matching the Rust
        // plugin's `use foo::*` rule.
        let fg = parse_at("import com.foo.*;\n", "/p/Test.java");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_eq!(
            edges[0].to, "com.foo.*",
            "wildcard imports must preserve the trailing .* verbatim"
        );
    }

    #[test]
    fn static_import_drops_static_modifier() {
        // `import static com.foo.Bar.STATIC_FIELD;` → `to =
        // "com.foo.Bar.STATIC_FIELD"`. The `static` keyword is an
        // anonymous child of `import_declaration` and is dropped by the
        // named-children walk; the field name (`STATIC_FIELD`) folds
        // into the scoped_identifier's text so no reconstruction is
        // needed beyond accepting the verbatim text.
        let fg = parse_at("import static com.foo.Bar.STATIC_FIELD;\n", "/p/Test.java");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_eq!(
            edges[0].to, "com.foo.Bar.STATIC_FIELD",
            "static-import target must include the field name; static modifier must be dropped"
        );
        assert!(
            !edges[0].to.contains("static"),
            "to field must not contain 'static'; got {:?}",
            edges[0].to
        );
    }

    #[test]
    fn static_wildcard_import_drops_static_and_keeps_wildcard() {
        // `import static com.foo.Bar.*;` → `to = "com.foo.Bar.*"`.
        // Combination form: the `static` keyword is anonymous (dropped),
        // the path is `com.foo.Bar`, and the trailing `.*` is
        // reconstructed from the sibling `asterisk` named child.
        let fg = parse_at("import static com.foo.Bar.*;\n", "/p/Test.java");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_eq!(
            edges[0].to, "com.foo.Bar.*",
            "static-wildcard import must drop `static` and preserve the trailing .*"
        );
        assert!(
            !edges[0].to.contains("static"),
            "to field must not contain 'static'; got {:?}",
            edges[0].to
        );
    }

    #[test]
    fn single_segment_import_is_supported() {
        // `import Foo;` — defensive cover. The grammar parses this as
        // `(import_declaration (identifier))` (NOT `scoped_identifier`)
        // and the `identifier` arm in `import_declaration_path` handles
        // it. Single-segment imports are rare in real Java code (the
        // language has no top-level package convention that produces
        // them) but the grammar accepts them and the parser must too.
        let fg = parse_at("import Foo;\n", "/p/Test.java");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_eq!(edges[0].to, "Foo");
    }

    #[test]
    fn multiple_imports_each_produce_edges() {
        let fg = parse_at(
            r#"
import com.foo.A;
import com.bar.B;
import com.baz.C;
"#,
            "/p/Test.java",
        );
        let edges = includes(&fg);
        assert_eq!(edges.len(), 3, "got: {:?}", edges);

        let tos: Vec<&str> = edges.iter().map(|e| e.to.as_str()).collect();
        assert!(
            tos.contains(&"com.foo.A"),
            "missing com.foo.A; got {:?}",
            tos
        );
        assert!(
            tos.contains(&"com.bar.B"),
            "missing com.bar.B; got {:?}",
            tos
        );
        assert!(
            tos.contains(&"com.baz.C"),
            "missing com.baz.C; got {:?}",
            tos
        );
    }

    #[test]
    fn from_field_is_the_file_path() {
        // The `Includes` edge's `from` is the file path (NOT a symbol
        // ID, NOT empty). Mirrors the Python/Go/Rust/C# convention; the
        // `Graph` engine routes Includes edges by file path.
        let fg = parse_at("import com.foo.Bar;\n", "/abs/Test.java");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1);
        assert_eq!(
            edges[0].from, "/abs/Test.java",
            "from must be the file path"
        );
        assert_eq!(
            edges[0].file, "/abs/Test.java",
            "file must match the path arg"
        );
    }

    #[test]
    fn import_edge_carries_correct_line() {
        // Line is 1-indexed and anchored at the `import_declaration`
        // node. Three imports on lines 2, 3, 4 (leading newline pushes
        // the first import past line 1). Mirrors the C# 2.4 line-anchor
        // pin.
        let fg = parse_at(
            "\nimport com.foo.A;\nimport com.bar.B;\nimport com.baz.C;\n",
            "/p/Test.java",
        );
        let edges = includes(&fg);
        assert_eq!(edges.len(), 3, "got: {:?}", edges);

        let line_for = |to: &str| -> u32 {
            edges
                .iter()
                .find(|e| e.to == to)
                .unwrap_or_else(|| panic!("missing edge to={:?}", to))
                .line
        };
        assert_eq!(line_for("com.foo.A"), 2);
        assert_eq!(line_for("com.bar.B"), 3);
        assert_eq!(line_for("com.baz.C"), 4);
    }

    #[test]
    fn empty_file_produces_no_includes_edges() {
        let fg = parse("");
        let edges = includes(&fg);
        assert!(edges.is_empty(), "got: {:?}", edges);
    }

    // ----------------------------------------------------------------
    // Phase 3.5 — inheritance extraction
    // ----------------------------------------------------------------

    /// Filter to just the `Inherits` edges of `fg`. Mirrors the `calls`
    /// and `includes` helpers above so each phase's assertions exercise
    /// only its own edge category. (Same pattern as the C# 2.5 test
    /// module.)
    fn inherits(fg: &FileGraph) -> Vec<&Edge> {
        fg.edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Inherits)
            .collect()
    }

    #[test]
    fn single_extends_produces_one_inherits_edge() {
        // `class Foo extends Bar { }` → 1 Inherits edge with from="Foo",
        // to="Bar". The bare-name `from`-field rule (Phase 1 / Phase 5
        // of RustRewrite, reaffirmed by Decision 9 in this design) is
        // load-bearing — see `crates/code-graph-graph/src/algorithms.rs`,
        // which looks up classes by `Symbol.name`.
        let fg = parse_at("class Foo extends Bar { }\n", "/p/Test.java");
        let edges = inherits(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        let e = edges[0];
        assert_eq!(e.from, "Foo");
        assert_eq!(e.to, "Bar");
        assert_eq!(e.kind, EdgeKind::Inherits);
    }

    #[test]
    fn multiple_implements_produces_one_edge_per_interface() {
        // `class Foo implements IBaz, IQux { }` → 2 Inherits edges from
        // the `super_interfaces > type_list > (_)` wildcard. The
        // ordering inside the type_list is preserved by tree-sitter but
        // not contractually asserted here — set membership is what
        // matters.
        let fg = parse_at("class Foo implements IBaz, IQux { }\n", "/p/Test.java");
        let edges = inherits(&fg);
        assert_eq!(
            edges.len(),
            2,
            "expected 2 Inherits edges; got: {:?}",
            edges
                .iter()
                .map(|e| (e.from.as_str(), e.to.as_str()))
                .collect::<Vec<_>>()
        );
        for e in &edges {
            assert_eq!(e.from, "Foo");
            assert_eq!(e.kind, EdgeKind::Inherits);
        }
        let tos: Vec<&str> = edges.iter().map(|e| e.to.as_str()).collect();
        assert!(tos.contains(&"IBaz"), "missing IBaz; got {:?}", tos);
        assert!(tos.contains(&"IQux"), "missing IQux; got {:?}", tos);
    }

    #[test]
    fn extends_and_implements_combined_produce_three_edges() {
        // `class Foo extends Bar implements IBaz, IQux { }` → 3 Inherits
        // edges, all from="Foo". One comes from `superclass` (`Bar`),
        // two from `super_interfaces` (`IBaz`, `IQux`). The plan brief's
        // headline example.
        let fg = parse_at(
            "class Foo extends Bar implements IBaz, IQux { }\n",
            "/p/Test.java",
        );
        let edges = inherits(&fg);
        assert_eq!(
            edges.len(),
            3,
            "expected 3 Inherits edges; got: {:?}",
            edges
                .iter()
                .map(|e| (e.from.as_str(), e.to.as_str()))
                .collect::<Vec<_>>()
        );
        for e in &edges {
            assert_eq!(e.from, "Foo");
            assert_eq!(e.kind, EdgeKind::Inherits);
        }
        let tos: Vec<&str> = edges.iter().map(|e| e.to.as_str()).collect();
        assert!(tos.contains(&"Bar"), "missing Bar; got {:?}", tos);
        assert!(tos.contains(&"IBaz"), "missing IBaz; got {:?}", tos);
        assert!(tos.contains(&"IQux"), "missing IQux; got {:?}", tos);
    }

    #[test]
    fn generic_class_and_base_preserve_type_params() {
        // `class Foo<T> extends Bar<T> { }` → 1 edge from="Foo<T>"
        // to="Bar<T>". Generic params survive in BOTH from and to per
        // Decision 9 (preserved verbatim, matching Rust's rule and the
        // C# 2.5 precedent — NOT Go's strip rule).
        //
        // **Known asymmetry pinned here**: while edge.from is "Foo<T>"
        // (generics preserved), Symbol.name for the same class is the
        // bare "Foo" (extract_definitions captures only the identifier
        // child). This means Graph::class_hierarchy at
        // crates/code-graph-graph/src/algorithms.rs cannot walk
        // inheritance for generic classes — it looks up symbols by
        // Symbol.name then walks adj.get(name), but the adjacency map
        // is keyed under "Foo<T>". Same limitation exists in the Rust
        // and C# plugins; the accepted Decision 9 trade-off is
        // documented in Phase 4.4's CLAUDE.md "Java Parser Limitations"
        // section.
        let fg = parse_at("class Foo<T> extends Bar<T> { }\n", "/p/Test.java");
        let edges = inherits(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        let e = edges[0];
        assert_eq!(
            e.from, "Foo<T>",
            "from must include generic param list verbatim"
        );
        assert_eq!(
            e.to, "Bar<T>",
            "to must include generic argument list verbatim"
        );

        // Side-by-side assertion making the asymmetry self-documenting
        // (mirrors C# 2.5's `generic_class_and_base_preserve_type_params`
        // test): Symbol.name is bare "Foo" (not "Foo<T>"). A future
        // refactor that changes extract_definitions to include generics
        // in Symbol.name would close the class_hierarchy gap but would
        // need to update this assertion at the same time.
        let s = sym(&fg, "Foo");
        assert_eq!(
            s.kind,
            SymbolKind::Class,
            "Symbol.name for class Foo<T> is bare 'Foo' — \
             the from/Symbol.name asymmetry is the documented limitation"
        );
    }

    #[test]
    fn generic_class_with_extends_constraint_does_not_pollute_to_field() {
        // `class Foo<T extends Comparable<T>> extends Bar<T> { }` — the
        // `extends Comparable<T>` is a CONSTRAINT inside the
        // `type_parameters > type_parameter > type_bound` sub-tree, NOT
        // a sibling of `superclass`. The query never sees `Comparable<T>`
        // through the inheritance path. Exactly one Inherits edge (to
        // `Bar<T>`); the constraint type is not double-counted. Mirrors
        // C# 2.5's `generic_class_with_where_constraints_does_not_pollute_to_field`.
        //
        // The `from` field is `"Foo<T extends Comparable<T>>"` —
        // [`enclosing_type_name_with_generics`] captures the verbatim
        // `type_parameters` text, and Java's constraint syntax lives
        // INSIDE that node, so the bound rides along. **This diverges
        // from C# 2.5**, where where-clauses sit in a SIBLING node and
        // the from-field is the cleaner `"Foo<T>"`. Decision 9 says
        // "preserve verbatim", which both plugins honor — Java's
        // grammar just produces a verbose verbatim. The to-field is
        // still clean `"Bar<T>"` because the constraint never enters
        // the superclass clause. Pin the actual observed text so any
        // future grammar change surfaces here rather than silently
        // shifting the contract.
        let fg = parse_at(
            "class Foo<T extends Comparable<T>> extends Bar<T> { }\n",
            "/p/Test.java",
        );
        let edges = inherits(&fg);
        assert_eq!(
            edges.len(),
            1,
            "constraint types must not leak into Inherits edges; got: {:?}",
            edges
                .iter()
                .map(|e| (e.from.as_str(), e.to.as_str()))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            edges[0].from, "Foo<T extends Comparable<T>>",
            "from preserves type_parameters verbatim (Decision 9) — the bound \
             text rides along because it lives inside the same syntactic node"
        );
        assert_eq!(
            edges[0].to, "Bar<T>",
            "to must reflect only the superclass clause — the constraint \
             type must NOT appear here"
        );
    }

    #[test]
    fn qualified_base_preserves_dotted_path() {
        // `class Foo extends Ns.Bar { }` → 1 edge to="Ns.Bar" (verbatim
        // scoped_type_identifier text; no resolution).
        let fg = parse_at("class Foo extends Ns.Bar { }\n", "/p/Test.java");
        let edges = inherits(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_eq!(edges[0].from, "Foo");
        assert_eq!(edges[0].to, "Ns.Bar");
    }

    #[test]
    fn interface_extending_interfaces_produces_inherits_edges() {
        // `interface I extends J, K { }` → 2 Inherits edges from="I".
        // Interfaces use the `extends_interfaces` node (unnamed-field
        // child of `interface_declaration`) — a different node kind
        // from `super_interfaces` on classes. Decision 2: interface
        // inheritance uses the same `Inherits` edge kind.
        let fg = parse_at("interface I extends J, K { }\n", "/p/Test.java");
        let edges = inherits(&fg);
        assert_eq!(edges.len(), 2, "got: {:?}", edges);
        for e in &edges {
            assert_eq!(e.from, "I");
            assert_eq!(e.kind, EdgeKind::Inherits);
        }
        let tos: Vec<&str> = edges.iter().map(|e| e.to.as_str()).collect();
        assert!(tos.contains(&"J"));
        assert!(tos.contains(&"K"));
    }

    #[test]
    fn record_implementing_interface_produces_inherits_edge() {
        // `record User(String name) implements Foo { }` → 1 edge
        // from="User" to="Foo". Records reach the inheritance extractor
        // through the `record_declaration` arm in INHERITANCE_QUERIES.
        // Records can ONLY implement interfaces — never extend a class —
        // so the only base-bearing clause is `interfaces:`.
        let fg = parse_at(
            "record User(String name) implements Foo { }\n",
            "/p/Test.java",
        );
        let edges = inherits(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_eq!(edges[0].from, "User");
        assert_eq!(edges[0].to, "Foo");
    }

    #[test]
    fn enum_implementing_interface_produces_inherits_edge() {
        // `enum Color implements Comparable<Color> { }` → 1 edge
        // from="Color" to="Comparable<Color>". Enums can ONLY implement
        // interfaces — never extend a class — so the only base-bearing
        // clause is `interfaces:`. The base is a `generic_type` node;
        // its verbatim text preserves the type argument.
        let fg = parse_at(
            "enum Color implements Comparable<Color> { }\n",
            "/p/Test.java",
        );
        let edges = inherits(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_eq!(edges[0].from, "Color");
        assert_eq!(edges[0].to, "Comparable<Color>");
    }

    #[test]
    fn sealed_interface_permits_clause_produces_no_inherits_edges() {
        // `sealed interface Shape permits Circle, Square { }` — the
        // `permits:` field is a SIBLING of `extends_interfaces`, not a
        // child. Decision 6: the `permits` clause produces NO edges.
        // No `extends_interfaces` clause is present here, so the total
        // count is zero.
        let fg = parse_at(
            "sealed interface Shape permits Circle, Square { }\n",
            "/p/Test.java",
        );
        let edges = inherits(&fg);
        assert!(
            edges.is_empty(),
            "permits clause must produce zero inheritance edges; got: {:?}",
            edges
                .iter()
                .map(|e| (e.from.as_str(), e.to.as_str()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn sealed_class_extends_plus_permits_only_records_extends_edge() {
        // `sealed class Animal extends LivingThing permits Dog, Cat { }`
        // — the `superclass:` field produces 1 edge (`LivingThing`); the
        // `permits:` field produces 0 edges (Decision 6). Net: exactly
        // one Inherits edge.
        let fg = parse_at(
            "sealed class Animal extends LivingThing permits Dog, Cat { }\n",
            "/p/Test.java",
        );
        let edges = inherits(&fg);
        assert_eq!(
            edges.len(),
            1,
            "permits clause must not contribute edges; got: {:?}",
            edges
                .iter()
                .map(|e| (e.from.as_str(), e.to.as_str()))
                .collect::<Vec<_>>()
        );
        assert_eq!(edges[0].from, "Animal");
        assert_eq!(edges[0].to, "LivingThing");
    }

    #[test]
    fn class_without_extends_or_implements_produces_no_inherits_edges() {
        // `class Foo { }` → 0 Inherits edges. No `superclass`, no
        // `super_interfaces`; the query produces zero matches.
        let fg = parse_at("class Foo { }\n", "/p/Test.java");
        let edges = inherits(&fg);
        assert!(edges.is_empty(), "got: {:?}", edges);
    }

    #[test]
    fn inherits_edge_carries_file_and_line() {
        // The edge.line is anchored at the *enclosing declaration*
        // (where the inheritance is declared), NOT at the base node.
        // Pin: with a leading newline the `class` keyword lands on
        // line 2; the edge's line must equal 2. Mirrors C# 2.5's
        // `inherits_edge_carries_file_and_line`.
        let fg = parse_at("\nclass Foo extends Bar { }\n", "/abs/Foo.java");
        let edges = inherits(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        let e = edges[0];
        assert_eq!(e.file, "/abs/Foo.java", "file must equal the path");
        assert_eq!(
            e.line, 2,
            "line must be the enclosing declaration's line (2), not the base's line"
        );
    }

    #[test]
    fn decision_2_no_edge_kind_distinction_between_extends_and_implements() {
        // Load-bearing Decision-2 pin: `class Foo extends Bar implements
        // IBaz, IQux { }` produces 3 edges, ALL with `EdgeKind::Inherits`.
        // Even though Java's grammar syntactically distinguishes `extends`
        // (the `superclass` field) from `implements` (the
        // `super_interfaces` field), the plugin deliberately collapses
        // both into one edge kind. Agents disambiguate from the target
        // Symbol's `kind` at query time, not from a separate
        // `Implements` edge.
        let fg = parse_at(
            "class Foo extends Bar implements IBaz, IQux { }\n",
            "/p/Test.java",
        );
        let edges = inherits(&fg);
        assert_eq!(edges.len(), 3);
        for e in &edges {
            assert_eq!(
                e.kind,
                EdgeKind::Inherits,
                "extends and implements must produce the same EdgeKind"
            );
        }
    }

    #[test]
    fn nested_class_with_base_records_inner_class_as_from() {
        // A nested class with a base list records the *inner* class's
        // name as `from`, not the outer. The query anchors on the
        // immediate `class_declaration` ancestor of the inheritance
        // clause. Mirrors C# 2.5's nested-class test.
        let fg = parse_at(
            r#"
class Outer {
    class Inner extends Base { }
}
"#,
            "/p/Test.java",
        );
        let edges = inherits(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_eq!(edges[0].from, "Inner");
        assert_eq!(edges[0].to, "Base");
    }

    #[test]
    fn empty_file_produces_no_inherits_edges() {
        let fg = parse("");
        let edges = inherits(&fg);
        assert!(edges.is_empty(), "got: {:?}", edges);
    }
}
