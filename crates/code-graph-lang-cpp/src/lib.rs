#![forbid(unsafe_code)]

//! C++ language plugin for code-graph-mcp.
//!
//! This crate ports the Go `internal/lang/cpp` package to Rust. It uses
//! tree-sitter (via the `tree-sitter` and `tree-sitter-cpp` crates) to extract
//! symbols, calls, includes, and inheritance edges from C/C++ source.
//!
//! # Extraction pipeline
//!
//! Four extraction loops (`extract_definitions`, `extract_calls`,
//! `extract_includes`, `extract_inheritance`) feed
//! [`CppParser::parse_file`]. The full behavioral corpus lives in
//! `tests/corpus.rs`; the inline tests at the bottom of this file cover
//! one representative example of every extraction path so regressions
//! surface immediately.
//!
//! # Macro stripping (`preprocess` / [`strip_macros`])
//!
//! The CppMacroStrip plan (Phases 1–3) added a [`LanguagePlugin::preprocess`]
//! override on `CppParser` that whole-word-replaces identifier tokens listed
//! in `[cpp].macro_strip` (from `.code-graph.toml`) with spaces of the same
//! length before tree-sitter parses. This recovers class extraction for
//! UE-style declarations such as `class CORE_API AActor : public UObject {};`
//! that the tree-sitter-cpp v0.23.4 grammar otherwise drops as an `ERROR`
//! node. The default behavior is unchanged — an empty `macro_strip` list
//! short-circuits to `Cow::Borrowed` with zero allocation. See
//! [`strip_macros`] for the substitution algorithm and rationale.
//!
//! # Known C++ parser limitations
//!
//! These were validated against tree-sitter-cpp v0.23.4 and apply to the Go
//! implementation as well; they are intentional, not bugs. Any change to this
//! list MUST be mirrored in `CLAUDE.md`.
//!
//! 1. **Macro-generated definitions** — Macros like `DEFINE_HANDLER(name)`
//!    that expand to function definitions are not visible to tree-sitter (it
//!    sees the macro call, not the expansion). Macro invocations that look
//!    like function calls ARE captured as call edges.
//! 2. **Complex template metaprogramming** — Deeply nested template
//!    specializations may produce incomplete or error-containing AST nodes.
//!    The parser skips error nodes gracefully.
//! 3. **Call resolution is heuristic** — Call edges are resolved via
//!    scope-aware heuristic matching (same file > same class > same
//!    namespace > global). This is syntactic, not semantic; overloaded
//!    functions may resolve to the wrong candidate.
//! 4. **C++ cast expressions** — `static_cast`, `dynamic_cast`, `const_cast`,
//!    and `reinterpret_cast` are filtered out (tree-sitter parses them as
//!    `call_expression`).
//! 5. **Forward declarations excluded** — Only `function_definition` (with
//!    body) produces symbols. Forward declarations (`void foo();`) are
//!    intentionally excluded to avoid duplicates.
//! 6. **Template method calls** — `obj.foo<T>()` via `template_method` node
//!    type is not matched in tree-sitter-cpp v0.23.4. These calls fall
//!    through to the regular `field_expression` pattern when possible.
//! 7. **Function pointer typedefs** — Captured via the alternation pattern
//!    (`type_definition` with a `function_declarator` containing a
//!    `parenthesized_declarator > pointer_declarator > type_identifier`).

pub(crate) mod helpers;
pub(crate) mod macro_define;
pub(crate) mod preprocess;
pub(crate) mod queries;

pub use preprocess::strip_macros;

use std::borrow::Cow;
use std::collections::HashSet;
use std::path::Path;

use code_graph_core::{Confidence, Edge, EdgeKind, FileGraph, Language, RootConfig, Symbol, SymbolKind};
use code_graph_lang::{LanguagePlugin, ParseError};
use streaming_iterator::StreamingIterator;
use tree_sitter::{
    Language as TsLanguage, Node, Parser as TsParser, Query, QueryCursor, Tree as TsTree,
};

