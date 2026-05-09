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
//! Phase 3.3 will wire `extract_calls` (`method_invocation`,
//! `object_creation_expression`, lambda body, anonymous-class body,
//! enum-constant body, method references).
//!
//! Phase 3.4 will wire `extract_imports` (`import_declaration` plain,
//! wildcard, static).
//!
//! Phase 3.5 will wire `extract_inheritance` (`superclass`,
//! `super_interfaces`, `extends_interfaces`). `permits` clauses on
//! sealed types are ignored per Decision 6.
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

use code_graph_core::{FileGraph, Language, Symbol, SymbolKind};
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
    /// Compiled call query (wired in 3.3).
    #[allow(dead_code)] // wired in Phase 3.3 (extract_calls)
    call_query: Query,
    /// Compiled import query (wired in 3.4).
    #[allow(dead_code)] // wired in Phase 3.4 (extract_imports)
    import_query: Query,
    /// Compiled inheritance query (wired in 3.5).
    #[allow(dead_code)] // wired in Phase 3.5 (extract_inheritance)
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
    /// fills [`DEFINITION_QUERIES`]; the other three remain empty until
    /// 3.3/3.4/3.5 land their respective extractors.
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
    /// while each per-extractor method (the upcoming 3.3/3.4/3.5
    /// extractors) can be tested via `parse_file` without exposing them.
    /// Mirrors the Python/C# plugins' `parse_to_filegraph` indirection.
    ///
    /// Phase 3.2 wires `extract_definitions` into the pipeline; the call,
    /// import, and inheritance extractors are added in 3.3, 3.4, and 3.5
    /// respectively. Until those land, `parse_file` produces only Symbol
    /// records — zero edges.
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
}

impl LanguagePlugin for JavaParser {
    fn id(&self) -> Language {
        Language::Java
    }

    fn extensions(&self) -> &'static [&'static str] {
        EXTENSIONS
    }

    /// Parse `content` (UTF-8 bytes) as Java and produce a [`FileGraph`].
    ///
    /// Phase 3.2 wires the definition extractor; Phases 3.3/3.4/3.5 wire
    /// the call, import, and inheritance extractors.
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
    //! coverage. Behavioral coverage of call / import / inheritance
    //! extraction lands in 3.3-3.5.
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
}
