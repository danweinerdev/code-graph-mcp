//! Indexer: per-job rayon parsing pool, language-aware edge resolution, and
//! progress reporting bridge.
//!
//! This module orchestrates the discover → parse → resolve pipeline for the
//! `analyze_codebase` MCP tool. The flow is:
//!
//! 1. [`super::discovery::discover`] enumerates source files (Phase 3.2).
//! 2. [`index_directory`] spins up a per-job `rayon::ThreadPool` sized by
//!    `cfg.parsing.max_threads`, calls
//!    `LanguagePlugin::parse_file` in parallel inside that pool, and reports
//!    progress through a [`ProgressSink`] (Phase 3.3).
//! 3. [`resolve_all_edges`] walks every produced [`FileGraph`] and rewrites
//!    `Calls` and `Includes` edges via the per-language
//!    `LanguagePlugin::resolve_call` / `LanguagePlugin::resolve_include`
//!    default impls, using a `(Language, name)`-keyed [`SymbolIndex`] so a
//!    Python `init` is never returned for a C++ caller (Phase 3.3).
//!
//! Per-job pool, not the global rayon pool — `analyze_codebase` runs other
//! work (search, BFS) concurrently, and we don't want one analyze to
//! monopolize rayon's process-wide pool.
//!
//! ## Progress notifications across the rayon ↔ tokio boundary
//!
//! The rayon parse pool runs inside `tokio::task::spawn_blocking`, so it
//! cannot directly `await peer.notify_progress(...)`. The
//! [`ChannelProgressSink`] takes a `tokio::sync::mpsc::Sender<ProgressEvent>`
//! and pushes events from rayon worker threads via `try_send` (best-effort:
//! a full channel drops events rather than blocking the parser pool). The
//! `analyze_codebase` handler — Phase 3.4 — owns the receiver and forwards
//! each event to `peer.notify_progress`. When the blocking job ends, the
//! sender drops and the receiver task exits cleanly.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use codegraph_core::{symbol_id, EdgeKind, FileGraph, RootConfig};
use codegraph_lang::{CallContext, FileIndex, LanguageRegistry, SymbolEntry, SymbolIndex};
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use rayon::ThreadPoolBuildError;

use crate::discovery;

/// Aggregate result of an indexing pass.
///
/// Phase 3.4's `analyze_codebase` handler will compose this from the values
/// returned by [`index_directory`] and [`resolve_all_edges`].
#[derive(Debug, Clone)]
pub struct IndexResult {
    pub files: u32,
    pub symbols: u32,
    pub edges: u32,
    pub root_path: PathBuf,
    pub warnings: Vec<String>,
}

/// Errors returned by [`index_directory`]. Per-file failures (read error,
/// parse error, unregistered language) become entries in the returned
/// `warnings` `Vec` rather than propagating — only catastrophic failures
/// (e.g. failed to construct the rayon pool) bubble up here.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("rayon pool init failed: {0}")]
    PoolInit(ThreadPoolBuildError),
}

/// One progress notification emitted by the indexer's parse loop.
///
/// `progress` is a monotonic per-job counter; `total` is the total file
/// count discovered before parsing began (`0` indicates an indeterminate
/// phase). `message` is a human-readable status line — for the parse loop
/// it has the form `Parsing: <absolute-path>`.
#[derive(Debug, Clone)]
pub struct ProgressEvent {
    pub progress: u32,
    pub total: u32,
    pub message: String,
}

/// Reports incremental progress from long-running indexing work.
///
/// `report` is called from rayon worker threads after each file finishes
/// parsing. Implementations must be `Send + Sync` and must not block — the
/// canonical implementation, [`ChannelProgressSink`], uses `try_send` and
/// silently drops events on a full channel.
pub trait ProgressSink: Send + Sync {
    /// Report progress as `progress` of `total` units complete with a
    /// human-readable status message.
    fn report(&self, progress: u32, total: u32, message: &str);
}

