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
//!    like function calls ARE captured as call edges. **Two opt-in config
//!    knobs recover macro-hidden definitions** (both default-empty, so the
//!    base limitation stands unless configured):
//!    - `[cpp].macro_define_function` SYNTHESIZES a bare `Function` symbol
//!      named `<arg><suffix>` for each invocation of a token-pasting macro
//!      (e.g. `IMPLEMENT_RELEASE_FN(Bar) → Bar_Release`). It does not touch
//!      the source bytes; the synthetic symbol carries a
//!      `/* synthesized by [cpp].macro_define_function: NAME */` signature
//!      marker (consulted by `get_orphans` reliability filtering).
//!    - `[cpp].macro_define_type` EXPANDS a struct/class-wrapping macro
//!      invocation IN PLACE into the native C++ it produces (byte-preserving:
//!      same length, every `\n` at its original offset), so tree-sitter
//!      parses the type natively and recovers the type symbol AND its members
//!      (methods, nested types) AND inheritance/call edges. See
//!      [`crate::macro_expand`] for the rewrite algorithm.
//!
//!    Caveats shared by both knobs: (a) **opt-in** — empty by default;
//!    (b) **body-vs-namespace scope is NOT discriminated** (a configured
//!    macro name invoked inside a function body is treated like a top-level
//!    invocation — a documented non-goal; choose macro names that don't
//!    collide with other uses, same caveat as `macro_strip`); (c) the
//!    same **raw-string-delimiter-collision** risk as `macro_strip` (a raw
//!    string whose tag equals a configured macro name can be mis-scanned).
//!    `macro_define_type` additionally requires the **keyword to FIT** in the
//!    macro-name span (`struct`/`class` is written over the leading bytes of
//!    the macro name; if longer, the invocation is skipped with a warning —
//!    engine macro names like `EXPORT_STRUCT` are long enough in practice).
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
pub(crate) mod macro_expand;
pub(crate) mod preprocess;
pub(crate) mod queries;

pub use preprocess::strip_macros;

use std::borrow::Cow;
use std::collections::HashSet;
use std::path::Path;

use code_graph_core::{
    Confidence, Edge, EdgeKind, FileGraph, Language, RootConfig, Symbol, SymbolKind,
};
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
        // Note: `extract_overrides` has moved to the whole-graph
        // [`CppParser::post_index`] hook. The per-file pass only saw
        // inheritance edges emitted from the SAME file as the override
        // method — broken for the UE-dominant pattern where the base
        // class's `class Derived : public Base {}` lives in a header and
        // the override's `void Derived::foo() override {}` body lives in
        // a `.cpp`. The post_index pass aggregates Inherits edges across
        // every FileGraph before emitting Overrides edges.

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
}

