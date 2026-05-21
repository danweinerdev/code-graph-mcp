//! Rust language plugin for code-graph-mcp.
//!
//! Uses tree-sitter (via the `tree-sitter` and `tree-sitter-rust` crates)
//! to extract symbols, calls, use-declarations, and trait-impl edges from
//! Rust source files.
//!
//! # Extraction surface
//!
//! The crate compiles query strings against tree-sitter-rust 0.24.x,
//! exposes the `RustParser` struct with cached `Query` objects, and
//! implements `LanguagePlugin`. Every extractor below is live and
//! `parse_file` is fully populated.
//!
//! `extract_definitions` feeds `parse_file`. `extract_uses` covers
//! use-tree expansion + `extern crate`. `extract_calls` covers direct,
//! method, scoped, and macro calls. `extract_inheritance` covers
//! `impl Trait for Type`.
//!
//! # Known Rust parser limitations
//!
//! These match the documented design and apply to the Rust parser as it is
//! built out. They are intentional, not bugs.
//!
//! 1. **`macro_rules!` definitions are not extracted as symbols.** Only
//!    invocations produce call edges. The `DEFINITION_QUERIES`
//!    constant explicitly does not match `macro_definition` (the
//!    tree-sitter-rust 0.24 node type that wraps `macro_rules!` blocks),
//!    and an anti-regression test
//!    (`macro_rules_definition_produces_zero_symbols`) asserts a
//!    fixture with `macro_rules! foo { ... }` yields no Symbol records.
//! 2. **Forward declarations excluded — except for trait method
//!    signatures.** Abstract trait method declarations
//!    (`function_signature_item` inside a `trait_item`, e.g.
//!    `fn bar();`) are extracted as `Method` symbols with
//!    `parent = trait_name`. Other forward declarations — bare
//!    `function_signature_item`s outside any trait (e.g. inside an
//!    `extern "C"` block) — remain excluded. Trait default methods
//!    (with bodies) classify as `Method`/parent=trait the same way
//!    abstract signatures do. Methods inside
//!    `impl Trait for Type { ... }` blocks still classify as Method
//!    with parent=Type (nearest definition ancestor wins; see
//!    `helpers::find_nearest_def_ancestor`). This is a deliberate,
//!    Rust-trait-scoped exception to the cross-language "forward
//!    declarations excluded" invariant.
//! 3. **`#[derive(...)]` and proc-macro attributes** appear as
//!    `attribute_item` (not `macro_invocation`) so they are NOT captured
//!    as call edges.
//! 4. **Call resolution is heuristic** — same-file > same-parent >
//!    same-namespace > global, identical to the C++ plugin's behavior via
//!    the default `LanguagePlugin::resolve_call` impl.
//! 5. **Complex use trees expanded but lifetime/generic constraints not
//!    represented.** Use-edge `to` fields record the dotted path; generic
//!    parameters and lifetime bounds are not part of the edge.

pub(crate) mod crate_model;
pub(crate) mod helpers;
pub(crate) mod queries;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use code_graph_core::{Edge, EdgeKind, FileGraph, Language, Symbol, SymbolKind};
use code_graph_lang::{FileIndex, LanguagePlugin, ParseError};
use streaming_iterator::StreamingIterator;
use tree_sitter::{
    Language as TsLanguage, Node, Parser as TsParser, Query, QueryCursor, Tree as TsTree,
};

use crate::crate_model::CrateModuleModel;
use crate::helpers::{
    enclosing_function_id, find_enclosing_kind, find_nearest_def_ancestor, resolve_mod_namespace,
    split_use_path, truncate_signature, NearestDefAncestor,
};
use crate::queries::{
    CALL_QUERIES, DEFINITION_QUERIES, INHERITANCE_QUERIES, MOD_DECL_QUERIES, USE_QUERIES,
};

/// File extensions the Rust parser claims.
pub const EXTENSIONS: &[&str] = &[".rs"];

/// Rust source-file parser. Holds the tree-sitter `Language` and the four
/// pre-compiled queries used to drive symbol/edge extraction in Phases
/// 5.2-5.4.
///
/// Construct with [`RustParser::new`]; share across threads (queries are
/// `Send + Sync`).
pub struct RustParser {
    /// Compiled Rust grammar. Held so per-call [`tree_sitter::Parser`]
    /// instances built inside `parse_file` can attach to it without
    /// rebuilding the `LanguageFn`.
    language: TsLanguage,
    /// Compiled definition query.
    def_query: Query,
    /// Compiled call query.
    call_query: Query,
    /// Compiled use-declaration query.
    use_query: Query,
    /// Compiled inheritance / trait-impl query.
    inh_query: Query,
    /// Compiled module-declaration query (drives external-`mod`
    /// provisional `Includes` edge emission).
    mod_query: Query,
}

impl RustParser {
    /// Build a new parser, compiling every tree-sitter query against the
    /// pinned tree-sitter-rust grammar. Returns an [`anyhow::Error`]
    /// (wrapping the query compiler's message) if any query fails to
    /// compile against the pinned grammar version.
    ///
    /// Successful return proves every query string in `queries.rs` parses
    /// against tree-sitter-rust 0.24.x.
    pub fn new() -> anyhow::Result<Self> {
        let language: TsLanguage = tree_sitter_rust::LANGUAGE.into();

        let def_query = Query::new(&language, DEFINITION_QUERIES)
            .map_err(|e| anyhow::anyhow!("definition query: {e}"))?;
        let call_query =
            Query::new(&language, CALL_QUERIES).map_err(|e| anyhow::anyhow!("call query: {e}"))?;
        let use_query =
            Query::new(&language, USE_QUERIES).map_err(|e| anyhow::anyhow!("use query: {e}"))?;
        let inh_query = Query::new(&language, INHERITANCE_QUERIES)
            .map_err(|e| anyhow::anyhow!("inheritance query: {e}"))?;
        let mod_query = Query::new(&language, MOD_DECL_QUERIES)
            .map_err(|e| anyhow::anyhow!("mod-declaration query: {e}"))?;

        Ok(Self {
            language,
            def_query,
            call_query,
            use_query,
            inh_query,
            mod_query,
        })
    }

