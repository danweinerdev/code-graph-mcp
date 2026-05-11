//! C# language plugin for code-graph-mcp.
//!
//! Uses tree-sitter (via the `tree-sitter` and `tree-sitter-c-sharp`
//! crates) to extract symbols, calls, import edges, and inheritance edges
//! from `.cs` source files.
//!
//! # Phase status
//!
//! Phase 2.1 shipped the crate scaffold: dependency wiring, empty query
//! string constants in [`queries`] that compile against tree-sitter-c-sharp
//! 0.23.5, the [`CSharpParser`] struct with cached `Query` objects, and
//! the [`LanguagePlugin`] impl.
//!
//! Phase 2.2 wires `extract_definitions` (classes, records, structs,
//! interfaces, enums, methods, constructors, local functions) and
//! switches `parse_to_filegraph` from the empty-graph stub to a real
//! tree-sitter-driven parse. Partial classes (Decision 3), default
//! interface methods (Decision 11), and extension methods (Decision 5) are
//! covered with inline tests.
//!
//! Phase 2.3 wires `extract_calls`, producing
//! [`EdgeKind::Calls`] edges for direct (`Foo()`), member-access
//! (`obj.Foo()`), chained (`a.B().C()` → 2 edges), null-conditional
//! (`obj?.Foo()`), generic (`Foo<int>()` → `to = "Foo"`), constructor
//! (`new Foo()` → `to = "Foo"`), lambda-body, and LINQ-select-clause
//! call patterns. The enclosing-function walk is transparent through
//! lambda and query expressions (a call inside `() => Foo()` or
//! `select Foo(x)` reports the enclosing method as `from`). Cast
//! expressions, `typeof`, `sizeof`, `default`, `checked`, and `unchecked`
//! parse as dedicated node kinds and do NOT produce spurious call edges.
//! `nameof(X)` parses as an `invocation_expression` in this grammar but
//! is filtered as a compile-time name operator, not a call.
//!
//! Phase 2.4 wires `extract_imports`, producing [`EdgeKind::Includes`]
//! edges for plain (`using System;`), dotted (`using
//! System.Collections.Generic;`), `using static` (the `static` modifier
//! is dropped from `to`), `using A = X.Y` (the alias is dropped; the
//! target path is preserved), and `global using` (the `global` modifier
//! is dropped) forms. The `using` directive inside a namespace block
//! (`namespace Foo { using Bar; ... }`) is captured because tree-sitter
//! queries walk the entire tree by default. Per Decision 7 the path is
//! recorded verbatim — no resolution against build metadata.
//!
//! Phase 2.5 wires `extract_inheritance`, producing
//! [`EdgeKind::Inherits`] edges for every `base_list` child under
//! `class_declaration`, `struct_declaration`, `interface_declaration`,
//! and `record_declaration`. Both class extension and interface
//! implementation produce the same edge kind per Decision 2 (no
//! separate `Implements` edge). Generic parameter text is preserved
//! verbatim per Decision 9: `class Foo<T> : Bar<T>` emits one edge with
//! `from = "Foo<T>", to = "Bar<T>"`. The `from` field is the bare class
//! name (including any generic parameter list), NOT a `symbol_id` — the
//! contract is consumed by `Graph::class_hierarchy` in
//! `crates/code-graph-graph/src/algorithms.rs`, which looks up classes
//! by `Symbol.name`.
//!
//! As of Phase 2.5, all four extractors are live; the C# plugin's
//! per-file parse surface is complete. Phase 2.6 will add the
//! `testdata/csharp/` corpus and the efcore dogfood baseline; the
//! plugin is NOT YET registered in the binary — Phase 4 wires
//! registration.
//!
//! # Default trait methods
//!
//! `CSharpParser` does NOT override [`LanguagePlugin::resolve_call`] or
//! [`LanguagePlugin::resolve_include`].
//!
//! - `resolve_call`: the default scope-aware heuristic (same file > same
//!   parent > same namespace > global) is the documented contract,
//!   mirroring the four shipped plugins. Phase 2.3's `extract_calls`
//!   produces purely syntactic edges — the `to` field is the rightmost
//!   identifier on the callee chain (`Foo` for `obj.Foo()`, `Foo` for
//!   `Ns.Type.Foo()`, `Foo` for `obj?.Foo()`, `Foo` for `Foo<int>()`,
//!   `Foo` for `new Foo()`). Resolution to a concrete Symbol happens at
//!   query time in the orchestrator's resolver, not in the parser.
//!   Extension method calls (`myString.CountWords()`) record `to =
//!   "CountWords"` and resolve through the same heuristic — the
//!   resolver may attribute the call to the syntactic
//!   `Extensions::CountWords` (correct) or to a same-named method on
//!   `string` if one exists (incorrect; same imperfection class as C++
//!   overloaded-function resolution per Decision 5).
//! - `resolve_include`: C# imports (`using System.Collections.Generic`)
//!   are dotted namespace paths, not filesystem paths — the default
//!   basename-match resolver returns `None` for them, mirroring the
//!   Python plugin's approach to dotted module-path imports.
//!
//! # C#-specific notes
//!
//! - **Default interface methods** are distinguished from abstract ones
//!   by **presence of a method body**, not by a `default` keyword.
//!   `interface I { void Foo() { ... } }` produces a Symbol;
//!   `interface I { void Bar(); }` does not (forward-declaration rule —
//!   per Decision 11's C# follow-up). Confirmed against tree-sitter-c-
//!   sharp 0.23.5: the discriminator is the `body:` field on
//!   `method_declaration`. The body kind can be `block` (curly-brace
//!   body) or `arrow_expression_clause` (`int Foo() => 42`); both forms
//!   count as "has body" and yield a Symbol. Abstract methods have no
//!   `body:` field at all and yield no Symbol.
//! - **Partial classes** (Decision 3) emit one Class symbol per
//!   declaration; merging across files is deferred to hierarchy-walk time
//!   via the bare-name `from`-field rule. The `partial` modifier is not
//!   inspected at extraction time.
//! - **Extension methods** are syntactic methods of their enclosing
//!   static class — the `this` modifier on the first parameter does NOT
//!   remap the parent (Decision 5). The extractor never inspects parameter
//!   modifiers.
//! - **Namespace resolution** walks the ancestor chain for
//!   `namespace_declaration` nodes (block form), joining names outermost-
//!   first with `.` to match C#'s dotted namespace syntax. The file-scoped
//!   form (`namespace MyApp;`) is a top-level sibling to subsequent
//!   declarations rather than their ancestor; we look for it at the
//!   compilation_unit level when no `namespace_declaration` ancestor is
//!   found. Dotted namespace names (`namespace A.B.C { ... }`) parse with
//!   the `name:` field as a `qualified_name`; the verbatim text (`A.B.C`)
//!   becomes the namespace string.
//! - **Records** (`record_declaration`) extract as
//!   [`SymbolKind::Class`] regardless of class-record vs struct-record
//!   form. tree-sitter-c-sharp 0.23.5 produces a single
//!   `record_declaration` node for `record User(string n)`, `record
//!   class User(string n)`, and `record struct Pt(int x, int y)`; all
//!   three dispatch to `Class` per Decision 11's C# follow-up (Java
//!   Decision 6 analog). Methods inside a record are recognised as
//!   members of the record (parent = record name), not orphan
//!   functions.

pub(crate) mod helpers;
pub(crate) mod queries;

use std::path::Path;

use code_graph_core::{Edge, EdgeKind, FileGraph, Language, Symbol, SymbolKind};
use code_graph_lang::helpers::{find_enclosing_kind, truncate_signature};
use code_graph_lang::{LanguagePlugin, ParseError};

use crate::helpers::enclosing_function_id;
use streaming_iterator::StreamingIterator;
use tree_sitter::{
    Language as TsLanguage, Node, Parser as TsParser, Query, QueryCursor, Tree as TsTree,
};

use crate::queries::{CALL_QUERIES, DEFINITION_QUERIES, IMPORT_QUERIES, INHERITANCE_QUERIES};

/// File extensions the C# parser claims. Single extension `.cs` —
/// C# does not have a stub-file analogue (no `.pyi`-equivalent), and
/// `.csx` script files use the same grammar but are out of scope for
/// this plan.
pub const EXTENSIONS: &[&str] = &[".cs"];

/// C# source-file parser. Holds the tree-sitter `Language` and the four
/// pre-compiled queries used to drive symbol/edge extraction. All four
/// extractors are live as of Phase 2.5.
///
/// Construct with [`CSharpParser::new`]; share across threads (queries
/// are `Send + Sync`).
pub struct CSharpParser {
    /// Compiled C# grammar. Held so per-call [`tree_sitter::Parser`]
    /// instances built inside `parse_file` can attach to it without
    /// rebuilding the `LanguageFn`.
    language: TsLanguage,
    /// Compiled definition query (drives [`Self::extract_definitions`]).
    def_query: Query,
    /// Compiled call query (drives [`Self::extract_calls`]).
    call_query: Query,
    /// Compiled import query (drives [`Self::extract_imports`]).
    import_query: Query,
    /// Compiled inheritance query (drives [`Self::extract_inheritance`]).
    inheritance_query: Query,
}