/// Whole-graph cross-file override extraction.
///
/// Aggregates `Inherits` edges and Method symbols across EVERY parsed
/// `FileGraph` before emitting `EdgeKind::Overrides` edges, so the
/// UE-dominant pattern — `class Derived : public Base { virtual void
/// foo() override; }` in a header + `void Derived::foo() {}` in a
/// `.cpp` — produces an Override edge even though the inheritance
/// edge and the override method live in different files. The per-file
/// pass (deleted in this commit) couldn't see cross-file inheritance
/// because it only inspected the calling file's own `fg.edges`.
///
/// Algorithm:
/// 1. Walk every FileGraph's symbols to build
///    `methods_by_class[class_name] = HashSet<method_name>` — every
///    method NAME defined under a class anywhere in the project.
/// 2. Walk every FileGraph's edges to build
///    `bases_by_class[derived_name] = Vec<base_name>` — direct
///    inheritance, aggregated across files. A class with `:public`
///    declared in one file and `:public` declared again in another
///    (e.g. cross-platform `#ifdef` branches) merges its bases.
/// 3. For each FileGraph and each `Method` symbol with a non-empty
///    `parent`, BFS up the inheritance ancestry of `parent` via
///    `bases_by_class`:
///    - For each ancestor class `C` such that
///      `methods_by_class[C]` contains `sym.name`, emit one Overrides
///      edge into THIS FileGraph: `from = symbol_id(sym)`,
///      `to = "C::sym.name"`. The resolver later promotes the bare
///      two-segment `to` to a fully-qualified `path:C::sym.name`
///      symbol_id via the project-wide symbol index.
///
/// **Looseness vs. strict C++ semantics.** The pre-cross-file
/// per-file pass gated emission on `is_override_candidate(sym.signature)`
/// — meaning the override method's own signature had to carry
/// `virtual` or `override`. That gate is dropped here because the
/// UE-dominant pattern keeps the `virtual`/`override` keywords in the
/// in-class declaration (a `field_declaration` in the header) which
/// the existing extractor intentionally does NOT emit as a Symbol.
/// The out-of-line definition `void Derived::foo() {}` carries
/// neither keyword. With the per-file gate intact, the cross-file
/// override pattern emits zero edges; with the gate dropped, every
/// same-named method in an inheriting class produces an Overrides
/// edge. The trade-off: non-virtual shadowing (`Base` has
/// `void Foo()` non-virtual; `Derived` declares `void Foo()` without
/// any virtual specifier anywhere) ALSO produces an Override edge —
/// strictly a false positive under C++ semantics, but vanishingly
/// rare in production code and the strictness cost of avoiding it
/// (in-class declaration extraction, schema bump) is large. The
/// existing `non_virtual_method_emits_no_override_edge` test was
/// updated accordingly.
///
/// Edge filter: skip the self-loop case `C == sym.parent`. A method
/// can't override itself. Argument-list matching (for C++ overload
/// resolution against sibling virtuals with different signatures) is
/// deferred to the resolver — the common case is name-matched.
fn extract_overrides_global(graphs: &mut [FileGraph]) {
    use std::collections::{HashMap, HashSet, VecDeque};

    // The C++ post_index hook is invoked by `index_directory` over
    // EVERY parsed FileGraph (not just C++ ones). Filter by language
    // before aggregating so a Python `class Foo(Bar)` Inherits edge
    // doesn't leak into the C++ override walk — and so this pass
    // doesn't bloat non-C++ FileGraphs with phantom Overrides edges.
    // The per-language post_index dispatch from the registry is
    // language-agnostic by design; gating happens here.

    // (1) methods_by_class: class_name -> set of method names defined
    // anywhere in the C++ subset of the project under that class.
    let mut methods_by_class: HashMap<String, HashSet<String>> = HashMap::new();
    for fg in graphs.iter() {
        if fg.language != Language::Cpp {
            continue;
        }
        for s in &fg.symbols {
            if matches!(s.kind, SymbolKind::Method) && !s.parent.is_empty() {
                methods_by_class
                    .entry(s.parent.clone())
                    .or_default()
                    .insert(s.name.clone());
            }
        }
    }

    // (2) bases_by_class: derived_name -> direct base names, aggregated
    // across every Inherits edge in the C++ subset.
    let mut bases_by_class: HashMap<String, Vec<String>> = HashMap::new();
    for fg in graphs.iter() {
        if fg.language != Language::Cpp {
            continue;
        }
        for e in &fg.edges {
            if e.kind == EdgeKind::Inherits {
                let entry = bases_by_class.entry(e.from.clone()).or_default();
                if !entry.contains(&e.to) {
                    entry.push(e.to.clone());
                }
            }
        }
    }

    // Fast path: if no C++ class has any base, no overrides are possible.
    if bases_by_class.is_empty() {
        return;
    }

    // (3) Per-file mutation: BFS ancestry per Method symbol and emit
    // Overrides edges. No signature-level `virtual`/`override` gate
    // here — see doc-comment for the rationale (cross-file UE
    // pattern keeps the keyword in the in-class declaration that's
    // intentionally not extracted as a Symbol; we accept the
    // shadowing false-positive in trade). C++ files only.
    for fg in graphs.iter_mut() {
        if fg.language != Language::Cpp {
            continue;
        }
        let path = fg.path.clone();
        let mut new_edges: Vec<Edge> = Vec::new();
        for sym in &fg.symbols {
            if !matches!(sym.kind, SymbolKind::Method) || sym.parent.is_empty() {
                continue;
            }
            // BFS up the inheritance graph starting from `sym.parent`.
            // The walk terminates when the queue drains; a `visited`
            // set guards against ill-formed cyclic inheritance.
            let mut queue: VecDeque<&String> = VecDeque::new();
            let mut visited: HashSet<String> = HashSet::new();
            if let Some(direct_bases) = bases_by_class.get(&sym.parent) {
                for b in direct_bases {
                    if visited.insert(b.clone()) {
                        queue.push_back(b);
                    }
                }
            }
            let from_id = code_graph_core::symbol_id(sym);
            while let Some(ancestor) = queue.pop_front() {
                if ancestor == &sym.parent {
                    continue;
                }
                // Emit an edge iff the ancestor class actually has a
                // method with the matching name (either in a header
                // declaration that's extracted as a body-bearing
                // method elsewhere, or in an out-of-line definition).
                // Without this gate every Method would emit edges to
                // every ancestor regardless of whether the method
                // name actually exists upstream — bloating the graph
                // with unresolvable pointers.
                if let Some(method_names) = methods_by_class.get(ancestor) {
                    if method_names.contains(&sym.name) {
                        new_edges.push(Edge {
                            from: from_id.clone(),
                            to: format!("{}::{}", ancestor, sym.name),
                            kind: EdgeKind::Overrides,
                            file: path.clone(),
                            line: sym.line,
                            confidence: Confidence::Resolved,
                        });
                    }
                }
                // Continue the walk: enqueue this ancestor's bases.
                if let Some(grand) = bases_by_class.get(ancestor) {
                    for g in grand {
                        if visited.insert(g.clone()) {
                            queue.push_back(g);
                        }
                    }
                }
            }
        }
        fg.edges.extend(new_edges);
    }
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
        // Fast-path: nothing configured -> zero-cost identity. Most non-UE
        // users will hit this branch. The expansion pass
        // (`macro_define_type`) is checked here too so an all-empty config
        // never allocates.
        if cfg.cpp.macro_strip.is_empty()
            && cfg.cpp.macro_strip_with_args.is_empty()
            && cfg.cpp.macro_define_type.is_empty()
        {
            return Cow::Borrowed(content);
        }

        // Pass 0: `macro_define_type` EXPANSION. Rewrites configured
        // struct/class-wrapping macro invocations in place into the native
        // C++ they expand to, producing a same-length owned buffer with
        // every `\n` preserved at its original offset. Runs FIRST on the
        // raw content so the subsequent strip passes see the now-revealed
        // type body (and can blank any API macro inside it). Returns `None`
        // when no configured macro matched — the borrowed content flows on
        // to the strip passes unchanged.
        let path_str: String;
        let expanded: Cow<'a, [u8]> = if cfg.cpp.macro_define_type.is_empty() {
            Cow::Borrowed(content)
        } else {
            // The expansion warns with a file path on skip; preprocess does
            // not carry the path, so use a neutral placeholder. (The
            // per-invocation warnings are diagnostic only.)
            path_str = String::from("<preprocess>");
            match crate::macro_expand::expand_macro_define_types(
                content,
                &cfg.cpp.macro_define_type,
                &path_str,
            ) {
                Some(buf) => Cow::Owned(buf),
                None => Cow::Borrowed(content),
            }
        };

        // If only the expansion pass had work to do (both strip lists
        // empty), return its result directly.
        if cfg.cpp.macro_strip.is_empty() && cfg.cpp.macro_strip_with_args.is_empty() {
            return expanded;
        }

        // Pass 1: whole-word identifier replacement (`macro_strip`), run on
        // the (possibly expanded) buffer. We materialize an owned buffer
        // here because at least one strip list is non-empty; `into_owned`
        // is a no-op move when expansion already owned, and a single
        // allocation otherwise. `strip_macros` returns Borrowed on an empty
        // `macro_strip` list, so reattach ownership.
        let mut buf: Vec<u8> =
            crate::preprocess::strip_macros(&expanded, &cfg.cpp.macro_strip).into_owned();

        // Short-circuit when only `macro_strip` is populated — avoid a
        // guaranteed-zero-replacement O(N) scan with an empty token set.
        if cfg.cpp.macro_strip_with_args.is_empty() {
            return Cow::Owned(buf);
        }

        // Pass 2: parameterized-macro replacement (`macro_strip_with_args`)
        // operates on `&mut [u8]` in place.
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

    /// Cross-file Overrides edge emission. The C++ extractor's per-file
    /// `Inherits` and Method-symbol extraction is complete by the time
    /// this hook fires (one parse_file per source file finished above);
    /// here we aggregate across every parsed FileGraph and emit
    /// Overrides edges into the file containing each override method.
    /// See [`extract_overrides_global`] for the algorithm.
    fn post_index(&self, graphs: &mut [FileGraph], _file_index: &code_graph_lang::FileIndex) {
        extract_overrides_global(graphs);
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
///
/// When the definition (or any ancestor) sits under a
/// `template_declaration` node, the signature is prefixed with the
/// `/* template */ ` sentinel that `get_orphans(reliability="very_high")`
/// uses to drop template-instantiated callables from the orphan list.
/// Methods of a templated class inherit the marker because the
/// template_declaration ancestor wraps the whole class body. The
/// prefix mirrors the existing `/* synthesized by ... */` convention,
/// keeping the wire shape unchanged (no schema bump). Applied to ALL
/// kinds, so a templated class/struct/typedef also carries the marker
/// — useful for clients filtering by signature even if the orphan
/// detector only consults the function-and-method subset.
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
    let raw_signature = truncate_signature(def_node.utf8_text(content).unwrap_or(""));
    let signature = if is_in_template_context(def_node) {
        format!("/* template */ {raw_signature}")
    } else {
        raw_signature
    };
    Symbol {
        name: name.to_owned(),
        kind,
        file: path.to_owned(),
        line: start.row as u32 + 1,
        column: start.column as u32,
        end_line: end.row as u32 + 1,
        signature,
        namespace,
        parent,
        language: Language::Cpp,
    }
}

/// Walk ancestors from `node` looking for a `template_declaration`.
/// Used by [`make_symbol`] to mark template-instantiated definitions
/// (including methods of a templated class) for the orphan filter's
/// `reliability="very_high"` tier.
fn is_in_template_context(node: Node<'_>) -> bool {
    let mut current = Some(node);
    while let Some(n) = current {
        if n.kind() == "template_declaration" {
            return true;
        }
        current = n.parent();
    }
    false
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