use crate::helpers::{
    enclosing_function_id, find_enclosing_kind, is_cpp_cast, resolve_namespace,
    resolve_parent_class, split_qualified, strip_include_path, truncate_signature,
};
use crate::queries::{CALL_QUERIES, DEFINITION_QUERIES, INCLUDE_QUERIES, INHERITANCE_QUERIES};

/// File extensions the C++ parser claims. Mirrors the Go
/// `(*CppParser).Extensions()` exactly.
pub const EXTENSIONS: &[&str] = &[".cpp", ".cc", ".cxx", ".c", ".h", ".hpp", ".hxx"];

/// C++ source-file parser. Holds the tree-sitter `Language` and the four
/// pre-compiled queries used to drive symbol/edge extraction.
///
/// Construct with [`CppParser::new`]; share across threads (queries are
/// `Send + Sync`).
///
/// `CppParser` overrides [`LanguagePlugin::preprocess`] to apply
/// [`strip_macros`] against `cfg.cpp.macro_strip` from the per-root
/// `.code-graph.toml`. With an empty list (the default), preprocessing
/// is a zero-cost `Cow::Borrowed`; with a non-empty list, listed
/// identifiers are whole-word-replaced with spaces before tree-sitter
/// parses. The replacement preserves byte offsets so symbol line/column
/// positions match the original source on disk.
pub struct CppParser {
    /// Compiled C++ grammar; held so [`tree_sitter::Parser`] instances built
    /// per `parse_file` call can attach to it without re-building the
    /// `LanguageFn`.
    language: TsLanguage,
    /// Compiled definition query.
    def_query: Query,
    /// Compiled call query.
    call_query: Query,
    /// Compiled include query.
    incl_query: Query,
    /// Compiled inheritance query.
    inh_query: Query,
}

impl CppParser {
    /// Build a new parser, compiling all four tree-sitter queries against the
    /// pinned tree-sitter-cpp grammar. Returns
    /// [`ParseError::Query`] carrying the query compiler's
    /// error message if any query fails to compile (this should not happen
    /// against the grammar version we pin in `Cargo.toml`; if it does, the
    /// error tells us which query is at fault).
    pub fn new() -> Result<Self, ParseError> {
        let language: TsLanguage = tree_sitter_cpp::LANGUAGE.into();

        let def_query = Query::new(&language, DEFINITION_QUERIES)
            .map_err(|e| ParseError::Query(format!("definition query: {e}")))?;
        let call_query = Query::new(&language, CALL_QUERIES)
            .map_err(|e| ParseError::Query(format!("call query: {e}")))?;
        let incl_query = Query::new(&language, INCLUDE_QUERIES)
            .map_err(|e| ParseError::Query(format!("include query: {e}")))?;
        let inh_query = Query::new(&language, INHERITANCE_QUERIES)
            .map_err(|e| ParseError::Query(format!("inheritance query: {e}")))?;

        Ok(Self {
            language,
            def_query,
            call_query,
            incl_query,
            inh_query,
        })
    }