impl CSharpParser {
    /// Build a new parser, compiling all four tree-sitter queries against
    /// the pinned tree-sitter-c-sharp grammar. Returns an [`anyhow::Error`]
    /// (wrapping the query compiler's message) if any query fails to
    /// compile against the pinned grammar version.
    ///
    /// Successful return is the Phase 2.1 acceptance gate that proves
    /// every query string in [`queries`] parses against tree-sitter-c-
    /// sharp 0.23.5. As of Phase 2.5 all four query constants
    /// ([`DEFINITION_QUERIES`], [`CALL_QUERIES`], [`IMPORT_QUERIES`],
    /// [`INHERITANCE_QUERIES`]) are live and drive their corresponding
    /// extractors.
    pub fn new() -> anyhow::Result<Self> {
        let language: TsLanguage = tree_sitter_c_sharp::LANGUAGE.into();

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

    /// Parse `content` (UTF-8 bytes) as C# and produce a [`FileGraph`].
    /// Internal entry point for [`Self::parse_file`] (the trait method);
    /// kept crate-private so the public surface stays the trait method
    /// while each per-extractor method can be tested via `parse_file`
    /// without exposing them. Mirrors the Python plugin's
    /// `parse_to_filegraph` indirection.
    ///
    /// All four extractors are live (Phases 2.2-2.5): `parse_file`
    /// produces Symbol records, `Calls` edges, `Includes` edges, and
    /// `Inherits` edges from a single tree-sitter parse. Order matters
    /// only for readability — the four passes are independent.
    fn parse_to_filegraph(&self, path: &Path, content: &[u8]) -> Result<FileGraph, ParseError> {
        let tree = parse_tree(&self.language, content)?;
        let root = tree.root_node();
        let path_str = path.to_string_lossy().into_owned();

        let mut fg = FileGraph {
            path: path_str.clone(),
            language: Language::CSharp,
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
    /// Go/Python plugins' capture-name dispatch: each capture name from
    /// `DEFINITION_QUERIES` maps to a small branch that builds the right
    /// [`Symbol`].
    ///
    /// Per-capture-name behavior:
    ///
    /// - `class.name` from `class_declaration` → [`SymbolKind::Class`].
    ///   Parent is the immediate enclosing class/struct/interface/record
    ///   (or empty for top-level classes; nested classes record the
    ///   immediate outer type). Partial classes (Decision 3): the
    ///   `partial` modifier is NOT inspected — each declaration produces
    ///   its own Symbol; agents disambiguate via path/line.
    /// - `record.name` from `record_declaration` → [`SymbolKind::Class`].
    ///   Parent computed identically to `class.name`. All record forms
    ///   (`record User(string n)`, `record class User(string n)`,
    ///   `record struct Pt(int x, int y)`) parse to the same
    ///   `record_declaration` node in tree-sitter-c-sharp 0.23.5 and all
    ///   dispatch to `Class` per Decision 11's C# follow-up.
    /// - `struct.name` from `struct_declaration` → [`SymbolKind::Struct`].
    /// - `interface.name` from `interface_declaration` →
    ///   [`SymbolKind::Interface`].
    /// - `enum.name` from `enum_declaration` → [`SymbolKind::Enum`]. Enum
    ///   members are not extracted (Decision 12 analog for C#).
    /// - `method.name` from `method_declaration` → Method with parent =
    ///   enclosing type (defensive Function fallback if no enclosing type
    ///   is found; not reachable in well-formed C#). Branches on
    ///   enclosing scope:
    ///     * Inside `interface_declaration` with a `body:` field present →
    ///       [`SymbolKind::Function`], no parent (per Decision 11's C#
    ///       follow-up — default interface methods extract as Function,
    ///       matching Rust trait default methods).
    ///     * Inside `interface_declaration` with no `body:` field →
    ///       skipped (forward-declaration rule, no Symbol record).
    ///     * Inside `class_declaration` / `struct_declaration` /
    ///       `record_declaration` → [`SymbolKind::Method`] with parent =
    ///       enclosing type name. Extension methods (Decision 5) extract
    ///       here too — the `this` parameter modifier is not inspected;
    ///       the syntactic parent wins. Methods inside records record
    ///       the record name as parent.
    ///     * No enclosing class/struct/interface/record →
    ///       [`SymbolKind::Function`] with no parent (defensive:
    ///       shouldn't happen in well-formed C# but the extractor
    ///       doesn't assume well-formedness).
    /// - `ctor.name` from `constructor_declaration` →
    ///   [`SymbolKind::Method`] with parent = enclosing
    ///   class/struct/record name (defensive Function fallback if no
    ///   enclosing type is found; not reachable in well-formed C#).
    ///   The captured name *is* the type identifier (C# constructor
    ///   syntax — the constructor's name matches its enclosing type's
    ///   name). When emitted, `Symbol.name` is the constructor
    ///   identifier itself; the parent is filled from the enclosing type.
    /// - `local.name` from `local_function_statement` →
    ///   [`SymbolKind::Function`] with no parent. Local functions are
    ///   nested inside method bodies; treating them as Function (no
    ///   parent) matches the Python/Go conventions for nested
    ///   function-shaped declarations.
    ///
    /// Captures consumed without emitting a Symbol:
    /// - `class.def` / `record.def` / `struct.def` / `interface.def` /
    ///   `enum.def` / `method.def` / `ctor.def` / `local.def`: structural
    ///   anchors used by the queries to bind captures to the same
    ///   definition. The `name`-arm above already resolves the enclosing
    ///   definition via `find_enclosing_kind`.
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
                        let namespace = enclosing_namespace(def_node, content);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Class,
                            path,
                            def_node,
                            content,
                            parent,
                            namespace,
                        ));
                    }

