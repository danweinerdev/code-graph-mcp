//! C# language plugin for code-graph-mcp.
//!
//! Uses tree-sitter (via the `tree-sitter` and `tree-sitter-c-sharp`
//! crates) to extract symbols, calls, import edges, and inheritance edges
//! from `.cs` source files.
//!
//! # Phase status
//!
//! Phase 2.1 ships the crate scaffold: dependency wiring, empty query
//! string constants in [`queries`] that compile against tree-sitter-c-sharp
//! 0.23.5, the [`CSharpParser`] struct with cached `Query` objects, and
//! the [`LanguagePlugin`] impl. `parse_file` returns a fresh empty
//! [`FileGraph`] tagged with [`Language::CSharp`] and the input path —
//! every extractor lands in 2.2-2.5.
//!
//! Phase 2.2 will wire `extract_definitions` (classes, structs,
//! interfaces, enums, methods, constructors, partial classes, default
//! interface methods, extension methods).
//!
//! Phase 2.3 will wire `extract_calls` (direct, member-access, chained,
//! null-conditional, lambda body, LINQ, `new`/constructor).
//!
//! Phase 2.4 will wire `extract_imports` (`using`, `using static`,
//! `using A = X.Y`, `global using`, `using` inside namespace blocks).
//!
//! Phase 2.5 will wire `extract_inheritance` (`base_list` for classes,
//! structs, and interfaces — both class extension and interface
//! implementation produce the same [`EdgeKind::Inherits`] per Decision 2).
//!
//! # Default trait methods
//!
//! `CSharpParser` does NOT override [`LanguagePlugin::resolve_call`] or
//! [`LanguagePlugin::resolve_include`].
//!
//! - `resolve_call`: the default scope-aware heuristic (same file > same
//!   parent > same namespace > global) is the documented contract,
//!   mirroring the four shipped plugins. Extension method calls
//!   (`myString.CountWords()`) resolve through this same path with the
//!   same imperfection class as C++ overloaded-function resolution
//!   (Decision 5).
//! - `resolve_include`: C# imports (`using System.Collections.Generic`)
//!   are dotted namespace paths, not filesystem paths — the default
//!   basename-match resolver returns `None` for them, mirroring the
//!   Python plugin's approach to dotted module-path imports.
//!
//! # C#-specific notes (preview for Phases 2.2-2.5)
//!
//! - **Default interface methods** are distinguished from abstract ones
//!   by **presence of a method body**, not by a `default` keyword.
//!   `interface I { void Foo() { ... } }` produces a Symbol;
//!   `interface I { void Bar(); }` does not (forward-declaration rule —
//!   Decision 11). Verify against the actual grammar in 2.2.
//! - **Partial classes** (Decision 3) emit one Class symbol per
//!   declaration; merging across files is deferred to hierarchy-walk time
//!   via the bare-name `from`-field rule.
//! - **Extension methods** are syntactic methods of their enclosing
//!   static class — the `this` modifier on the first parameter does NOT
//!   remap the parent (Decision 5).

pub(crate) mod helpers;
pub(crate) mod queries;

use std::path::Path;

use code_graph_core::{FileGraph, Language};
use code_graph_lang::{LanguagePlugin, ParseError};
use tree_sitter::{Language as TsLanguage, Query};

use crate::queries::{CALL_QUERIES, DEFINITION_QUERIES, IMPORT_QUERIES, INHERITANCE_QUERIES};

/// File extensions the C# parser claims. Single extension `.cs` —
/// C# does not have a stub-file analogue (no `.pyi`-equivalent), and
/// `.csx` script files use the same grammar but are out of scope for
/// this plan.
pub const EXTENSIONS: &[&str] = &[".cs"];

/// C# source-file parser. Holds the tree-sitter `Language` and the four
/// pre-compiled queries used to drive symbol/edge extraction in Phases
/// 2.2-2.5.
///
/// Construct with [`CSharpParser::new`]; share across threads (queries
/// are `Send + Sync`).
//
// 2.1 NOTE: `language` and the four `Query` fields are cached but not
// yet read by `parse_to_filegraph` — the extractors that consume them
// land in 2.2-2.5. The `#[allow(dead_code)]` annotation is a
// one-task-only suppression: 2.2 wires `def_query` into
// `extract_definitions` and removes this annotation as part of that
// task. The same applies field-by-field through 2.5.
#[allow(dead_code)]
pub struct CSharpParser {
    /// Compiled C# grammar. Held so per-call [`tree_sitter::Parser`]
    /// instances built inside `parse_file` can attach to it without
    /// rebuilding the `LanguageFn`.
    language: TsLanguage,
    /// Compiled definition query (wired in 2.2).
    def_query: Query,
    /// Compiled call query (wired in 2.3).
    call_query: Query,
    /// Compiled import query (wired in 2.4).
    import_query: Query,
    /// Compiled inheritance query (wired in 2.5).
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
    /// sharp 0.23.5. The 2.1 query strings are all empty placeholders —
    /// empty queries compile to a no-op against any grammar — so the
    /// gate is trivially satisfied until Phase 2.2 fills them in.
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
    /// while each per-extractor method (the upcoming 2.2/2.3/2.4/2.5
    /// extractors) can be tested via `parse_file` without exposing them.
    /// Mirrors the Python plugin's `parse_to_filegraph` indirection.
    ///
    /// Phase 2.1 stub: returns an empty [`FileGraph`] tagged with
    /// [`Language::CSharp`] and the input path. The four `extract_*`
    /// calls land in 2.2-2.5; until then, parsing any C# file yields no
    /// symbols and no edges. The empty graph is correct as a default
    /// (no false positives) and keeps the trait contract satisfied so
    /// the dispatch path can be exercised end-to-end without waiting on
    /// the extractors.
    fn parse_to_filegraph(&self, path: &Path, _content: &[u8]) -> Result<FileGraph, ParseError> {
        Ok(FileGraph {
            path: path.to_string_lossy().into_owned(),
            language: Language::CSharp,
            symbols: Vec::new(),
            edges: Vec::new(),
        })
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
    ///
    /// Phase 2.1 ships the empty-graph stub; Phases 2.2/2.3/2.4/2.5 wire
    /// the definition, call, import, and inheritance extractors.
    fn parse_file(&self, path: &Path, content: &[u8]) -> Result<FileGraph, ParseError> {
        self.parse_to_filegraph(path, content)
    }

    // resolve_call and resolve_include intentionally NOT overridden — see
    // the crate-level docstring for the rationale (default heuristic
    // matches the C++/Rust/Go/Python plugins; default basename resolver
    // is a no-op for C#'s dotted namespace `using` paths, which is the
    // intended behavior).
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_graph_lang::LanguagePlugin;

    #[test]
    fn parser_is_object_safe_and_id_returns_csharp() {
        let p: Box<dyn LanguagePlugin> = Box::new(CSharpParser::new().unwrap());
        assert_eq!(p.id(), Language::CSharp);
    }
}