/// Sink that forwards events to a `tokio::sync::mpsc::Sender` via
/// `try_send`. Used by the `analyze_codebase` handler to bridge the rayon
/// (sync) parse pool to the tokio (async) MCP notification stream.
///
/// `try_send` drops events on a full channel rather than blocking the
/// rayon worker — progress is best-effort by design.
pub struct ChannelProgressSink(pub tokio::sync::mpsc::Sender<ProgressEvent>);

impl ProgressSink for ChannelProgressSink {
    fn report(&self, progress: u32, total: u32, message: &str) {
        let _ = self.0.try_send(ProgressEvent {
            progress,
            total,
            message: message.to_string(),
        });
    }
}

/// No-op sink. Used by tests, the parse-test binary, and any caller that
/// doesn't care about progress.
pub struct NoopProgressSink;

impl ProgressSink for NoopProgressSink {
    fn report(&self, _: u32, _: u32, _: &str) {}
}

/// Discover and parse every source file under `root` in parallel.
///
/// Returns the per-file [`FileGraph`]s plus a flat `Vec<String>` of
/// warnings (discovery walk errors, per-file read/parse failures, unknown
/// languages). Catastrophic errors — currently only failure to construct
/// the rayon pool — bubble up as [`IndexError`].
///
/// The pool is built per-call and dropped before returning, so this never
/// touches rayon's global pool. Pool size is `cfg.parsing.max_threads`,
/// which the caller must have already resolved via
/// [`RootConfig::resolve_concurrency`].
pub fn index_directory(
    root: &Path,
    registry: &LanguageRegistry,
    cfg: &RootConfig,
    progress: &dyn ProgressSink,
) -> Result<(Vec<FileGraph>, Vec<String>), IndexError> {
    let discovered = discovery::discover(root, registry, &cfg.discovery);
    let mut warnings = discovered.warnings.clone();

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(cfg.parsing.max_threads)
        .thread_name(|i| format!("codegraph-parse-{i}"))
        .build()
        .map_err(IndexError::PoolInit)?;

    let total = discovered.files.len() as u32;
    let counter = AtomicU32::new(0);

    let results: Vec<Result<FileGraph, String>> = pool.install(|| {
        discovered
            .files
            .par_iter()
            .map(|df| {
                let plugin = registry.plugin_for(df.language).ok_or_else(|| {
                    format!(
                        "{}: no plugin for language {:?}",
                        df.path.display(),
                        df.language
                    )
                })?;
                let content = std::fs::read(&df.path)
                    .map_err(|e| format!("{}: read error: {e}", df.path.display()))?;
                let cleaned = plugin.preprocess(&content, cfg);
                let fg = plugin
                    .parse_file(&df.path, &cleaned)
                    .map_err(|e| format!("{}: parse error: {e}", df.path.display()))?;
                let n = counter.fetch_add(1, Ordering::Relaxed) + 1;
                progress.report(n, total, &format!("Parsing: {}", df.path.display()));
                Ok(fg)
            })
            .collect()
    });

    let mut graphs = Vec::with_capacity(results.len());
    for r in results {
        match r {
            Ok(fg) => graphs.push(fg),
            Err(w) => warnings.push(w),
        }
    }
    Ok((graphs, warnings))
}

/// Build the `(Language, name)` → [`SymbolEntry`] inverted index over a
/// slice of [`FileGraph`]s.
///
/// Mirrors the Go `buildSymbolIndex` in `internal/tools/analyze.go`,
/// indexing each symbol under multiple keys so the resolver can match
/// callees written as bare names, `Parent::Name`, `Namespace::Name`, or
/// the fully-qualified `Namespace::Parent::Name`. The Rust port differs
/// from Go in one place: the lookup is keyed by `(Language, name)` rather
/// than `name` alone, so cross-language collisions are impossible.
pub fn build_symbol_index(graphs: &[FileGraph]) -> SymbolIndex {
    let mut index = SymbolIndex::new();
    for fg in graphs {
        for s in &fg.symbols {
            let id = symbol_id(s);
            let entry = SymbolEntry {
                id,
                file: PathBuf::from(&s.file),
                parent: s.parent.clone(),
                namespace: s.namespace.clone(),
            };

            // Bare name.
            push(&mut index, fg.language, &s.name, entry.clone());

            // Parent::Name for methods.
            if !s.parent.is_empty() {
                let qualified = format!("{}::{}", s.parent, s.name);
                push(&mut index, fg.language, &qualified, entry.clone());
            }

            // Namespace::Name and Namespace::Parent::Name.
            if !s.namespace.is_empty() {
                let ns_qualified = format!("{}::{}", s.namespace, s.name);
                push(&mut index, fg.language, &ns_qualified, entry.clone());
                if !s.parent.is_empty() {
                    let full = format!("{}::{}::{}", s.namespace, s.parent, s.name);
                    push(&mut index, fg.language, &full, entry.clone());
                }
            }
        }
    }
    index
}

