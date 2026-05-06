//! Python language plugin for code-graph-mcp.
//!
//! Uses tree-sitter (via the `tree-sitter` and `tree-sitter-python` crates)
//! to extract symbols, calls, import edges, and inheritance edges from
//! `.py` and `.pyi` source files.
//!
//! # Phase status
//!
//! Phase 7.1 shipped the crate scaffold: dependency wiring, query strings
//! that compile against tree-sitter-python 0.25, the `PythonParser` struct
//! with cached `Query` objects, and the `LanguagePlugin` impl.
//!
//! Phase 7.2 wires `extract_definitions` — function/method/class extraction
//! with method-vs-function disambiguation (via [`find_enclosing_class`]),
//! decorator transparency (queries match the inner `function_definition` /
//! `class_definition` directly through any `decorated_definition` wrapper),
//! `async def` support (parses as `function_definition` in tree-sitter-python
//! 0.25), nested-class parent assignment (the inner class records the outer
//! class as its parent), and `.pyi` stub-file parity (stubs use the same
//! grammar — `def f() -> int: ...` is still a `function_definition`).
//!
//! Phase 7.3 wires `extract_calls` — direct calls, attribute calls,
//! chained calls, and the module-top-level fallback (`from = path` when
//! there is no enclosing `function_definition`).
//!
//! Phase 7.4 wires `extract_imports` — both `import_statement` (`import
//! foo`, `import foo.bar`, `import foo as f`, `import a, b`) and
//! `import_from_statement` (`from foo import bar`, `from foo.bar import
//! baz`, `from foo import bar, qux`, `from . import utils`, `from
//! ..pkg.mod import x`) plus the dedicated `future_import_statement`
//! node kind for `from __future__ import annotations`. Conditional
//! imports (`if TYPE_CHECKING: import x`) are filtered by the module-
//! top-level guard in `extract_imports` so the dependency graph stays
//! stable across files that guard imports behind feature flags.
//!
//! Phase 7.5 wires `extract_inheritance`. After 7.5, `parse_file` is
//! fully populated and every extractor is live.
//!
//! # Default trait methods
//!
//! `PythonParser` does NOT override [`LanguagePlugin::resolve_call`] or
//! [`LanguagePlugin::resolve_include`].
//!
//! - `resolve_call`: the default scope-aware heuristic (same file > same
//!   parent > same namespace > global) is the documented contract.
//!   Python's dynamic typing makes any static call resolution inherently
//!   noisy — a method call `obj.foo()` cannot be resolved to a concrete
//!   `foo` without type inference, which is out of scope for a tree-sitter-
//!   based static analyzer. The default heuristic produces the same kind
//!   of best-effort result as for C++/Rust/Go.
//! - `resolve_include`: the default basename match against the
//!   [`codegraph_lang::FileIndex`] is **a no-op for Python module paths**
//!   because they are dotted module strings (e.g. `"foo.bar"`), not
//!   filesystem paths. The wire format records the full module path
//!   verbatim as the `to` field; leaving it unresolved is the intended
//!   behavior — `import x.y.z` does not trivially map to `x/y/z.py`
//!   without consulting `sys.path` and the project layout, both of which
//!   are out of scope.
//!
//! # Python-specific notes
//!
//! - The tree-sitter-python grammar uses the node kind **`call`**, NOT
//!   `call_expression`. This is the most common footgun when porting a
//!   Python plugin from muscle memory of the C++/Rust/Go grammars.
//! - **`async def` parses as `function_definition`** in tree-sitter-python
//!   0.25 — there is no separate `async_function_definition` node. The
//!   single `function_definition` query in `queries.rs` covers both sync
//!   and async forms. Confirmed by fixture in 7.2's tests.
//! - `.py` and `.pyi` files share the same grammar; both extensions
//!   dispatch to the same parser. `.pyi` stub files use the same
//!   `function_definition` / `class_definition` nodes — `def f() -> int:
//!   ...` parses as a `function_definition` whose body is a single
//!   `expression_statement` containing `...`. No separate query path is
//!   needed.

pub(crate) mod helpers;
pub(crate) mod queries;

use std::path::Path;

use codegraph_core::{Edge, EdgeKind, FileGraph, Language, Symbol, SymbolKind};
use codegraph_lang::{LanguagePlugin, ParseError};
use streaming_iterator::StreamingIterator;
use tree_sitter::{
    Language as TsLanguage, Node, Parser as TsParser, Query, QueryCursor, Tree as TsTree,
};

use crate::helpers::{
    enclosing_function_id, extract_module_path, find_enclosing_class, truncate_signature,
};
use crate::queries::{CALL_QUERIES, DEFINITION_QUERIES, IMPORT_QUERIES, INHERITANCE_QUERIES};

/// File extensions the Python parser claims. Both `.py` (regular sources)
/// and `.pyi` (stub files) dispatch to the same parser — the grammar is
/// identical and the stubs use the same `function_definition` /
/// `class_definition` nodes with `...` bodies.
pub const EXTENSIONS: &[&str] = &[".py", ".pyi"];

/// Python source-file parser. Holds the tree-sitter `Language` and the
/// four pre-compiled queries used to drive symbol/edge extraction in
/// Phases 7.2-7.5.
///
/// Construct with [`PythonParser::new`]; share across threads (queries are
/// `Send + Sync`).
pub struct PythonParser {
    /// Compiled Python grammar. Held so per-call [`tree_sitter::Parser`]
    /// instances built inside `parse_file` can attach to it without
    /// rebuilding the `LanguageFn`.
    language: TsLanguage,
    /// Compiled definition query (wired in 7.2).
    def_query: Query,
    /// Compiled call query (wired in 7.3).
    call_query: Query,
    /// Compiled import query (wired in 7.4).
    import_query: Query,
    /// Compiled inheritance query (wired in 7.5).
    #[allow(dead_code)]
    inheritance_query: Query,
}

