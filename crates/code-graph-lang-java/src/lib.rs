//! Java language plugin for code-graph-mcp.
//!
//! Uses tree-sitter (via the `tree-sitter` and `tree-sitter-java` crates)
//! to extract symbols, calls, import edges, and inheritance edges from
//! `.java` source files.
//!
//! # Phase status
//!
//! Phase 3.1 ships the crate scaffold: dependency wiring, query-string
//! placeholders that compile against tree-sitter-java 0.23.5, the
//! `JavaParser` struct with cached `Query` objects, and the
//! `LanguagePlugin` impl. `parse_to_filegraph` returns an empty
//! `FileGraph` until 3.2 wires the first extractor — the Phase 3.1
//! acceptance gate is structural (object-safe, `id()` returns
//! `Language::Java`), not behavioral.
//!
//! Phases 3.2-3.5 wire the per-extractor methods (definitions, calls,
//! imports, inheritance). Phase 3.6 adds testdata + corpus + watch +
//! commons-lang dogfood. Phase 3.7 runs the structural gates and writes
//! the phase debrief.
//!
//! # Default trait methods
//!
//! `JavaParser` does NOT (yet) override [`LanguagePlugin::resolve_call`]
//! or [`LanguagePlugin::resolve_include`]. The default scope-aware
//! heuristic and basename-import resolver are the contract until a
//! later phase decides otherwise. Java imports record the dotted path
//! verbatim per Decision 7 of the design, so basename resolution will
//! be a near-no-op once 3.4 lands — same situation as Python's dotted
//! module paths.

pub(crate) mod helpers;
pub(crate) mod queries;

use std::path::Path;

use code_graph_core::{FileGraph, Language};
use code_graph_lang::{LanguagePlugin, ParseError};
use tree_sitter::{Language as TsLanguage, Parser as TsParser, Query, Tree as TsTree};

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
//
// 3.1 NOTE: `language` and the four `Query` fields are cached but not
// yet read by `parse_to_filegraph` — the extractors that consume them
// land in 3.2-3.5. The `#[allow(dead_code)]` annotation is a
// one-task-only suppression: 3.2 wires `def_query` into
// `extract_definitions` and removes this annotation as part of that
// task. The same applies field-by-field through 3.5.
#[allow(dead_code)]
pub struct JavaParser {
    /// Compiled Java grammar. Held so per-call [`tree_sitter::Parser`]
    /// instances built inside `parse_file` can attach to it without
    /// rebuilding the `LanguageFn`.
    language: TsLanguage,
    /// Compiled definition query (wired in 3.2).
    def_query: Query,
    /// Compiled call query (wired in 3.3).
    call_query: Query,
    /// Compiled import query (wired in 3.4).
    import_query: Query,
    /// Compiled inheritance query (wired in 3.5).
    inheritance_query: Query,
}

impl JavaParser {
    /// Build a new parser, compiling all four tree-sitter queries against
    /// the pinned tree-sitter-java grammar. Returns an [`anyhow::Error`]
    /// (wrapping the query compiler's message) if any query fails to
    /// compile against the pinned grammar version.
    ///
    /// In Phase 3.1 every query string is empty (`""`), so the compile
    /// step is trivially successful — what matters is that the wiring
    /// is in place for 3.2-3.5 to fill in real query bodies and have
    /// the same compile gate catch malformed queries early.
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
    /// while each per-extractor method (`extract_definitions`, etc.) can
    /// be tested via `parse_file` without exposing them. Mirrors the
    /// Python plugin's `parse_to_filegraph` indirection.
    ///
    /// In Phase 3.1 this returns an empty `FileGraph` — the parse tree
    /// is built (proving the grammar attaches and parses successfully)
    /// but no extractors run. 3.2 wires the first extractor here.
    fn parse_to_filegraph(&self, path: &Path, content: &[u8]) -> Result<FileGraph, ParseError> {
        let _tree = parse_tree(&self.language, content)?;
        let path_str = path.to_string_lossy().into_owned();

        let fg = FileGraph {
            path: path_str,
            language: Language::Java,
            symbols: Vec::new(),
            edges: Vec::new(),
        };

        // Extractors land in 3.2-3.5. The 3.2 implementer should rebind
        // `_tree` to `tree` and pull `let root = tree.root_node();`,
        // then call:
        //   self.extract_definitions(root, content, &path_str, &mut fg);
        //   self.extract_calls(root, content, &path_str, &mut fg);
        //   self.extract_imports(root, content, &path_str, &mut fg);
        //   self.extract_inheritance(root, content, &path_str, &mut fg);

        Ok(fg)
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
    /// Phase 3.1 returns an empty graph (path + language populated);
    /// Phases 3.2-3.5 wire the four extractors.
    fn parse_file(&self, path: &Path, content: &[u8]) -> Result<FileGraph, ParseError> {
        self.parse_to_filegraph(path, content)
    }

    // resolve_call and resolve_include intentionally NOT overridden in
    // 3.1 — see the crate-level docstring for the rationale (default
    // heuristic + basename resolver match the C++/Rust/Go/Python
    // plugins). Later phases may override if the dogfood baseline shows
    // resolution drift.

    fn close(&self) {}
}

/// Build a tree-sitter [`TsTree`] for `content` against the Java grammar.
/// The caller-supplied [`TsLanguage`] is borrowed; the returned tree owns
/// its AST. Returns [`ParseError::Parse`] if `set_language` fails or if
/// tree-sitter declines to produce a tree (e.g. on cancellation). Mirrors
/// `parse_tree` in the C++/Rust/Go/Python plugins byte-for-byte modulo
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

#[cfg(test)]
mod tests {
    use super::*;
    use code_graph_lang::LanguagePlugin;

    #[test]
    fn parser_is_object_safe_and_id_returns_java() {
        let p: Box<dyn LanguagePlugin> = Box::new(JavaParser::new().unwrap());
        assert_eq!(p.id(), Language::Java);
    }
}