    /// File extensions handled by this plugin. Exposed as an associated
    /// function so the trait implementation and external callers (e.g.
    /// CLI argument parsing) share the single source of truth.
    pub fn extensions() -> &'static [&'static str] {
        EXTENSIONS
    }

    /// Parse `content` (UTF-8 bytes) as Rust and produce a [`FileGraph`].
    /// Used internally by [`Self::parse_file`] and by the inline tests;
    /// kept crate-private so the public surface stays the trait method.
    fn parse_to_filegraph(&self, path: &Path, content: &[u8]) -> Result<FileGraph, ParseError> {
        let tree = parse_tree(&self.language, content)?;
        let root = tree.root_node();
        let path_str = path.to_string_lossy().into_owned();

        let mut fg = FileGraph {
            path: path_str.clone(),
            language: Language::Rust,
            symbols: Vec::new(),
            edges: Vec::new(),
        };

        self.extract_definitions(root, content, &path_str, &mut fg);
        self.extract_uses(root, content, &path_str, &mut fg);
        self.extract_mod_decls(root, content, &path_str, &mut fg);
        self.extract_calls(root, content, &path_str, &mut fg);
        self.extract_inheritance(root, content, &path_str, &mut fg);

        Ok(fg)
    }

    /// Run the definition query and produce symbols. Mirrors the C++
    /// `extract_definitions`'s capture-name dispatch: each capture name
    /// from `DEFINITION_QUERIES` maps to a small branch that builds the
    /// right `Symbol`.
    ///
    /// Per-node-type behavior:
    ///
    /// - `function_item` whose nearest definition ancestor (see
    ///   [`find_nearest_def_ancestor`]) is an `impl_item` →
    ///   [`SymbolKind::Method`], parent =
    ///   `impl_node.child_by_field_name("type")` text. For
    ///   `impl Trait for Type { fn m() }` the parent is **`Type`, not
    ///   `Trait`** — the trait relationship lives only in the inheritance
    ///   edge. The trait-impl-method test
    ///   (`trait_impl_method_parent_is_type_not_trait`) is the
    ///   anti-regression for that rule.
    /// - `function_item` whose nearest definition ancestor is a
    ///   `trait_item` (and not an `impl_item` — see "nearest ancestor
    ///   wins" in [`NearestDefAncestor`]) → [`SymbolKind::Method`],
    ///   parent = trait name. This is a deliberate, Rust-trait-scoped
    ///   exception to the cross-language "forward declarations
    ///   excluded" invariant. No new [`SymbolKind`] variant; trait
    ///   identity rides entirely on the `parent` field of the symbol
    ///   (supertrait `Inherits` edges are a separate future addition).
    /// - `function_signature_item` (`fn f(&self);` no-body
    ///   declarations) inside a `trait_item` → same as a trait default
    ///   method: [`SymbolKind::Method`], parent = trait name. A bare
    ///   `function_signature_item` OUTSIDE any trait (theoretically
    ///   possible in some grammar constructs, rare in real Rust) is
    ///   silently dropped — the dispatch's gating on the trait-ancestor
    ///   branch is the load-bearing exclusion mechanism.
    /// - `function_item` at module level → [`SymbolKind::Function`], no
    ///   parent.
    /// - `struct_item` → [`SymbolKind::Struct`].
    /// - `enum_item` → [`SymbolKind::Enum`].
    /// - `trait_item` → [`SymbolKind::Trait`].
    /// - `type_item` → [`SymbolKind::Typedef`].
    /// - `mod_item` is **not** emitted as a `Symbol` itself — modules act
    ///   as namespace anchors only. `resolve_mod_namespace` walks the
    ///   ancestor chain to populate `Symbol.namespace` (`a::b::c`) on the
    ///   symbols *inside* a `mod a { mod b { mod c { ... } } }` chain.
    fn extract_definitions(&self, root: Node<'_>, content: &[u8], path: &str, fg: &mut FileGraph) {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.def_query, root, content);
        let cap_names = self.def_query.capture_names();
        let content_str = std::str::from_utf8(content).unwrap_or("");

        while let Some(m) = matches.next() {
            for capture in m.captures {
                let cap_node = capture.node;
                if cap_node.has_error() {
                    continue;
                }
                let cap_name = capture_name_for_index(cap_names, capture.index);
                let text = cap_node.utf8_text(content).unwrap_or("");

                match cap_name {
                    "func.name" => {
                        // The `func.name` capture fires for BOTH
                        // `function_item` (free fns, impl methods, trait
                        // default methods with bodies) and
                        // `function_signature_item` (abstract trait method
                        // declarations, no body). Resolve the enclosing
                        // definition node by trying both, taking whichever
                        // is found first.
                        let Some(def_node) = find_enclosing_kind(cap_node, "function_item")
                            .or_else(|| find_enclosing_kind(cap_node, "function_signature_item"))
                        else {
                            continue;
                        };
                        let is_signature = def_node.kind() == "function_signature_item";

                        // Nearest-ancestor-wins dispatch. The walk halts at
                        // the first impl_item OR trait_item encountered, so
                        // `impl Trait for Type { fn m() { … } }` correctly
                        // picks the impl branch (m's nearest ancestor is the
                        // impl_item that lexically encloses it; the
                        // `Trait` declaration sits elsewhere in the file
                        // and is not an ancestor at all).
                        let nearest = find_nearest_def_ancestor(cap_node);
                        let (kind, parent) = match nearest {
                            Some(NearestDefAncestor::Impl(impl_node)) => {
                                // Trait-impl disambiguation: parent is the
                                // `type` field (the implementing type),
                                // never the `trait` field. For
                                // `impl Trait for Type { fn m() }` parent
                                // = Type. For `impl Type { fn m() }` parent
                                // = Type. For both, the symbol ID becomes
                                // `path:Type::m`.
                                let parent_text = impl_node
                                    .child_by_field_name("type")
                                    .and_then(|n| n.utf8_text(content).ok())
                                    .unwrap_or("")
                                    .to_owned();
                                (SymbolKind::Method, parent_text)
                            }
                            Some(NearestDefAncestor::Trait(trait_node)) => {
                                // Trait default method (with body) OR
                                // abstract trait method signature (no
                                // body) — both classify as Method, with
                                // the trait name as parent. No new
                                // SymbolKind variant; trait identity
                                // rides entirely on the parent field
                                // (supertrait Inherits edges are a
                                // separate future addition).
                                let parent_text = trait_node
                                    .child_by_field_name("name")
                                    .and_then(|n| n.utf8_text(content).ok())
                                    .unwrap_or("")
                                    .to_owned();
                                (SymbolKind::Method, parent_text)
                            }
                            None => {
                                // No enclosing impl or trait. A free
                                // function gets `Function`; a bare
                                // `function_signature_item` outside any
                                // trait (rare — e.g. an `extern "C"`
                                // block) is DROPPED to preserve the
                                // cross-language "forward declarations
                                // excluded" invariant for everything
                                // except trait method declarations.
                                if is_signature {
                                    continue;
                                }
                                (SymbolKind::Function, String::new())
                            }
                        };
                        let ns = resolve_mod_namespace(cap_node, content_str);
                        fg.symbols
                            .push(make_symbol(text, kind, path, def_node, content, ns, parent));
                    }

                    "struct.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "struct_item") else {
                            continue;
                        };
                        let ns = resolve_mod_namespace(cap_node, content_str);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Struct,
                            path,
                            def_node,
                            content,
                            ns,
                            String::new(),
                        ));
                    }

                    "enum.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "enum_item") else {
                            continue;
                        };
                        let ns = resolve_mod_namespace(cap_node, content_str);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Enum,
                            path,
                            def_node,
                            content,
                            ns,
                            String::new(),
                        ));
                    }

                    "trait.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "trait_item") else {
                            continue;
                        };
                        let ns = resolve_mod_namespace(cap_node, content_str);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Trait,
                            path,
                            def_node,
                            content,
                            ns,
                            String::new(),
                        ));
                    }

                    "type.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "type_item") else {
                            continue;
                        };
                        let ns = resolve_mod_namespace(cap_node, content_str);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Typedef,
                            path,
                            def_node,
                            content,
                            ns,
                            String::new(),
                        ));
                    }

                    // mod_item captures are intentionally consumed without
                    // emitting a Symbol — modules are namespace anchors,
                    // not symbols. `resolve_mod_namespace` walks the
                    // ancestor chain on the symbols defined *inside* a
                    // mod block to populate `Symbol.namespace`.
                    "mod.name" => {}

                    _ => {}
                }
            }
        }
    }

    /// Run the use/extern-crate query and produce `Includes` edges. Mirrors
    /// the C++ plugin's `extract_includes` shape: the edge `from` is the
    /// source file path (not a symbol ID) and the `to` is the dotted
    /// import path; the `Graph` engine routes `Includes` edges into a
    /// per-file map keyed by `from` (see `Graph::merge_file_graph`).
    ///
    /// Per-capture behavior:
    ///
    /// - `use.tree` — the `argument` field of a `use_declaration`. Handed
    ///   to [`split_use_path`] which recursively expands grouped
    ///   (`use_list`/`scoped_use_list`), wildcard (`use_wildcard`),
    ///   aliased (`use_as_clause`), and `self`-in-list forms. Each
    ///   returned path produces one edge; the line number is taken from
    ///   the `use_declaration` start position so all edges from a single
    ///   `use` statement share the same line.
    /// - `extern.name` — the `name` field of an
    ///   `extern_crate_declaration` (i.e. `extern crate alloc;` →
    ///   `"alloc"`). The `as bar` alias is dropped, mirroring the
    ///   `use foo as bar` rule. The line number comes from the
    ///   `extern_crate_declaration` itself.
    fn extract_uses(&self, root: Node<'_>, content: &[u8], path: &str, fg: &mut FileGraph) {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.use_query, root, content);
        let cap_names = self.use_query.capture_names();
        let content_str = std::str::from_utf8(content).unwrap_or("");

        while let Some(m) = matches.next() {
            for capture in m.captures {
                let cap_node = capture.node;
                if cap_node.has_error() {
                    continue;
                }
                let cap_name = capture_name_for_index(cap_names, capture.index);

                match cap_name {
                    "use.tree" => {
                        // Anchor the line at the enclosing `use_declaration`
                        // so all paths from one statement share a line.
                        let line_node =
                            find_enclosing_kind(cap_node, "use_declaration").unwrap_or(cap_node);
                        let line = line_node.start_position().row as u32 + 1;
                        for to in split_use_path(cap_node, content_str) {
                            fg.edges.push(Edge {
                                from: path.to_owned(),
                                to,
                                kind: EdgeKind::Includes,
                                file: path.to_owned(),
                                line,
                            });
                        }
                    }

                    "extern.name" => {
                        let name = cap_node.utf8_text(content).unwrap_or("").to_owned();
                        if name.is_empty() {
                            continue;
                        }
                        let line_node = find_enclosing_kind(cap_node, "extern_crate_declaration")
                            .unwrap_or(cap_node);
                        let line = line_node.start_position().row as u32 + 1;
                        fg.edges.push(Edge {
                            from: path.to_owned(),
                            to: name,
                            kind: EdgeKind::Includes,
                            file: path.to_owned(),
                            line,
                        });
                    }

                    _ => {}
                }
            }
        }
    }

    /// Run the module-declaration query and produce **provisional**
    /// `Includes` edges for every external `mod foo;` declaration.
    ///
    /// External vs inline discriminator: tree-sitter-rust gives a
    /// `mod_item` a `body` field of kind `declaration_list` when the
    /// source is `mod foo { … }`, and omits the field when the source is
    /// `mod foo;`. Visibility modifiers (`pub`, `pub(crate)`, etc.) are
    /// siblings of the `mod` keyword inside the `mod_item`; they do not
    /// affect the discriminator. So:
    ///
    /// - `mod_item` whose `body` field is **absent** → external
    ///   declaration → emit one `Includes` edge with `from` = the
    ///   declaring file path, `to` = the bare modname token captured by
    ///   `@mod.name`, `line` = the `mod_item`'s start row (1-indexed). The
    ///   bare token is a provisional placeholder; whole-graph resolution
    ///   to the concrete child file (`dir/foo.rs`, `dir/foo/mod.rs`,
    ///   `#[path]` override) lives in a follow-up resolver step and will
    ///   be implemented in or alongside `post_index`. The link to
    ///   `post_index` is intentionally omitted here because that method
    ///   does not yet perform the resolution — its current body rewrites
    ///   symbol namespaces only.
    /// - `mod_item` whose `body` field is **present** → inline declaration
    ///   → emit nothing. The body's contents live in the same file, so a
    ///   self-edge would only add noise to file-coupling/cycle queries.
    ///   Inner items (functions, nested mods, …) still extract through
    ///   their own query branches; the suppression is local to this
    ///   handler's `Includes` emission.
    ///
    /// Resolution to a concrete child file (`dir/foo.rs`,
    /// `dir/foo/mod.rs`, or a `#[path = "x.rs"]` override) is performed
    /// in [`Self::post_index`]: it re-walks each Rust file's AST to
    /// extract the `mod_item` set (modname + line + optional `#[path]`
    /// override + inline-mod-ancestor flag) and matches that set
    /// against this file's provisional edges by `(edge.to, edge.line)`.
    /// Matched edges get rewritten to the absolute path of the resolved
    /// child file (or dropped if the candidate is not in the
    /// `FileIndex`). Unmatched `Includes` edges (i.e. those produced
    /// by `extract_uses` for `use`/`extern crate`) are left untouched
    /// and continue to drop at `resolve_all_edges` time exactly as
    /// today — the Rust `resolve_include` override only ever returns
    /// `Some(_)` for an absolute path that is already in the
    /// `FileIndex`, which covers post-`post_index` mod edges and
    /// nothing else.
    ///
    /// **Inline-nested mod limitation (v1).** A `mod b;` declared
    /// inside an inline `mod a { … }` block emits an edge here with
    /// `to = "b"` (the bare modname capture, with no namespace
    /// prefix). Per Rust's module rules, `mod b;` inside `mod a` would
    /// resolve relative to `a/b.rs` / `a/b/mod.rs` — but the
    /// inline-mod ancestor path is not encoded on the emitted edge,
    /// and `post_index` does not reconstruct it. As a conservative,
    /// no-false-edges fallback, `post_index` **drops** any mod edge
    /// whose declaring `mod_item` has an inline `mod_item` ancestor.
    /// This is the documented v1 scope boundary; resolving
    /// inline-nested mod decls is a follow-up.
    fn extract_mod_decls(&self, root: Node<'_>, content: &[u8], path: &str, fg: &mut FileGraph) {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.mod_query, root, content);
        let cap_names = self.mod_query.capture_names();

        while let Some(m) = matches.next() {
            // Locate the captured `mod_item` (the `@mod.decl` capture) and
            // the `@mod.name` identifier. Either may be absent only if the
            // query stops matching the way the constant declares — defensive
            // skips keep the loop robust to grammar drift without panicking.
            let mut decl_node: Option<Node<'_>> = None;
            let mut modname: Option<String> = None;

            for capture in m.captures {
                let cap_node = capture.node;
                if cap_node.has_error() {
                    continue;
                }
                match capture_name_for_index(cap_names, capture.index) {
                    "mod.decl" => decl_node = Some(cap_node),
                    "mod.name" => {
                        let text = cap_node.utf8_text(content).unwrap_or("");
                        if !text.is_empty() {
                            modname = Some(text.to_owned());
                        }
                    }
                    _ => {}
                }
            }

            let (Some(mod_item), Some(name)) = (decl_node, modname) else {
                continue;
            };

            // External vs inline: `body` field present → inline → suppress.
            if mod_item.child_by_field_name("body").is_some() {
                continue;
            }

            let line = mod_item.start_position().row as u32 + 1;
            fg.edges.push(Edge {
                from: path.to_owned(),
                to: name,
                kind: EdgeKind::Includes,
                file: path.to_owned(),
                line,
            });
        }
    }

    /// Run the call query and produce `Calls` edges. Mirrors the C++
    /// plugin's `extract_calls`: each capture is a callee identifier (or
    /// dotted path), the line is anchored at the enclosing
    /// `call_expression` (or `macro_invocation` for macro forms), and the
    /// `from` field is built by [`enclosing_function_id`] so it matches
    /// the `symbol_id()` of the surrounding function/method.
    ///
    /// Per-capture behavior:
    ///
    /// - `call.name` — bare identifier (direct call `foo()`, method call
    ///   `obj.foo()` via `field_expression > field`, turbofish bare-ident
    ///   `foo::<T>()`, or macro invocation `println!()`). The `to` is the
    ///   identifier text.
    /// - `call.qname` — scoped path (`foo::bar::baz()`, scoped turbofish
    ///   `foo::bar::<T>()`, or scoped macro `foo::bar!()`). The full
    ///   dotted path is preserved as `to` (callers downstream may split
    ///   it; the wire format records the unmodified text).
    ///
    /// Lines come from the enclosing `call_expression` or
    /// `macro_invocation`. For chained calls `a.b().c()` tree-sitter
    /// produces nested `call_expression` nodes, each with its own
    /// `field_expression` capture, so two edges fall out naturally (one
    /// per chain link). Closure bodies are walked transparently — calls
    /// inside a `closure_expression` have the enclosing `function_item`'s
    /// ID as `from`, matching the C++ behavior for lambda bodies.
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
                if cap_name != "call.name" && cap_name != "call.qname" {
                    continue;
                }

                let callee = cap_node.utf8_text(content).unwrap_or("");
                if callee.is_empty() {
                    continue;
                }

                // Anchor the line at the enclosing call/macro form so the
                // reported line tracks the call site, not the inner
                // identifier (which can be on a continuation line for
                // multi-line chains).
                let call_node = find_enclosing_kind(cap_node, "call_expression")
                    .or_else(|| find_enclosing_kind(cap_node, "macro_invocation"))
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

    /// Run the inheritance query and produce `Inherits` edges for trait
    /// impls. Mirrors the C++ plugin's `extract_inheritance` for the
    /// single-base case (Rust's `impl Trait for Type` is always one base
    /// per impl block; multi-trait impls are written as separate blocks).
    ///
    /// The query (`INHERITANCE_QUERIES`) requires both the `type` AND
    /// `trait` fields to be present — inherent impls (no `trait`) do not
    /// match, so no edge is emitted. Generic impls
    /// (`impl<T> Trait for Vec<T>`) and impls with `where` clauses match
    /// the same way; the `type` and `trait` field text is captured
    /// verbatim (`Vec<T>` and `Trait`), with generics included as written.
    ///
    /// Edge shape: `from = type_text, to = trait_text, kind = Inherits,
    /// file = path, line = impl_item.start_position().row + 1`. The
    /// implementing type is the `from` (the "child" in the inheritance
    /// hierarchy); the trait is the `to` (the "parent"). Matches the C++
    /// `derived → base` direction.
    fn extract_inheritance(&self, root: Node<'_>, content: &[u8], path: &str, fg: &mut FileGraph) {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.inh_query, root, content);
        let cap_names = self.inh_query.capture_names();

        while let Some(m) = matches.next() {
            let mut type_text = String::new();
            let mut trait_text = String::new();
            let mut impl_node: Option<Node<'_>> = None;

            for capture in m.captures {
                let cap_node = capture.node;
                if cap_node.has_error() {
                    continue;
                }
                let cap_name = capture_name_for_index(cap_names, capture.index);
                let text = cap_node.utf8_text(content).unwrap_or("").to_owned();

                match cap_name {
                    "impl.type" => type_text = text,
                    "impl.trait" => trait_text = text,
                    "impl.def" => impl_node = Some(cap_node),
                    _ => {}
                }
            }

            // Defensive: the query requires both fields, so both should be
            // populated; skip silently rather than emitting a half-formed
            // edge if either is missing.
            if type_text.is_empty() || trait_text.is_empty() {
                continue;
            }

            let line = impl_node
                .map(|n| n.start_position().row as u32 + 1)
                .unwrap_or(0);
            fg.edges.push(Edge {
                from: type_text,
                to: trait_text,
                kind: EdgeKind::Inherits,
                file: path.to_owned(),
                line,
            });
        }
    }
}

impl LanguagePlugin for RustParser {
    fn id(&self) -> Language {
        Language::Rust
    }