impl PythonParser {
    /// Build a new parser, compiling all four tree-sitter queries against
    /// the pinned tree-sitter-python grammar. Returns an [`anyhow::Error`]
    /// (wrapping the query compiler's message) if any query fails to
    /// compile against the pinned grammar version.
    ///
    /// Successful return is the Phase 7.1 acceptance gate that proves
    /// every query string in `queries.rs` parses against tree-sitter-
    /// python 0.25.
    pub fn new() -> anyhow::Result<Self> {
        let language: TsLanguage = tree_sitter_python::LANGUAGE.into();

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

    /// Parse `content` (UTF-8 bytes) as Python and produce a [`FileGraph`].
    /// Internal entry point for [`Self::parse_file`] (the trait method);
    /// kept crate-private so the public surface stays the trait method
    /// while each per-extractor method (`extract_definitions`, and the
    /// upcoming 7.3/7.4/7.5 extractors) can be tested via `parse_file`
    /// without exposing them. Mirrors the Phase 6 Go plugin's structural
    /// pattern (`parse_to_filegraph` indirection).
    fn parse_to_filegraph(&self, path: &Path, content: &[u8]) -> Result<FileGraph, ParseError> {
        let tree = parse_tree(&self.language, content)?;
        let root = tree.root_node();
        let path_str = path.to_string_lossy().into_owned();

        let mut fg = FileGraph {
            path: path_str.clone(),
            language: Language::Python,
            symbols: Vec::new(),
            edges: Vec::new(),
        };

        self.extract_definitions(root, content, &path_str, &mut fg);
        self.extract_calls(root, content, &path_str, &mut fg);
        self.extract_imports(root, content, &path_str, &mut fg);

        Ok(fg)
    }

    /// Run the definition query and produce symbols. Mirrors the C++/Rust/
    /// Go plugins' capture-name dispatch: each capture name from
    /// `DEFINITION_QUERIES` maps to a small branch that builds the right
    /// `Symbol`. Every emitted Python symbol carries
    /// `Symbol.namespace = ""` — Python's module concept is captured in
    /// the file path itself, not in a namespace tag.
    ///
    /// Per-capture-name behavior:
    ///
    /// - `func.name` (from `function_definition`) → branches on whether
    ///   the enclosing scope contains a `class_definition`:
    ///     * No enclosing class → [`SymbolKind::Function`], no parent.
    ///     * Enclosing class → [`SymbolKind::Method`], parent = innermost
    ///       enclosing class name. The walk transparently passes through
    ///       any `decorated_definition` wrapper (decorators do not block
    ///       method classification — `@property def x(self)` is still a
    ///       method of its enclosing class). `async def` parses as
    ///       `function_definition` in tree-sitter-python 0.25, so the same
    ///       code path covers async methods inside classes.
    /// - `class.name` (from `class_definition`) → [`SymbolKind::Class`].
    ///   Nested classes (`class Outer: class Inner: ...`) record the
    ///   innermost enclosing class as the parent — for `Inner`, parent =
    ///   `"Outer"`. Top-level classes have no parent.
    ///
    /// Dunder methods (`__init__`, `__str__`, `__repr__`, `__call__`)
    /// receive no special handling — they are ordinary methods produced
    /// through the same code path as any other method inside a class.
    ///
    /// `.pyi` stub files use the same grammar: `def foo(x: int) -> str:
    /// ...` parses as a `function_definition` whose body is an
    /// `expression_statement` containing `...`. The function-vs-method
    /// classification, decorator transparency, and parent assignment all
    /// behave identically to `.py` files — confirmed by fixture in the
    /// tests module.
    ///
    /// Captures consumed without emitting a Symbol:
    /// - `func.def` / `class.def`: structural anchors used by the queries
    ///   to bind captures to the same definition. The `name` capture
    ///   already resolves the enclosing definition via the parent chain.
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
                    "func.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "function_definition")
                        else {
                            continue;
                        };
                        // Decorator transparency: a `function_definition`
                        // wrapped by `decorated_definition` is still a
                        // function/method — `find_enclosing_class` walks
                        // through the wrapper looking for a
                        // `class_definition` ancestor.
                        let (kind, parent) = match find_enclosing_class(def_node) {
                            Some(cls) => {
                                let class_name = cls
                                    .child_by_field_name("name")
                                    .and_then(|n| n.utf8_text(content).ok())
                                    .unwrap_or("");
                                if class_name.is_empty() {
                                    // Defensive: if a class_definition has no
                                    // resolvable name, fall back to a free
                                    // function classification rather than an
                                    // empty-parent Method (which would render
                                    // as `path:::name` via symbol_id).
                                    (SymbolKind::Function, String::new())
                                } else {
                                    (SymbolKind::Method, class_name.to_owned())
                                }
                            }
                            None => (SymbolKind::Function, String::new()),
                        };
                        fg.symbols
                            .push(make_symbol(text, kind, path, def_node, content, parent));
                    }