    /// File extensions handled by this plugin. Mirrors the Go method of the
    /// same name. Exposed as an associated function so the trait
    /// implementation and external callers (e.g. CLI argument parsing) share
    /// the single source of truth.
    pub fn extensions() -> &'static [&'static str] {
        EXTENSIONS
    }

    /// Parse `content` (UTF-8 bytes) as C++ and produce a [`FileGraph`]. Used
    /// internally by [`Self::parse_file`] and by the inline tests; kept
    /// crate-private so the public surface stays the trait method.
    fn parse_to_filegraph(&self, path: &Path, content: &[u8]) -> Result<FileGraph, ParseError> {
        let tree = parse_tree(&self.language, content)?;
        let root = tree.root_node();
        let path_str = path.to_string_lossy().into_owned();

        let mut fg = FileGraph {
            path: path_str.clone(),
            language: Language::Cpp,
            symbols: Vec::new(),
            edges: Vec::new(),
        };

        self.extract_definitions(root, content, &path_str, &mut fg);
        self.extract_calls(root, content, &path_str, &mut fg);
        self.extract_includes(root, content, &path_str, &mut fg);
        self.extract_inheritance(root, content, &path_str, &mut fg);
        // `extract_overrides` MUST run after both `extract_definitions`
        // (so we have method Symbols to scan) and `extract_inheritance`
        // (so we have the parent-class Inherits edges to walk for base
        // names). The pass produces one `EdgeKind::Overrides` edge per
        // (override-method, base-class) pair; the resolver later
        // promotes the bare `BaseClass::methodName` target to a fully
        // qualified `path:BaseClass::methodName` symbol_id when one
        // exists in the project's symbol index.
        self.extract_overrides(&path_str, &mut fg);

        Ok(fg)
    }

    /// Synthesize `Function` symbols for `[cpp].macro_define_function`
    /// matches in `content`. For each configured macro, scan the
    /// source for invocations and produce one Symbol per match named
    /// `<captured_arg><suffix>`.
    ///
    /// Called from [`CppParser::preprocess`] which already gets the
    /// `RootConfig` reference. Done as a byte-level scan rather than
    /// a tree-sitter query because tree-sitter cannot expand `##`
    /// token-pasting — the source still shows the macro INVOCATION,
    /// not the produced function definition, so the parser would
    /// only see the macro identifier itself. The byte scanner mirrors
    /// the patterns `macro_strip_with_args` uses for the same reason.
    pub(crate) fn synthesize_macro_define_function_symbols(
        &self,
        content: &[u8],
        path: &str,
        cfg: &RootConfig,
        fg: &mut FileGraph,
    ) {
        for entry in &cfg.cpp.macro_define_function {
            if entry.name.is_empty() {
                continue;
            }
            crate::macro_define::scan_macro_invocations(content, &entry.name, |arg_text, line| {
                // Pick the requested arg index; skip silently if the
                // invocation has too few args. Match by `entry.arg`
                // 0-based.
                let arg = match arg_text.get(entry.arg) {
                    Some(a) => a.trim(),
                    None => return,
                };
                if arg.is_empty() {
                    return;
                }
                let name = format!("{}{}", arg, entry.suffix);
                fg.symbols.push(Symbol {
                    name,
                    kind: SymbolKind::Function,
                    file: path.to_string(),
                    line,
                    column: 0,
                    end_line: line,
                    signature: format!(
                        "/* synthesized by [cpp].macro_define_function: {} */",
                        entry.name
                    ),
                    namespace: String::new(),
                    parent: String::new(),
                    language: Language::Cpp,
                });
            });
        }
    }

    /// Run the definition query and produce symbols. Mirrors the Go
    /// `extractDefinitions` switch on capture name.
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

                match cap_name {
                    "func.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "function_definition")
                        else {
                            continue;
                        };
                        let ns = resolve_namespace(cap_node, content);
                        let parent_class = resolve_parent_class(cap_node, content);
                        let kind = if parent_class.is_empty() {
                            SymbolKind::Function
                        } else {
                            SymbolKind::Method
                        };
                        fg.symbols.push(make_symbol(
                            text,
                            kind,
                            path,
                            def_node,
                            content,
                            ns,
                            parent_class,
                        ));
                    }

                    "method.qname" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "function_definition")
                        else {
                            continue;
                        };
                        let (parent, method_name) = split_qualified(text);
                        let ns = resolve_namespace(cap_node, content);
                        fg.symbols.push(make_symbol(
                            &method_name,
                            SymbolKind::Method,
                            path,
                            def_node,
                            content,
                            ns,
                            parent,
                        ));
                    }

                    "class.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "class_specifier")
                        else {
                            continue;
                        };
                        let ns = resolve_namespace(cap_node, content);
                        // For nested classes, find the outer class by walking
                        // up from the class definition node itself.
                        let parent_class = resolve_parent_class(def_node, content);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Class,
                            path,
                            def_node,
                            content,
                            ns,
                            parent_class,
                        ));
                    }

                    "struct.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "struct_specifier")
                        else {
                            continue;
                        };
                        let ns = resolve_namespace(cap_node, content);
                        let parent_class = resolve_parent_class(def_node, content);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Struct,
                            path,
                            def_node,
                            content,
                            ns,
                            parent_class,
                        ));
                    }

                    "enum.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "enum_specifier") else {
                            continue;
                        };
                        let ns = resolve_namespace(cap_node, content);
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

                    "inline.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "function_definition")
                        else {
                            continue;
                        };
                        let ns = resolve_namespace(cap_node, content);
                        let parent_class = resolve_parent_class(cap_node, content);
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Method,
                            path,
                            def_node,
                            content,
                            ns,
                            parent_class,
                        ));
                    }

                    "operator.name" => {
                        let Some(def_node) = find_enclosing_kind(cap_node, "function_definition")
                        else {
                            continue;
                        };
                        let ns = resolve_namespace(cap_node, content);
                        let parent_class = resolve_parent_class(cap_node, content);
                        // Go uses KindFunction for operator overloads even
                        // when defined in-class. Preserve that quirk.
                        fg.symbols.push(make_symbol(
                            text,
                            SymbolKind::Function,
                            path,
                            def_node,
                            content,
                            ns,
                            parent_class,
                        ));
                    }

                    "typedef.name" => {
                        let def_node = find_enclosing_kind(cap_node, "type_definition")
                            .or_else(|| find_enclosing_kind(cap_node, "alias_declaration"));
                        let Some(def_node) = def_node else {
                            continue;
                        };
                        let ns = resolve_namespace(cap_node, content);
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

                    _ => {}
                }
            }
        }
    }

    /// Run the call query and produce call edges. Mirrors the Go
    /// `extractCalls`, including the cast filter and enclosing-function
    /// fallback to the bare path.
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
                if is_cpp_cast(callee) {
                    continue;
                }

                // Use enclosing call_expression for line info; fall back to
                // the capture node itself if we somehow aren't inside one.
                let call_node =
                    find_enclosing_kind(cap_node, "call_expression").unwrap_or(cap_node);
                let from = enclosing_function_id(cap_node, content, path);

                fg.edges.push(Edge {
                    from,
                    to: callee.to_owned(),
                    kind: EdgeKind::Calls,
                    file: path.to_owned(),
                    line: call_node.start_position().row as u32 + 1,
                    confidence: Confidence::Resolved,
                });
            }
        }
    }

    /// Run the include query and produce include edges. Quotes/angle brackets
    /// are stripped; otherwise this mirrors Go's `extractIncludes`.
    fn extract_includes(&self, root: Node<'_>, content: &[u8], path: &str, fg: &mut FileGraph) {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.incl_query, root, content);
        let cap_names = self.incl_query.capture_names();

        while let Some(m) = matches.next() {
            for capture in m.captures {
                let cap_node = capture.node;
                if cap_node.has_error() {
                    continue;
                }
                let cap_name = capture_name_for_index(cap_names, capture.index);
                if cap_name != "include.path" {
                    continue;
                }

                let raw = cap_node.utf8_text(content).unwrap_or("");
                let cleaned = strip_include_path(raw);

                fg.edges.push(Edge {
                    from: path.to_owned(),
                    to: cleaned,
                    kind: EdgeKind::Includes,
                    file: path.to_owned(),
                    line: cap_node.start_position().row as u32 + 1,
                    confidence: Confidence::Resolved,
                });
            }
        }
    }

    /// Run the inheritance query and produce inherits edges. Emits one edge
    /// per (derived, base) pair; mirrors Go's `extractInheritance`, including
    /// its decision to use the bare derived name (not `path:Name`) as the
    /// edge `from` and to record `line: 0` since the query does not carry a
    /// reliable single line number.
    fn extract_inheritance(&self, root: Node<'_>, content: &[u8], path: &str, fg: &mut FileGraph) {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.inh_query, root, content);
        let cap_names = self.inh_query.capture_names();

        while let Some(m) = matches.next() {
            let mut derived_name = String::new();
            let mut base_names: Vec<String> = Vec::new();

            for capture in m.captures {
                let cap_node = capture.node;
                if cap_node.has_error() {
                    continue;
                }
                let cap_name = capture_name_for_index(cap_names, capture.index);
                let text = cap_node.utf8_text(content).unwrap_or("").to_owned();

                match cap_name {
                    "derived.name" => derived_name = text,
                    "base.name" => base_names.push(text),
                    _ => {}
                }
            }

            for base in base_names {
                fg.edges.push(Edge {
                    from: derived_name.clone(),
                    to: base,
                    kind: EdgeKind::Inherits,
                    file: path.to_owned(),
                    line: 0,
                    confidence: Confidence::Resolved,
                });
            }
        }
    }

    /// Emit `EdgeKind::Overrides` edges for methods that declare
    /// `virtual` or `override` and whose enclosing class has one or
    /// more `Inherits` edges already extracted into `fg.edges`.
    ///
    /// For each (override-method, base-class) pair, emits one edge:
    /// - `from` = the override method's symbol_id (`file:Parent::name`).
    /// - `to` = `<base_class>::<method_name>` (bare two-segment form,
    ///   same shape `Calls` edges use before resolution).
    /// - `kind` = `EdgeKind::Overrides`.
    /// - `line` = the override method's declaration line.
    ///
    /// The resolver (`code_graph_tools::indexer::resolve_edges_with_indexes`)
    /// promotes the bare `to` to a fully-qualified symbol_id when a
    /// matching base method exists in the project's symbol index;
    /// otherwise the edge survives unresolved (treated as a dangling
    /// override pointer that `find_overrides` will filter out via
    /// `is_resolved_node`).
    ///
    /// Detection is conservative: a method whose `Symbol.signature`
    /// starts with `virtual ` or contains `override` (after the
    /// closing `)`) is considered an override candidate. Pure-virtual
    /// declarations (`= 0` suffix) are ALSO override candidates —
    /// they're the base method, not an override, but we don't have
    /// enough context here to distinguish, and downstream resolution
    /// drops self-loops naturally (a method's Override edge with
    /// to=`SameClass::SameMethod` resolves to itself; we skip those
    /// emitting to begin with).
    ///
    /// Signature matching for "is this an override of THAT base
    /// method" reduces to a name match here: any method in any base
    /// with the same name is a candidate. Argument-list matching
    /// would catch the C++-overload edge cases (sibling virtuals with
    /// different argument lists) but is deferred — the common case is
    /// the dominant case in engine-style code, and name-match plus
    /// the resolver's symbol-index lookup catches it.
    fn extract_overrides(&self, path: &str, fg: &mut FileGraph) {
        // First, build a quick lookup: parent_class -> [base names].
        // We can read this from the `Inherits` edges just emitted by
        // `extract_inheritance`.
        let mut bases_by_class: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for edge in &fg.edges {
            if edge.kind == EdgeKind::Inherits {
                bases_by_class
                    .entry(edge.from.clone())
                    .or_default()
                    .push(edge.to.clone());
            }
        }

        if bases_by_class.is_empty() {
            // Fast path: a file with no inheritance can't have
            // overrides. Common in C++ codebases dominated by free
            // functions or template-heavy code.
            return;
        }

        let mut new_edges: Vec<Edge> = Vec::new();
        for sym in &fg.symbols {
            // Only methods (not free functions / classes / structs)
            // can override. Methods carry a non-empty `parent` per
            // the existing extractor's convention.
            if !matches!(sym.kind, SymbolKind::Method) || sym.parent.is_empty() {
                continue;
            }
            if !is_override_candidate(&sym.signature) {
                continue;
            }
            let bases = match bases_by_class.get(&sym.parent) {
                Some(b) => b,
                None => continue, // parent class has no bases — nothing to override
            };
            let from_id = code_graph_core::symbol_id(sym);
            for base in bases {
                // Skip the self-loop case (which can happen if the
                // base name accidentally equals the parent class
                // name; defensive guard).
                if base == &sym.parent {
                    continue;
                }
                let to = format!("{}::{}", base, sym.name);
                new_edges.push(Edge {
                    from: from_id.clone(),
                    to,
                    kind: EdgeKind::Overrides,
                    file: path.to_owned(),
                    line: sym.line,
                    confidence: Confidence::Resolved,
                });
            }
        }
        fg.edges.extend(new_edges);
    }
}