    fn extensions(&self) -> &'static [&'static str] {
        EXTENSIONS
    }

    fn parse_file(&self, path: &Path, content: &[u8]) -> Result<FileGraph, ParseError> {
        self.parse_to_filegraph(path, content)
    }

    // resolve_call intentionally NOT overridden: the default scope-aware
    // heuristic used by the C++ plugin is the right baseline for Rust.

    /// Stateless `Includes` resolver for the Rust plugin.
    ///
    /// By the time [`code_graph_tools::indexer::resolve_all_edges`]
    /// calls into this method, surviving Rust `Includes` edges fall
    /// into exactly two shapes:
    ///
    /// 1. **`mod`-decl edges that [`Self::post_index`] resolved.** Their
    ///    `to` field is the absolute path of a `.rs` file already
    ///    present in the `FileIndex` (per the in-place rewrite inside
    ///    `post_index`). The override returns `Some(PathBuf::from(raw))`
    ///    so the edge survives the surrounding
    ///    `language_for_path(&resolved).is_some()` filter — the resolved
    ///    path is an indexed `.rs` file, which the registry's Rust
    ///    plugin claims, and the call-tools layer keeps the edge.
    /// 2. **`use`/`extern crate` dotted tokens** (`"std::io"`,
    ///    `"ark_core::reactor"`, `"alloc"`, …). These are not paths
    ///    and not in any `FileIndex` bucket — the override returns
    ///    `None`, the surrounding loop drops the edge, identical to
    ///    today's drop-via-basename-miss path.
    ///
    /// The override does **no I/O** and **no caching**. It consults
    /// only `raw` and the passed `file_index`. The hard work — module
    /// path discovery, `#[path]` attribute extraction, sibling /
    /// `mod.rs` / `#[path]` candidate probing — lives in
    /// [`Self::post_index`], which writes its results back into the
    /// `graphs` slice in place. This keeps the per-edge resolve path
    /// minimal and statelessness invariants intact.
    ///
    /// Bare absolute paths that happen to land in the `FileIndex` (an
    /// adversarial `use /abs/path::foo;` is rejected by Rust at compile
    /// time, but a future Rust grammar shift could conceivably surface
    /// one) are accepted — the membership test is the sole gate, not
    /// a `mod`-edge marker. This is intentionally permissive in the
    /// direction that produces no false positives: an absolute path
    /// *in* the index points at a real source file, so any Rust edge
    /// whose `to` is that path is by definition resolved.
    fn resolve_include(&self, raw: &str, file_index: &FileIndex) -> Option<PathBuf> {
        let candidate = Path::new(raw);
        if !candidate.is_absolute() {
            // `use`/`extern crate` tokens (`"std::io"`, `"alloc"`) are
            // dotted module paths — never absolute, never in the
            // `FileIndex`. Drop them here so the surrounding loop
            // matches the today's behavior byte-for-byte.
            return None;
        }
        if file_index.contains_path(candidate) {
            Some(candidate.to_path_buf())
        } else {
            None
        }
    }

    /// Crate-aware whole-graph rewrite. Two passes over the `graphs`
    /// slice:
    ///
    /// 1. **Namespace rewrite.** Builds one [`CrateModuleModel`] over
    ///    the indexed Rust file set + discovered `Cargo.toml`s and
    ///    overwrites each Rust `Symbol.namespace` with
    ///    `crate_name::module::path` composed with any in-file inline
    ///    `mod` nesting already populated by `parse_file`.
    /// 2. **`mod`-decl resolution.** Each Rust `FileGraph`'s
    ///    provisional `mod` `Includes` edges (`to = bare modname`,
    ///    `line = mod_item start row`) are resolved to an absolute
    ///    indexed file path via Rust's module-file rules — sibling
    ///    `dir/foo.rs` → `dir/foo/mod.rs`, with a `#[path = "x.rs"]`
    ///    attribute on the `mod_item` overriding both. Edges whose
    ///    candidates are not in the passed [`FileIndex`] are removed
    ///    here; edges whose declaring `mod_item` is nested inside an
    ///    inline `mod` block are removed too (v1 limitation — see
    ///    "Inline-nested mod limitation" below). The per-file AST walk
    ///    that drives this lives in [`scan_mod_decls`].
    ///
    /// # Namespace composition rule
    ///
    /// `parse_file` populated each symbol's `namespace` field via
    /// [`resolve_mod_namespace`], which collects only inline `mod_item`
    /// ancestors. Call its output `inline`. RCMM contributes the
    /// crate-qualified prefix `rcmm`. The post-rewrite namespace is:
    ///
    /// | `rcmm`        | `inline` | result               |
    /// |---------------|----------|----------------------|
    /// | `Some(p)`     | `""`     | `p`                  |
    /// | `Some(p)`     | non-empty| `format!("{p}::{inline}")` |
    /// | `None`        | any      | `inline` (unchanged) |
    ///
    /// The `None` arm preserves the no-Cargo.toml fallback behavior:
    /// the existing inline-only namespace stays in place, including the
    /// empty string that renders as `<global>` in `get_symbol_summary`.
    ///
    /// # `mod`-edge discriminator
    ///
    /// `extract_uses` and `extract_mod_decls` both emit `Includes`
    /// edges from a Rust file with the same shape (`from = file_path`,
    /// `file = file_path`, `kind = Includes`); only the `to` and `line`
    /// differ. To tell them apart in this hook, we re-walk each Rust
    /// source file's AST and collect every external `mod_item`'s
    /// `(modname, line, optional #[path] override, is_inline_nested)`
    /// tuple. An edge is a `mod`-decl edge iff its `(to, line)` pair
    /// matches one of those tuples. Every other `Includes` edge is a
    /// `use` or `extern crate` token and survives this pass untouched
    /// — it will drop later via the `resolve_include` override above
    /// (the dotted token is not an absolute path, so it is filtered
    /// out at edge-resolution time exactly as today).
    ///
    /// # Inline-nested mod limitation (v1)
    ///
    /// `mod b;` declared inside an inline `mod a { … }` block emits an
    /// edge with `to = "b"` (no namespace prefix on the wire). Per
    /// Rust's module rules, that `b` would resolve relative to the
    /// inline mod's namespace directory (`parent/a/b.rs` or
    /// `parent/a/b/mod.rs`), but the inline-mod ancestor path is not
    /// encoded on the emitted edge, and this pass does not
    /// reconstruct it. The conservative, no-false-edges fallback is
    /// to **drop** any mod edge whose declaring `mod_item` has an
    /// inline `mod_item` ancestor. The
    /// `mod_inside_inline_mod_remains_unresolved_in_v1` test pins
    /// this behavior. Resolving inline-nested mod decls is a follow-up.
    ///
    /// # State
    ///
    /// Nothing is stored on `&self`. The RCMM, the per-file
    /// mod-decl tables, and any tree-sitter trees built for the
    /// `#[path]` walk are all local to this call.
    fn post_index(&self, graphs: &mut [FileGraph], file_index: &FileIndex) {
        // Pass 1: collect every Rust file's absolute path.
        let rust_paths: Vec<PathBuf> = graphs
            .iter()
            .filter(|fg| fg.language == Language::Rust)
            .map(|fg| PathBuf::from(&fg.path))
            .collect();

        // No Rust files → nothing to do. Skip the manifest walk and
        // model build entirely.
        if rust_paths.is_empty() {
            return;
        }

        // Pass 2: discover every `Cargo.toml` reachable up the ancestor
        // chain of each Rust file. The indexer's discovery layer does not
        // collect `Cargo.toml` (no plugin claims `.toml`), so RCMM cannot
        // rely on them being in the FileGraph slice — we walk to disk
        // ourselves. Dedupe via a HashSet to avoid stat-ing the same
        // manifest once per `.rs` file in a deep crate.
        let mut manifest_paths: HashSet<PathBuf> = HashSet::new();
        for rs in &rust_paths {
            let mut ancestor: Option<&Path> = rs.parent();
            while let Some(dir) = ancestor {
                let candidate = dir.join("Cargo.toml");
                if candidate.is_file() {
                    manifest_paths.insert(candidate);
                }
                ancestor = dir.parent();
            }
        }

        // Pass 3: build the RCMM. `CrateModuleModel::build` expects a
        // single iterator carrying both manifests and `.rs` files; it
        // splits them internally. The production manifest reader wraps
        // `std::fs::read_to_string(...).ok()` — the reader-callback seam
        // RCMM exposes for filesystem-free unit testing; unreadable
        // manifests get skipped without panic.
        let combined = rust_paths.iter().cloned().chain(manifest_paths);
        let rcmm = CrateModuleModel::build(combined, |p| std::fs::read_to_string(p).ok());

        // Pass 4: rewrite each Rust FileGraph's symbol namespaces.
        for fg in graphs.iter_mut() {
            if fg.language != Language::Rust {
                continue;
            }
            let file_path = PathBuf::from(&fg.path);
            let rcmm_path = rcmm.module_path_for(&file_path);
            for sym in fg.symbols.iter_mut() {
                // `sym.namespace` is the inline-mod-only path populated by
                // `parse_file::resolve_mod_namespace`. Compose with the
                // RCMM prefix per the table in this method's docstring.
                let inline = std::mem::take(&mut sym.namespace);
                sym.namespace = match (rcmm_path, inline.is_empty()) {
                    (Some(prefix), true) => prefix.to_owned(),
                    (Some(prefix), false) => format!("{prefix}::{inline}"),
                    // No RCMM prefix → preserve today's behavior. If
                    // `inline` is empty, the empty namespace will render
                    // as `<global>` downstream in `get_symbol_summary`.
                    (None, _) => inline,
                };
            }
        }

        // Pass 5: resolve provisional `mod` Includes edges.
        //
        // For each Rust file, build a `(modname, line) -> ModDeclInfo`
        // map by re-walking the file's AST. Then iterate the file's
        // edges in place: an Includes edge whose `(to, line)` pair is
        // in the map is a mod-decl edge — resolve it via the rules in
        // `resolve_mod_to_path` (path-override → sibling → mod.rs);
        // rewrite or drop. Edges absent from the map are use/extern
        // tokens; pass them through untouched (they drop later via
        // `resolve_include`'s `None` arm).
        for fg in graphs.iter_mut() {
            if fg.language != Language::Rust {
                continue;
            }
            // Read the file's source on disk and scan for mod_items.
            // The file is normally still on disk at this point (the
            // analyze and watch paths both call post_index moments
            // after parsing the same bytes); a missing/unreadable file
            // produces an empty mod-decl map, which causes every
            // candidate edge to fail the `(to, line)` lookup and pass
            // through to the resolve_include `None` drop. That fallback
            // is the same shape as the today's drop and is safe.
            //
            // Cost note for future readers: `scan_mod_decls` re-reads
            // the file from disk AND re-runs tree-sitter on it. In
            // `analyze_codebase` this happens once per Rust file at
            // index time. In watch mode (`try_reindex_file`), this
            // loop runs over the FULL graph snapshot on every
            // file-save event — so the cost is O(N) reads + parses
            // per keystroke-triggered reindex (N = total Rust files
            // in the codebase). For typical crates (100–200 files)
            // this is acceptable. If watch-mode latency complaints
            // surface on larger codebases, two future optimization
            // options without changing the production behavior here:
            // (i) cache `scan_mod_decls` results by `(path, mtime)`,
            // OR (ii) extract `#[path]` / inline-nested-flag into
            // `parse_file`'s existing AST walk and side-channel via a
            // Rust-specific field on `FileGraph` so post_index can
            // skip the re-parse entirely. Neither is implemented now.
            let file_path = PathBuf::from(&fg.path);
            let Some(mod_decls) = scan_mod_decls(&file_path, &self.language) else {
                continue;
            };

            let parent = file_path.parent().map(Path::to_path_buf);
            fg.edges.retain_mut(|edge| {
                if edge.kind != EdgeKind::Includes {
                    return true;
                }
                // Only consider edges whose `from` matches the file we
                // just scanned. Defensive: extract_mod_decls always
                // emits edges with `from = file_path`, but a future
                // refactor could break that invariant; treat anything
                // else as "not a mod-decl edge".
                if edge.from != fg.path {
                    return true;
                }
                let Some(info) = mod_decls.get(&(edge.to.clone(), edge.line)) else {
                    return true;
                };
                // We have a mod-decl edge. Inline-nested mod decls are
                // dropped as a v1 limitation; see the docstring on
                // `post_index`.
                if info.is_inline_nested {
                    return false;
                }
                let Some(parent) = parent.as_deref() else {
                    return false;
                };
                match resolve_mod_to_path(parent, &edge.to, info, file_index) {
                    Some(resolved) => {
                        edge.to = resolved.to_string_lossy().into_owned();
                        true
                    }
                    None => false,
                }
            });
        }
    }

    fn close(&self) {}
}

/// One external `mod_item`'s metadata, collected by [`scan_mod_decls`]
/// from a Rust source file's AST.
///
/// Keyed by `(modname, line)` in the per-file table because that is
/// the discriminator [`RustParser::post_index`] uses to match a
/// provisional `Includes` edge to its declaring `mod_item`. The fields
/// here are everything the resolution rules (`#[path]` → sibling →
/// `mod.rs`) and the inline-nested drop need.
#[derive(Debug, Clone)]
struct ModDeclInfo {
    /// Value of `#[path = "..."]` if present on the `mod_item`. The
    /// override takes precedence over both sibling and `mod.rs`
    /// candidates per Rust's module rules. Multiple `#[path]`
    /// attributes on one `mod_item` is a Rust syntax error and not
    /// expected; if it ever occurs, the last one wins (consistent with
    /// the tree-sitter walk order).
    path_override: Option<String>,
    /// `true` iff the declaring `mod_item` has at least one `mod_item`
    /// ancestor (i.e. it's `mod b;` inside an inline `mod a { … }`).
    /// Per the v1 limitation in `post_index`'s docstring, these edges
    /// are dropped rather than misresolved.
    is_inline_nested: bool,
}

/// Parse `file` as Rust and collect every external `mod_item`'s
/// metadata into a `(modname, line) -> ModDeclInfo` map.
///
/// Returns `None` if the file cannot be read or the parser cannot
/// produce a tree. Returns `Some(empty)` if the file is readable but
/// declares no external mods — distinguishing the two would force
/// every call site to special-case unreadable files, which is the
/// drop-everything semantics we already want (no map ⇒ no edges
/// match ⇒ no resolution).
///
/// The returned map keys `(modname, line)` so [`RustParser::post_index`]
/// can match provisional `Includes` edges (whose `to` and `line`
/// fields carry the same values, by construction in
/// [`RustParser::extract_mod_decls`]).
fn scan_mod_decls(
    file: &Path,
    language: &TsLanguage,
) -> Option<HashMap<(String, u32), ModDeclInfo>> {
    let content = std::fs::read(file).ok()?;
    let tree = parse_tree(language, &content).ok()?;
    let mut out: HashMap<(String, u32), ModDeclInfo> = HashMap::new();
    collect_mod_decls(tree.root_node(), &content, false, &mut out);
    Some(out)
}

/// Recursively walk a Rust AST collecting every external `mod_item`'s
/// `(modname, line) -> ModDeclInfo` entry.
///
/// `inside_inline_mod` flips to `true` when descending into a
/// `declaration_list` child of a `mod_item` (i.e. an inline mod
/// body), and stays `true` for the duration of that subtree. Any
/// external `mod_item` discovered while the flag is `true` is
/// recorded with `is_inline_nested = true`.
fn collect_mod_decls(
    node: Node<'_>,
    content: &[u8],
    inside_inline_mod: bool,
    out: &mut HashMap<(String, u32), ModDeclInfo>,
) {
    if node.kind() == "mod_item" {
        let modname = node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(content).ok())
            .map(|s| s.to_owned());
        let has_body = node.child_by_field_name("body").is_some();
        if let Some(modname) = modname {
            if !has_body {
                // External `mod foo;` declaration. The provisional
                // edge that `extract_mod_decls` emitted carries
                // `line = mod_item.start_position().row + 1`; match
                // that here.
                let line = node.start_position().row as u32 + 1;
                let path_override = extract_path_attribute(node, content);
                out.insert(
                    (modname, line),
                    ModDeclInfo {
                        path_override,
                        is_inline_nested: inside_inline_mod,
                    },
                );
            }
            // For inline mods (`mod foo { … }`), descend into the
            // body with `inside_inline_mod = true` so nested external
            // mods are flagged.
            if has_body {
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.named_children(&mut cursor) {
                        collect_mod_decls(child, content, true, out);
                    }
                    return; // already descended
                }
            }
        } else {
            // Nameless `mod_item` is only reachable from tree-sitter
            // ERROR recovery on mid-edit source (e.g. `mod ` with no
            // identifier yet). Stop here rather than falling through
            // to the generic named-children walk below: that walk
            // would descend with the OUTER caller's `inside_inline_mod`
            // flag, which is the wrong scope for a body that lives
            // inside a (broken) `mod_item`. A nameless mod_item
            // contributes nothing to the mod-decl table anyway —
            // there's no name to key on — so dropping the subtree is
            // safe.
            return;
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_mod_decls(child, content, inside_inline_mod, out);
    }
}