                    "class.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "class_definition")
                        else {
                            continue;
                        };
                        // Nested classes: `class Outer: class Inner: ...`
                        // — Inner records Outer as its parent. The walk
                        // climbs from the *class_definition* (not the name
                        // node), so it skips past Inner itself and finds
                        // Outer.
                        let parent = match find_enclosing_class(def_node) {
                            Some(outer) => outer
                                .child_by_field_name("name")
                                .and_then(|n| n.utf8_text(content).ok())
                                .unwrap_or("")
                                .to_owned(),
                            None => String::new(),
                        };
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Class,
                            path,
                            def_node,
                            content,
                            parent,
                        ));
                    }

                    // `func.def` / `class.def` are structural anchors —
                    // the `name` arms above already resolved the enclosing
                    // definition node via the parent chain.
                    _ => {}
                }
            }
        }
    }

    /// Run the call query and produce `Calls` edges. Mirrors the C++/Rust/
    /// Go plugins' `extract_calls`: each capture is a callee identifier,
    /// the line is anchored at the enclosing `call` node, and the `from`
    /// field is built by [`enclosing_function_id`] so it lines up exactly
    /// with the `symbol_id()` shape produced by [`Self::extract_definitions`].
    ///
    /// Per-capture-name behavior (single capture name `call.name` shared
    /// across both query patterns):
    ///
    /// - Direct call (`(call function: (identifier) @call.name)`) →
    ///   edge `to` = identifier text. Covers `foo()`, constructor calls
    ///   (`MyClass()` → `to = "MyClass"`; the agent interprets the edge
    ///   as construction), `super()`, and built-in calls (`print`, `len`).
    /// - Attribute call (`(call function: (attribute attribute:
    ///   (identifier) @call.name))`) → edge `to` = trailing-attribute
    ///   identifier text. Covers method calls (`obj.method()`),
    ///   module-qualified calls (`mod.func()`), and chained calls
    ///   (`a.b().c()` produces 2 edges — one for `b`, one for `c` —
    ///   because tree-sitter parses each chain link as its own `call`
    ///   node, each with its own attribute trailer).
    ///
    /// Calls inside list/set/dict comprehensions, lambdas, and default
    /// arguments are walked transparently by [`enclosing_function_id`]:
    /// none of those are `function_definition` nodes, so the walk passes
    /// through them and reports the enclosing top-level function/method
    /// as the `from`. A call at module top level (no enclosing
    /// `function_definition`, e.g. `print("hi")` at the top of a file)
    /// falls back to the bare file path as the `from` — matching the
    /// C++/Rust/Go top-level-call rule.
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

                // Anchor the line at the enclosing `call` node so the
                // reported line tracks the call site, not the inner
                // identifier (which can be on a continuation line for
                // multi-line chains). Note tree-sitter-python uses the
                // node kind `call`, NOT `call_expression`.
                let call_node = find_enclosing_kind(cap_node, "call").unwrap_or(cap_node);
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

    /// Run the import query and produce `Includes` edges. Mirrors the C++
    /// plugin's `extract_includes`, the Rust plugin's `extract_uses`, and
    /// the Go plugin's `extract_imports`: the edge `from` is the source-
    /// file path (not a symbol ID) and the `to` is the dotted module path.
    /// The `Graph` engine routes `Includes` edges into a per-file map keyed
    /// by `from` (see `Graph::merge_file_graph`).
    ///
    /// Per-capture behavior:
    ///
    /// - `import.module` — the `name` field of an `import_statement`
    ///   (either a bare `dotted_name` or the inner `dotted_name` reached
    ///   through an `aliased_import` wrapper). Each captured node's text is
    ///   the dotted path — `foo`, `foo.bar`, etc. Aliases (`import foo as
    ///   f`) are dropped: only the path is captured, never the alias name.
    ///   Multi-name imports (`import a, b`) produce one capture per name
    ///   because `import_statement` allows multiple `name:` field children
    ///   and the query matches each one.
    /// - `import.from_module` — the `module_name` field of an
    ///   `import_from_statement` when it is a `dotted_name`. Records the
    ///   *module*, not the imported symbol(s) — `from foo import bar, qux`
    ///   yields exactly one edge with `to = "foo"`. Dunder modules
    ///   (`__future__`) flow through this same path with no special-casing.
    /// - `import.from_module_relative` — the `module_name` field when it is
    ///   a `relative_import` (leading-dot form: `from . import x`,
    ///   `from .utils import y`, `from ..pkg import z`).
    ///   * If the `relative_import` text contains a module name after the
    ///     dots (`.utils`, `..pkg.mod`), it is recorded verbatim — the
    ///     imported names are dropped, matching the absolute-form rule.
    ///   * If the `relative_import` text is dots-only (`.`, `..`), the
    ///     imported names are sibling modules of the current package, not
    ///     names inside one module. We emit one edge per imported name
    ///     with `to = <dots><name>` — `from . import a, b` produces two
    ///     edges (`.a`, `.b`); `from . import utils` produces one edge
    ///     (`.utils`). This matches the 7.4 verification field's
    ///     `from . import utils → To='.utils'` rule and preserves the
    ///     leading-dot prefix so consumers can distinguish relative from
    ///     absolute.
    ///
    ///   The default `resolve_include` (basename-match against the
    ///   FileIndex) returns `None` for any of these dotted strings — they
    ///   are not filesystem paths — so the wire format records the
    ///   relative path verbatim and the engine never accidentally
    ///   resolves it.
    ///
    /// The line is anchored at the enclosing `import_statement` /
    /// `import_from_statement` so multi-name imports share a single line
    /// (matches the C++/Rust/Go convention for multi-spec import groups).
    ///
    /// **Conditional imports are NOT extracted by design.** Patterns like
    /// `if TYPE_CHECKING: import x` parse with the inner `import_statement`
    /// nested inside an `if_statement > block`. Tree-sitter queries walk
    /// the whole tree, so the inner `import_statement` would normally
    /// match — but the `extract_imports` walk filters out matches whose
    /// enclosing import-statement is not a direct child of the `module`
    /// root. This keeps the dependency graph stable across files that
    /// guard imports behind feature flags or runtime checks.
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

                // Resolve the enclosing import statement so we can:
                //   1. anchor `line` at the statement (multi-name imports
                //      share a line),
                //   2. filter out conditional imports (those whose
                //      statement is nested inside an `if_statement`,
                //      `try_statement`, `with_statement`, or other
                //      non-module ancestor).
                let stmt_kind = match cap_name {
                    "import.module" => "import_statement",
                    "import.from_module" | "import.from_module_relative" => "import_from_statement",
                    "import.future" => "future_import_statement",
                    _ => continue,
                };
                let Some(stmt_node) = find_enclosing_kind(cap_node, stmt_kind) else {
                    continue;
                };
                if !is_module_top_level(stmt_node) {
                    continue;
                }
                let line = stmt_node.start_position().row as u32 + 1;

                match cap_name {
                    "import.future" => {
                        // `from __future__ import X[, Y, ...]` — emit a
                        // single edge with the synthetic module name
                        // `__future__`. The actual feature names (the
                        // `name:` field children) are dropped, matching
                        // the absolute-import "module is the dependency,
                        // not the imported symbol" rule.
                        fg.edges.push(Edge {
                            from: path.to_owned(),
                            to: "__future__".to_owned(),
                            kind: EdgeKind::Includes,
                            file: path.to_owned(),
                            line,
                        });
                    }
                    "import.module" => {
                        let to = cap_node.utf8_text(content).unwrap_or("").to_owned();
                        if to.is_empty() {
                            continue;
                        }
                        fg.edges.push(Edge {
                            from: path.to_owned(),
                            to,
                            kind: EdgeKind::Includes,
                            file: path.to_owned(),
                            line,
                        });
                    }
                    "import.from_module" => {
                        let to = extract_module_path(cap_node, content);
                        if to.is_empty() {
                            continue;
                        }
                        fg.edges.push(Edge {
                            from: path.to_owned(),
                            to,
                            kind: EdgeKind::Includes,
                            file: path.to_owned(),
                            line,
                        });
                    }
                    "import.from_module_relative" => {
                        // Relative-import bookkeeping. The relative_import
                        // node text is either dots-only (`.`, `..`) or
                        // dots-plus-module (`.utils`, `..pkg.mod`).
                        let rel_text = extract_module_path(cap_node, content);
                        if rel_text.is_empty() {
                            continue;
                        }
                        if rel_text.chars().all(|c| c == '.') {
                            // Dots-only: each imported name is a sibling
                            // module. Walk the import_from_statement's
                            // `name:` field children and emit one edge
                            // per imported name with `to = <dots><name>`.
                            for name in imported_names(stmt_node, content) {
                                fg.edges.push(Edge {
                                    from: path.to_owned(),
                                    to: format!("{rel_text}{name}"),
                                    kind: EdgeKind::Includes,
                                    file: path.to_owned(),
                                    line,
                                });
                            }
                        } else {
                            // Dots-plus-module: the dependency target is
                            // the relative_import text itself; imported
                            // names are dropped (same as absolute form).
                            fg.edges.push(Edge {
                                from: path.to_owned(),
                                to: rel_text,
                                kind: EdgeKind::Includes,
                                file: path.to_owned(),
                                line,
                            });
                        }
                    }
                    _ => continue,
                }
            }
        }
    }
}