                    "record.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "record_declaration")
                        else {
                            continue;
                        };
                        let parent = enclosing_type_name(def_node, content);
                        let namespace = enclosing_namespace(def_node, content);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Class,
                            path,
                            def_node,
                            content,
                            parent,
                            namespace,
                        ));
                    }

                    "struct.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "struct_declaration")
                        else {
                            continue;
                        };
                        let parent = enclosing_type_name(def_node, content);
                        let namespace = enclosing_namespace(def_node, content);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Struct,
                            path,
                            def_node,
                            content,
                            parent,
                            namespace,
                        ));
                    }

                    "interface.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "interface_declaration")
                        else {
                            continue;
                        };
                        let parent = enclosing_type_name(def_node, content);
                        let namespace = enclosing_namespace(def_node, content);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Interface,
                            path,
                            def_node,
                            content,
                            parent,
                            namespace,
                        ));
                    }

                    "enum.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "enum_declaration")
                        else {
                            continue;
                        };
                        let parent = enclosing_type_name(def_node, content);
                        let namespace = enclosing_namespace(def_node, content);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Enum,
                            path,
                            def_node,
                            content,
                            parent,
                            namespace,
                        ));
                    }

                    "method.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "method_declaration")
                        else {
                            continue;
                        };

                        // Decision 11: inside an interface_declaration, a
                        // method with a body extracts as Function (no
                        // parent) — matching the Rust trait-default-method
                        // contract. A method without a body is an abstract
                        // declaration and produces no Symbol (forward-
                        // declaration rule, mirroring C++/Rust/Go).
                        let in_interface =
                            find_enclosing_kind(def_node, "interface_declaration").is_some();
                        let has_body = def_node.child_by_field_name("body").is_some();

                        if in_interface {
                            if !has_body {
                                continue;
                            }
                            let namespace = enclosing_namespace(def_node, content);
                            fg.symbols.push(make_symbol(
                                text,
                                SymbolKind::Function,
                                path,
                                def_node,
                                content,
                                String::new(),
                                namespace,
                            ));
                            continue;
                        }

                        // Outside an interface: classify as Method when an
                        // enclosing class/struct exists; otherwise fall
                        // back to Function (defensive — well-formed C#
                        // can't have a method outside a type, but the
                        // extractor stays robust to recovery from syntax
                        // errors).
                        let parent = enclosing_type_name(def_node, content);
                        let namespace = enclosing_namespace(def_node, content);
                        let kind = if parent.is_empty() {
                            SymbolKind::Function
                        } else {
                            SymbolKind::Method
                        };
                        fg.symbols.push(make_symbol(
                            text, kind, path, def_node, content, parent, namespace,
                        ));
                    }

                    "ctor.name" => {
                        let Some(def_node) =
                            find_enclosing_kind(cap_node, "constructor_declaration")
                        else {
                            continue;
                        };
                        let parent = enclosing_type_name(def_node, content);
                        let namespace = enclosing_namespace(def_node, content);
                        let kind = if parent.is_empty() {
                            SymbolKind::Function
                        } else {
                            SymbolKind::Method
                        };
                        fg.symbols.push(make_symbol(
                            text, kind, path, def_node, content, parent, namespace,
                        ));
                    }

                    "local.name" => {
                        let Some(def_node) =
                            find_enclosing_kind(cap_node, "local_function_statement")
                        else {
                            continue;
                        };
                        // Local functions are nested inside method bodies;
                        // they are not members of the enclosing type, so
                        // they extract as Function with no parent (matches
                        // the Python/Go convention for nested function-
                        // shaped declarations).
                        let namespace = enclosing_namespace(def_node, content);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Function,
                            path,
                            def_node,
                            content,
                            String::new(),
                            namespace,
                        ));
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
    /// the C++/Rust/Go/Python plugins' `extract_calls`: each capture is a
    /// callee identifier, the line is anchored at the enclosing
    /// `invocation_expression` / `object_creation_expression` so the
    /// reported line tracks the call site (not an inner identifier on a
    /// chain-continuation line), and the `from` field is built by
    /// [`enclosing_function_id`] so it lines up exactly with the
    /// `symbol_id()` shape produced by [`Self::extract_definitions`].
    ///
    /// Per-capture-name behavior (single capture name `call.name` shared
    /// across all eight query patterns):
    ///
    /// - Direct call (`Foo()`) → `to = "Foo"`. Covers calls inside lambda
    ///   bodies (`() => Foo()`), LINQ select clauses (`select Foo(x)`),
    ///   property accessors, field initializers, and expression-bodied
    ///   methods (each parses the leaf as its own `invocation_expression`
    ///   with an `identifier` callee).
    /// - Member-access call (`obj.Foo()`, `this.Foo()`, `base.Foo()`,
    ///   `Ns.Type.Method()`) → `to = "Foo"` / `to = "Method"` (rightmost
    ///   identifier only). Chained calls (`a.B().C()`) produce two
    ///   matches because the grammar nests `invocation_expression`s; the
    ///   query returns one edge per chain link.
    /// - Null-conditional call (`obj?.Foo()`) → `to = "Foo"`. The
    ///   `member_binding_expression` node carries the rightmost
    ///   identifier.
    /// - Generic call (`Foo<int>()`) → `to = "Foo"` (NOT `Foo<int>`).
    ///   The query captures only the inner identifier of the
    ///   `generic_name`, dropping the type-argument list.
    /// - Constructor (`new Foo()`, `new Ns.Foo()`, `new List<int>()`,
    ///   `new Ns.List<int>()`) → `to = "Foo"` / `to = "List"`. Recorded
    ///   as a call to the constructed type's bare name; the agent
    ///   interprets the edge as construction. Matches the convention
    ///   Python uses for `MyClass()`.
    ///
    /// **Lambda and LINQ transparency** is implemented in
    /// [`enclosing_function_id`] — the enclosing-function walk skips
    /// `lambda_expression` and `query_expression` nodes, so calls inside
    /// `() => Foo()` or `select Foo(x)` report the enclosing
    /// method/constructor as the `from` field, not the lambda or query.
    ///
    /// **Callee filtering.** C# casts (`(Foo)x`) parse as a distinct
    /// `cast_expression` node, NOT `invocation_expression`, so no
    /// `is_cpp_cast`-style filter is needed. `typeof`, `sizeof`,
    /// `default`, `checked`, and `unchecked` similarly have dedicated
    /// expression kinds and never reach this query. The one exception
    /// is `nameof(X)`: it IS an `invocation_expression` in tree-sitter-
    /// c-sharp 0.23.5 (the grammar treats `nameof` as an ordinary call),
    /// but `nameof` is a compile-time name operator, not a method call.
    /// We filter it out of the call graph — same precedent as the C++
    /// plugin's `is_cpp_cast` filter for `static_cast` and friends.
    /// Without this filter, every method that uses `nameof` for logging
    /// or reflection would record a call to `nameof`, polluting
    /// `get_callees` results.
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

                let callee = cap_node.utf8_text(content).unwrap_or("");
                if callee.is_empty() {
                    continue;
                }

                // Filter `nameof(X)` — semantically a compile-time name
                // operator, not a call. Mirrors the C++ `is_cpp_cast`
                // precedent for `static_cast` and friends.
                if callee == "nameof" {
                    continue;
                }

                // Anchor the line at the enclosing call/object-creation
                // expression so the reported line tracks the call site.
                // For chained or multi-line calls the inner identifier
                // can land on a continuation line; the outer
                // invocation_expression's start_position is the
                // semantically-correct anchor. Falls back to the capture
                // node when neither ancestor is found (defensive — the
                // query patterns guarantee at least one).
                let call_node = find_enclosing_kind(cap_node, "invocation_expression")
                    .or_else(|| find_enclosing_kind(cap_node, "object_creation_expression"))
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

    /// Run the import query and produce [`EdgeKind::Includes`] edges,
    /// one per `using_directive`. Mirrors the C++/Rust/Go/Python plugins'
    /// import extraction conventions: the edge `from` is the source-file
    /// path (NOT a symbol ID); the edge `to` is the dotted namespace
    /// path *as written* in the source, per Decision 7.
    ///
    /// **Modifier handling** (verified against tree-sitter-c-sharp
    /// 0.23.5 via scratch-crate probe):
    /// - `using System;` → `to = "System"`.
    /// - `using System.Collections.Generic;` → `to =
    ///   "System.Collections.Generic"` (the `qualified_name` text is
    ///   preserved verbatim).
    /// - `using static System.Console;` → `to = "System.Console"`. The
    ///   `static` keyword is an anonymous child of `using_directive` and
    ///   is excluded from the path; the path is the named identifier /
    ///   qualified_name child.
    /// - `using FooAlias = Some.Long.Type.Name;` → `to =
    ///   "Some.Long.Type.Name"`. The alias name is held in the `name:`
    ///   field of `using_directive`; the target path is the named child
    ///   that is NOT the `name:` field (after the anonymous `=`
    ///   keyword). We capture the latter — same rule as Python `import
    ///   foo as f` (path captured; alias dropped) and Go aliased
    ///   imports.
    /// - `global using System.Linq;` → `to = "System.Linq"`. The
    ///   `global` keyword is an anonymous child preceding `using` and
    ///   is excluded.
    /// - Combination forms (`global using static X.Y;`, `global using A
    ///   = X.Y;`) parse to the same `using_directive` node and follow
    ///   the same rules — modifiers are anonymous, path is the named
    ///   non-`name:` child.
    ///
    /// **Namespace-scoped usings.** `namespace Foo { using Bar; ... }`
    /// parses with the `using_directive` as a child of the namespace's
    /// `declaration_list`. Tree-sitter queries walk the entire tree, so
    /// the `(using_directive) @using.dir` pattern matches both top-level
    /// and namespace-scoped usings. There is intentionally no
    /// module-top-level filter (in contrast to Python's
    /// `is_module_top_level` filter for conditional imports) — both
    /// kinds of C# `using` produce identical dependency information.
    ///
    /// **Line anchoring.** The line is anchored at the
    /// `using_directive` node itself (`start_position().row + 1`),
    /// matching the convention every other plugin uses for import-style
    /// statements.
    ///
    /// **Resolution.** `CSharpParser` does NOT override
    /// `resolve_include`. The default basename-match resolver returns
    /// `None` for these dotted namespace paths (they are not filesystem
    /// paths), mirroring Python — see the crate-level docstring for the
    /// rationale.
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
                if cap_name != "using.dir" {
                    continue;
                }

                let Some(path_text) = using_directive_path(cap_node, content) else {
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
    /// edges, one per base in each `base_list`. Mirrors the C++/Rust
    /// plugins' `extract_inheritance` for the bare-name `from`-field
    /// contract (see Phase 1 / Phase 5 of RustRewrite).
    ///
    /// **Per-match shape.** The query returns one match *per base* — a
    /// declaration with three bases (`class Foo : Bar, IBaz, IQux`)
    /// produces three matches, each carrying the same `@inherit.def`
    /// (the enclosing declaration node) and a distinct `@inherit.base`
    /// (one of `Bar` / `IBaz` / `IQux`). The `inherit.base` capture is
    /// a single named child of `base_list`, which can be any of:
    ///
    /// - `identifier` — bare type name (`Bar`)
    /// - `qualified_name` — dotted type name (`Ns.Bar`,
    ///   `Ns.Generic<int,string>`, `global::Ns.Bar`)
    /// - `generic_name` — generic type (`Bar<T>`,
    ///   `IComparable<Pt>`)
    ///
    /// For every form the captured node's `utf8_text` is the verbatim
    /// source text — generic argument lists, dotted qualifications, and
    /// `global::` prefixes survive into the `to` field as written
    /// (Decision 9 — generic params preserved verbatim).
    ///
    /// **`from`-field composition.** The `from` field is the bare
    /// enclosing type name + the type-parameter list text (if any). For
    /// `class Foo` → `from = "Foo"`; for `class Foo<T>` →
    /// `from = "Foo<T>"`. See [`enclosing_type_name_with_generics`]
    /// for the full rule and a documented known asymmetry: `Symbol.name`
    /// in `extract_definitions`'s output is bare (`"Foo"` for
    /// `class Foo<T>`), so `Graph::class_hierarchy` cannot walk
    /// inheritance edges for generic classes. Same limitation as the
    /// Rust plugin. The
    /// `type_parameter_list` node kind is the canonical place tree-
    /// sitter records the angle-bracket parameter list; it appears as
    /// an unnamed-field child of `class_declaration` /
    /// `struct_declaration` / `record_declaration` and as a
    /// `type_parameters:`-field child of `interface_declaration` (the
    /// grammar is asymmetric across declaration kinds in 0.23.5; the
    /// kind-based scan in [`enclosing_type_name_with_generics`] handles
    /// both).
    ///
    /// **Decision 2 — no edge-kind distinction.** All bases — whether
    /// they reference a class, an interface, or a struct — produce the
    /// same [`EdgeKind::Inherits`]. The agent disambiguates from the
    /// target Symbol's `kind` at query time.
    ///
    /// **Edge shape per match:**
    /// - `from = bare_name + type_parameter_list` (e.g. `Foo<T>`)
    /// - `to = base_node.utf8_text` (verbatim — `Bar<T>`, `Ns.Bar`, etc.)
    /// - `kind = EdgeKind::Inherits`
    /// - `file = path`
    /// - `line = enclosing declaration's start_position().row + 1`
    ///   (i.e., the class/struct/interface/record declaration line, not
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
                    // Defensive catch-all — guards against future
                    // grammar revisions that might introduce additional
                    // capture names under the same query patterns.
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

impl LanguagePlugin for CSharpParser {
    fn id(&self) -> Language {
        Language::CSharp
    }

    fn extensions(&self) -> &'static [&'static str] {
        EXTENSIONS
    }

    /// Parse `content` (UTF-8 bytes) as C# and produce a [`FileGraph`].
    /// All four extractors (definitions, calls, imports, inheritance)
    /// are live as of Phase 2.5.
    fn parse_file(&self, path: &Path, content: &[u8]) -> Result<FileGraph, ParseError> {
        self.parse_to_filegraph(path, content)
    }

    // resolve_call and resolve_include intentionally NOT overridden — see
    // the crate-level docstring for the rationale (default heuristic
    // matches the C++/Rust/Go/Python plugins; default basename resolver
    // is a no-op for C#'s dotted namespace `using` paths, which is the
    // intended behavior).
}

/// Build a tree-sitter [`TsTree`] for `content` against the C# grammar.
/// The caller-supplied [`TsLanguage`] is borrowed; the returned tree owns
/// its AST. Returns [`ParseError::Parse`] if `set_language` fails or if
/// tree-sitter declines to produce a tree (e.g. on cancellation). Mirrors
/// `parse_tree` in the C++/Rust/Go/Python plugins byte-for-byte modulo the
/// language identity.
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
/// indices, matching the C++/Rust/Go/Python plugins' silent fallback.
fn capture_name_for_index<'a>(cap_names: &[&'a str], index: u32) -> &'a str {
    cap_names.get(index as usize).copied().unwrap_or("")
}