/// Conservative detector for "this method declaration looks like an
/// override candidate." Matches:
/// - Signatures starting with `virtual ` (with the trailing space to
///   avoid identifiers like `virtualize`).
/// - Signatures containing `override` as a trailing decorator (any
///   substring match — `override` is a contextual keyword in C++
///   only valid AFTER the parameter list, so false positives on
///   identifier names are vanishingly unlikely in well-formed code).
///
/// Returns `false` for empty signatures and signatures that contain
/// neither keyword.
fn is_override_candidate(signature: &str) -> bool {
    if signature.is_empty() {
        return false;
    }
    if signature.starts_with("virtual ") || signature.contains(" virtual ") {
        return true;
    }
    // `override` as a substring is safe enough — see doc-comment.
    signature.contains("override")
}

/// `LanguagePlugin` implementation for C++.
///
/// The [`preprocess`](LanguagePlugin::preprocess) override forwards to
/// [`strip_macros`] with the per-root `cfg.cpp.macro_strip` list. The
/// indexer (and watch handler) call `preprocess` on the raw file bytes
/// before passing them to [`parse_file`](LanguagePlugin::parse_file), so
/// `parse_file` always sees post-substitution bytes when `macro_strip` is
/// non-empty. The override is the entry point for the CppMacroStrip
/// feature; everything else (queries, extraction loops, edge resolution)
/// runs unchanged.
impl LanguagePlugin for CppParser {
    fn id(&self) -> Language {
        Language::Cpp
    }