impl LanguagePlugin for PythonParser {
    fn id(&self) -> Language {
        Language::Python
    }

    fn extensions(&self) -> &'static [&'static str] {
        EXTENSIONS
    }

    /// Parse `content` (UTF-8 bytes) as Python and produce a [`FileGraph`].
    ///
    /// Phase 7.2 wires the definition extractor; Phase 7.3 wires the call
    /// extractor; Phase 7.4 wires the import extractor (both forms plus
    /// the special `future_import_statement` node and the conditional-
    /// import filter); inheritance (7.5) follows.
    fn parse_file(&self, path: &Path, content: &[u8]) -> Result<FileGraph, ParseError> {
        self.parse_to_filegraph(path, content)
    }

    // resolve_call and resolve_include intentionally NOT overridden — see
    // the crate-level docstring for the rationale (default heuristic
    // matches the C++/Rust/Go plugins; default basename resolver is a
    // no-op for Python's dotted module-path imports, which is the
    // intended behavior).

    fn close(&self) {}
}

/// Build a tree-sitter [`TsTree`] for `content` against the Python grammar.
/// The caller-supplied [`TsLanguage`] is borrowed; the returned tree owns
/// its AST. Returns [`ParseError::Parse`] if `set_language` fails or if
/// tree-sitter declines to produce a tree (e.g. on cancellation). Mirrors
/// `parse_tree` in the C++/Rust/Go plugins byte-for-byte modulo the
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
/// indices, matching the C++/Rust/Go plugins' silent fallback.
fn capture_name_for_index<'a>(cap_names: &[&'a str], index: u32) -> &'a str {
    cap_names.get(index as usize).copied().unwrap_or("")
}

/// Walk up `node`'s parent chain, returning the first ancestor (including
/// `node` itself) whose kind matches `kind`. Local copy of the C++/Rust/Go
/// plugins' `find_enclosing_kind`.
fn find_enclosing_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut current = Some(node);
    while let Some(n) = current {
        if n.kind() == kind {
            return Some(n);
        }
        current = n.parent();
    }
    None
}

/// Return `true` when `stmt` is a direct child of the `module` root — i.e.
/// a top-level statement, not nested inside a conditional / try / with /
/// function / class block.
///
/// Used by [`PythonParser::extract_imports`] to filter out conditional
/// imports such as:
///
/// ```python
/// if TYPE_CHECKING:
///     import expensive_module
/// ```
///
/// Tree-sitter queries walk the entire tree by default, so the inner
/// `import_statement` would otherwise match `IMPORT_QUERIES`. The phase 7.4
/// contract is that conditional imports are excluded from the dependency
/// graph: a file's import edges should reflect what it depends on
/// unconditionally at module load. This filter is the enforcement point.
fn is_module_top_level(stmt: Node<'_>) -> bool {
    stmt.parent().map(|p| p.kind() == "module").unwrap_or(false)
}

/// Return the imported-name texts from an `import_from_statement` —
/// the `name:` field children. Each child is a `dotted_name` (single
/// identifier or qualified) or an `aliased_import` wrapping one; in either
/// case we return the path text and drop any alias.
///
/// Used by [`PythonParser::extract_imports`] when the `module_name` is a
/// dots-only `relative_import` (`from . import a, b`): each imported name
/// is a separate sibling module and contributes its own dependency edge.
fn imported_names<'a>(stmt: Node<'a>, content: &'a [u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = stmt.walk();
    for child in stmt.children_by_field_name("name", &mut cursor) {
        let path_node = if child.kind() == "aliased_import" {
            child.child_by_field_name("name").unwrap_or(child)
        } else {
            child
        };
        let text = path_node.utf8_text(content).unwrap_or("");
        if !text.is_empty() {
            out.push(text.to_owned());
        }
    }
    out
}

/// Build a [`Symbol`] from a definition node. Centralises the row/column/
/// signature math so each branch in `extract_definitions` stays small.
/// Mirrors the C++/Rust/Go plugins' `make_symbol`.
///
/// Python `Symbol.namespace` is always `""` — the module concept is
/// encoded in the file path. Phase 7.7's documentation makes this
/// explicit.
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
        language: Language::Python,
    }
}

#[cfg(test)]
mod tests {
    //! Phase 7.1 structural smoke tests + Phase 7.2 definition-extraction
    //! coverage. Behavioral coverage of call / import / inheritance
    //! extraction lands in 7.3-7.5.
    use super::*;
    use codegraph_core::symbol_id;
    use codegraph_lang::LanguagePlugin;

    // ----------------------------------------------------------------
    // Phase 7.1 — structural smoke tests
    // ----------------------------------------------------------------

    #[test]
    fn new_compiles_all_four_queries() {
        // The whole point of Phase 7.1: every query string parses against
        // the pinned tree-sitter-python. Failure here means a query needs
        // updating.
        let p = PythonParser::new().expect("PythonParser::new must succeed");
        let _ = (
            &p.language,
            &p.def_query,
            &p.call_query,
            &p.import_query,
            &p.inheritance_query,
        );
    }

    #[test]
    fn extensions_match_expected_list() {
        assert_eq!(PythonParser::extensions(), &[".py", ".pyi"]);
        let p = PythonParser::new().unwrap();
        assert_eq!(LanguagePlugin::extensions(&p), PythonParser::extensions());
    }

    #[test]
    fn id_is_python() {
        let p = PythonParser::new().unwrap();
        assert_eq!(p.id(), Language::Python);
    }

    /// Canonical compile-time-interface check + `id() -> Language::Python`
    /// assertion. Mirrors the C++ test at
    /// `crates/codegraph-lang-cpp/src/lib.rs:542-545` exactly. This is the
    /// Phase 7.1 verification field's named test
    /// (`python_parser_is_object_safe_via_box_dyn`).
    #[test]
    fn python_parser_is_object_safe_via_box_dyn() {
        let p: Box<dyn LanguagePlugin> = Box::new(PythonParser::new().unwrap());
        assert_eq!(p.id(), Language::Python);
    }

    // ----------------------------------------------------------------
    // Phase 7.2 — definition extraction
    // ----------------------------------------------------------------