/// Return the immediate enclosing class/record/struct/interface name for
/// a declaration node, walking ancestors. Returns `""` when the
/// declaration is top-level (or only nested inside namespaces). Used to
/// populate the `parent` field for both nested types and
/// methods/constructors.
///
/// Walks past `cap_node.parent()` so a class at a top-level position
/// returns `""` (not its own name); a class nested inside another class
/// records the outer class as parent. `record_declaration` is recognised
/// as a type ancestor — methods declared inside a `record` body record
/// the record name as parent, NOT as orphan free functions.
fn enclosing_type_name(def_node: Node<'_>, content: &[u8]) -> String {
    let mut current = def_node.parent();
    while let Some(n) = current {
        match n.kind() {
            "class_declaration"
            | "record_declaration"
            | "struct_declaration"
            | "interface_declaration" => {
                if let Some(name_node) = n.child_by_field_name("name") {
                    return name_node.utf8_text(content).unwrap_or("").to_owned();
                }
                return String::new();
            }
            _ => {}
        }
        current = n.parent();
    }
    String::new()
}

/// Return the bare enclosing-type name *with* the generic parameter
/// list text appended (when present), suitable for the `from` field of
/// an `Inherits` edge. For `class Foo` returns `"Foo"`; for
/// `class Foo<T>` returns `"Foo<T>"`; for `interface I<T, U>` returns
/// `"I<T, U>"`.
///
/// `def_node` must be one of `class_declaration`, `struct_declaration`,
/// `interface_declaration`, or `record_declaration`. Other node kinds
/// return the empty string defensively.
///
/// Decision 9 (generic parameter text preserved verbatim) — and the
/// Phase 1 / Phase 5 bare-name `from`-field rule — are both enforced
/// here. The result is the bare type name, EXCEPT for generic types
/// where the `type_parameter_list` text is appended verbatim.
///
/// **Known asymmetry with `Symbol.name`** (matches the Rust plugin's
/// pre-existing behavior — accepted as a documented limitation):
/// `extract_definitions` stores `Symbol.name` as the bare identifier
/// (`"Foo"` for `class Foo<T>`), but the `from` field of an `Inherits`
/// edge produced here is the generics-preserving form (`"Foo<T>"`).
/// `Graph::class_hierarchy` at `crates/code-graph-graph/src/algorithms.rs`
/// looks up symbols by `Symbol.name` then walks `adj.get(name)` — for a
/// generic class queried as `class_hierarchy("Foo")` the symbol is found
/// but the adjacency lookup misses (edges are keyed under `"Foo<T>"`).
/// Generic-class hierarchy walks are effectively unsupported by the
/// graph layer in its current form. Same limitation exists in the Rust
/// plugin and is the trade-off Decision 9 inherits from the Rust
/// precedent. Phase 4.4's CLAUDE.md "C# Parser Limitations" section
/// documents this for agent-facing visibility.
///
/// `type_parameter_list` is an unnamed-field child on
/// `class_declaration` / `struct_declaration` / `record_declaration`,
/// but a `type_parameters:`-field child on `interface_declaration` in
/// tree-sitter-c-sharp 0.23.5. The asymmetry across declaration kinds
/// is real; we scan named children by *kind* to handle both cases
/// without per-declaration branching.
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
        if child.kind() == "type_parameter_list" {
            let tpl = child.utf8_text(content).unwrap_or("");
            if !tpl.is_empty() {
                return format!("{bare}{tpl}");
            }
        }
    }

    bare
}

/// Return the dotted namespace path for a declaration node by walking
/// `namespace_declaration` ancestors and joining their names outermost-
/// first with `.`. Falls back to the file-scoped form
/// (`file_scoped_namespace_declaration`) at the compilation_unit level
/// when no block-form namespace ancestor is found.
///
/// Examples:
/// - `namespace A { class X { void M() { } } }` → `M`'s namespace = `A`
/// - `namespace A { namespace B { class X { } } }` → `X`'s namespace =
///   `A.B`
/// - `namespace A.B.C { class X { } }` → `X`'s namespace = `A.B.C`
///   (the qualified_name's verbatim text)
/// - `namespace MyApp; class X { }` → `X`'s namespace = `MyApp` (the
///   file-scoped form is a sibling, not ancestor)
fn enclosing_namespace(def_node: Node<'_>, content: &[u8]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut current = def_node.parent();
    while let Some(n) = current {
        if n.kind() == "namespace_declaration" {
            if let Some(name_node) = n.child_by_field_name("name") {
                let text = name_node.utf8_text(content).unwrap_or("").to_owned();
                if !text.is_empty() {
                    parts.push(text);
                }
            }
        }
        current = n.parent();
    }
    parts.reverse();

    if !parts.is_empty() {
        return parts.join(".");
    }

    // No block-form namespace ancestor — check for a file-scoped
    // namespace declaration at the compilation_unit level. The
    // file-scoped form is a sibling of subsequent declarations, not their
    // ancestor.
    if let Some(comp_unit) = find_enclosing_kind(def_node, "compilation_unit") {
        let mut cursor = comp_unit.walk();
        for child in comp_unit.children(&mut cursor) {
            if child.kind() == "file_scoped_namespace_declaration" {
                if let Some(name_node) = child.child_by_field_name("name") {
                    return name_node.utf8_text(content).unwrap_or("").to_owned();
                }
            }
        }
    }

    String::new()
}

/// Return the dotted namespace path written in a `using_directive`
/// node, or `None` if the directive has no recoverable path (defensive
/// — well-formed C# always has one named path child).
///
/// Rules (per the tree-sitter-c-sharp 0.23.5 probe at Phase 2.4):
/// - **Plain / static / global / static+global** (no `name:` field):
///   the path is the single named child whose kind is either
///   `identifier` (single-segment path) or `qualified_name` (dotted).
///   Modifier keywords (`global`, `static`) are anonymous children and
///   are skipped.
/// - **Alias** (with `name:` field — `using A = X.Y;` or `global using A
///   = X.Y;`): the path is the first named child whose kind is
///   `identifier` or `qualified_name` AND that is NOT the
///   `name:`-field child. The alias name is held in the `name:` field
///   and is intentionally dropped, mirroring Python `import foo as f` →
///   `to = "foo"`.
///
/// The returned text is the verbatim source text of the path node, so
/// `using System.Collections.Generic;` returns `Some("System.Collections.Generic")`
/// (the whole `qualified_name`'s text) and the dotted structure is
/// preserved without re-walking the qualified_name children.
fn using_directive_path(directive: Node<'_>, content: &[u8]) -> Option<String> {
    // If `name:` is set, this is an alias form. The path is the first
    // named identifier/qualified_name child that is NOT the alias node.
    let alias_node = directive.child_by_field_name("name");

    let mut cursor = directive.walk();
    for child in directive.children(&mut cursor) {
        if !child.is_named() {
            continue;
        }
        if let Some(alias) = alias_node {
            if child.id() == alias.id() {
                continue;
            }
        }
        // `alias_qualified_name` covers the bare `using global::System;`
        // form — tree-sitter-c-sharp 0.23.5 produces an
        // `alias_qualified_name` node directly under `using_directive`
        // when the path starts with `global::` and has no further
        // qualification. The verbatim text (`"global::System"`) is what
        // Decision 7 wants in the `to` field.
        if matches!(
            child.kind(),
            "identifier" | "qualified_name" | "alias_qualified_name"
        ) {
            return Some(child.utf8_text(content).unwrap_or("").to_owned());
        }
    }
    None
}

/// Build a [`Symbol`] from a definition node. Centralises the row/column/
/// signature math so each branch in `extract_definitions` stays small.
/// Mirrors the C++/Rust/Go/Python plugins' `make_symbol`.
#[allow(clippy::too_many_arguments)]
fn make_symbol(
    name: &str,
    kind: SymbolKind,
    path: &str,
    def_node: Node<'_>,
    content: &[u8],
    parent: String,
    namespace: String,
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
        namespace,
        parent,
        language: Language::CSharp,
    }
}

#[cfg(test)]
mod tests {
    //! Phase 2.1 structural smoke tests + Phase 2.2 definition-extraction
    //! coverage + Phase 2.3 call-extraction coverage + Phase 2.4
    //! import-extraction coverage + Phase 2.5 inheritance-extraction
    //! coverage. All four extractors are exercised here.
    use super::*;
    use code_graph_core::symbol_id;
    use code_graph_lang::LanguagePlugin;

    // ----------------------------------------------------------------
    // Phase 2.1 — structural smoke tests
    // ----------------------------------------------------------------

    #[test]
    fn parser_is_object_safe_and_id_returns_csharp() {
        let p: Box<dyn LanguagePlugin> = Box::new(CSharpParser::new().unwrap());
        assert_eq!(p.id(), Language::CSharp);
    }

    // ----------------------------------------------------------------
    // Phase 2.2 — definition extraction
    // ----------------------------------------------------------------

    /// Parse `src` against `CSharpParser` at a synthetic absolute path.
    /// Used by every Phase 2.2 behavioral test below.
    fn parse(src: &str) -> FileGraph {
        parse_at(src, "/tmp/test.cs")
    }

    /// Parse `src` against `CSharpParser` at a caller-chosen path. Lets
    /// the partial-class anti-regression exercise two distinct paths.
    fn parse_at(src: &str, path: &str) -> FileGraph {
        let p = CSharpParser::new().unwrap();
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
        assert_eq!(fg.path, "/tmp/test.cs");
        assert_eq!(fg.language, Language::CSharp);
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
        assert!(
            s.namespace.is_empty(),
            "top-level class outside any namespace must have empty namespace"
        );
    }

    #[test]
    fn struct_produces_struct_kind() {
        let fg = parse("struct Pt { public int X; }");
        let s = sym(&fg, "Pt");
        assert_eq!(s.kind, SymbolKind::Struct);
    }