/// Build the basename → absolute-path file index used by the include
/// resolver. Mirrors the Go `buildFileIndex` in `internal/tools/analyze.go`.
///
/// One entry per discovered file path; multiple paths sharing a basename
/// all live in the same `Vec`, and the resolver disambiguates at lookup
/// time via suffix matching.
pub fn build_file_index(graphs: &[FileGraph]) -> FileIndex {
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

fn push(index: &mut SymbolIndex, lang: codegraph_core::Language, key: &str, entry: SymbolEntry) {
    index
        .by_name
        .entry((lang, key.to_string()))
        .or_default()
        .push(entry);
}

/// Walk every edge in `graphs` and rewrite `Calls` and `Includes` edges
/// using the per-language resolvers.
///
/// Dispatches via `LanguagePlugin::resolve_call` /
/// `LanguagePlugin::resolve_include`, both of which default to the
/// scope-aware / basename heuristics from `codegraph-lang`. Plugins that
/// override the defaults (e.g. a future Python plugin doing dotted-import
/// resolution) get free dispatch through this function.
///
/// `Inherits` edges are left alone (the bare derived class name is the
/// canonical form). Unknown edge kinds and edges from files whose plugin
/// is no longer in the registry are silently skipped.
pub fn resolve_all_edges(graphs: &mut [FileGraph], registry: &LanguageRegistry) {
    let symbol_index = build_symbol_index(graphs);
    let file_index = build_file_index(graphs);

    for fg in graphs.iter_mut() {
        let plugin = match registry.plugin_for(fg.language) {
            Some(p) => p,
            None => continue,
        };
        let path_for_ctx = PathBuf::from(&fg.path);
        for edge in &mut fg.edges {
            match edge.kind {
                EdgeKind::Includes => {
                    if let Some(resolved) = plugin.resolve_include(&edge.to, &file_index) {
                        edge.to = resolved.to_string_lossy().into_owned();
                    }
                }
                EdgeKind::Calls => {
                    let ctx = CallContext {
                        caller_id: &edge.from,
                        caller_file: &path_for_ctx,
                        language: fg.language,
                    };
                    if let Some(id) = plugin.resolve_call(&edge.to, &ctx, &symbol_index) {
                        edge.to = id;
                    }
                }
                // Bare derived class names are the canonical form; the
                // graph engine resolves them to a concrete class node only
                // at hierarchy-query time.
                EdgeKind::Inherits => {}
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::{
        DiscoveryConfig, Edge, FileGraph, Language, ParsingConfig, RootConfig, Symbol, SymbolKind,
    };
    use codegraph_lang::{LanguagePlugin, LanguageRegistry, ParseError};
    use std::fs;
    use std::path::Path;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Test plugin: claims a fixed extension list and produces one symbol
    /// per file (named `f_<basename>`) plus a single bogus call edge so we
    /// can exercise resolve_all_edges. The C++ parser would do the heavy
    /// lifting in production; the stub keeps the test fast and isolated
    /// from tree-sitter quirks.
    struct StubPlugin {
        id: Language,
        exts: &'static [&'static str],
    }

    impl LanguagePlugin for StubPlugin {
        fn id(&self) -> Language {
            self.id
        }
        fn extensions(&self) -> &'static [&'static str] {
            self.exts
        }
        fn parse_file(&self, path: &Path, _content: &[u8]) -> Result<FileGraph, ParseError> {
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
    }

    fn cpp_only_registry() -> LanguageRegistry {
        let mut reg = LanguageRegistry::new();
        reg.register(Box::new(StubPlugin {
            id: Language::Cpp,
            exts: &[".cpp", ".h"],
        }))
        .unwrap();
        reg
    }

    fn touch(path: &Path, content: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn cfg_with_threads(n: usize) -> RootConfig {
        RootConfig {
            discovery: DiscoveryConfig {
                max_threads: n,
                ..Default::default()
            },
            parsing: ParsingConfig { max_threads: n },
            ..Default::default()
        }
    }

    /// Sink that records every reported event for inspection.
    struct VecSink(Mutex<Vec<ProgressEvent>>);

    impl ProgressSink for VecSink {
        fn report(&self, progress: u32, total: u32, message: &str) {
            self.0.lock().unwrap().push(ProgressEvent {
                progress,
                total,
                message: message.to_string(),
            });
        }
    }

    /// Build a registry wired to the real C++ parser. Used to exercise the
    /// indexer end-to-end against tree-sitter rather than the StubPlugin.
    fn real_cpp_registry() -> LanguageRegistry {
        let mut reg = LanguageRegistry::new();
        reg.register(Box::new(
            codegraph_lang_cpp::CppParser::new().expect("CppParser::new"),
        ))
        .unwrap();
        reg
    }

    #[test]
    fn index_directory_processes_all_cpp_files() {
        // Real CppParser, real tree-sitter — proves the indexer handles the
        // production parse path, not just the StubPlugin happy path.
        let dir = TempDir::new().unwrap();
        for i in 0..5 {
            touch(
                &dir.path().join(format!("f{i}.cpp")),
                format!("void f{i}() {{}}\n").as_bytes(),
            );
        }

        let reg = real_cpp_registry();
        let cfg = cfg_with_threads(2);
        let sink = NoopProgressSink;
        let (graphs, warnings) = index_directory(dir.path(), &reg, &cfg, &sink).unwrap();
        assert_eq!(graphs.len(), 5, "expected 5 graphs, got {}", graphs.len());
        assert!(
            warnings.is_empty(),
            "expected no warnings, got: {warnings:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn index_directory_surfaces_read_errors_as_warnings() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        touch(&dir.path().join("readable.cpp"), b"// readable\n");
        let unreadable = dir.path().join("unreadable.cpp");
        touch(&unreadable, b"// unreadable\n");
        // chmod 000: no read.
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();

        let running_as_privileged = fs::read(&unreadable).is_ok();

        let reg = cpp_only_registry();
        let cfg = cfg_with_threads(2);
        let sink = NoopProgressSink;
        let result = index_directory(dir.path(), &reg, &cfg, &sink);

        // Restore permissions before any assertion so tempdir cleanup works.
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644)).unwrap();

        if running_as_privileged {
            eprintln!(
                "index_directory_surfaces_read_errors_as_warnings: skipped (privileged user)"
            );
            return;
        }

        let (graphs, warnings) = result.unwrap();
        // The readable file produced a graph; the unreadable did not.
        assert_eq!(
            graphs.len(),
            1,
            "expected only the readable graph: {:?}",
            graphs.iter().map(|g| g.path.clone()).collect::<Vec<_>>()
        );
        assert!(graphs[0].path.ends_with("readable.cpp"));
        // And a warning surfaces for the unreadable one.
        assert!(
            warnings.iter().any(|w| w.contains("read error")),
            "expected a 'read error' warning, got: {warnings:?}"
        );
    }

    #[test]
    fn index_directory_progress_events_reach_total() {
        let dir = TempDir::new().unwrap();
        let n = 7u32;
        for i in 0..n {
            touch(&dir.path().join(format!("p{i}.cpp")), b"// noop\n");
        }

        let reg = cpp_only_registry();
        let cfg = cfg_with_threads(3);
        let sink = VecSink(Mutex::new(Vec::new()));
        let (graphs, warnings) = index_directory(dir.path(), &reg, &cfg, &sink).unwrap();
        assert_eq!(graphs.len(), n as usize);
        assert!(warnings.is_empty());

        let events = sink.0.lock().unwrap();
        assert_eq!(events.len(), n as usize, "one event per parsed file");
        // The last reported progress must equal the total. Order of
        // intermediate events is rayon-dependent; only the high-water mark
        // is guaranteed.
        let max_progress = events.iter().map(|e| e.progress).max().unwrap();
        assert_eq!(max_progress, n);
        for e in events.iter() {
            assert_eq!(e.total, n);
            assert!(e.message.starts_with("Parsing: "));
        }
    }

    #[test]
    fn noop_sink_does_not_panic() {
        let sink = NoopProgressSink;
        // 100 calls; nothing should panic, no state change visible.
        for i in 0..100 {
            sink.report(i, 100, "ignored");
        }
    }

    #[test]
    fn channel_sink_drops_on_full_channel() {
        // capacity=1, so after one buffered event the next try_send returns
        // Err and we drop the event silently.
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            let (tx, _rx) = tokio::sync::mpsc::channel::<ProgressEvent>(1);
            let sink = ChannelProgressSink(tx);
            // 10 try_sends. The first succeeds, subsequent ones drop; none
            // panic.
            for i in 0..10 {
                sink.report(i, 10, "msg");
            }
        });
    }

    #[test]
    fn resolve_all_edges_resolves_includes_and_calls_in_place() {
        // Build two FileGraphs with raw, unresolved edges. After
        // resolve_all_edges, the include edge points at the absolute path
        // and the call edge points at the symbol ID.
        let header_path = "/proj/inc/foo.h".to_string();
        let header = FileGraph {
            path: header_path.clone(),
            language: Language::Cpp,
            symbols: vec![Symbol {
                name: "do_thing".to_string(),
                kind: SymbolKind::Function,
                file: header_path.clone(),
                line: 1,
                column: 0,
                end_line: 1,
                signature: "void do_thing()".to_string(),
                namespace: String::new(),
                parent: String::new(),
                language: Language::Cpp,
            }],
            edges: Vec::new(),
        };
        let main_path = "/proj/src/main.cpp".to_string();
        let main = FileGraph {
            path: main_path.clone(),
            language: Language::Cpp,
            symbols: vec![Symbol {
                name: "main".to_string(),
                kind: SymbolKind::Function,
                file: main_path.clone(),
                line: 5,
                column: 0,
                end_line: 7,
                signature: "int main()".to_string(),
                namespace: String::new(),
                parent: String::new(),
                language: Language::Cpp,
            }],
            edges: vec![
                Edge {
                    from: format!("{main_path}:main"),
                    // Bare include text — basename should resolve to the
                    // header's absolute path.
                    to: "foo.h".to_string(),
                    kind: EdgeKind::Includes,
                    file: main_path.clone(),
                    line: 1,
                },
                Edge {
                    from: format!("{main_path}:main"),
                    // Bare callee name — should resolve to the symbol ID.
                    to: "do_thing".to_string(),
                    kind: EdgeKind::Calls,
                    file: main_path.clone(),
                    line: 6,
                },
            ],
        };

        let mut graphs = vec![header, main];
        let reg = cpp_only_registry();
        resolve_all_edges(&mut graphs, &reg);

        // Locate the rewritten edges on the main graph.
        let main_after = graphs.iter().find(|g| g.path == main_path).unwrap();
        let include_edge = main_after
            .edges
            .iter()
            .find(|e| matches!(e.kind, EdgeKind::Includes))
            .unwrap();
        assert_eq!(
            include_edge.to, header_path,
            "include must resolve to header's absolute path"
        );
        let call_edge = main_after
            .edges
            .iter()
            .find(|e| matches!(e.kind, EdgeKind::Calls))
            .unwrap();
        assert_eq!(
            call_edge.to,
            format!("{header_path}:do_thing"),
            "call must resolve to symbol ID"
        );
    }
}