    /// Parse `src` against `PythonParser` at a synthetic absolute path.
    /// Used by every Phase 7.2 behavioral test below.
    fn parse(src: &str) -> FileGraph {
        parse_at(src, "/tmp/test.py")
    }

    /// Parse `src` against `PythonParser` at a caller-chosen path. Lets
    /// the `.pyi` parity test exercise the same code path with a stub
    /// extension.
    fn parse_at(src: &str, path: &str) -> FileGraph {
        let p = PythonParser::new().unwrap();
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
        // The path/language assertion still belongs at this layer — 7.2
        // populates symbols for non-empty files but the wrapper fields
        // remain meaningful.
        let fg = parse("");
        assert_eq!(fg.path, "/tmp/test.py");
        assert_eq!(fg.language, Language::Python);
    }

    #[test]
    fn empty_file_produces_no_symbols() {
        // Anti-regression: an empty source file yields zero symbols and
        // zero edges (the parse must succeed; tree-sitter handles the
        // empty input and produces a minimal `module` node).
        let fg = parse("");
        assert!(fg.symbols.is_empty(), "got: {:?}", fg.symbols);
        assert!(fg.edges.is_empty(), "got: {:?}", fg.edges);
    }

    #[test]
    fn free_function_produces_function_kind_no_parent() {
        // `def foo(): pass` → 1 symbol, Function, no parent, signature
        // truncated correctly (no body).
        let fg = parse("def foo():\n    pass\n");
        assert_eq!(fg.symbols.len(), 1, "got: {:?}", fg.symbols);
        let s = sym(&fg, "foo");
        assert_eq!(s.kind, SymbolKind::Function);
        assert!(s.parent.is_empty(), "free func must have empty parent");
        assert!(
            s.namespace.is_empty(),
            "Python namespace stays empty (file path encodes the module)"
        );
        assert_eq!(s.language, Language::Python);
        assert_eq!(symbol_id(s), "/tmp/test.py:foo");
        // `truncate_signature` stops at `{` or `;` — neither appears in
        // Python source. The signature is the entire function node text
        // because Python bodies use `:`, not `{` or `;`. We assert the
        // signature contains the def-line head.
        assert!(
            s.signature.contains("def foo()"),
            "signature must contain `def foo()`, got: {:?}",
            s.signature
        );
    }