    fn extensions(&self) -> &'static [&'static str] {
        EXTENSIONS
    }

    fn synthesize_symbols(
        &self,
        path: &Path,
        content: &[u8],
        cfg: &RootConfig,
        fg: &mut FileGraph,
    ) {
        // Run macro-defined function synthesis on the ORIGINAL bytes
        // (NOT the post-preprocess bytes). Token-pasting macros that
        // `macro_strip` would otherwise blank still get a chance to
        // produce their symbols here.
        if cfg.cpp.macro_define_function.is_empty() {
            return;
        }
        let path_str = path.to_string_lossy().into_owned();
        self.synthesize_macro_define_function_symbols(content, &path_str, cfg, fg);
    }

    fn preprocess<'a>(&self, content: &'a [u8], cfg: &RootConfig) -> Cow<'a, [u8]> {
        // Fast-path: both lists empty -> zero-cost identity. Most non-UE
        // users will hit this branch.
        if cfg.cpp.macro_strip.is_empty() && cfg.cpp.macro_strip_with_args.is_empty() {
            return Cow::Borrowed(content);
        }
        // Pass 1: existing whole-word identifier replacement. Returns a
        // fresh Cow; on `macro_strip = []` this is Borrowed-identity (no
        // allocation), on non-empty it's Owned with the substitutions
        // applied.
        let cow = crate::preprocess::strip_macros(content, &cfg.cpp.macro_strip);
        // Short-circuit when only `macro_strip` is populated. Without this
        // guard we'd `into_owned()` the buffer (cheap if pass 1 already
        // owns it; a fresh allocation if pass 1 returned Borrowed), then
        // walk every byte calling `strip_macros_with_args` with an empty
        // token set — a guaranteed-zero-replacement O(N) scan. The outer
        // fast-path covers the "both empty" case; this covers "only
        // pass-1 has work to do."
        if cfg.cpp.macro_strip_with_args.is_empty() {
            return cow;
        }
        // Pass 2 needs `&mut [u8]`; force ownership. If pass 1 returned
        // Borrowed (macro_strip empty, macro_strip_with_args non-empty),
        // `into_owned()` allocates the buffer pass 2 will mutate — that
        // allocation has a purpose and clippy accepts it.
        let mut buf: Vec<u8> = cow.into_owned();
        // Build the owned-bytes token set per call. Tiny lists in practice
        // (UE preset is ~25 tokens); HashSet construction is amortized
        // well below tree-sitter parse cost.
        let tokens: HashSet<Vec<u8>> = cfg
            .cpp
            .macro_strip_with_args
            .iter()
            .map(|s| s.as_bytes().to_vec())
            .collect();
        crate::preprocess::strip_macros_with_args(&mut buf, &tokens);
        Cow::Owned(buf)
    }

    fn parse_file(&self, path: &Path, content: &[u8]) -> Result<FileGraph, ParseError> {
        self.parse_to_filegraph(path, content)
    }
}

