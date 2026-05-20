//! Shared test helper: a `LanguagePlugin` that records every `post_index`
//! invocation and optionally mutates the FileGraph slice via a caller-
//! supplied closure.
//!
//! Two `#[cfg(test)]` call sites consume this:
//!
//! - `crates/code-graph-tools/src/indexer.rs::tests` — proves the analyze
//!   path (`index_directory`) invokes `post_index` once over the full set
//!   of freshly-parsed FileGraphs.
//! - `crates/code-graph-tools/src/handlers/watch.rs::tests` — proves the
//!   watch path (`try_reindex_file`) invokes the same hook, and that the
//!   post-hook rewrites the hook performs survive the copy-back into
//!   `new_fg` (the mutating round-trip test).
//!
//! `id` and `exts` are parameterized so the indexer tests can keep their
//! `.fake` extension and the watch tests can use `.rec`, both with
//! `Language::Cpp`, without either site hard-coding anything in this
//! helper.

#![cfg(test)]

use std::path::Path;
use std::sync::{Arc, Mutex};

use code_graph_core::{FileGraph, Language, Symbol, SymbolKind};
use code_graph_lang::{FileIndex, LanguagePlugin, ParseError};

/// Per-invocation log: each entry is the sorted `Vec<String>` of
/// `FileGraph::path` values the hook observed for that call.
pub(crate) type Log = Arc<Mutex<Vec<Vec<String>>>>;

/// Optional caller-supplied mutator. When `Some`, `post_index` invokes the
/// closure on the FileGraph slice (and the file index) *after* recording
/// the path log. The mutating round-trip test uses this to write a
/// sentinel into the last graph's symbols.
///
/// The closure runs under `&` — multiple `post_index` invocations may
/// share one plugin — so `Fn + Send + Sync` rather than `FnMut`.
pub(crate) type Mutator = Box<dyn Fn(&mut [FileGraph], &FileIndex) + Send + Sync>;

/// Test plugin that logs every `post_index` call and (optionally) mutates
/// the FileGraph slice via a caller-supplied closure. `parse_file`
/// produces one bare `Function` symbol per file so the indexer pipeline
/// has something to walk end-to-end.
pub(crate) struct RecordingPlugin {
    id: Language,
    exts: &'static [&'static str],
    calls: Log,
    mutator: Option<Mutator>,
}

impl RecordingPlugin {
    /// Recording-only constructor: every `post_index` invocation appends
    /// to `calls`, no mutation runs. Use this when the test only needs to
    /// prove the hook fires (and over which paths).
    pub(crate) fn new(id: Language, exts: &'static [&'static str], calls: Log) -> Self {
        Self {
            id,
            exts,
            calls,
            mutator: None,
        }
    }

    /// Constructor that also runs `mutator` on the FileGraph slice after
    /// recording. Use this when the test needs to prove that mutations
    /// the hook writes into the slice survive into downstream state
    /// (e.g. the watch path's copy-back).
    pub(crate) fn with_mutator(
        id: Language,
        exts: &'static [&'static str],
        calls: Log,
        mutator: Mutator,
    ) -> Self {
        Self {
            id,
            exts,
            calls,
            mutator: Some(mutator),
        }
    }
}

impl LanguagePlugin for RecordingPlugin {
    fn id(&self) -> Language {
        self.id
    }

    fn extensions(&self) -> &'static [&'static str] {
        self.exts
    }

    fn parse_file(&self, path: &Path, _content: &[u8]) -> Result<FileGraph, ParseError> {
        // Mirror StubPlugin: one bare Function symbol per file so the
        // graph is non-empty and downstream resolution has something to
        // walk if a future test wires it in.
        let basename = path.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
        let sym_name = format!("f_{basename}");
        let file = path.to_string_lossy().into_owned();
        let symbols = vec![Symbol {
            name: sym_name.clone(),
            kind: SymbolKind::Function,
            file: file.clone(),
            line: 1,
            column: 0,
            end_line: 1,
            signature: format!("void {sym_name}()"),
            namespace: String::new(),
            parent: String::new(),
            language: self.id,
        }];
        Ok(FileGraph {
            path: file,
            language: self.id,
            symbols,
            edges: Vec::new(),
        })
    }

    fn post_index(&self, graphs: &mut [FileGraph], file_index: &FileIndex) {
        let mut paths: Vec<String> = graphs.iter().map(|g| g.path.clone()).collect();
        paths.sort();
        self.calls.lock().unwrap().push(paths);
        if let Some(m) = self.mutator.as_ref() {
            m(graphs, file_index);
        }
    }
}