/// Extract the `#[path = "x.rs"]` attribute value from the
/// `mod_item`'s preceding `attribute_item` siblings, if any.
///
/// AST shape: an `attribute_item` is a sibling of `mod_item` (NOT a
/// child); chained attributes (`#[cfg(test)] #[path = "x.rs"]
/// mod foo;`) produce a chain of `attribute_item` siblings. We walk
/// `prev_named_sibling()` for as long as the previous node is an
/// `attribute_item`; the first one whose inner `attribute > identifier`
/// is `path` wins. Returns the `string_content` text (the bare
/// `x.rs`, without quotes).
fn extract_path_attribute(mod_item: Node<'_>, content: &[u8]) -> Option<String> {
    let mut sibling = mod_item.prev_named_sibling();
    while let Some(node) = sibling {
        if node.kind() != "attribute_item" {
            return None;
        }
        // attribute_item -> attribute -> identifier "path", string_literal -> string_content
        if let Some(attr) = node.named_child(0) {
            if attr.kind() == "attribute" {
                // First named child is the attribute's path identifier
                // (or a scoped_identifier for qualified attribute names).
                if let Some(ident) = attr.named_child(0) {
                    if ident.kind() == "identifier" && ident.utf8_text(content).ok() == Some("path")
                    {
                        // Find the `string_literal` -> `string_content`
                        // capturing the override value.
                        let mut cursor = attr.walk();
                        for child in attr.named_children(&mut cursor) {
                            if child.kind() == "string_literal" {
                                let mut sc = child.walk();
                                for grand in child.named_children(&mut sc) {
                                    if grand.kind() == "string_content" {
                                        return grand.utf8_text(content).ok().map(str::to_owned);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        sibling = node.prev_named_sibling();
    }
    None
}

/// Resolve one `mod` declaration to an indexed file path.
///
/// Candidate order (Rust's documented module-file rules):
///
/// 1. **`#[path = "x.rs"]` override**: `parent.join(x_rs)`. Wins
///    unconditionally when present — Rust does not fall back to
///    sibling / `mod.rs` if the `#[path]` target is missing.
/// 2. **Sibling**: `parent.join("{modname}.rs")`.
/// 3. **`mod.rs` subdir**: `parent.join(modname).join("mod.rs")`.
///
/// For each candidate, the function probes the passed [`FileIndex`].
/// **First match wins**; non-indexed candidates are skipped silently
/// (an unindexed sibling shouldn't suppress a `mod.rs` that's in the
/// index, since that means the indexer discovered the latter and
/// the former is — from the graph's perspective — invisible).
///
/// Returns the resolved absolute path on a hit, `None` if no
/// candidate is in the index. The `None` result causes
/// [`RustParser::post_index`] to drop the edge rather than leave a
/// bare-modname `to` for `resolve_all_edges` to misresolve.
fn resolve_mod_to_path(
    parent: &Path,
    modname: &str,
    info: &ModDeclInfo,
    file_index: &FileIndex,
) -> Option<PathBuf> {
    if let Some(override_str) = &info.path_override {
        let candidate = parent.join(override_str);
        if file_index.contains_path(&candidate) {
            return Some(candidate);
        }
        // `#[path]` is authoritative when present: if the override
        // target is not in the FileIndex, do NOT fall through to
        // sibling/`mod.rs` candidates. Rust itself would refuse to
        // compile in this case (the file would just be missing); our
        // index-side equivalent is to drop the edge entirely.
        return None;
    }
    let sibling = parent.join(format!("{modname}.rs"));
    if file_index.contains_path(&sibling) {
        return Some(sibling);
    }
    let mod_rs = parent.join(modname).join("mod.rs");
    if file_index.contains_path(&mod_rs) {
        return Some(mod_rs);
    }
    None
}

/// Build a tree-sitter [`TsTree`] for `content` against the Rust grammar.
/// The caller-supplied [`TsLanguage`] is borrowed; the returned tree owns
/// its AST. Returns [`ParseError::Parse`] if `set_language` fails or if
/// tree-sitter declines to produce a tree (e.g. on cancellation).
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
/// indices, matching the C++ plugin's silent fallback.
fn capture_name_for_index<'a>(cap_names: &[&'a str], index: u32) -> &'a str {
    cap_names.get(index as usize).copied().unwrap_or("")
}

/// Build a [`Symbol`] from a definition node. Centralizes the row/column/
/// signature math so each branch in `extract_definitions` stays small.
/// Mirrors the C++ plugin's `make_symbol`.
fn make_symbol(
    name: &str,
    kind: SymbolKind,
    path: &str,
    def_node: Node<'_>,
    content: &[u8],
    namespace: String,
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
        namespace,
        parent,
        language: Language::Rust,
    }
}

#[cfg(test)]
mod tests {
    //! Structural smoke tests plus definition-extraction coverage, and
    //! behavioral coverage for uses, calls, and inheritance alongside the
    //! corresponding `extract_*` loops.
    use super::*;
    use code_graph_core::symbol_id;

    // ----------------------------------------------------------------
    // Structural smoke tests
    // ----------------------------------------------------------------

    #[test]
    fn new_compiles_all_queries() {
        // Every query string must parse against the pinned
        // tree-sitter-rust. Failure here means a query needs updating.
        let p = RustParser::new().expect("RustParser::new must succeed");
        let _ = (
            &p.language,
            &p.def_query,
            &p.call_query,
            &p.use_query,
            &p.inh_query,
            &p.mod_query,
        );
    }

    #[test]
    fn extensions_match_expected_list() {
        assert_eq!(RustParser::extensions(), &[".rs"]);
        let p = RustParser::new().unwrap();
        assert_eq!(LanguagePlugin::extensions(&p), RustParser::extensions());
    }

    #[test]
    fn id_is_rust() {
        let p = RustParser::new().unwrap();
        assert_eq!(p.id(), Language::Rust);
    }

    #[test]
    fn rust_parser_is_object_safe_via_box_dyn() {
        let p: Box<dyn LanguagePlugin> = Box::new(RustParser::new().unwrap());
        assert_eq!(p.id(), Language::Rust);
    }

    // ----------------------------------------------------------------
    // Definition extraction
    // ----------------------------------------------------------------

    /// Parse `src` against `RustParser` and return the resulting
    /// FileGraph at a synthetic absolute path. Used by every
    /// definition-extraction behavioral test below.
    fn parse(src: &str) -> FileGraph {
        let p = RustParser::new().unwrap();
        p.parse_file(Path::new("/tmp/test.rs"), src.as_bytes())
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
        // `parse_file` populates symbols; this test pins the
        // path/language assertion that belongs at this layer.
        let fg = parse("fn foo() {}");
        assert_eq!(fg.path, "/tmp/test.rs");
        assert_eq!(fg.language, Language::Rust);
    }

    #[test]
    fn free_function_produces_function_kind_no_parent() {
        let fg = parse("fn foo() {}");
        let s = sym(&fg, "foo");
        assert_eq!(s.kind, SymbolKind::Function);
        assert!(s.parent.is_empty(), "free fn must have empty parent");
        assert!(s.namespace.is_empty(), "top-level fn has no namespace");
        assert_eq!(s.language, Language::Rust);
    }

    #[test]
    fn inherent_impl_method_has_parent_equals_type() {
        let fg = parse("struct Foo; impl Foo { fn bar() {} }");
        let bar = sym(&fg, "bar");
        assert_eq!(bar.kind, SymbolKind::Method);
        assert_eq!(bar.parent, "Foo");
        assert_eq!(symbol_id(bar), "/tmp/test.rs:Foo::bar");
    }

    /// CRITICAL anti-regression: for `impl Trait for Type { fn m() }` the
    /// method's parent MUST be `Type`, never `Trait`. The trait
    /// relationship lives only in the inheritance edge.
    #[test]
    fn trait_impl_method_parent_is_type_not_trait() {
        let src = "trait Trait {} struct Foo; impl Trait for Foo { fn bar() {} }";
        let fg = parse(src);
        let bar = sym(&fg, "bar");
        assert_eq!(bar.kind, SymbolKind::Method);
        assert_eq!(
            bar.parent, "Foo",
            "trait-impl method parent must be the implementing type, not the trait"
        );
        assert_ne!(bar.parent, "Trait", "must NOT use trait name as parent");
        assert_eq!(symbol_id(bar), "/tmp/test.rs:Foo::bar");
    }

    #[test]
    fn struct_item_produces_struct_kind() {
        let fg = parse("struct Foo { x: i32 }");
        let s = sym(&fg, "Foo");
        assert_eq!(s.kind, SymbolKind::Struct);
        assert!(s.parent.is_empty());
    }

    #[test]
    fn enum_item_produces_enum_kind() {
        let fg = parse("enum Color { Red, Green, Blue }");
        let s = sym(&fg, "Color");
        assert_eq!(s.kind, SymbolKind::Enum);
    }

    /// CRITICAL: the `Speak` trait is still extracted as
    /// `SymbolKind::Trait`, AND the abstract method signature
    /// `fn hello(&self);` is also extracted as a Method symbol with
    /// `parent = "Speak"`. Rust traits are a deliberate, scoped
    /// exception to the cross-language "forward declarations excluded"
    /// invariant: trait method declarations (with OR without bodies)
    /// always produce Method symbols whose parent is the enclosing
    /// trait. This test is the anti-regression for that exception.
    #[test]
    fn abstract_trait_method_signature_produces_method_with_trait_parent() {
        let fg = parse("trait Speak { fn hello(&self); }");

        // The trait itself still extracts as Trait, namespace empty
        // (top-level), no parent.
        let speak = sym(&fg, "Speak");
        assert_eq!(speak.kind, SymbolKind::Trait);
        assert!(speak.parent.is_empty(), "trait has no parent");

        // The abstract method signature now produces a Method symbol
        // with the trait name as parent.
        let hello = sym(&fg, "hello");
        assert_eq!(
            hello.kind,
            SymbolKind::Method,
            "abstract trait method signature must classify as Method"
        );
        assert_eq!(
            hello.parent, "Speak",
            "abstract trait method signature parent must be the trait name"
        );
        // The symbol records the source line (Speak is on line 1; the
        // `fn hello(&self);` declaration is on the same line in this
        // fixture, so its line is also 1).
        assert!(
            hello.line >= 1,
            "abstract trait method line must be 1-indexed and populated, got: {}",
            hello.line
        );
        // Exactly two symbols: the trait + its one method.
        assert_eq!(
            fg.symbols.len(),
            2,
            "expected exactly 2 symbols (trait + abstract method), got: {:?}",
            fg.symbols
                .iter()
                .map(|s| (s.name.as_str(), s.kind, s.parent.as_str()))
                .collect::<Vec<_>>()
        );
    }

    /// Trait DEFAULT methods (with a body) classify as `Method` with
    /// `parent = trait_name`, identical to the abstract-signature
    /// case. They differ only in their AST node kind:
    /// `function_item` (has body) vs `function_signature_item`
    /// (no body) — but the dispatch treats both identically once
    /// `find_nearest_def_ancestor` finds a `trait_item` ancestor.
    #[test]
    fn trait_default_method_with_body_produces_method_with_trait_parent() {
        let fg = parse("trait Greet { fn greet(&self) -> String { String::from(\"hello\") } }");

        let greet_trait = sym(&fg, "Greet");
        assert_eq!(greet_trait.kind, SymbolKind::Trait);

        let greet_method = sym(&fg, "greet");
        assert_eq!(
            greet_method.kind,
            SymbolKind::Method,
            "trait default method must classify as Method"
        );
        assert_eq!(
            greet_method.parent, "Greet",
            "trait default method parent must be the trait name"
        );
        assert!(
            greet_method.line >= 1,
            "trait default method line must be 1-indexed and populated, got: {}",
            greet_method.line
        );
    }

    /// `top_level_only=true` semantics: trait methods (Method kind,
    /// non-empty parent) are filterable like impl methods. This is
    /// verified at the parser layer by asserting every newly-classified
    /// trait method has a non-empty `parent` — the handler filter
    /// (`crates/code-graph-tools/src/handlers/symbols.rs`) drops any
    /// symbol with a non-empty parent, so this is the necessary and
    /// sufficient condition for `top_level_only=true` to filter them
    /// out.
    #[test]
    fn trait_methods_have_non_empty_parent_so_top_level_only_filters_them() {
        let fg =
            parse("trait Speak {\n  fn abstract_method(&self);\n  fn default_method(&self) {}\n}");

        let abstract_method = sym(&fg, "abstract_method");
        let default_method = sym(&fg, "default_method");
        for m in [abstract_method, default_method] {
            assert_eq!(m.kind, SymbolKind::Method);
            assert!(
                !m.parent.is_empty(),
                "trait method must have non-empty parent so top_level_only filters it; got: {m:?}"
            );
            assert_eq!(m.parent, "Speak");
        }
    }

    /// `impl Trait for Type { fn m() { … } }` — the trait declaration
    /// is in scope but the function's nearest definition ancestor is
    /// the `impl_item` that lexically encloses it. The impl rule wins:
    /// parent = `Type`, NOT `Trait`. This is the anti-regression for
    /// the "nearest ancestor wins" dispatch in
    /// [`find_nearest_def_ancestor`] — without it, a trait-impl
    /// method's symbol parent would silently flip to the trait name
    /// whenever the trait declaration happened to be discoverable
    /// (which it always is in the same file).
    #[test]
    fn trait_impl_method_parent_is_type_when_both_ancestors_visible() {
        // Two top-level items in one file: a trait declaration AND an
        // impl of that trait for a struct. The function inside the
        // impl has both items reachable via the file's named-children
        // list, but only one — the `impl_item` — is an ANCESTOR via
        // the parent chain. The trait declaration is a sibling, not
        // an ancestor, so the dispatch correctly picks Impl.
        let src = "trait Trait { fn declared(&self); }\n\
                   struct Foo;\n\
                   impl Trait for Foo {\n  fn declared(&self) {}\n}";
        let fg = parse(src);
        // The function inside `impl Trait for Foo` carries parent=Foo.
        // The trait's abstract signature also produces a symbol named
        // `declared`, but with parent=Trait. Verify both:
        let impl_method = fg
            .symbols
            .iter()
            .find(|s| s.name == "declared" && s.parent == "Foo")
            .expect("impl method `declared` with parent=Foo must exist");
        assert_eq!(impl_method.kind, SymbolKind::Method);
        // And separately, the abstract signature inside the trait
        // produces its own symbol — parent=Trait.
        let trait_sig = fg
            .symbols
            .iter()
            .find(|s| s.name == "declared" && s.parent == "Trait")
            .expect("abstract trait signature `declared` with parent=Trait must exist");
        assert_eq!(trait_sig.kind, SymbolKind::Method);
    }

    /// A bare `function_signature_item` outside any `trait_item`
    /// (e.g. inside an `extern "C"` block) MUST NOT produce a symbol.
    /// This preserves the cross-language "forward declarations
    /// excluded" invariant for everything except trait method
    /// declarations.
    #[test]
    fn bare_function_signature_outside_trait_produces_no_symbol() {
        // `extern "C"` blocks contain `function_signature_item`s
        // representing FFI declarations — bodies declared elsewhere.
        // These must NOT classify as Method/parent=anything; the
        // dispatch's None-arm-with-is_signature gating drops them.
        let fg = parse("extern \"C\" {\n    fn bare_extern(x: i32) -> i32;\n}");
        assert!(
            !fg.symbols.iter().any(|s| s.name == "bare_extern"),
            "bare `function_signature_item` outside any trait must NOT produce a symbol; got: {:?}",
            fg.symbols
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
        );
    }

    /// Composition with `post_index`: a trait-method symbol's
    /// namespace is rewritten to the crate-qualified path during
    /// `post_index`, exactly like any other symbol. Trait-method
    /// classification (parse-time) and the namespace rewrite
    /// (post-index) compose orthogonally because `post_index` operates
    /// on every Rust symbol's `namespace` field regardless of kind or
    /// parent.
    #[test]
    fn post_index_rewrites_abstract_trait_method_namespace_to_crate_path() {
        let dir = TempDir::new().expect("TempDir");
        write_cargo_toml(dir.path(), "ark_core");
        let traits = write_rs(
            dir.path(),
            "src/traits.rs",
            "pub trait Speak {\n    fn hello(&self);\n}\n",
        );

        let graphs = run_post_index(std::slice::from_ref(&traits));
        let fg = fg_for(&graphs, &traits);

        // The abstract method symbol carries the rewritten namespace,
        // proving abstract-signature symbol emission flows through
        // `post_index`'s crate-qualified namespace assignment.
        let hello = sym(fg, "hello");
        assert_eq!(hello.kind, SymbolKind::Method);
        assert_eq!(hello.parent, "Speak");
        assert_eq!(
            hello.namespace, "ark_core::traits",
            "abstract trait method must inherit the crate-qualified namespace"
        );
    }

    #[test]
    fn type_item_produces_typedef_kind() {
        let fg = parse("type MyInt = i32;");
        let s = sym(&fg, "MyInt");
        assert_eq!(s.kind, SymbolKind::Typedef);
    }

    #[test]
    fn generic_function_with_type_bound() {
        // `fn foo<T: Display>(x: T) {}` — must parse without crashing
        // and the signature must be truncated at `{`.
        let fg = parse("use std::fmt::Display; fn foo<T: Display>(x: T) {}");
        let s = sym(&fg, "foo");
        assert_eq!(s.kind, SymbolKind::Function);
        assert!(
            !s.signature.contains('{'),
            "signature must be truncated at the body opener, got: {:?}",
            s.signature
        );
        assert!(
            s.signature.contains("foo<T: Display>"),
            "type bound must survive truncation, got: {:?}",
            s.signature
        );
    }

    #[test]
    fn generic_function_with_where_clause() {
        let fg = parse("use std::fmt::Display; fn foo<T>(x: T) where T: Display { let _ = x; }");
        let s = sym(&fg, "foo");
        assert_eq!(s.kind, SymbolKind::Function);
        assert!(
            s.signature.contains("where T: Display"),
            "where clause must survive truncation, got: {:?}",
            s.signature
        );
        assert!(!s.signature.contains('{'));
    }

    #[test]
    fn lifetime_parameters() {
        let fg = parse("fn longest<'a>(x: &'a str) -> &'a str { x }");
        let s = sym(&fg, "longest");
        assert_eq!(s.kind, SymbolKind::Function);
        assert!(
            s.signature.contains("longest<'a>"),
            "lifetime param must survive, got: {:?}",
            s.signature
        );
        assert!(s.signature.contains("-> &'a str"));
    }

    #[test]
    fn async_const_unsafe_fn() {
        // All three modifier forms produce Function (or Method inside
        // an impl). Body content irrelevant — we only check kind.
        let fg = parse("async fn a_fn() {} const fn c_fn() -> i32 { 0 } unsafe fn u_fn() {}");
        for name in ["a_fn", "c_fn", "u_fn"] {
            let s = sym(&fg, name);
            assert_eq!(
                s.kind,
                SymbolKind::Function,
                "async/const/unsafe fn must extract as Function, got {:?} for {name}",
                s.kind
            );
        }
    }

    #[test]
    fn async_fn_inside_impl_is_method() {
        // Same modifier handling, but inside an impl → Method.
        let fg = parse("struct Foo; impl Foo { async fn run(&self) {} }");
        let s = sym(&fg, "run");
        assert_eq!(s.kind, SymbolKind::Method);
        assert_eq!(s.parent, "Foo");
    }

    /// Pinned to the **no-Cargo.toml fallback**. This test goes through
    /// `parse()` (which only calls `parse_file`, not `post_index`), so the
    /// observed namespace is the inline-mod-only path
    /// [`resolve_mod_namespace`] populates — exactly today's behavior.
    /// The composed `crate::a::b::c` path that emerges when `post_index`
    /// runs over a real Cargo crate is covered by
    /// `post_index_composes_inline_mods_onto_crate_prefix` below; this
    /// test is the fallback's anti-regression and stays unchanged.
    #[test]
    fn nested_mods_produce_namespace_a_b_c() {
        let fg = parse("mod a { mod b { mod c { fn x() {} } } }");
        let x = sym(&fg, "x");
        assert_eq!(
            x.namespace, "a::b::c",
            "nested mods must produce namespace joined with `::`"
        );
        // mod_items themselves do NOT produce Symbols (they're namespace
        // anchors). The only symbol in this fixture is `x`.
        assert!(
            !fg.symbols.iter().any(|s| s.name == "a"),
            "mod_item must not emit a Symbol named after the module"
        );
        assert!(!fg.symbols.iter().any(|s| s.name == "b"));
        assert!(!fg.symbols.iter().any(|s| s.name == "c"));
        assert_eq!(
            fg.symbols.len(),
            1,
            "exactly one symbol expected (the inner fn x)"
        );
    }

    /// CRITICAL anti-regression: `macro_rules! foo { ... }` parses as a
    /// `macro_definition` node (tree-sitter-rust 0.24 names the wrapping
    /// node `macro_definition`, not `macro_rules_definition`). The
    /// DEFINITION_QUERIES intentionally do not match it, so this fixture
    /// must yield zero symbols. If the queries ever drift to capture
    /// macro definitions, this test catches it.
    #[test]
    fn macro_rules_definition_produces_zero_symbols() {
        let fg = parse("macro_rules! foo { () => {} }");
        assert!(
            fg.symbols.is_empty(),
            "macro_rules! definitions must produce zero symbols; got: {:?}",
            fg.symbols.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn signature_is_truncated_at_body_opener() {
        // Belt-and-suspenders: the signature for `fn foo() { ... }`
        // must drop the body. Verifies truncate_signature is wired.
        let fg = parse("fn foo() { let _ = 42; let _ = \"abc\"; }");
        let s = sym(&fg, "foo");
        assert_eq!(s.signature, "fn foo()");
    }

    // ----------------------------------------------------------------
    // Use-tree expansion + extern crate edges
    // ----------------------------------------------------------------

    /// Collect just the `Includes`-kind edges from a `FileGraph`. This
    /// filter isolates use-tree edges so the assertions stay robust when a
    /// fixture also produces `Calls`/`Inherits` edges.
    fn includes(fg: &FileGraph) -> Vec<&Edge> {
        fg.edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Includes)
            .collect()
    }

    /// Just the `to` fields of every include edge, in emission order.
    fn include_targets(fg: &FileGraph) -> Vec<&str> {
        includes(fg).into_iter().map(|e| e.to.as_str()).collect()
    }

    /// Verify that every include edge points at the synthetic test path,
    /// is `Kind=Includes`, and has a non-zero line. Used by every Phase
    /// 5.3 test below to keep the per-edge invariants out of the body.
    fn assert_include_edge_invariants(fg: &FileGraph) {
        for e in includes(fg) {
            assert_eq!(e.kind, EdgeKind::Includes, "edge kind must be Includes");
            assert_eq!(
                e.from, "/tmp/test.rs",
                "include edge `from` must be the source file path"
            );
            assert_eq!(
                e.file, "/tmp/test.rs",
                "include edge `file` must be the source file path"
            );
            assert!(
                e.line >= 1,
                "include edge line must be 1-indexed and populated, got: {}",
                e.line
            );
        }
    }

    #[test]
    fn use_simple() {
        let fg = parse("use foo;");
        assert_eq!(include_targets(&fg), vec!["foo"]);
        assert_include_edge_invariants(&fg);
    }

    #[test]
    fn use_scoped() {
        let fg = parse("use foo::bar;");
        assert_eq!(include_targets(&fg), vec!["foo::bar"]);
        assert_include_edge_invariants(&fg);
    }

    #[test]
    fn use_list() {
        let fg = parse("use foo::{a, b};");
        assert_eq!(include_targets(&fg), vec!["foo::a", "foo::b"]);
        assert_include_edge_invariants(&fg);
    }

    #[test]
    fn use_nested_list() {
        let fg = parse("use foo::{a, b::c};");
        assert_eq!(include_targets(&fg), vec!["foo::a", "foo::b::c"]);
        assert_include_edge_invariants(&fg);
    }

    #[test]
    fn use_wildcard() {
        let fg = parse("use foo::*;");
        assert_eq!(include_targets(&fg), vec!["foo::*"]);
        assert_include_edge_invariants(&fg);
    }

    #[test]
    fn use_as_clause() {
        let fg = parse("use foo as bar;");
        // Alias dropped — the wire format records the path, not the local
        // name, matching the `use std::io as IO` documented behavior.
        assert_eq!(include_targets(&fg), vec!["foo"]);
        assert_include_edge_invariants(&fg);
    }

    #[test]
    fn use_self_in_list() {
        let fg = parse("use std::io::{self, Read};");
        // `self` re-emits the parent scope, so two edges: std::io and
        // std::io::Read.
        assert_eq!(include_targets(&fg), vec!["std::io", "std::io::Read"]);
        assert_include_edge_invariants(&fg);
    }

    #[test]
    fn use_deeply_nested() {
        let fg = parse("use std::{io::{self, Read}, collections::HashMap};");
        assert_eq!(
            include_targets(&fg),
            vec!["std::io", "std::io::Read", "std::collections::HashMap"]
        );
        assert_include_edge_invariants(&fg);
    }

    #[test]
    fn extern_crate_simple() {
        let fg = parse("extern crate alloc;");
        assert_eq!(include_targets(&fg), vec!["alloc"]);
        assert_include_edge_invariants(&fg);
    }

    #[test]
    fn extern_crate_with_alias() {
        // Alias dropped, same rule as `use foo as bar;`.
        let fg = parse("extern crate foo as bar;");
        assert_eq!(include_targets(&fg), vec!["foo"]);
        assert_include_edge_invariants(&fg);
    }

    #[test]
    fn use_edge_line_matches_use_declaration() {
        // Verify the line number is anchored at the `use_declaration`
        // (not at the inner identifier) and survives across all paths
        // expanded from a single statement.
        let src = "fn _placeholder() {}\n\nuse foo::{a, b};";
        let fg = parse(src);
        let lines: Vec<u32> = includes(&fg).iter().map(|e| e.line).collect();
        // Both expanded paths share the use_declaration's start line (3).
        assert_eq!(lines, vec![3, 3]);
    }

    // ----------------------------------------------------------------
    // Mod-declaration Includes edges
    // ----------------------------------------------------------------
    //
    // These tests pin the parser-side emission rule for `mod` items: one
    // provisional `Includes` edge per *external* `mod foo;` declaration
    // (bare modname as `to`); zero edges for *inline* `mod foo { … }`.
    // Whole-graph resolution of the bare modname to a concrete sibling
    // file is the post_index follow-up; until then, the default
    // `resolve_include` basename match drops these edges at resolution
    // time. That drop is verified separately in the indexer's
    // `resolve_all_edges_drops_include_to_non_source_target` anti-regression.

    #[test]
    fn mod_external_decl_emits_provisional_includes_edge() {
        // `pub mod foo;` is a file-split mod declaration: the body lives
        // in a sibling file, so the parser emits one provisional
        // Includes edge with the bare modname as `to`.
        let fg = parse("pub mod foo;");
        let ts = include_targets(&fg);
        assert_eq!(
            ts,
            vec!["foo"],
            "external `mod foo;` must produce exactly one Includes edge to bare modname"
        );
        assert_include_edge_invariants(&fg);
        // Pin the single-edge invariants explicitly: `from` and `file`
        // both equal the declaring file's path; `line` is the source row
        // of the `mod` declaration (line 1 for this single-line fixture).
        let e = includes(&fg)[0];
        assert_eq!(e.from, "/tmp/test.rs");
        assert_eq!(e.to, "foo");
        assert_eq!(e.file, "/tmp/test.rs");
        assert_eq!(e.line, 1);
        assert_eq!(e.kind, EdgeKind::Includes);
    }

    #[test]
    fn mod_inline_block_does_not_emit_includes_edge() {
        // `mod foo { fn bar() {} }` is an inline mod block: the body
        // lives in the same file, so emitting a self-edge would only
        // pollute coupling/cycle queries. Suppression happens at
        // emission time (the `body` field discriminator), not at
        // resolution time.
        let fg = parse("mod foo { fn bar() {} }");
        assert!(
            includes(&fg).is_empty(),
            "inline `mod foo {{ ... }}` must produce zero Includes edges, got: {:?}",
            include_targets(&fg)
        );
        // Inner items still parse normally — the suppression is scoped
        // to the mod self-edge, not to the inner symbol set.
        assert!(
            fg.symbols.iter().any(|s| s.name == "bar"),
            "inner `fn bar()` inside an inline mod must still extract as a Symbol"
        );
    }

    #[test]
    fn mod_inline_outer_external_inner_emits_edge_for_inner_only() {
        // Mixed case: outer `mod a` is inline (has a body), inner
        // `mod b;` is external (no body). The outer is suppressed by the
        // `body`-field discriminator; the inner emits one provisional
        // Includes edge with the bare modname `b` as `to`. The bare
        // token deliberately does NOT carry the outer module's
        // namespace prefix — `to` is the raw `@mod.name` capture text,
        // not a path. A future resolver step will need this baseline:
        // resolving `b` relative to `a`'s on-disk directory differs
        // from resolving a top-level `mod b;`, and silently shifting
        // the wire `to` value (to e.g. `a::b` or `a.b`) would lose the
        // emission-vs-resolution boundary that this fixture pins.
        let fg = parse("mod a { mod b; }");
        let ts = include_targets(&fg);
        assert_eq!(
            ts,
            vec!["b"],
            "external inner `mod b;` inside inline outer `mod a {{ ... }}` must \
             emit exactly one Includes edge to bare `b` (no `a::b` prefix), got: {ts:?}"
        );
        assert!(
            !includes(&fg).iter().any(|e| e.to == "a"),
            "inline outer `mod a {{ ... }}` must NOT emit an Includes edge for `a`"
        );
        assert_include_edge_invariants(&fg);
    }

    #[test]
    fn mod_empty_inline_block_does_not_emit_includes_edge() {
        // Literal empty body `mod foo {}`: tree-sitter-rust still emits
        // a `declaration_list` node for the empty `{}`, so the
        // `child_by_field_name("body")` lookup returns `Some(empty)` and
        // the discriminator correctly treats this as inline. Pinning
        // this defends against a future grammar drift where `{}` might
        // stop producing the body field (which would silently flip the
        // emission and create a phantom self-edge).
        let fg = parse("mod foo {}");
        assert!(
            includes(&fg).is_empty(),
            "empty inline `mod foo {{}}` must produce zero Includes edges, got: {:?}",
            include_targets(&fg)
        );
    }

    #[test]
    fn mod_nested_inline_does_not_emit_edges() {
        // Two levels of inline mod nesting: both blocks have bodies, so
        // both are suppressed. The inner `fn c()` still extracts and
        // carries the `a::b` inline-mod namespace.
        let fg = parse("mod a { mod b { fn c() {} } }");
        assert!(
            includes(&fg).is_empty(),
            "nested inline mods must produce zero Includes edges, got: {:?}",
            include_targets(&fg)
        );
        // Sanity: the inner symbol still resolves with the nested
        // inline-mod namespace.
        let c = sym(&fg, "c");
        assert_eq!(c.namespace, "a::b");
    }

    #[test]
    fn mod_external_with_pub_visibility_emits_edge() {
        // Visibility modifiers (`pub`, `pub(crate)`) are siblings of the
        // `mod` keyword inside the `mod_item`; they do not affect
        // emission. Three forms — bare, `pub`, `pub(crate)` — each
        // produce one Includes edge to their bare modname.
        let fg = parse("mod a;\npub mod b;\npub(crate) mod c;\n");
        let ts = include_targets(&fg);
        assert_eq!(
            ts,
            vec!["a", "b", "c"],
            "visibility modifier must not affect mod-decl Includes emission, got: {ts:?}"
        );
        assert_include_edge_invariants(&fg);
    }

    #[test]
    fn mod_external_line_matches_source_line() {
        // Multi-line file: the `mod target;` declaration sits on line 4
        // (two comment lines + one blank). The emitted edge's `line`
        // must be the start row of the `mod_item`, 1-indexed.
        let src = "// header\n// header\n\npub mod target;\n";
        let fg = parse(src);
        let es = includes(&fg);
        assert_eq!(es.len(), 1, "expected exactly 1 Includes edge, got {es:?}");
        assert_eq!(es[0].to, "target");
        assert_eq!(
            es[0].line, 4,
            "mod-decl line must be 1-indexed start row of the `mod_item`"
        );
    }

    #[test]
    fn use_extern_crate_emission_unchanged() {
        // Cross-section invariant: adding mod-decl emission must not
        // alter the use/extern_crate edge set. A fixture with one of
        // each kind exercises all three paths in one pass and pins the
        // emission order: `use std::io;` runs through the use-tree
        // walker, then `extern crate foo;` through the extern-crate
        // branch (both inside `extract_uses`), then `mod bar;` through
        // the dedicated mod-decl extractor. All three produce one
        // Includes edge each, with the expected `to` strings.
        let src = "use std::io;\nextern crate foo;\nmod bar;\n";
        let fg = parse(src);
        let ts = include_targets(&fg);
        assert_eq!(
            ts,
            vec!["std::io", "foo", "bar"],
            "use/extern_crate/mod edges must coexist with their existing targets and ordering, got: {ts:?}"
        );
        assert_include_edge_invariants(&fg);
        // Pin per-edge line numbers so any future reordering of the
        // three branches in `parse_to_filegraph` doesn't silently
        // scramble them.
        let edges = includes(&fg);
        assert_eq!(edges[0].line, 1, "`use std::io;` is on line 1");
        assert_eq!(edges[1].line, 2, "`extern crate foo;` is on line 2");
        assert_eq!(edges[2].line, 3, "`mod bar;` is on line 3");
    }

    // ----------------------------------------------------------------
    // Call extraction
    // ----------------------------------------------------------------

    /// Just the call edges from a `FileGraph`, in emission order.
    fn calls(fg: &FileGraph) -> Vec<&Edge> {
        fg.edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .collect()
    }

    /// Just the inheritance edges from a `FileGraph`, in emission order.
    fn inherits(fg: &FileGraph) -> Vec<&Edge> {
        fg.edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Inherits)
            .collect()
    }

    #[test]
    fn direct_call_produces_calls_edge() {
        let fg = parse("fn caller() { foo(); }");
        let cs = calls(&fg);
        assert_eq!(cs.len(), 1, "expected 1 Calls edge, got {cs:?}");
        let e = cs[0];
        assert_eq!(e.to, "foo");
        assert_eq!(e.from, "/tmp/test.rs:caller");
        assert_eq!(e.file, "/tmp/test.rs");
        assert!(e.line >= 1);
    }

    #[test]
    fn method_call_via_field_expression() {
        let fg = parse("fn caller() { obj.method(); }");
        let cs = calls(&fg);
        assert_eq!(cs.len(), 1, "expected 1 Calls edge, got {cs:?}");
        let e = cs[0];
        assert_eq!(
            e.to, "method",
            "method-call `to` must be the field_identifier, not the receiver"
        );
        assert_eq!(e.from, "/tmp/test.rs:caller");
    }

    #[test]
    fn scoped_call() {
        // `foo::bar::baz()` — the full scoped path is preserved as `to`.
        let fg = parse("fn caller() { foo::bar::baz(); }");
        let cs = calls(&fg);
        assert_eq!(cs.len(), 1, "expected 1 Calls edge, got {cs:?}");
        let e = cs[0];
        assert_eq!(e.to, "foo::bar::baz");
        assert_eq!(e.from, "/tmp/test.rs:caller");
    }

    #[test]
    fn macro_invocation_call() {
        let fg = parse("fn caller() { println!(); }");
        let cs = calls(&fg);
        assert_eq!(cs.len(), 1, "expected 1 Calls edge, got {cs:?}");
        let e = cs[0];
        assert_eq!(
            e.to, "println",
            "macro name must be the bare identifier (no `!`)"
        );
        assert_eq!(e.from, "/tmp/test.rs:caller");
    }

    #[test]
    fn turbofish_call() {
        // `foo::<u32>()` — the turbofish wraps a bare identifier so the
        // capture name is `call.name` and `to` is the underlying ident.
        let fg = parse("fn caller() { foo::<u32>(); }");
        let cs = calls(&fg);
        assert_eq!(cs.len(), 1, "expected 1 Calls edge, got {cs:?}");
        let e = cs[0];
        assert_eq!(
            e.to, "foo",
            "turbofish bare-ident call must record the underlying identifier"
        );
        assert_eq!(e.from, "/tmp/test.rs:caller");
    }

    #[test]
    fn turbofish_scoped_call() {
        // `foo::bar::<u32>()` — turbofish wrapping a scoped_identifier
        // produces a `call.qname` capture with the full path as `to`.
        let fg = parse("fn caller() { foo::bar::<u32>(); }");
        let cs = calls(&fg);
        assert_eq!(cs.len(), 1, "expected 1 Calls edge, got {cs:?}");
        let e = cs[0];
        assert_eq!(e.to, "foo::bar");
        assert_eq!(e.from, "/tmp/test.rs:caller");
    }

    #[test]
    fn chained_call_produces_two_edges() {
        // `a.b().c()` produces nested call_expressions; each method-call
        // capture yields one edge.
        let fg = parse("fn caller() { a.b().c(); }");
        let cs = calls(&fg);
        let names: Vec<&str> = cs.iter().map(|e| e.to.as_str()).collect();
        assert_eq!(
            cs.len(),
            2,
            "expected 2 Calls edges for chained call, got {names:?}"
        );
        assert!(
            names.contains(&"b"),
            "chained call must include `b`, got {names:?}"
        );
        assert!(
            names.contains(&"c"),
            "chained call must include `c`, got {names:?}"
        );
        for e in cs {
            assert_eq!(e.from, "/tmp/test.rs:caller");
        }
    }

    #[test]
    fn closure_calls_have_enclosing_fn_as_from() {
        // Closures have no name; calls inside a closure must walk past the
        // closure node and report the enclosing function as `from`.
        let fg = parse("fn outer() { let _ = || foo(); }");
        let cs = calls(&fg);
        assert_eq!(cs.len(), 1, "expected 1 Calls edge, got {cs:?}");
        let e = cs[0];
        assert_eq!(e.to, "foo");
        assert_eq!(
            e.from, "/tmp/test.rs:outer",
            "closure body call must use enclosing fn as `from`"
        );
    }

    #[test]
    fn call_inside_impl_method_has_qualified_from() {
        let fg = parse("struct Foo; impl Foo { fn bar(&self) { baz(); } }");
        let cs = calls(&fg);
        assert_eq!(cs.len(), 1, "expected 1 Calls edge, got {cs:?}");
        let e = cs[0];
        assert_eq!(e.to, "baz");
        assert_eq!(e.from, "/tmp/test.rs:Foo::bar");
    }

    /// CRITICAL anti-regression: for `impl Trait for Foo { fn bar(...) }`
    /// the `from` of any inner call MUST be `Foo::bar`, never `Trait::bar`.
    /// Mirrors the trait-impl disambiguation enforced by 5.2's definition
    /// extractor — call edges must use the same prefix scheme.
    #[test]
    fn call_inside_trait_impl_method_has_type_qualified_from_not_trait() {
        let src = "trait Trait {} struct Foo; impl Trait for Foo { fn bar(&self) { baz(); } }";
        let fg = parse(src);
        let cs = calls(&fg);
        assert_eq!(cs.len(), 1, "expected 1 Calls edge, got {cs:?}");
        let e = cs[0];
        assert_eq!(e.to, "baz");
        assert_eq!(
            e.from, "/tmp/test.rs:Foo::bar",
            "trait-impl inner call `from` must be the implementing type, not the trait"
        );
        assert_ne!(
            e.from, "/tmp/test.rs:Trait::bar",
            "trait-impl inner call must NOT use trait name as `from`"
        );
    }

    // ----------------------------------------------------------------
    // Inheritance edges (impl Trait for Type)
    // ----------------------------------------------------------------

    #[test]
    fn inherent_impl_produces_no_inheritance_edge() {
        // `impl Foo { ... }` (no `trait` field) must not emit an Inherits
        // edge; the INHERITANCE_QUERIES require both `type` AND `trait`.
        let fg = parse("struct Foo; impl Foo { fn x(&self) {} }");
        let is = inherits(&fg);
        assert!(
            is.is_empty(),
            "inherent impl must produce zero Inherits edges, got {is:?}"
        );
    }

    #[test]
    fn trait_impl_produces_one_inheritance_edge() {
        let fg = parse("trait Trait {} struct Foo; impl Trait for Foo {}");
        let is = inherits(&fg);
        assert_eq!(is.len(), 1, "expected 1 Inherits edge, got {is:?}");
        let e = is[0];
        assert_eq!(
            e.from, "Foo",
            "Inherits `from` must be the implementing type"
        );
        assert_eq!(
            e.to, "Trait",
            "Inherits `to` must be the trait being implemented"
        );
        assert_eq!(e.file, "/tmp/test.rs");
        assert!(e.line >= 1, "line must be 1-indexed and populated");
    }

    #[test]
    fn generic_trait_impl() {
        // `impl<T> Trait for Vec<T> {}` — the `type` field text is `Vec<T>`
        // (generics included as written in the source), and the `trait`
        // field text is the bare `Trait`.
        let fg = parse("trait Trait {} impl<T> Trait for Vec<T> {}");
        let is = inherits(&fg);
        assert_eq!(is.len(), 1, "expected 1 Inherits edge, got {is:?}");
        let e = is[0];
        assert_eq!(e.from, "Vec<T>", "type field text includes generics");
        assert_eq!(e.to, "Trait");
    }

    #[test]
    fn generic_impl_with_where_clause() {
        // The `where` clause is a sibling of the `type`/`trait` fields and
        // doesn't change their captured text.
        let fg =
            parse("trait Trait {} struct Foo<T>(T); impl<T> Trait for Foo<T> where T: Send {}");
        let is = inherits(&fg);
        assert_eq!(is.len(), 1, "expected 1 Inherits edge, got {is:?}");
        let e = is[0];
        assert_eq!(e.from, "Foo<T>");
        assert_eq!(e.to, "Trait");
    }

    #[test]
    fn multiple_trait_impls_per_type() {
        // Each `impl Trait for Type {}` block is its own match → one
        // Inherits edge per block.
        let fg = parse("trait A {} trait B {} struct Foo; impl A for Foo {} impl B for Foo {}");
        let is = inherits(&fg);
        assert_eq!(is.len(), 2, "expected 2 Inherits edges, got {is:?}");
        let pairs: Vec<(&str, &str)> = is
            .iter()
            .map(|e| (e.from.as_str(), e.to.as_str()))
            .collect();
        assert!(
            pairs.contains(&("Foo", "A")),
            "expected Foo -> A, got {pairs:?}"
        );
        assert!(
            pairs.contains(&("Foo", "B")),
            "expected Foo -> B, got {pairs:?}"
        );
    }

    // ----------------------------------------------------------------
    // post_index — crate-qualified namespace composition
    // ----------------------------------------------------------------
    //
    // These tests exercise `RustParser::post_index`, which composes the
    // RCMM-derived crate prefix onto the inline-mod-only namespace that
    // `parse_file` populates. The hook walks the filesystem for
    // `Cargo.toml` discovery, so the fixtures here materialize real
    // multi-file crates in a `tempfile::TempDir` rather than using the
    // in-memory `parse(src)` helper used above.

    use std::fs;
    use tempfile::TempDir;

    /// Materialize one `.rs` file under `dir`, ensuring parent directories
    /// exist first. `rel_path` is forward-slash-separated relative to
    /// `dir` and converted to a platform-native PathBuf at write time.
    fn write_rs(dir: &Path, rel_path: &str, contents: &str) -> PathBuf {
        let abs = dir.join(rel_path);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).expect("create_dir_all parent");
        }
        fs::write(&abs, contents).expect("write .rs file");
        // Canonicalize so the assertion comparisons survive symlinked
        // tempdir roots (e.g. /tmp -> /private/tmp on macOS).
        fs::canonicalize(&abs).expect("canonicalize written file")
    }

    /// Write a minimal `Cargo.toml` containing only `[package].name` plus
    /// the stock version/edition fields. Returns the canonicalized
    /// manifest path.
    fn write_cargo_toml(dir: &Path, crate_name: &str) -> PathBuf {
        let manifest_path = dir.join("Cargo.toml");
        let body = format!(
            "[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"
        );
        fs::write(&manifest_path, body).expect("write Cargo.toml");
        fs::canonicalize(&manifest_path).expect("canonicalize Cargo.toml")
    }

    /// Drive `parse_file` + `post_index` over a list of absolute Rust
    /// file paths, mirroring what the analyze path does after the rayon
    /// parse loop. Returns the post-hook FileGraph slice keyed by path.
    ///
    /// Builds a real [`FileIndex`] over the parsed graph set (matching
    /// `code_graph_tools::indexer::build_file_index`) so the
    /// post_index mod-resolution pass has the index entries it probes.
    /// The 1.x namespace-only tests work fine with an empty index too —
    /// they don't exercise mod resolution — but the 2.2 mod-resolution
    /// tests do, and sharing one helper keeps the test surface uniform.
    fn run_post_index(rs_paths: &[PathBuf]) -> Vec<FileGraph> {
        let parser = RustParser::new().expect("RustParser::new");
        let mut graphs: Vec<FileGraph> = rs_paths
            .iter()
            .map(|p| {
                let content = fs::read(p).expect("read source file");
                parser.parse_file(p, &content).expect("parse_file")
            })
            .collect();
        let file_index = build_test_file_index(&graphs);
        parser.post_index(&mut graphs, &file_index);
        graphs
    }

    /// Build a `FileIndex` over the parsed `graphs` exactly the way
    /// `code_graph_tools::indexer::build_file_index` does (basename
    /// keyed, every graph contributes its path). Reproduced here so
    /// `code-graph-lang-rust`'s unit tests don't take a dep on the
    /// `code-graph-tools` crate.
    //
    // Production source of truth lives at
    // `crates/code-graph-tools/src/indexer.rs::build_file_index`
    // (line numbers may drift). If you change either, change both —
    // these two functions must stay byte-equivalent so the test path's
    // FileIndex matches what production builds at runtime.
    fn build_test_file_index(graphs: &[FileGraph]) -> FileIndex {
        let mut index = FileIndex::new();
        for fg in graphs {
            let path = PathBuf::from(&fg.path);
            if let Some(base) = path.file_name().and_then(|s| s.to_str()) {
                index
                    .by_basename
                    .entry(base.to_string())
                    .or_default()
                    .push(path);
            }
        }
        index
    }

    /// Find the FileGraph in `graphs` whose `path` matches `target`
    /// after canonicalization. Panics with a helpful message if absent.
    fn fg_for<'a>(graphs: &'a [FileGraph], target: &Path) -> &'a FileGraph {
        let want = target.to_string_lossy().into_owned();
        graphs.iter().find(|fg| fg.path == want).unwrap_or_else(|| {
            panic!(
                "expected FileGraph for {want}; got: {:?}",
                graphs.iter().map(|fg| &fg.path).collect::<Vec<_>>()
            )
        })
    }

    /// CRITICAL: multi-file Cargo crate fixture must produce the
    /// canonical crate-qualified namespaces for every classic rule case:
    /// - `src/lib.rs` → bare crate name
    /// - `src/reactor.rs` → `crate::reactor`
    /// - `src/a/b.rs` → `crate::a::b`
    /// - `src/foo/mod.rs` → `crate::foo`
    #[test]
    fn post_index_assigns_crate_qualified_namespaces_across_layout_rules() {
        let dir = TempDir::new().expect("TempDir");
        write_cargo_toml(dir.path(), "ark_core");

        let lib = write_rs(dir.path(), "src/lib.rs", "fn root_fn() {}\n");
        let reactor = write_rs(dir.path(), "src/reactor.rs", "fn react() {}\n");
        let nested = write_rs(dir.path(), "src/a/b.rs", "fn deep() {}\n");
        let modrs = write_rs(dir.path(), "src/foo/mod.rs", "fn in_mod_rs() {}\n");

        let graphs = run_post_index(&[lib.clone(), reactor.clone(), nested.clone(), modrs.clone()]);

        let root = sym(fg_for(&graphs, &lib), "root_fn");
        assert_eq!(
            root.namespace, "ark_core",
            "lib.rs symbols must carry the bare crate name as namespace"
        );

        let r = sym(fg_for(&graphs, &reactor), "react");
        assert_eq!(
            r.namespace, "ark_core::reactor",
            "src/reactor.rs symbols must carry crate::module"
        );

        let n = sym(fg_for(&graphs, &nested), "deep");
        assert_eq!(
            n.namespace, "ark_core::a::b",
            "src/a/b.rs symbols must carry crate::a::b"
        );

        let m = sym(fg_for(&graphs, &modrs), "in_mod_rs");
        assert_eq!(
            m.namespace, "ark_core::foo",
            "src/foo/mod.rs symbols must collapse to crate::foo (no `::mod` segment)"
        );
    }

    /// Composition rule: inline `mod tests { ... }` inside a crate-owned
    /// file must produce `crate_name::file_stem::tests`. Demonstrates the
    /// `Some(rcmm_path) + non-empty inline` arm of post_index's
    /// composition table.
    #[test]
    fn post_index_composes_inline_mods_onto_crate_prefix() {
        let dir = TempDir::new().expect("TempDir");
        write_cargo_toml(dir.path(), "ark_core");
        let reactor = write_rs(
            dir.path(),
            "src/reactor.rs",
            "fn outer() {}\nmod tests { fn helper() {} }\n",
        );

        let graphs = run_post_index(std::slice::from_ref(&reactor));
        let fg = fg_for(&graphs, &reactor);

        let outer = sym(fg, "outer");
        assert_eq!(
            outer.namespace, "ark_core::reactor",
            "module-level fn in src/reactor.rs takes the file-level prefix"
        );

        let helper = sym(fg, "helper");
        assert_eq!(
            helper.namespace, "ark_core::reactor::tests",
            "inline `mod tests` composes `::tests` onto the file's crate prefix"
        );
    }

    /// Three-level inline-mod nesting (`mod a { mod b { mod c { … } } }`)
    /// inside `src/lib.rs` of a real crate composes onto the bare crate
    /// name. Anti-regression for the composed-path counterpart of
    /// `nested_mods_produce_namespace_a_b_c` (which exercises the
    /// no-Cargo.toml fallback via `parse_file` directly).
    #[test]
    fn post_index_composes_deeply_nested_inline_mods_onto_crate_prefix() {
        let dir = TempDir::new().expect("TempDir");
        write_cargo_toml(dir.path(), "ark_core");
        let lib = write_rs(
            dir.path(),
            "src/lib.rs",
            "mod a { mod b { mod c { fn x() {} } } }\n",
        );

        let graphs = run_post_index(std::slice::from_ref(&lib));
        let fg = fg_for(&graphs, &lib);

        let x = sym(fg, "x");
        // `src/lib.rs` resolves to the bare crate name `ark_core`; the
        // three inline mod ancestors compose as `::a::b::c`.
        assert_eq!(
            x.namespace, "ark_core::a::b::c",
            "deep inline-mod nesting must compose onto the crate prefix"
        );
    }

    /// Crate name with `-` must canonicalize to `_` (Cargo's identifier
    /// conversion rule). Anti-regression for the post_index-side wiring
    /// of `CrateModuleModel`'s normalization.
    #[test]
    fn post_index_normalizes_dash_in_crate_name_to_underscore() {
        let dir = TempDir::new().expect("TempDir");
        write_cargo_toml(dir.path(), "my-cool-crate");
        let foo = write_rs(dir.path(), "src/foo.rs", "fn f() {}\n");

        let graphs = run_post_index(std::slice::from_ref(&foo));
        let fg = fg_for(&graphs, &foo);
        assert_eq!(
            sym(fg, "f").namespace,
            "my_cool_crate::foo",
            "Cargo's `-` → `_` conversion must reach the rewritten namespace"
        );
    }

    /// A `.rs` file with NO `Cargo.toml` anywhere up its ancestor chain
    /// must keep its inline-mod-only namespace (preserves today's
    /// behavior, including the empty-string `<global>` rendering).
    #[test]
    fn post_index_fallback_when_no_cargo_toml_preserves_inline_only_namespace() {
        let dir = TempDir::new().expect("TempDir");
        // Intentionally no Cargo.toml written. The fixture lives directly
        // under the tempdir root, which is itself under /tmp — and
        // /tmp/.. is /, neither of which has a Cargo.toml. The walk's
        // termination condition is `dir.parent() == None`, so this is
        // safe to rely on for the duration of the test.
        let standalone = write_rs(
            dir.path(),
            "standalone.rs",
            "mod inner { fn helper() {} }\nfn top() {}\n",
        );

        let graphs = run_post_index(std::slice::from_ref(&standalone));
        let fg = fg_for(&graphs, &standalone);

        let top = sym(fg, "top");
        assert!(
            top.namespace.is_empty(),
            "top-level fn outside any crate must keep empty namespace (renders as <global>); got {:?}",
            top.namespace
        );

        let helper = sym(fg, "helper");
        assert_eq!(
            helper.namespace, "inner",
            "inline-mod-only namespace must survive the no-Cargo.toml fallback path"
        );
    }

    /// Multi-crate workspace: each member crate owns its own `.rs` files,
    /// and `post_index` resolves each file against the nearest owning
    /// `Cargo.toml`. Confirms the discovery walk finds nested manifests
    /// independently for each file.
    #[test]
    fn post_index_resolves_each_member_independently_in_workspace() {
        let dir = TempDir::new().expect("TempDir");
        // Create both member crate directories first; `write_cargo_toml`
        // assumes its directory exists.
        fs::create_dir_all(dir.path().join("crates/a/src")).unwrap();
        fs::create_dir_all(dir.path().join("crates/b/src")).unwrap();
        write_cargo_toml(&dir.path().join("crates/a"), "a");
        write_cargo_toml(&dir.path().join("crates/b"), "b");

        let a_file = write_rs(dir.path(), "crates/a/src/foo.rs", "fn af() {}\n");
        let b_file = write_rs(dir.path(), "crates/b/src/bar.rs", "fn bf() {}\n");

        let graphs = run_post_index(&[a_file.clone(), b_file.clone()]);

        let af = sym(fg_for(&graphs, &a_file), "af");
        assert_eq!(
            af.namespace, "a::foo",
            "files in crate `a` must resolve to its owning manifest"
        );

        let bf = sym(fg_for(&graphs, &b_file), "bf");
        assert_eq!(
            bf.namespace, "b::bar",
            "files in crate `b` must resolve to its owning manifest"
        );
    }

    /// `post_index` stores nothing on `&self`. Drive a single
    /// `RustParser` instance against TWO independent fixtures with
    /// disjoint crate names and disjoint file paths; the second call's
    /// resolutions must reflect ONLY the second fixture's manifest. A
    /// stateful cache on `&self` (e.g. memoizing the first call's
    /// manifest discoveries by directory or carrying a residual
    /// `CrateModuleModel`) would leak the first crate's name into the
    /// second result and trip the negative assertion below.
    #[test]
    fn post_index_does_not_leak_state_between_calls_on_different_crates() {
        // Fixture A: crate `crate_a` with src/lib.rs + src/foo.rs.
        let dir_a = TempDir::new().expect("TempDir A");
        write_cargo_toml(dir_a.path(), "crate_a");
        let a_lib = write_rs(dir_a.path(), "src/lib.rs", "pub fn la() {}\n");
        let a_foo = write_rs(dir_a.path(), "src/foo.rs", "fn af() {}\n");

        // Fixture B: a SEPARATE TempDir, different crate name, different
        // module file names — no overlap with A on any axis.
        let dir_b = TempDir::new().expect("TempDir B");
        write_cargo_toml(dir_b.path(), "crate_b");
        let b_lib = write_rs(dir_b.path(), "src/lib.rs", "pub fn lb() {}\n");
        let b_bar = write_rs(dir_b.path(), "src/bar.rs", "fn bf() {}\n");

        let parser = RustParser::new().expect("RustParser::new");
        let file_index = FileIndex::new();

        let parse_one = |p: &PathBuf| {
            let content = fs::read(p).expect("read source file");
            parser.parse_file(p, &content).expect("parse_file")
        };

        // First call: fixture A only.
        let mut graphs_a = vec![parse_one(&a_lib), parse_one(&a_foo)];
        parser.post_index(&mut graphs_a, &file_index);

        let la_ns = &sym(fg_for(&graphs_a, &a_lib), "la").namespace;
        let af_ns = &sym(fg_for(&graphs_a, &a_foo), "af").namespace;
        assert_eq!(
            la_ns, "crate_a",
            "fixture A's lib.rs must resolve under `crate_a`"
        );
        assert_eq!(
            af_ns, "crate_a::foo",
            "fixture A's foo.rs must resolve under `crate_a::foo`"
        );

        // Second call: SAME parser instance, fixture B's independent
        // FileGraph vec. None of fixture A's manifest/path data is
        // visible to this call.
        let mut graphs_b = vec![parse_one(&b_lib), parse_one(&b_bar)];
        parser.post_index(&mut graphs_b, &file_index);

        let lb_ns = &sym(fg_for(&graphs_b, &b_lib), "lb").namespace;
        let bf_ns = &sym(fg_for(&graphs_b, &b_bar), "bf").namespace;
        assert_eq!(
            lb_ns, "crate_b",
            "fixture B's lib.rs must resolve under `crate_b`, not the prior call's crate"
        );
        assert_eq!(
            bf_ns, "crate_b::bar",
            "fixture B's bar.rs must resolve under `crate_b::bar`, not the prior call's crate"
        );

        // Negative assertions: a stateful-cache bug would surface as
        // `crate_a`-prefixed namespaces leaking into fixture B's output.
        assert!(
            !lb_ns.starts_with("crate_a"),
            "state leak: fixture B's lib.rs resolved with crate_a prefix ({lb_ns})"
        );
        assert!(
            !bf_ns.starts_with("crate_a"),
            "state leak: fixture B's bar.rs resolved with crate_a prefix ({bf_ns})"
        );
    }

    /// Non-Rust FileGraphs in the slice must be untouched by Rust's
    /// `post_index`. Anti-regression for accidental cross-language
    /// mutation when the slice carries graphs from multiple plugins.
    #[test]
    fn post_index_leaves_non_rust_filegraphs_unmodified() {
        let dir = TempDir::new().expect("TempDir");
        write_cargo_toml(dir.path(), "mixed");
        let foo = write_rs(dir.path(), "src/foo.rs", "fn f() {}\n");

        let parser = RustParser::new().expect("RustParser::new");
        let content = fs::read(&foo).unwrap();
        let rust_fg = parser.parse_file(&foo, &content).expect("parse_file");

        // A synthetic non-Rust FileGraph with a hand-set namespace. The
        // Rust plugin must NOT touch its symbols.
        let other = FileGraph {
            path: "/synthetic/other.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![Symbol {
                name: "untouched".to_string(),
                kind: SymbolKind::Function,
                file: "/synthetic/other.cpp".to_string(),
                line: 1,
                column: 0,
                end_line: 1,
                signature: "void untouched()".to_string(),
                namespace: "original_namespace".to_string(),
                parent: String::new(),
                language: Language::Cpp,
            }],
            edges: Vec::new(),
        };

        let mut graphs = vec![rust_fg, other.clone()];
        let file_index = FileIndex::new();
        parser.post_index(&mut graphs, &file_index);

        let touched_other = &graphs[1];
        assert_eq!(
            touched_other, &other,
            "Rust post_index must leave non-Rust FileGraphs byte-identical"
        );
        // And the Rust file must have been rewritten as expected.
        assert_eq!(sym(&graphs[0], "f").namespace, "mixed::foo");
    }

    // ----------------------------------------------------------------
    // post_index — mod-declaration Includes edge resolution (2.2)
    // ----------------------------------------------------------------
    //
    // These tests exercise the `post_index` pass that resolves
    // provisional `mod` `Includes` edges (emitted by `extract_mod_decls`
    // with `to = bare modname`) to absolute indexed file paths using
    // Rust's module-file rules (#[path] → sibling → mod.rs). They share
    // the `run_post_index` helper above, which now builds a real
    // `FileIndex` over the parsed graph set.

    /// Just the Includes-kind edges from a `FileGraph`, in emission order.
    fn fg_include_edges(fg: &FileGraph) -> Vec<&Edge> {
        fg.edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Includes)
            .collect()
    }

    /// `pub mod foo;` in `src/lib.rs` with a sibling `src/foo.rs`
    /// present → after `post_index`, the provisional Includes edge is
    /// rewritten to the absolute path of `foo.rs`.
    #[test]
    fn mod_sibling_file_resolves() {
        let dir = TempDir::new().expect("TempDir");
        write_cargo_toml(dir.path(), "ark_core");
        let lib = write_rs(dir.path(), "src/lib.rs", "pub mod foo;\n");
        let foo = write_rs(dir.path(), "src/foo.rs", "fn f() {}\n");

        let graphs = run_post_index(&[lib.clone(), foo.clone()]);
        let lib_fg = fg_for(&graphs, &lib);

        let includes = fg_include_edges(lib_fg);
        assert_eq!(
            includes.len(),
            1,
            "expected exactly 1 surviving Includes edge after post_index, got: {:?}",
            includes
                .iter()
                .map(|e| (&e.from, &e.to))
                .collect::<Vec<_>>()
        );
        let e = includes[0];
        assert_eq!(
            e.to,
            foo.to_string_lossy(),
            "sibling `mod foo;` must resolve to the absolute path of foo.rs"
        );
        // Identity invariants — `from`/`file` unchanged from emission.
        assert_eq!(e.from, lib.to_string_lossy());
        assert_eq!(e.file, lib.to_string_lossy());
        assert_eq!(e.kind, EdgeKind::Includes);
    }

    /// `pub mod foo;` in `src/lib.rs` with `src/foo/mod.rs` (mod.rs
    /// subdir layout) present → edge resolves to the absolute path of
    /// `foo/mod.rs`.
    #[test]
    fn mod_mod_rs_layout_resolves() {
        let dir = TempDir::new().expect("TempDir");
        write_cargo_toml(dir.path(), "ark_core");
        let lib = write_rs(dir.path(), "src/lib.rs", "pub mod foo;\n");
        let mod_rs = write_rs(dir.path(), "src/foo/mod.rs", "fn f() {}\n");

        let graphs = run_post_index(&[lib.clone(), mod_rs.clone()]);
        let lib_fg = fg_for(&graphs, &lib);

        let includes = fg_include_edges(lib_fg);
        assert_eq!(
            includes.len(),
            1,
            "expected exactly 1 surviving Includes edge"
        );
        assert_eq!(
            includes[0].to,
            mod_rs.to_string_lossy(),
            "`mod foo;` with foo/mod.rs layout must resolve to the absolute path of mod.rs"
        );
    }

    /// `mod orphan;` in `src/lib.rs` with NO matching `orphan.rs` or
    /// `orphan/mod.rs` in the FileIndex → edge is removed from the
    /// FileGraph's `edges` vector. Pinned via a sibling check that
    /// confirms no Includes edge points at any path-shaped string.
    #[test]
    fn mod_no_indexed_file_is_dropped() {
        let dir = TempDir::new().expect("TempDir");
        write_cargo_toml(dir.path(), "ark_core");
        let lib = write_rs(dir.path(), "src/lib.rs", "mod orphan;\n");

        let graphs = run_post_index(std::slice::from_ref(&lib));
        let lib_fg = fg_for(&graphs, &lib);

        let includes = fg_include_edges(lib_fg);
        assert!(
            includes.is_empty(),
            "mod-decl pointing at no indexed file must be dropped post_index; surviving edges: {:?}",
            includes.iter().map(|e| (&e.from, &e.to)).collect::<Vec<_>>()
        );
    }

    /// `#[path = "x.rs"] mod foo;` with both `x.rs` AND `foo.rs`
    /// present → resolution picks `x.rs` (the attribute wins
    /// unconditionally). Without `#[path]`, `foo.rs` would have been
    /// the sibling-rule winner — so this fixture exercises the
    /// path-override-beats-sibling discriminator directly.
    #[test]
    fn mod_path_attribute_override_resolves() {
        let dir = TempDir::new().expect("TempDir");
        write_cargo_toml(dir.path(), "ark_core");
        let lib = write_rs(
            dir.path(),
            "src/lib.rs",
            "#[path = \"x.rs\"]\npub mod foo;\n",
        );
        let foo = write_rs(dir.path(), "src/foo.rs", "fn would_lose() {}\n");
        let x = write_rs(dir.path(), "src/x.rs", "fn wins() {}\n");

        let graphs = run_post_index(&[lib.clone(), foo.clone(), x.clone()]);
        let lib_fg = fg_for(&graphs, &lib);

        let includes = fg_include_edges(lib_fg);
        assert_eq!(
            includes.len(),
            1,
            "expected exactly 1 surviving Includes edge after post_index"
        );
        let e = includes[0];
        assert_eq!(
            e.to,
            x.to_string_lossy(),
            "#[path = \"x.rs\"] must override the sibling-rule winner foo.rs and resolve to x.rs"
        );
        assert_ne!(
            e.to,
            foo.to_string_lossy(),
            "#[path] override must NOT fall through to sibling foo.rs even when present"
        );
    }

    /// `#[path = "missing.rs"] pub mod foo;` with `missing.rs` NOT in
    /// the FileIndex but a sibling `foo.rs` that IS — the override is
    /// authoritative and must NOT fall through to the sibling rule.
    /// Mirrors rustc: a `#[path]` whose target file doesn't exist is a
    /// compile error in real Rust, not a silent fallback to the
    /// default file-lookup rules. Our index-side equivalent is to
    /// drop the provisional edge entirely.
    #[test]
    fn mod_path_attribute_target_not_indexed_drops_edge_no_fallback() {
        let dir = TempDir::new().expect("TempDir");
        write_cargo_toml(dir.path(), "ark_core");
        let lib = write_rs(
            dir.path(),
            "src/lib.rs",
            "#[path = \"missing.rs\"]\npub mod foo;\n",
        );
        // `foo.rs` is the file the sibling rule would resolve to if
        // the `#[path]` override silently fell through. It IS in the
        // index, so a fallback bug would surface as a surviving edge
        // pointing at this path.
        let foo = write_rs(dir.path(), "src/foo.rs", "fn would_be_fallback() {}\n");
        // `missing.rs` is deliberately NOT created on disk and
        // therefore not in the FileIndex.

        let graphs = run_post_index(&[lib.clone(), foo.clone()]);
        let lib_fg = fg_for(&graphs, &lib);

        let includes = fg_include_edges(lib_fg);
        assert!(
            includes.is_empty(),
            "#[path] override pointing at a non-indexed file must drop the edge — \
             NOT silently fall through to the sibling foo.rs. Surviving edges: {:?}",
            includes
                .iter()
                .map(|e| (&e.from, &e.to))
                .collect::<Vec<_>>()
        );
        // Belt-and-suspenders: no surviving edge points at foo.rs.
        // Equivalent assertion to `includes.is_empty()`, framed so a
        // future regression that resolves to foo.rs (rather than
        // missing.rs) produces a more pointed failure message.
        let foo_str = foo.to_string_lossy().into_owned();
        assert!(
            !includes.iter().any(|e| e.to == foo_str),
            "no surviving Includes edge may resolve to the sibling foo.rs; \
             that would be a #[path]-fallback regression"
        );
    }

    /// **v1 documented limitation.** `mod b;` declared inside an inline
    /// `mod a { … }` block in `src/lib.rs`, with `b.rs` present in the
    /// file's parent directory → the provisional edge is **dropped**
    /// (not resolved to `b.rs`), because v1 cannot determine the
    /// inline-mod ancestor path (`a/b.rs`/`a/b/mod.rs`) from the bare
    /// emitted edge. This anti-regression pins the documented
    /// conservative-drop behavior. Resolving inline-nested mod decls is
    /// a future follow-up.
    #[test]
    fn mod_inside_inline_mod_remains_unresolved_in_v1() {
        let dir = TempDir::new().expect("TempDir");
        write_cargo_toml(dir.path(), "ark_core");
        let lib = write_rs(dir.path(), "src/lib.rs", "mod a {\n    mod b;\n}\n");
        // `b.rs` sits in the same dir as lib.rs — Rust would NOT resolve
        // `mod b;` (inside inline `mod a { … }`) to this file; the
        // correct resolution would be `src/a/b.rs` (which we don't
        // create). The v1 drop is the right outcome either way; the
        // sibling `b.rs` here is the false-positive a naive resolver
        // would emit.
        let _decoy = write_rs(dir.path(), "src/b.rs", "fn d() {}\n");

        let graphs = run_post_index(std::slice::from_ref(&lib));
        let lib_fg = fg_for(&graphs, &lib);

        let includes = fg_include_edges(lib_fg);
        assert!(
            includes.is_empty(),
            "inline-nested `mod b;` inside `mod a {{ … }}` must drop in v1 — \
             resolving it would risk a false src/b.rs edge. Got: {:?}",
            includes
                .iter()
                .map(|e| (&e.from, &e.to))
                .collect::<Vec<_>>()
        );
    }

    /// Cross-section scope-boundary anti-regression: a mixed fixture
    /// with `use std::io;`, `extern crate foo;`, a top-level `mod foo;`
    /// (matched against the same-named sibling), and an unresolvable
    /// `mod gone;` exercises every emission path in one pass.
    ///
    /// Expected post_index outcome:
    /// - `use std::io;` edge → unchanged (dotted token; will drop later
    ///   at `resolve_include` time via the `None` arm).
    /// - `extern crate foo;` edge → unchanged (also a bare token; will
    ///   drop later for the same reason — `"foo"` is not an absolute
    ///   path).
    /// - `mod foo;` edge → rewritten to absolute `foo.rs`.
    /// - `mod gone;` edge → dropped (no candidate in FileIndex).
    ///
    /// After the surrounding `resolve_include` filter runs (mimicked
    /// here by checking which `to` values are absolute paths in the
    /// FileIndex), only the `mod foo;`-resolved edge survives.
    #[test]
    fn use_extern_crate_still_drop_after_resolve_include_override() {
        let dir = TempDir::new().expect("TempDir");
        write_cargo_toml(dir.path(), "ark_core");
        let lib = write_rs(
            dir.path(),
            "src/lib.rs",
            "use std::io;\nextern crate foo;\nmod foo;\nmod gone;\n",
        );
        let foo = write_rs(dir.path(), "src/foo.rs", "fn f() {}\n");

        let parser = RustParser::new().expect("RustParser::new");
        let mut graphs: Vec<FileGraph> = [lib.clone(), foo.clone()]
            .iter()
            .map(|p| {
                let content = fs::read(p).expect("read source file");
                parser.parse_file(p, &content).expect("parse_file")
            })
            .collect();
        let file_index = build_test_file_index(&graphs);
        parser.post_index(&mut graphs, &file_index);

        // Post-post_index: confirm `mod foo;` resolved, `mod gone;`
        // dropped, and `use std::io;` + `extern crate foo;` still
        // present (they'll drop at the next layer).
        let lib_fg = fg_for(&graphs, &lib);
        let includes = fg_include_edges(lib_fg);
        let targets: Vec<&str> = includes.iter().map(|e| e.to.as_str()).collect();
        let foo_str = foo.to_string_lossy().into_owned();
        assert!(
            targets.contains(&foo_str.as_str()),
            "`mod foo;` must be rewritten to absolute foo.rs path, got targets: {targets:?}"
        );
        assert!(
            targets.contains(&"std::io"),
            "`use std::io;` edge must still be present post_index, got: {targets:?}"
        );
        assert!(
            targets.contains(&"foo"),
            "`extern crate foo;` edge (`to = \"foo\"`) must still be present post_index, got: {targets:?}"
        );
        assert!(
            !targets.contains(&"gone"),
            "`mod gone;` must be dropped post_index (no indexed file), got: {targets:?}"
        );

        // Apply the resolve_include override + the surrounding
        // `language_for_path(&resolved).is_some()` filter to simulate
        // the analyze-path drop. Only the absolute-path mod edge
        // survives; the dotted use/extern_crate tokens both drop.
        let surviving_after_resolve: Vec<String> = includes
            .iter()
            .filter_map(|e| {
                parser
                    .resolve_include(&e.to, &file_index)
                    .map(|p| p.to_string_lossy().into_owned())
            })
            .collect();
        assert_eq!(
            surviving_after_resolve,
            vec![foo_str.clone()],
            "after resolve_include filtering, only the mod-resolved foo.rs path must survive"
        );
    }

    /// Direct unit test of the `resolve_include` override: indexed
    /// absolute path → `Some(path)`.
    #[test]
    fn resolve_include_indexed_absolute_path_returns_some() {
        let parser = RustParser::new().expect("RustParser::new");
        let mut file_index = FileIndex::new();
        file_index
            .by_basename
            .entry("foo.rs".to_string())
            .or_default()
            .push(PathBuf::from("/proj/src/foo.rs"));

        let resolved = parser.resolve_include("/proj/src/foo.rs", &file_index);
        assert_eq!(
            resolved.as_deref(),
            Some(Path::new("/proj/src/foo.rs")),
            "indexed absolute path must resolve to itself"
        );
    }

    /// Direct unit test of the `resolve_include` override: dotted
    /// use-path → `None`. Covers `use std::io;` (`std::io`) and
    /// `extern crate alloc;` (`alloc`) — both shapes drop.
    #[test]
    fn resolve_include_dotted_use_path_returns_none() {
        let parser = RustParser::new().expect("RustParser::new");
        let file_index = FileIndex::new();

        assert!(
            parser.resolve_include("std::io", &file_index).is_none(),
            "dotted `use` path must drop (not an absolute path)"
        );
        assert!(
            parser.resolve_include("alloc", &file_index).is_none(),
            "bare `extern crate` name must drop (not an absolute path)"
        );
        assert!(
            parser
                .resolve_include("foo::bar::baz", &file_index)
                .is_none(),
            "scoped path must drop"
        );
    }

    /// Boundary case: an absolute path that is NOT in the FileIndex →
    /// `None`. Pinned because the override deliberately doesn't blindly
    /// echo absolute strings — it must also be a known indexed file.
    #[test]
    fn resolve_include_absolute_but_unindexed_returns_none() {
        let parser = RustParser::new().expect("RustParser::new");
        let file_index = FileIndex::new();
        assert!(
            parser
                .resolve_include("/proj/src/foo.rs", &file_index)
                .is_none(),
            "absolute path not in FileIndex must drop"
        );
    }

    /// Real cycle: `src/a.rs` has `mod b;` and `src/b.rs` has `mod a;`,
    /// both at the top level. After post_index resolution, an
    /// `a.rs <-> b.rs` cycle exists in the file graph; the higher-level
    /// integration test that calls `Graph::detect_cycles` lives in
    /// `crates/code-graph-tools/tests/rust_mod_resolution.rs`. This
    /// unit test just pins the per-file resolved edges in both
    /// directions.
    #[test]
    fn real_detect_cycles_with_mod_a_mod_b_mod_a() {
        let dir = TempDir::new().expect("TempDir");
        write_cargo_toml(dir.path(), "two_way");
        let _lib = write_rs(dir.path(), "src/lib.rs", "pub mod a;\npub mod b;\n");
        let a = write_rs(dir.path(), "src/a.rs", "mod b;\nfn f() {}\n");
        let b = write_rs(dir.path(), "src/b.rs", "mod a;\nfn g() {}\n");

        // Just the two crate-file modules need to round-trip post_index.
        let graphs = run_post_index(&[_lib.clone(), a.clone(), b.clone()]);

        let a_fg = fg_for(&graphs, &a);
        let b_fg = fg_for(&graphs, &b);

        let a_includes = fg_include_edges(a_fg);
        let b_includes = fg_include_edges(b_fg);

        assert_eq!(
            a_includes.iter().map(|e| e.to.as_str()).collect::<Vec<_>>(),
            vec![b.to_string_lossy().as_ref()],
            "a.rs must resolve `mod b;` to absolute b.rs"
        );
        assert_eq!(
            b_includes.iter().map(|e| e.to.as_str()).collect::<Vec<_>>(),
            vec![a.to_string_lossy().as_ref()],
            "b.rs must resolve `mod a;` to absolute a.rs"
        );
    }

    /// `extract_path_attribute` walks chained attributes
    /// (`#[cfg(test)] #[path = "x.rs"] mod foo;`) so the path override
    /// is still found when other attributes precede it. Pinned to
    /// defend against a future "only the immediately-preceding
    /// attribute counts" regression.
    #[test]
    fn mod_path_attribute_resolution_through_chained_attributes() {
        let dir = TempDir::new().expect("TempDir");
        write_cargo_toml(dir.path(), "ark_core");
        let lib = write_rs(
            dir.path(),
            "src/lib.rs",
            "#[cfg(test)]\n#[path = \"x.rs\"]\npub mod foo;\n",
        );
        let x = write_rs(dir.path(), "src/x.rs", "fn fx() {}\n");

        let graphs = run_post_index(&[lib.clone(), x.clone()]);
        let lib_fg = fg_for(&graphs, &lib);
        let includes = fg_include_edges(lib_fg);
        assert_eq!(
            includes.len(),
            1,
            "expected exactly 1 surviving include edge"
        );
        assert_eq!(
            includes[0].to,
            x.to_string_lossy(),
            "chained attributes must not hide the #[path] override"
        );
    }

    /// 2.1 baseline test must STILL PASS as-emitted: the parser
    /// emits `to = "b"` for `mod a { mod b; }` regardless of whether
    /// post_index later drops or resolves the edge. This test calls
    /// only `parse_file` (no post_index), so it pins the emission
    /// boundary — re-asserting it here against the 2.2 changes
    /// ensures parser emission was not accidentally rewritten to
    /// encode inline-nesting info on the edge `to` (which would
    /// have changed `"b"` to e.g. `"a::b"` or similar).
    #[test]
    fn mod_inline_outer_external_inner_emits_edge_to_bare_b_still_2_2() {
        let fg = parse("mod a { mod b; }");
        let ts = include_targets(&fg);
        assert_eq!(
            ts,
            vec!["b"],
            "2.1 baseline must survive 2.2: parser still emits `to = \"b\"` \
             for `mod a {{ mod b; }}` (resolution behavior is post_index's job)"
        );
    }
}