    #[test]
    fn method_in_class_produces_method_kind_with_class_parent() {
        // `class C: def m(self): pass` → 2 symbols (C: Class, m: Method
        // with parent=C).
        let fg = parse("class C:\n    def m(self):\n        pass\n");
        assert_eq!(fg.symbols.len(), 2, "got: {:?}", fg.symbols);
        let c = sym(&fg, "C");
        assert_eq!(c.kind, SymbolKind::Class);
        assert!(c.parent.is_empty(), "top-level class must have no parent");
        assert_eq!(symbol_id(c), "/tmp/test.py:C");
        let m = sym(&fg, "m");
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.parent, "C", "method's parent must be the enclosing class");
        assert_eq!(symbol_id(m), "/tmp/test.py:C::m");
    }

    #[test]
    fn async_method_in_class_produces_method_kind_with_class_parent() {
        // `class Server: async def handle(self): pass` — async def parses
        // as `function_definition` in tree-sitter-python 0.25, so the same
        // code path covers it. Confirm with a fixture (the 7.2 verification
        // field's "confirmed by fixture" requirement).
        let fg = parse("class Server:\n    async def handle(self):\n        pass\n");
        let server = sym(&fg, "Server");
        assert_eq!(server.kind, SymbolKind::Class);
        let handle = sym(&fg, "handle");
        assert_eq!(
            handle.kind,
            SymbolKind::Method,
            "async def in class must be Method, not Function"
        );
        assert_eq!(handle.parent, "Server");
        assert_eq!(symbol_id(handle), "/tmp/test.py:Server::handle");
    }

    #[test]
    fn async_free_function_produces_function_kind_no_parent() {
        // `async def fetch(): pass` at module scope → Function, no parent.
        // Same code path as the sync form.
        let fg = parse("async def fetch():\n    pass\n");
        let s = sym(&fg, "fetch");
        assert_eq!(s.kind, SymbolKind::Function);
        assert!(s.parent.is_empty());
    }

    #[test]
    fn decorated_free_function_is_function_kind() {
        // `@property def x(): pass` at module scope. Even though
        // `@property` is conventionally used inside classes, applying it
        // to a free function is syntactically valid Python and tree-sitter
        // wraps it in `decorated_definition > function_definition`. The
        // queries match the inner node directly — decorator transparency.
        let fg = parse("@property\ndef x():\n    pass\n");
        let s = sym(&fg, "x");
        assert_eq!(
            s.kind,
            SymbolKind::Function,
            "free function with decorator stays a Function (decorator transparent)"
        );
        assert!(s.parent.is_empty());
    }

    #[test]
    fn decorated_method_is_method_kind_with_class_parent() {
        // `class A: @property def x(self): pass` — decorated method.
        // The `decorated_definition` wrapper does not block the
        // class-ancestor walk; `find_enclosing_class` finds A through it.
        let fg = parse("class A:\n    @property\n    def x(self):\n        return 1\n");
        let x = sym(&fg, "x");
        assert_eq!(
            x.kind,
            SymbolKind::Method,
            "decorated method stays a Method (decorator transparent)"
        );
        assert_eq!(x.parent, "A");
        assert_eq!(symbol_id(x), "/tmp/test.py:A::x");
    }

    #[test]
    fn staticmethod_decorated_method_is_method_kind() {
        // `class A: @staticmethod def s(): pass` — same decorator-
        // transparency rule. `@staticmethod` is the canonical case:
        // omitting `self` is legal because of the decorator.
        let fg = parse("class A:\n    @staticmethod\n    def s():\n        pass\n");
        let s = sym(&fg, "s");
        assert_eq!(s.kind, SymbolKind::Method);
        assert_eq!(s.parent, "A");
    }

    #[test]
    fn nested_class_records_outer_class_as_parent() {
        // `class A: class B: pass` — A is a top-level Class with no
        // parent; B is a Class with parent=A.
        let fg = parse("class A:\n    class B:\n        pass\n");
        let a = sym(&fg, "A");
        assert_eq!(a.kind, SymbolKind::Class);
        assert!(a.parent.is_empty(), "top-level class must have no parent");
        let b = sym(&fg, "B");
        assert_eq!(b.kind, SymbolKind::Class);
        assert_eq!(
            b.parent, "A",
            "inner class's parent must be the outer class"
        );
        assert_eq!(symbol_id(b), "/tmp/test.py:A::B");
    }

    #[test]
    fn method_in_nested_class_records_innermost_class_as_parent() {
        // `class Outer: class Inner: def m(self): pass` — m is a Method
        // with parent=Inner (the innermost enclosing class), NOT Outer
        // and NOT "Outer::Inner". This matches the
        // `enclosing_function_id` and `find_enclosing_class` rules
        // documented in `helpers.rs`.
        let src = "class Outer:\n    class Inner:\n        def m(self):\n            pass\n";
        let fg = parse(src);
        let m = sym(&fg, "m");
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(
            m.parent, "Inner",
            "innermost enclosing class wins for nested-class methods"
        );
        assert_eq!(symbol_id(m), "/tmp/test.py:Inner::m");
    }

    #[test]
    fn dunder_init_is_ordinary_method() {
        // `__init__` receives no special handling — ordinary method.
        let fg = parse("class C:\n    def __init__(self):\n        pass\n");
        let init = sym(&fg, "__init__");
        assert_eq!(init.kind, SymbolKind::Method);
        assert_eq!(init.parent, "C");
        assert_eq!(symbol_id(init), "/tmp/test.py:C::__init__");
    }

    #[test]
    fn dunder_str_repr_call_are_ordinary_methods() {
        // Belt-and-suspenders for the "no special handling for dunders"
        // rule documented in 7.2.
        let src = "class C:\n    def __str__(self):\n        return ''\n    def __repr__(self):\n        return ''\n    def __call__(self):\n        pass\n";
        let fg = parse(src);
        for name in &["__str__", "__repr__", "__call__"] {
            let s = sym(&fg, name);
            assert_eq!(s.kind, SymbolKind::Method, "{name} must be Method");
            assert_eq!(s.parent, "C", "{name} parent must be C");
        }
    }

    #[test]
    fn class_with_base_still_emits_class_symbol() {
        // `class D(B): pass` — the `superclasses: argument_list` doesn't
        // change the Class symbol. Inheritance edges land in 7.5; here we
        // only verify that the presence of bases doesn't break definition
        // extraction.
        let fg = parse("class D(B):\n    pass\n");
        let d = sym(&fg, "D");
        assert_eq!(d.kind, SymbolKind::Class);
        assert!(
            d.parent.is_empty(),
            "top-level class with base has no parent symbol"
        );
    }

    #[test]
    fn line_and_end_line_are_one_indexed_and_populated() {
        // Sanity check that line/end_line track the def/class span
        // (1-indexed). The function below starts at row 0 (line 1).
        let fg = parse("def foo():\n    pass\n");
        let s = sym(&fg, "foo");
        assert_eq!(s.line, 1, "def on line 1");
        // end_line is the last line of the function_definition node,
        // which includes the body (the `pass` on line 2).
        assert!(s.end_line >= s.line, "end_line >= line");
        assert_eq!(s.column, 0, "def starts at column 0");
    }

    #[test]
    fn signature_truncates_to_def_line() {
        // `def foo(): pass` on a single physical line. `truncate_signature`
        // stops at `;` or `{` which Python lacks; the def's text is the
        // whole statement, so the signature contains both the head and
        // the body marker. The contract is "no body brace", which Python
        // trivially satisfies.
        let fg = parse("def foo(x, y):\n    return x + y\n");
        let s = sym(&fg, "foo");
        assert!(
            s.signature.contains("def foo(x, y)"),
            "signature must contain the def head, got: {:?}",
            s.signature
        );
    }

    #[test]
    fn pyi_stub_file_extracts_function_identically_to_py() {
        // `.pyi` parity (verification field requirement): parsing a
        // snippet at a path ending in `.pyi` must produce the same Symbol
        // shape as the `.py` equivalent. Stub bodies (`...`) parse as
        // expression_statement, not as a separate node kind, so the
        // function_definition query matches and the function symbol is
        // emitted.
        let src = "def foo(x: int) -> str: ...\n";
        let fg = parse_at(src, "/tmp/stubs.pyi");
        assert_eq!(fg.path, "/tmp/stubs.pyi");
        let s = sym(&fg, "foo");
        assert_eq!(s.kind, SymbolKind::Function);
        assert!(s.parent.is_empty());
        assert_eq!(s.language, Language::Python);
        assert_eq!(symbol_id(s), "/tmp/stubs.pyi:foo");
    }

    #[test]
    fn pyi_stub_file_extracts_class_with_method_stub_identically_to_py() {
        // Class stub with a method stub: both symbols must extract.
        let src = "class C:\n    def m(self) -> int: ...\n";
        let fg = parse_at(src, "/tmp/stubs.pyi");
        assert_eq!(fg.symbols.len(), 2, "got: {:?}", fg.symbols);
        let c = sym(&fg, "C");
        assert_eq!(c.kind, SymbolKind::Class);
        let m = sym(&fg, "m");
        assert_eq!(m.kind, SymbolKind::Method);
        assert_eq!(m.parent, "C");
    }

    // ----------------------------------------------------------------
    // Phase 7.3 — call extraction
    // ----------------------------------------------------------------

    /// Filter `fg.edges` to just the `Calls` edges for ergonomic asserts.
    fn calls(fg: &FileGraph) -> Vec<&Edge> {
        fg.edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .collect()
    }

    #[test]
    fn direct_call_in_free_function_produces_one_calls_edge() {
        // `def f(): foo()` → 1 edge, To=foo, From=path:f.
        let fg = parse("def f():\n    foo()\n");
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", fg.edges);
        let e = edges[0];
        assert_eq!(e.to, "foo");
        assert_eq!(e.from, "/tmp/test.py:f");
        assert_eq!(e.kind, EdgeKind::Calls);
        assert_eq!(e.file, "/tmp/test.py");
    }

    #[test]
    fn builtin_call_is_captured_as_direct_call() {
        // `def f(): print("x")` → 1 edge, To=print. Built-ins receive no
        // special handling; they look like any other identifier callee.
        let fg = parse("def f():\n    print(\"x\")\n");
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", fg.edges);
        assert_eq!(edges[0].to, "print");
        assert_eq!(edges[0].from, "/tmp/test.py:f");
    }

    #[test]
    fn attribute_call_records_trailing_identifier() {
        // `def f(): obj.method()` → 1 edge, To=method (the trailing
        // attribute), From=path:f.
        let fg = parse("def f():\n    obj.method()\n");
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", fg.edges);
        assert_eq!(edges[0].to, "method");
        assert_eq!(edges[0].from, "/tmp/test.py:f");
    }

    #[test]
    fn chained_call_produces_two_edges() {
        // `def f(): a.b().c()` — outer call has attribute=c, inner call
        // has attribute=b. Each chain link is its own `call` node, so
        // two edges fall out naturally.
        let fg = parse("def f():\n    a.b().c()\n");
        let edges = calls(&fg);
        assert_eq!(edges.len(), 2, "got: {:?}", fg.edges);
        let tos: Vec<&str> = edges.iter().map(|e| e.to.as_str()).collect();
        assert!(tos.contains(&"b"), "expected To=b, got {tos:?}");
        assert!(tos.contains(&"c"), "expected To=c, got {tos:?}");
        for e in &edges {
            assert_eq!(e.from, "/tmp/test.py:f");
        }
    }

    #[test]
    fn constructor_call_is_captured_as_direct_call() {
        // `def f(): MyClass()` → 1 edge, To=MyClass. Constructor calls
        // naturally match the direct-call pattern; the agent interprets
        // class-named edges as construction.
        let fg = parse("def f():\n    MyClass()\n");
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", fg.edges);
        assert_eq!(edges[0].to, "MyClass");
        assert_eq!(edges[0].from, "/tmp/test.py:f");
    }

    #[test]
    fn super_dot_init_produces_two_edges_super_then_init() {
        // `def f(): super().__init__()` — the outer call's attribute is
        // `__init__`, the inner call's function is the bare identifier
        // `super`. Two edges: To=super (direct) and To=__init__ (attr).
        let fg = parse("def f():\n    super().__init__()\n");
        let edges = calls(&fg);
        assert_eq!(edges.len(), 2, "got: {:?}", fg.edges);
        let tos: Vec<&str> = edges.iter().map(|e| e.to.as_str()).collect();
        assert!(tos.contains(&"super"), "expected To=super, got {tos:?}");
        assert!(
            tos.contains(&"__init__"),
            "expected To=__init__, got {tos:?}"
        );
        for e in &edges {
            assert_eq!(e.from, "/tmp/test.py:f");
        }
    }

    #[test]
    fn method_call_records_class_qualified_from() {
        // `class C: def m(self): self.helper()` — From=path:C::m,
        // To=helper. The enclosing-function walk in `enclosing_function_id`
        // finds the `function_definition` first and the `class_definition`
        // ancestor, so the From carries the class prefix.
        let fg = parse("class C:\n    def m(self):\n        self.helper()\n");
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", fg.edges);
        assert_eq!(edges[0].to, "helper");
        assert_eq!(edges[0].from, "/tmp/test.py:C::m");
    }

    #[test]
    fn calls_inside_lambda_attribute_to_enclosing_function() {
        // `def f(): g = lambda: helper(); g()` — the body of the lambda
        // contains a real `call` (`helper()`). Lambda is NOT a
        // function_definition, so `enclosing_function_id`'s walk passes
        // through it. The expected edges include To=helper (called inside
        // the lambda) and To=g (the lambda invocation). All edges'
        // From=path:f exercises the lambda-transparent walk end-to-end.
        let fg = parse("def f():\n    g = lambda: helper()\n    g()\n");
        let edges = calls(&fg);
        assert!(edges.len() >= 2, "expected >=2 edges, got: {:?}", fg.edges);
        let tos: Vec<&str> = edges.iter().map(|e| e.to.as_str()).collect();
        assert!(
            tos.contains(&"helper"),
            "expected To=helper (call inside lambda body), got {tos:?}"
        );
        assert!(
            tos.contains(&"g"),
            "expected To=g (the lambda invocation), got {tos:?}"
        );
        for e in &edges {
            assert_eq!(
                e.from, "/tmp/test.py:f",
                "all edges (including the call inside the lambda) must \
                 attribute From to the enclosing top-level function"
            );
        }
    }

    #[test]
    fn calls_inside_list_comprehension_attribute_to_enclosing_function() {
        // `def f(): [foo(x) for x in items]` → 1 edge, To=foo, From=path:f.
        // List comprehensions are not function_definitions, so the walk
        // passes through transparently.
        let fg = parse("def f():\n    [foo(x) for x in items]\n");
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", fg.edges);
        assert_eq!(edges[0].to, "foo");
        assert_eq!(edges[0].from, "/tmp/test.py:f");
    }

    #[test]
    fn module_level_call_falls_back_to_bare_file_path() {
        // `print("hi")` at the top of the file — no enclosing
        // function_definition, so the From falls back to the bare file
        // path (no `:name` suffix). Mirrors the C++/Rust/Go top-level-
        // call rule.
        let fg = parse("print(\"hi\")\n");
        let edges = calls(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", fg.edges);
        assert_eq!(edges[0].to, "print");
        assert_eq!(
            edges[0].from, "/tmp/test.py",
            "module-level call's From must be the bare file path"
        );
    }

    // ----------------------------------------------------------------
    // Phase 7.4 — import extraction
    // ----------------------------------------------------------------

    /// Filter `fg.edges` to just the `Includes` edges for ergonomic asserts.
    fn includes(fg: &FileGraph) -> Vec<&Edge> {
        fg.edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Includes)
            .collect()
    }

    #[test]
    fn import_simple_records_one_edge_with_module_path() {
        // `import foo` → 1 edge, To='foo', From=path, kind=Includes.
        let fg = parse("import foo\n");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", fg.edges);
        let e = edges[0];
        assert_eq!(e.to, "foo");
        assert_eq!(e.from, "/tmp/test.py");
        assert_eq!(e.kind, EdgeKind::Includes);
        assert_eq!(e.file, "/tmp/test.py");
    }

    #[test]
    fn import_dotted_records_full_module_path() {
        // `import foo.bar` → 1 edge, To='foo.bar' (NOT 'foo' or 'bar').
        let fg = parse("import foo.bar\n");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", fg.edges);
        assert_eq!(edges[0].to, "foo.bar");
    }

    #[test]
    fn import_aliased_drops_alias_records_path() {
        // `import foo as f` → 1 edge, To='foo' (alias dropped). The
        // `aliased_import` query targets the inner `dotted_name` directly,
        // so the alias never reaches the extractor.
        let fg = parse("import foo as f\n");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", fg.edges);
        assert_eq!(edges[0].to, "foo");
    }

    #[test]
    fn import_multi_name_produces_one_edge_per_name() {
        // `import a, b` → 2 edges, To='a' and To='b'. Tree-sitter parses
        // this as one `import_statement` with two `name:` field children;
        // the query matches each one.
        let fg = parse("import a, b\n");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 2, "got: {:?}", fg.edges);
        let tos: Vec<&str> = edges.iter().map(|e| e.to.as_str()).collect();
        assert!(tos.contains(&"a"), "expected To=a, got {tos:?}");
        assert!(tos.contains(&"b"), "expected To=b, got {tos:?}");
        for e in &edges {
            assert_eq!(e.from, "/tmp/test.py");
            assert_eq!(e.kind, EdgeKind::Includes);
        }
    }

    #[test]
    fn from_import_records_module_not_imported_name() {
        // `from foo import bar` → 1 edge, To='foo' (NOT 'bar'). The
        // dependency target is the *module*, not the imported symbol.
        let fg = parse("from foo import bar\n");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", fg.edges);
        assert_eq!(
            edges[0].to, "foo",
            "from-import's To must be the module, not the imported name"
        );
    }

    #[test]
    fn from_import_dotted_records_full_module_path() {
        // `from foo.bar import baz` → 1 edge, To='foo.bar'.
        let fg = parse("from foo.bar import baz\n");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", fg.edges);
        assert_eq!(edges[0].to, "foo.bar");
    }

    #[test]
    fn from_import_multi_name_records_one_edge() {
        // `from foo import bar, qux` → still 1 edge, To='foo'. Multiple
        // imported names share one module, so they share one edge.
        let fg = parse("from foo import bar, qux\n");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", fg.edges);
        assert_eq!(edges[0].to, "foo");
    }

    #[test]
    fn from_typing_import_list_dict_records_one_edge() {
        // `from typing import List, Dict` → 1 edge, To='typing'. Belt-and-
        // suspenders for the multi-name rule with a real-world case.
        let fg = parse("from typing import List, Dict\n");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", fg.edges);
        assert_eq!(edges[0].to, "typing");
    }

    #[test]
    fn from_relative_import_preserves_dot() {
        // `from . import utils` → 1 edge, To='.utils' (relative_import
        // preserves the leading dot verbatim — distinguishes relative
        // from absolute imports for downstream consumers).
        // Pin edge.line to anchor the line at the statement (not the
        // inner `relative_import` node).
        let fg = parse("from . import utils\n");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", fg.edges);
        assert_eq!(edges[0].to, ".utils");
        assert_eq!(
            edges[0].line, 1,
            "line is anchored at the import_from_statement"
        );
    }

    #[test]
    fn from_relative_double_dot_import_preserves_double_dot() {
        // `from ..pkg import x` → 1 edge, To='..pkg' — both leading dots
        // preserved. Belt-and-suspenders for the relative-import rule.
        let fg = parse("from ..pkg import x\n");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", fg.edges);
        assert_eq!(edges[0].to, "..pkg");
    }

    #[test]
    fn from_future_import_records_dunder_module() {
        // `from __future__ import annotations` → 1 edge, To='__future__'.
        // tree-sitter-python parses this as a *distinct* node kind
        // `future_import_statement` (NOT `import_from_statement`) — the
        // grammar special-cases `__future__` imports because they have
        // unique runtime semantics. The IMPORT_QUERIES match this node
        // kind too so the dunder case lands here.
        let fg = parse("from __future__ import annotations\n");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", fg.edges);
        assert_eq!(edges[0].to, "__future__");
    }

    #[test]
    fn conditional_import_inside_if_block_produces_zero_edges() {
        // Anti-regression: `if TYPE_CHECKING: import expensive_module`
        // parses with the inner `import_statement` nested inside an
        // `if_statement > block`. The 7.4 contract is that conditional
        // imports do NOT contribute to the dependency graph — `is_module_top_level`
        // filters out matches whose enclosing import-statement is not a
        // direct child of the `module` root.
        //
        // This file should produce zero `Includes` edges. (The
        // `TYPE_CHECKING = False` line and `if TYPE_CHECKING:` itself
        // contain no calls or imports that would cause noise in this
        // assertion.)
        let src = "TYPE_CHECKING = False\nif TYPE_CHECKING:\n    import expensive_module\n";
        let fg = parse(src);
        let edges = includes(&fg);
        assert!(
            edges.is_empty(),
            "conditional imports must not produce Includes edges, got: {:?}",
            edges
        );
    }

    #[test]
    fn conditional_from_import_inside_try_block_produces_zero_edges() {
        // Belt-and-suspenders for the conditional-import filter: imports
        // inside a `try_statement > block` are also filtered.
        let src = "try:\n    from foo import bar\nexcept ImportError:\n    pass\n";
        let fg = parse(src);
        let edges = includes(&fg);
        assert!(
            edges.is_empty(),
            "imports inside try blocks must not produce Includes edges, got: {:?}",
            edges
        );
    }

    #[test]
    fn relative_import_records_to_field_verbatim_for_default_resolve_include() {
        // The default `resolve_include` (basename match against the
        // FileIndex) is a no-op for Python's dotted module strings. End-to-
        // end this means an edge from `src/app.py` containing `from .
        // import utils` records `to = ".utils"` verbatim — no resolution
        // happens at parse_file. This test pins the wire format directly
        // (no need to invoke `resolve_edges` — the default trait method's
        // no-op behavior is asserted by structure: parse_file produces
        // an `Includes` edge whose `to` is the literal dotted/relative
        // module string, never a resolved file path).
        let fg = parse_at("from . import utils\n", "/repo/src/app.py");
        let edges = includes(&fg);
        assert_eq!(edges.len(), 1, "got: {:?}", fg.edges);
        let e = edges[0];
        assert_eq!(
            e.to, ".utils",
            "wire format records the relative module string verbatim"
        );
        assert_eq!(e.from, "/repo/src/app.py");
        assert_eq!(e.file, "/repo/src/app.py");
        assert_eq!(e.kind, EdgeKind::Includes);
    }

    #[test]
    fn mixed_imports_produce_distinct_edges() {
        // Sanity: a module with several import forms produces the
        // expected union — one edge per distinct module reference, alias
        // dropped, multi-name expanded for `import a, b` and collapsed
        // for `from foo import a, b`.
        let src = "\
import foo
import bar.baz
import qux as q
from typing import List, Dict
from . import utils
import a, b
";
        let fg = parse(src);
        let edges = includes(&fg);
        let tos: Vec<&str> = edges.iter().map(|e| e.to.as_str()).collect();
        // Expected Tos: foo, bar.baz, qux, typing, .utils, a, b
        assert_eq!(edges.len(), 7, "expected 7 Includes edges, got {:?}", tos);
        for expected in ["foo", "bar.baz", "qux", "typing", ".utils", "a", "b"] {
            assert!(
                tos.contains(&expected),
                "expected To={expected:?} in {tos:?}"
            );
        }
        for e in &edges {
            assert_eq!(e.kind, EdgeKind::Includes);
            assert_eq!(e.from, "/tmp/test.py");
        }
    }
}