    #[test]
    fn interface_produces_interface_kind() {
        let fg = parse("interface IFoo { }");
        let s = sym(&fg, "IFoo");
        assert_eq!(s.kind, SymbolKind::Interface);
    }

    #[test]
    fn enum_produces_enum_kind_and_members_are_not_extracted() {
        let fg = parse("enum Status { Active, Inactive, Pending }");
        // Exactly one Symbol — the enum type. The enum members
        // (Active/Inactive/Pending) are NOT extracted as symbols
        // (Decision 12 analog for C#).
        assert_eq!(
            fg.symbols.len(),
            1,
            "enum members must not produce symbols: got {:?}",
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
    public void Bar() { }
}
"#,
        );
        let m = sym(&fg, "Bar");
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.parent, "Foo");
    }

    #[test]
    fn method_in_struct_produces_method_kind_with_struct_parent() {
        let fg = parse(
            r#"
struct Pt {
    public int Sum() { return 0; }
}
"#,
        );
        let m = sym(&fg, "Sum");
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.parent, "Pt");
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

    // ---- Decision 11: default interface methods --------------------

    #[test]
    fn default_interface_method_extracts_as_function_no_parent() {
        // `void Foo() { ... }` inside an interface (Decision 11) —
        // method body present → extracts as Function, NOT Method;
        // parent is empty (matches Rust trait-default-method rule).
        let fg = parse(
            r#"
interface I {
    void Foo() { return; }
}
"#,
        );
        let s = sym(&fg, "Foo");
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
    fn expression_bodied_default_interface_method_extracts_as_function() {
        // `int Foo() => 42` inside an interface — the body field is an
        // arrow_expression_clause, not a block, but it counts as
        // "has body" for the default-interface-method rule.
        let fg = parse(
            r#"
interface I {
    int Foo() => 42;
}
"#,
        );
        let s = sym(&fg, "Foo");
        assert_eq!(
            s.kind,
            SymbolKind::Function,
            "expression-bodied default interface method must extract as Function"
        );
        assert!(s.parent.is_empty());
    }

    #[test]
    fn abstract_interface_method_produces_no_symbol() {
        // `void Bar();` inside an interface (no body) — forward
        // declaration; produces no Symbol record (mirroring C++/Rust/Go).
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
        // the abstract one is dropped. This is the load-bearing
        // anti-regression for Decision 11.
        let fg = parse(
            r#"
interface I {
    void HasBody() { return; }
    void NoBody();
}
"#,
        );
        // Interface + HasBody method = 2 symbols total; NoBody is
        // filtered.
        assert_eq!(
            fg.symbols.len(),
            2,
            "expected interface + HasBody; got: {:?}",
            fg.symbols
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
        );
        let s = sym(&fg, "HasBody");
        assert_eq!(s.kind, SymbolKind::Function);
        assert!(fg.symbols.iter().all(|sym| sym.name != "NoBody"));
    }

    // ---- Decision 5: extension methods -----------------------------

    #[test]
    fn extension_method_records_static_class_as_parent_not_extended_type() {
        // `this string s` parameter modifier marks `Count` as an
        // extension method on `string` (semantically). Decision 5:
        // the extractor uses the *syntactic* enclosing parent (`Ext`),
        // not the semantic extended type (`string`). The `this`
        // modifier is not inspected.
        let fg = parse(
            r#"
static class Ext {
    public static int Count(this string s) { return s.Length; }
}
"#,
        );
        let m = sym(&fg, "Count");
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(
            m.parent, "Ext",
            "extension method must record syntactic parent (Ext), not extended type (string)"
        );
    }

    // ---- Decision 3: partial classes -------------------------------

    #[test]
    fn two_partial_class_declarations_in_same_file_yield_two_class_symbols() {
        // Single-file form of the partial-class case: two `partial
        // class Foo {}` declarations side-by-side in one file. Each
        // produces its own Class symbol with the bare name `Foo`;
        // merging across declarations is deferred to hierarchy-walk
        // time.
        let fg = parse(
            r#"
public partial class Foo { void A() { } }
public partial class Foo { void B() { } }
"#,
        );
        let foos: Vec<&Symbol> = fg
            .symbols
            .iter()
            .filter(|s| s.name == "Foo" && s.kind == SymbolKind::Class)
            .collect();
        assert_eq!(
            foos.len(),
            2,
            "partial classes in the same file must produce two Class symbols; got: {:?}",
            foos
        );
    }

    #[test]
    fn two_partial_class_declarations_across_files_yield_two_class_symbols() {
        // Anti-regression for Decision 3 (verification field): two
        // `partial class Foo {}` declarations in *different files*
        // produce exactly two Class symbols, distinguishable by
        // `Symbol.file` + `Symbol.line`. The extractor does NOT merge
        // across files at extraction time.
        let fg_a = parse_at(
            "public partial class Foo { void A() { } }\n",
            "/tmp/partial_a.cs",
        );
        let fg_b = parse_at(
            "public partial class Foo { void B() { } }\n",
            "/tmp/partial_b.cs",
        );

        let a_class = fg_a
            .symbols
            .iter()
            .find(|s| s.name == "Foo" && s.kind == SymbolKind::Class)
            .expect("file A must have a Class Foo");
        let b_class = fg_b
            .symbols
            .iter()
            .find(|s| s.name == "Foo" && s.kind == SymbolKind::Class)
            .expect("file B must have a Class Foo");

        // Same name → same key into `(Language, name)` SymbolIndex,
        // matching the partial-class merge-by-bare-name rule.
        assert_eq!(a_class.name, b_class.name);
        // Different file paths → two distinct Symbol records.
        assert_ne!(
            a_class.file, b_class.file,
            "partial-class symbols must carry distinct file paths"
        );
        // Each file emits exactly one Foo Class symbol — extraction
        // does not merge across declarations at this layer.
        assert_eq!(
            fg_a.symbols
                .iter()
                .filter(|s| s.name == "Foo" && s.kind == SymbolKind::Class)
                .count(),
            1
        );
        assert_eq!(
            fg_b.symbols
                .iter()
                .filter(|s| s.name == "Foo" && s.kind == SymbolKind::Class)
                .count(),
            1
        );
    }

    // ---- Local functions ------------------------------------------

    #[test]
    fn local_function_inside_method_produces_function_no_parent() {
        // `void Helper() { }` declared inside a method body extracts as
        // Function with no parent (matching Python/Go conventions for
        // nested function-shaped declarations).
        let fg = parse(
            r#"
class C {
    public void Foo() {
        void Helper() { }
        Helper();
    }
}
"#,
        );
        let h = sym(&fg, "Helper");
        assert_eq!(h.kind, SymbolKind::Function);
        assert!(
            h.parent.is_empty(),
            "local function must have empty parent; got {:?}",
            h.parent
        );
    }

    // ---- Records ---------------------------------------------------

    #[test]
    fn record_declaration_extracts_as_class_with_methods_as_methods() {
        // Class-record (`record User(string name)`) extracts as Class;
        // methods inside the record body extract as Method with parent =
        // record name (NOT as orphan Function symbols, which was the
        // pre-fix bug — `enclosing_type_name` did not recognise
        // `record_declaration` as a type ancestor before this fix).
        let fg = parse(
            r#"
public record User(string name) {
    public override string ToString() => $"User({name})";
    public bool IsAdmin() { return name == "admin"; }
}
"#,
        );
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
        assert_eq!(
            user.kind,
            SymbolKind::Class,
            "record extracts as Class (per Decision 11's C# follow-up)"
        );

        let to_string = sym(&fg, "ToString");
        assert_eq!(to_string.kind, SymbolKind::Method);
        assert_eq!(
            to_string.parent, "User",
            "method inside record must record record name as parent, not be orphan"
        );

        let is_admin = sym(&fg, "IsAdmin");
        assert_eq!(is_admin.kind, SymbolKind::Method);
        assert_eq!(is_admin.parent, "User");
    }

    #[test]
    fn record_struct_extracts_as_class_with_methods_as_methods() {
        // Struct-record (`record struct Pt(int x, int y)`) — same node
        // kind as a class-record in tree-sitter-c-sharp 0.23.5, so it
        // also dispatches to Class. Methods inside parent the struct-
        // record's name.
        let fg = parse(
            r#"
public record struct Pt(int x, int y) {
    public int Sum() { return x + y; }
}
"#,
        );
        let pt = sym(&fg, "Pt");
        assert_eq!(
            pt.kind,
            SymbolKind::Class,
            "struct-record extracts as Class — both record forms share the same node kind"
        );

        let sum = sym(&fg, "Sum");
        assert_eq!(sum.kind, SymbolKind::Method);
        assert_eq!(sum.parent, "Pt");
    }

    // ---- Namespace handling ---------------------------------------

    #[test]
    fn class_inside_namespace_records_namespace_field() {
        let fg = parse(
            r#"
namespace MyApp {
    class Foo { void M() { } }
}
"#,
        );
        let foo = sym(&fg, "Foo");
        assert_eq!(foo.namespace, "MyApp");
        let m = sym(&fg, "M");
        assert_eq!(m.namespace, "MyApp", "method inherits namespace too");
    }

    #[test]
    fn nested_namespaces_join_with_dot() {
        let fg = parse(
            r#"
namespace Outer {
    namespace Inner {
        class X { }
    }
}
"#,
        );
        let x = sym(&fg, "X");
        assert_eq!(
            x.namespace, "Outer.Inner",
            "nested namespaces must join with '.'"
        );
    }

    #[test]
    fn dotted_namespace_preserves_text() {
        let fg = parse(
            r#"
namespace A.B.C {
    class X { }
}
"#,
        );
        let x = sym(&fg, "X");
        assert_eq!(x.namespace, "A.B.C");
    }

    #[test]
    fn file_scoped_namespace_populates_namespace_field() {
        // C# 10+ file-scoped namespace: `namespace MyApp;` is a sibling
        // of subsequent declarations, not their ancestor. The extractor
        // looks for it at the compilation_unit level when no block-form
        // ancestor is found.
        let fg = parse(
            r#"
namespace MyApp;

class Foo { void M() { } }
"#,
        );
        let foo = sym(&fg, "Foo");
        assert_eq!(foo.namespace, "MyApp");
        let m = sym(&fg, "M");
        assert_eq!(m.namespace, "MyApp");
    }

    // ---- Symbol shape sanity --------------------------------------

    #[test]
    fn line_and_end_line_are_one_indexed_and_populated() {
        let fg = parse(
            r#"
class Foo {
    public void Bar() {
        return;
    }
}
"#,
        );
        let foo = sym(&fg, "Foo");
        assert!(foo.line >= 1, "line is 1-indexed");
        assert!(foo.end_line >= foo.line);

        let bar = sym(&fg, "Bar");
        assert!(bar.line >= 1);
        assert!(bar.end_line >= bar.line);
    }

    #[test]
    fn signature_truncates_at_method_body() {
        let fg = parse(
            r#"
class Foo {
    public int Bar() {
        return 42;
    }
}
"#,
        );
        let bar = sym(&fg, "Bar");
        // truncate_signature drops the body — `{` is a hard cutoff.
        assert!(
            !bar.signature.contains('{'),
            "signature should drop body: got {:?}",
            bar.signature
        );
        // Whatever survives must still mention the method name.
        assert!(
            bar.signature.contains("Bar"),
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
    public void Bar() { }
}
"#,
            "/abs/foo.cs",
        );
        let bar = sym(&fg, "Bar");
        assert_eq!(symbol_id(bar), "/abs/foo.cs:Foo::Bar");
    }

    // ----------------------------------------------------------------
    // Phase 2.3 — call extraction
    // ----------------------------------------------------------------

    /// Filter to just the `Calls` edges of `fg` (drops Inherits and
    /// Includes edges, which are now also produced by `parse_file`).
    fn calls(fg: &FileGraph) -> Vec<&Edge> {
        fg.edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .collect()
    }

    /// Assert that exactly one Calls edge with `from = expected_from`
    /// and `to = expected_to` exists. Panics with a helpful message
    /// listing every Calls edge if not.
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
        let fg = parse_at(
            r#"
class C {
    void m() { Foo(); }
}
"#,
            "/p/x.cs",
        );
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_one_call(&fg, "/p/x.cs:C::m", "Foo");
    }

    #[test]
    fn member_access_call_records_rightmost_name_only() {
        // `obj.Foo()` → `to = "Foo"`. The receiver `obj` is part of the
        // chain syntax but is not the callee identifier.
        let fg = parse_at(
            r#"
class C {
    void m() { obj.Foo(); }
}
"#,
            "/p/x.cs",
        );
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_one_call(&fg, "/p/x.cs:C::m", "Foo");
    }

    #[test]
    fn chained_call_produces_one_edge_per_chain_link() {
        // `a.B().C()` — tree-sitter parses two nested
        // invocation_expressions; the query matches both. Expect 2
        // edges (to `B` and to `C`), each from `<path>:C::m`.
        let fg = parse_at(
            r#"
class C {
    void m() { a.B().C(); }
}
"#,
            "/p/x.cs",
        );
        let edges = calls(&fg);
        assert_eq!(
            edges.len(),
            2,
            "expected 2 Calls edges (B, C); got: {:?}",
            edges
                .iter()
                .map(|e| (e.from.as_str(), e.to.as_str()))
                .collect::<Vec<_>>()
        );
        assert_one_call(&fg, "/p/x.cs:C::m", "B");
        assert_one_call(&fg, "/p/x.cs:C::m", "C");
    }

    #[test]
    fn null_conditional_call_records_callee_name() {
        // `obj?.Foo()` → 1 edge to `Foo`. The conditional_access_expression
        // wraps a member_binding_expression whose `name:` is the callee.
        let fg = parse_at(
            r#"
class C {
    void m() { obj?.Foo(); }
}
"#,
            "/p/x.cs",
        );
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_one_call(&fg, "/p/x.cs:C::m", "Foo");
    }

    #[test]
    fn call_inside_lambda_body_is_attributed_to_enclosing_method() {
        // Lambda transparency: the call inside `() => Foo()` reports the
        // enclosing method `m` as the `from`, not the lambda.
        let fg = parse_at(
            r#"
class C {
    void m() {
        System.Action a = () => Foo();
    }
}
"#,
            "/p/x.cs",
        );
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_one_call(&fg, "/p/x.cs:C::m", "Foo");
    }

    #[test]
    fn call_inside_linq_select_is_attributed_to_enclosing_method() {
        // LINQ transparency: `select Foo(x)` reports the enclosing
        // method `m` as the `from`, not the query expression.
        let fg = parse_at(
            r#"
class C {
    void m() {
        var r = from x in xs select Foo(x);
    }
}
"#,
            "/p/x.cs",
        );
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_one_call(&fg, "/p/x.cs:C::m", "Foo");
    }

    #[test]
    fn constructor_call_records_type_name_as_callee() {
        // `new Foo()` produces a call edge to `Foo` (the agent
        // interprets the edge as construction).
        let fg = parse_at(
            r#"
class C {
    void m() {
        var x = new Foo();
    }
}
"#,
            "/p/x.cs",
        );
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_one_call(&fg, "/p/x.cs:C::m", "Foo");
    }

    #[test]
    fn generic_call_records_bare_name_not_type_arguments() {
        // `Foo<int>()` → `to = "Foo"`, NOT `Foo<int>`.
        let fg = parse_at(
            r#"
class C {
    void m() { Foo<int>(); }
}
"#,
            "/p/x.cs",
        );
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        let edge = edges[0];
        assert_eq!(
            edge.to, "Foo",
            "generic call must record bare name, not type-argument list"
        );
        assert!(
            !edge.to.contains('<'),
            "to field must not contain '<'; got {:?}",
            edge.to
        );
    }

    #[test]
    fn namespace_qualified_call_records_rightmost_name() {
        // `System.Console.WriteLine()` → `to = "WriteLine"` only.
        let fg = parse_at(
            r#"
class C {
    void m() { System.Console.WriteLine(); }
}
"#,
            "/p/x.cs",
        );
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_one_call(&fg, "/p/x.cs:C::m", "WriteLine");
    }

    #[test]
    fn cast_expression_does_not_produce_call_edge() {
        // `(Foo)x` parses as cast_expression, NOT invocation_expression.
        // No spurious call edge.
        let fg = parse_at(
            r#"
class C {
    void m() { var y = (Foo)x; }
}
"#,
            "/p/x.cs",
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
    fn typeof_sizeof_default_do_not_produce_call_edges() {
        // typeof / sizeof / default each parse as their own expression
        // node (typeof_expression, sizeof_expression, default_expression),
        // NOT as invocation_expression. None should produce call edges.
        let fg = parse_at(
            r#"
class C {
    void m() {
        var t = typeof(int);
        var s = sizeof(int);
        var d = default(int);
    }
}
"#,
            "/p/x.cs",
        );
        let edges = calls(&fg);
        assert!(
            edges.is_empty(),
            "typeof/sizeof/default must not produce Calls edges; got: {:?}",
            edges
                .iter()
                .map(|e| (e.from.as_str(), e.to.as_str()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn nameof_is_filtered_and_produces_no_call_edge() {
        // `nameof(X)` parses as an ordinary `invocation_expression` in
        // tree-sitter-c-sharp 0.23.5 (the grammar does not have a
        // dedicated nameof node). Without filtering, every method that
        // uses `nameof` for logging/reflection would record a call to
        // `nameof`, polluting `get_callees` results. We filter it in
        // `extract_calls` — same precedent as the C++ plugin's
        // `is_cpp_cast` filter for `static_cast`. This test locks the
        // filter in: a future refactor that drops the filter will fail.
        let fg = parse_at(
            r#"
class C {
    void m() {
        var name = nameof(C);
        var memberName = nameof(C.m);
    }
}
"#,
            "/p/x.cs",
        );
        let edges = calls(&fg);
        assert!(
            edges.is_empty(),
            "nameof(X) must NOT produce a Calls edge; got: {:?}",
            edges
                .iter()
                .map(|e| (e.from.as_str(), e.to.as_str()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn unchecked_does_not_produce_call_edge() {
        // Like `checked(expr)`, `unchecked(expr)` parses as a dedicated
        // `unchecked_expression` node, not `invocation_expression`. The
        // query never matches it. Pins the symmetric behavior alongside
        // typeof/sizeof/default/checked. (No `checked` test exists for
        // the same reason — both fall out of the grammar's node shape.)
        let fg = parse_at(
            r#"
class C {
    void m() {
        var u = unchecked(42 + 1);
        var c = checked(42 + 1);
    }
}
"#,
            "/p/x.cs",
        );
        let edges = calls(&fg);
        assert!(
            edges.is_empty(),
            "unchecked/checked must not produce Calls edges; got: {:?}",
            edges
                .iter()
                .map(|e| (e.from.as_str(), e.to.as_str()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn call_inside_constructor_records_enclosing_class_as_parent() {
        // The from-field for a call inside a constructor is
        // `<path>:Class::Class` (the constructor's name matches its
        // class). This pins that constructor calls (Phase 2.2's `ctor`
        // capture) and method calls (Phase 2.2's `method` capture)
        // route through the same enclosing-function rule.
        let fg = parse_at(
            r#"
class C {
    public C() { Init(); }
}
"#,
            "/p/x.cs",
        );
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_one_call(&fg, "/p/x.cs:C::C", "Init");
    }

    #[test]
    fn call_inside_local_function_uses_local_name_as_from() {
        // Local functions extract as Function with no parent. A call
        // inside the local function reports `<path>:Helper`, NOT
        // `<path>:C::Helper` — the local-function boundary is the
        // immediate enclosing function-shaped declaration.
        let fg = parse_at(
            r#"
class C {
    void M() {
        void Helper() { Inner(); }
        Helper();
    }
}
"#,
            "/p/x.cs",
        );
        // Two edges total: `Inner` (from inside Helper) and `Helper`
        // (from inside M).
        assert_one_call(&fg, "/p/x.cs:Helper", "Inner");
        assert_one_call(&fg, "/p/x.cs:C::M", "Helper");
    }

    #[test]
    fn call_in_default_interface_method_omits_parent() {
        // Decision 11: default interface methods extract as Function
        // (no parent). The call's `from` follows: `<path>:DoFoo`, NOT
        // `<path>:I::DoFoo`.
        let fg = parse_at(
            r#"
interface I {
    void DoFoo() { Helper(); }
}
"#,
            "/p/x.cs",
        );
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_one_call(&fg, "/p/x.cs:DoFoo", "Helper");
    }

    #[test]
    fn call_at_field_initializer_falls_back_to_bare_path() {
        // A static field initializer is not inside a method/constructor/
        // local-function. The call's `from` is the bare file path.
        let fg = parse_at(
            r#"
class C {
    static int x = Compute();
}
"#,
            "/p/x.cs",
        );
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_one_call(&fg, "/p/x.cs", "Compute");
    }

    #[test]
    fn constructor_with_qualified_type_records_rightmost_name() {
        // `new System.Collections.Generic.List<int>()` → `to = "List"`.
        // The qualified_name's rightmost `name:` field is the inner
        // generic_name, which carries a bare identifier.
        let fg = parse_at(
            r#"
class C {
    void m() {
        var x = new System.Collections.Generic.List<int>();
    }
}
"#,
            "/p/x.cs",
        );
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        let edge = edges[0];
        assert_eq!(edge.to, "List");
        assert!(
            !edge.to.contains('.') && !edge.to.contains('<'),
            "to must be bare name; got {:?}",
            edge.to
        );
    }

    #[test]
    fn call_edge_carries_file_and_line() {
        // Sanity: edge.file and edge.line populate as expected (file =
        // the path, line = 1-indexed call-site row).
        let fg = parse_at(
            r#"
class C {
    void m() {
        Foo();
    }
}
"#,
            "/p/x.cs",
        );
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].file, "/p/x.cs");
        // The body of `m()` starts on a line >= 1. Don't pin a precise
        // row (whitespace fragility); just assert > 1 so the math is
        // populated and 1-indexed (the leading newline pushes the call
        // past line 1).
        assert!(edges[0].line >= 1);
    }

    #[test]
    fn empty_file_produces_no_call_edges() {
        let fg = parse("");
        let edges = calls(&fg);
        assert!(edges.is_empty(), "got: {:?}", edges);
    }

    // ----------------------------------------------------------------
    // Phase 2.4 — import extraction
    // ----------------------------------------------------------------

    /// Filter to just the `Includes` edges of `fg`.
    fn includes(fg: &FileGraph) -> Vec<&Edge> {
        fg.edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Includes)
            .collect()
    }

    #[test]
    fn plain_using_records_includes_edge() {
        let fg = parse_at("using System;\n", "/p/x.cs");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_eq!(edges[0].to, "System");
        assert_eq!(edges[0].kind, EdgeKind::Includes);
    }

    #[test]
    fn dotted_using_preserves_full_path() {
        let fg = parse_at("using System.Collections.Generic;\n", "/p/x.cs");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_eq!(
            edges[0].to, "System.Collections.Generic",
            "dotted path must be preserved verbatim"
        );
    }

    #[test]
    fn static_using_drops_static_modifier() {
        // `using static System.Console;` — the `static` keyword is an
        // anonymous child of `using_directive`; only the named path
        // child is captured.
        let fg = parse_at("using static System.Console;\n", "/p/x.cs");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_eq!(
            edges[0].to, "System.Console",
            "static modifier must be dropped from `to`"
        );
        assert!(
            !edges[0].to.contains("static"),
            "to field must not contain 'static'; got {:?}",
            edges[0].to
        );
    }

    #[test]
    fn aliased_using_preserves_target_path() {
        // `using FooAlias = Some.Long.Type.Name;` — the alias is the
        // `name:` field; the path is the other named child. The
        // extractor records the *path*, dropping the alias (same rule
        // as Python `import foo as f`).
        let fg = parse_at("using FooAlias = Some.Long.Type.Name;\n", "/p/x.cs");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_eq!(
            edges[0].to, "Some.Long.Type.Name",
            "alias form must record target path, not alias name"
        );
        assert!(
            !edges[0].to.contains("FooAlias"),
            "to field must not contain alias name; got {:?}",
            edges[0].to
        );
    }

    #[test]
    fn simple_aliased_using_preserves_target_path() {
        // `using A = Foo;` — both the alias and the target path parse
        // as bare `identifier` nodes. The `name:` field disambiguates
        // them; the extractor picks the non-`name:` identifier.
        let fg = parse_at("using A = Foo;\n", "/p/x.cs");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_eq!(
            edges[0].to, "Foo",
            "alias-with-single-identifier-target must record target, not alias"
        );
    }

    #[test]
    fn global_using_drops_global_modifier() {
        let fg = parse_at("global using System.Linq;\n", "/p/x.cs");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_eq!(
            edges[0].to, "System.Linq",
            "global modifier must be dropped from `to`"
        );
        assert!(
            !edges[0].to.contains("global"),
            "to field must not contain 'global'; got {:?}",
            edges[0].to
        );
    }

    #[test]
    fn global_using_static_drops_both_modifiers() {
        // Combination: `global using static System.Math;` — both
        // modifiers are anonymous children and are dropped; the path
        // survives verbatim.
        let fg = parse_at("global using static System.Math;\n", "/p/x.cs");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_eq!(edges[0].to, "System.Math");
    }

    #[test]
    fn bare_alias_qualified_using_preserves_global_prefix() {
        // tree-sitter-c-sharp 0.23.5 produces a direct `alias_qualified_name`
        // child under `using_directive` for `using global::System;` (no
        // dotted suffix). Without an `alias_qualified_name` arm in
        // using_directive_path, this case silently produced no edge. The
        // verbatim text `"global::System"` is what Decision 7 wants —
        // record it as the path.
        let fg = parse_at("using global::System;\n", "/p/x.cs");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_eq!(edges[0].to, "global::System");
    }

    #[test]
    fn using_inside_namespace_block_is_captured() {
        // Anti-regression per the 2.4 verification field: a `using`
        // inside a `namespace { ... }` block must be captured (the
        // tree-sitter query walks the whole tree by default, so the
        // `using_directive` inside the namespace's `declaration_list`
        // surfaces alongside any top-level usings).
        let fg = parse_at(
            r#"
namespace Foo {
    using Bar;
    class C {}
}
"#,
            "/p/x.cs",
        );
        let edges = includes(&fg);
        assert_eq!(
            edges.len(),
            1,
            "namespace-scoped using must produce one Includes edge; got: {:?}",
            edges
                .iter()
                .map(|e| (e.from.as_str(), e.to.as_str()))
                .collect::<Vec<_>>()
        );
        assert_eq!(edges[0].to, "Bar");
    }

    #[test]
    fn multiple_usings_at_file_scope_each_produce_edges() {
        let fg = parse_at(
            r#"
using System;
using System.IO;
using System.Collections.Generic;
"#,
            "/p/x.cs",
        );
        let edges = includes(&fg);
        assert_eq!(edges.len(), 3, "got: {:?}", edges);

        let tos: Vec<&str> = edges.iter().map(|e| e.to.as_str()).collect();
        assert!(tos.contains(&"System"), "missing System; got {:?}", tos);
        assert!(
            tos.contains(&"System.IO"),
            "missing System.IO; got {:?}",
            tos
        );
        assert!(
            tos.contains(&"System.Collections.Generic"),
            "missing System.Collections.Generic; got {:?}",
            tos
        );
    }

    #[test]
    fn using_inside_namespace_and_top_level_produces_two_edges() {
        // Combination of file-scope and namespace-scope usings — both
        // surface, neither is dropped.
        let fg = parse_at(
            r#"
using System;
namespace Foo {
    using Bar;
    class C {}
}
"#,
            "/p/x.cs",
        );
        let edges = includes(&fg);
        assert_eq!(edges.len(), 2, "got: {:?}", edges);

        let tos: Vec<&str> = edges.iter().map(|e| e.to.as_str()).collect();
        assert!(tos.contains(&"System"));
        assert!(tos.contains(&"Bar"));
    }

    #[test]
    fn from_field_is_the_file_path() {
        // The `Includes` edge's `from` is the file path (NOT a symbol
        // ID, NOT empty). Mirrors the Python/Go/Rust convention; the
        // `Graph` engine routes Includes edges by file path.
        let fg = parse_at("using System;\n", "/abs/x.cs");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].from, "/abs/x.cs", "from must be the file path");
        assert_eq!(edges[0].file, "/abs/x.cs", "file must match the path arg");
    }

    #[test]
    fn import_edge_carries_correct_line() {
        // Line is 1-indexed and anchored at the `using_directive` node.
        // Three usings on lines 2, 3, 4 (leading newline pushes the
        // first using past line 1).
        let fg = parse_at(
            "\nusing System;\nusing System.IO;\nusing System.Collections.Generic;\n",
            "/p/x.cs",
        );
        let edges = includes(&fg);
        assert_eq!(edges.len(), 3, "got: {:?}", edges);

        // Find each edge by its target and assert its line.
        let line_for = |to: &str| -> u32 {
            edges
                .iter()
                .find(|e| e.to == to)
                .unwrap_or_else(|| panic!("missing edge to={:?}", to))
                .line
        };
        assert_eq!(line_for("System"), 2);
        assert_eq!(line_for("System.IO"), 3);
        assert_eq!(line_for("System.Collections.Generic"), 4);
    }

    #[test]
    fn empty_file_produces_no_includes_edges() {
        let fg = parse("");
        let edges = includes(&fg);
        assert!(edges.is_empty(), "got: {:?}", edges);
    }

    // ----------------------------------------------------------------
    // Phase 2.5 — inheritance extraction
    // ----------------------------------------------------------------

    /// Filter to just the `Inherits` edges of `fg`. Mirrors the `calls`
    /// and `includes` helpers above so each phase's assertions exercise
    /// only its own edge category.
    fn inherits(fg: &FileGraph) -> Vec<&Edge> {
        fg.edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Inherits)
            .collect()
    }

    #[test]
    fn single_base_class_produces_one_inherits_edge() {
        // `class Foo : Bar { }` → 1 Inherits edge with from="Foo",
        // to="Bar". The bare-name `from`-field rule (Phase 1 / Phase 5
        // of RustRewrite, reaffirmed by Decision 9 in this design) is
        // load-bearing — see `crates/code-graph-graph/src/algorithms.rs`,
        // which looks up classes by `Symbol.name`.
        let fg = parse_at("class Foo : Bar { }\n", "/p/x.cs");
        let edges = inherits(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        let e = edges[0];
        assert_eq!(e.from, "Foo");
        assert_eq!(e.to, "Bar");
        assert_eq!(e.kind, EdgeKind::Inherits);
    }

    #[test]
    fn multiple_bases_produce_one_edge_per_base() {
        // `class Foo : Bar, IBaz, IQux { }` → 3 Inherits edges, all
        // from="Foo". The query produces one match per base via the
        // `(base_list (_) @inherit.base)` wildcard.
        let fg = parse_at("class Foo : Bar, IBaz, IQux { }\n", "/p/x.cs");
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
            assert_eq!(
                e.from, "Foo",
                "every base's `from` must be the enclosing type"
            );
            assert_eq!(e.kind, EdgeKind::Inherits);
        }
        let tos: Vec<&str> = edges.iter().map(|e| e.to.as_str()).collect();
        assert!(tos.contains(&"Bar"), "missing Bar; got {:?}", tos);
        assert!(tos.contains(&"IBaz"), "missing IBaz; got {:?}", tos);
        assert!(tos.contains(&"IQux"), "missing IQux; got {:?}", tos);
    }

    #[test]
    fn generic_class_and_base_preserve_type_params() {
        // `class Foo<T> : Bar<T> { }` → 1 edge from="Foo<T>" to="Bar<T>".
        // Generic params survive in BOTH from and to per Decision 9
        // (preserved verbatim, matching Rust's rule — NOT Go's strip
        // rule).
        //
        // **Known asymmetry pinned here**: while edge.from is
        // "Foo<T>" (generics preserved), Symbol.name for the same class
        // is the bare "Foo" (extract_definitions captures only the
        // identifier child). This means Graph::class_hierarchy at
        // crates/code-graph-graph/src/algorithms.rs cannot walk
        // inheritance for generic classes — it looks up symbols by
        // Symbol.name then walks adj.get(name), but the adjacency map
        // is keyed under "Foo<T>". Same limitation exists in the Rust
        // plugin and is the accepted Decision 9 trade-off. Phase 4.4's
        // CLAUDE.md documents this for agent-facing visibility.
        let fg = parse_at("class Foo<T> : Bar<T> { }\n", "/p/x.cs");
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

        // Side-by-side assertion making the asymmetry self-documenting:
        // Symbol.name is bare "Foo" (not "Foo<T>"); a future refactor
        // that changes extract_definitions to include generics in
        // Symbol.name would close the class_hierarchy gap but would
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
    fn qualified_base_preserves_dotted_path() {
        // `class Foo : Ns.Bar { }` → 1 edge to="Ns.Bar" (verbatim
        // qualified_name text; no resolution).
        let fg = parse_at("class Foo : Ns.Bar { }\n", "/p/x.cs");
        let edges = inherits(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_eq!(edges[0].from, "Foo");
        assert_eq!(edges[0].to, "Ns.Bar");
    }

    #[test]
    fn qualified_generic_base_preserves_full_path_with_args() {
        // `class Foo : Ns.Generic<int, string> { }` → the base is a
        // single qualified_name whose rightmost name field is a
        // generic_name; `utf8_text` preserves the dotted+generic form.
        let fg = parse_at("class Foo : Ns.Generic<int, string> { }\n", "/p/x.cs");
        let edges = inherits(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_eq!(edges[0].to, "Ns.Generic<int, string>");
    }

    #[test]
    fn alias_qualified_base_preserves_global_prefix() {
        // `class Foo : global::Ns.Bar { }` → the base's verbatim text
        // includes the `global::` prefix. Matches the `using
        // global::System;` rule from 2.4 — alias-qualified text is
        // captured as written.
        let fg = parse_at("class Foo : global::Ns.Bar { }\n", "/p/x.cs");
        let edges = inherits(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_eq!(edges[0].to, "global::Ns.Bar");
    }

    #[test]
    fn interface_extending_interfaces_produces_inherits_edges() {
        // `interface I : J, K { }` → 2 Inherits edges from="I". Mirrors
        // the multi-base class case but on an `interface_declaration`.
        // Decision 2: interface inheritance uses the same `Inherits`
        // edge kind.
        let fg = parse_at("interface I : J, K { }\n", "/p/x.cs");
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
    fn record_with_base_produces_inherits_edge() {
        // `record User(string n) : Base { }` → 1 edge from="User"
        // to="Base". Records reach the inheritance extractor through
        // the `record_declaration` arm in INHERITANCE_QUERIES — same
        // shape as classes.
        let fg = parse_at("record User(string n) : Base { }\n", "/p/x.cs");
        let edges = inherits(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_eq!(edges[0].from, "User");
        assert_eq!(edges[0].to, "Base");
    }

    #[test]
    fn struct_implementing_interface_produces_inherits_edge() {
        // `struct Pt : IComparable<Pt> { }` → 1 edge from="Pt"
        // to="IComparable<Pt>". Decision 2: structs implementing
        // interfaces produce `Inherits` edges, no distinction from
        // class-implements-interface.
        let fg = parse_at("struct Pt : IComparable<Pt> { }\n", "/p/x.cs");
        let edges = inherits(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_eq!(edges[0].from, "Pt");
        assert_eq!(edges[0].to, "IComparable<Pt>");
    }

    #[test]
    fn class_without_base_list_produces_no_inherits_edges() {
        // `class Foo { }` → 0 Inherits edges. No `base_list` child
        // means zero matches under any declaration-kind arm.
        let fg = parse_at("class Foo { }\n", "/p/x.cs");
        let edges = inherits(&fg);
        assert!(edges.is_empty(), "got: {:?}", edges);
    }

    #[test]
    fn inherits_edge_carries_file_and_line() {
        // The edge.line is anchored at the *enclosing declaration*
        // (where the inheritance is declared), NOT at the base node
        // (which could span multiple lines for a long base list).
        // Pin: with a leading newline the `class` keyword lands on
        // line 2; the edge's line must equal 2.
        let fg = parse_at("\nclass Foo : Bar { }\n", "/abs/foo.cs");
        let edges = inherits(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        let e = edges[0];
        assert_eq!(e.file, "/abs/foo.cs", "file must equal the path");
        assert_eq!(
            e.line, 2,
            "line must be the enclosing declaration's line (2), not the base's line"
        );
    }

    #[test]
    fn decision_2_no_edge_kind_distinction_between_class_and_interface_bases() {
        // Load-bearing Decision-2 pin: `class Foo : Bar, IBaz, IQux`
        // produces 3 edges, ALL with `EdgeKind::Inherits`. Even though
        // syntactic intuition might assign `Bar` (class extension)
        // differently from `IBaz` / `IQux` (interface implementation),
        // the C# grammar does NOT make that distinction in `base_list`
        // and the plugin deliberately preserves that uniformity. The
        // agent disambiguates from the target Symbol's `kind` at query
        // time, not from a separate `Implements` edge.
        let fg = parse_at("class Foo : Bar, IBaz, IQux { }\n", "/p/x.cs");
        let edges = inherits(&fg);
        assert_eq!(edges.len(), 3);
        for e in &edges {
            assert_eq!(
                e.kind,
                EdgeKind::Inherits,
                "every base, whether a class or an interface, produces the same EdgeKind"
            );
        }
    }

    #[test]
    fn nested_class_with_base_records_inner_class_as_from() {
        // A nested class with a base list records the *inner* class's
        // name as `from`, not the outer. The query anchors on the
        // immediate `class_declaration` ancestor of the `base_list`.
        let fg = parse_at(
            r#"
class Outer {
    class Inner : Base { }
}
"#,
            "/p/x.cs",
        );
        let edges = inherits(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", edges);
        assert_eq!(edges[0].from, "Inner");
        assert_eq!(edges[0].to, "Base");
    }

    #[test]
    fn generic_class_with_where_constraints_does_not_pollute_to_field() {
        // `class Foo<T> : Bar<T> where T : IComparable { }` — the
        // `type_parameter_constraints_clause` is a SIBLING of
        // `base_list`, not a child. The query never sees `IComparable`
        // through the where-clause path. Exactly one Inherits edge
        // (to Bar<T>); the constraint type is not double-counted.
        let fg = parse_at(
            "class Foo<T> : Bar<T> where T : IComparable { }\n",
            "/p/x.cs",
        );
        let edges = inherits(&fg);
        assert_eq!(
            edges.len(),
            1,
            "where-clause types must not leak into Inherits edges; got: {:?}",
            edges
                .iter()
                .map(|e| (e.from.as_str(), e.to.as_str()))
                .collect::<Vec<_>>()
        );
        assert_eq!(edges[0].from, "Foo<T>");
        assert_eq!(edges[0].to, "Bar<T>");
    }

    #[test]
    fn empty_file_produces_no_inherits_edges() {
        let fg = parse("");
        let edges = inherits(&fg);
        assert!(edges.is_empty(), "got: {:?}", edges);
    }
}