/// Build a tree-sitter [`TsTree`] for `content` against the C++ grammar. The
/// caller-supplied [`TsLanguage`] is borrowed; the returned tree owns its
/// AST. Returns [`ParseError::Parse`] if `set_language` fails or if
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

/// Look up a capture name by index. Mirrors the Go
/// `(*CppParser).captureNameForIndex`. Returns `""` (empty) on out-of-range
/// indices, matching Go's silent fallback.
fn capture_name_for_index<'a>(cap_names: &[&'a str], index: u32) -> &'a str {
    cap_names.get(index as usize).copied().unwrap_or("")
}

/// Build a [`Symbol`] from a definition node. Centralizes the row/column/
/// signature math so each branch in `extract_definitions` stays small.
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
        language: Language::Cpp,
    }
}

// Compile-time interface check. Mirrors the Go
// `var _ parser.Parser = (*CppParser)(nil)` line at the top of cpp.go.
const _: fn() = || {
    fn assert_plugin<T: LanguagePlugin + ?Sized>() {}
    assert_plugin::<CppParser>();
};

#[cfg(test)]
mod tests {
    //! Structural smoke tests that rely on `CppParser` internals
    //! (`language`, `def_query`, `call_query`, etc.). Behavioral coverage
    //! lives in `tests/corpus.rs` (the full corpus).
    use super::*;

    #[test]
    fn new_compiles_all_four_queries() {
        // Every query string must parse against tree-sitter-cpp 0.23.4.
        // Failure here means a query needs updating.
        let p = CppParser::new().expect("CppParser::new must succeed");
        let _ = (
            &p.language,
            &p.def_query,
            &p.call_query,
            &p.incl_query,
            &p.inh_query,
        );
    }

    #[test]
    fn extensions_match_go_list() {
        assert_eq!(
            CppParser::extensions(),
            &[".cpp", ".cc", ".cxx", ".c", ".h", ".hpp", ".hxx"]
        );
        let p = CppParser::new().unwrap();
        assert_eq!(LanguagePlugin::extensions(&p), CppParser::extensions());
    }

    #[test]
    fn id_is_cpp() {
        let p = CppParser::new().unwrap();
        assert_eq!(p.id(), Language::Cpp);
    }

    #[test]
    fn cpp_parser_is_object_safe_via_box_dyn() {
        let p: Box<dyn LanguagePlugin> = Box::new(CppParser::new().unwrap());
        assert_eq!(p.id(), Language::Cpp);
    }

    #[test]
    fn parse_file_returns_correct_path_and_language() {
        let p = CppParser::new().unwrap();
        let path = Path::new("/tmp/test.cpp");
        let fg = p.parse_file(path, b"void foo() {}").unwrap();
        assert_eq!(fg.path, "/tmp/test.cpp");
        assert_eq!(fg.language, Language::Cpp);
        assert!(!fg.symbols.is_empty(), "extraction must populate symbols");
    }
}
