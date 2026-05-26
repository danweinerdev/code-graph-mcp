//! Indexer: per-job rayon parsing pool, language-aware edge resolution, and
//! progress reporting bridge.
//!
//! This module orchestrates the discover → parse → resolve pipeline for the
//! `analyze_codebase` MCP tool. The flow is:
//!
//! 1. [`super::discovery::discover`] enumerates source files.
//! 2. [`index_directory`] spins up a per-job `rayon::ThreadPool` sized by
//!    `cfg.parsing.max_threads`, calls
//!    `LanguagePlugin::parse_file` in parallel inside that pool, and reports
//!    progress through a [`ProgressSink`].
//! 3. [`resolve_all_edges`] walks every produced [`FileGraph`] and rewrites
//!    `Calls` and `Includes` edges via the per-language
//!    `LanguagePlugin::resolve_call` / `LanguagePlugin::resolve_include`
//!    default impls, using a `(Language, name)`-keyed [`SymbolIndex`] so a
//!    Python `init` is never returned for a C++ caller.
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
//! `analyze_codebase` handler owns the receiver and forwards
//! each event to `peer.notify_progress`. When the blocking job ends, the
//! sender drops and the receiver task exits cleanly.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use code_graph_core::{symbol_id, EdgeKind, FileGraph, RootConfig};
use code_graph_lang::{CallContext, FileIndex, LanguageRegistry, SymbolEntry, SymbolIndex};
use rayon::iter::{IntoParallelRefIterator, IntoParallelRefMutIterator, ParallelIterator};
use rayon::ThreadPoolBuildError;

use crate::discovery;

/// Aggregate result of an indexing pass.
///
/// The `analyze_codebase` handler composes this from the values
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
/// `progress` is monotonic non-decreasing **within a phase** and resets
/// when the worker crosses a phase boundary (parse counts `1..=total`,
/// then resolve resets and counts `1..=total` again). Phase identity
/// rides on `message`: `Parsing: <path>` during parse, `Resolving: <path>`
/// during resolve, etc. Consumers asserting monotonicity must scope to
/// a single phase via the `message` prefix. `total` is the file count
/// discovered before parsing began (`0` indicates an indeterminate
/// phase). `message` is a human-readable status line — for the parse
/// loop it has the form `Parsing: <absolute-path>`.
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
    let discovered = discovery::discover(root, registry, cfg, progress);
    let mut warnings = discovered.warnings.clone();

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(cfg.parsing.max_threads)
        .thread_name(|i| format!("code-graph-parse-{i}"))
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
                let mut fg = plugin
                    .parse_file(&df.path, &cleaned)
                    .map_err(|e| format!("{}: parse error: {e}", df.path.display()))?;
                // Per-file post-parse synthesis hook. Sees the ORIGINAL
                // bytes (NOT the preprocessed `cleaned`) so language
                // plugins running secondary extractors over source
                // structure that the preprocess pass would have rewritten
                // (e.g. `[cpp].macro_define_function` invocations that
                // `macro_strip` could otherwise blank) can append the
                // synthesized symbols.
                plugin.synthesize_symbols(&df.path, &content, cfg, &mut fg);
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

    // Whole-graph post-parse pass. `index_directory` has no cache merge —
    // on every analyze re-index the returned `graphs` already contains the
    // complete set of freshly-parsed `FileGraph`s, so the build-once
    // `FileIndex` covers the full file world the post-pass needs. Plugins
    // with no crate-aware work inherit the trait's no-op default.
    let file_index = build_file_index(&graphs);
    for plugin in registry.plugins() {
        plugin.post_index(&mut graphs, &file_index);
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
    extend_symbol_index(&mut index, graphs);
    index
}

/// Layer additional `graphs` onto an existing [`SymbolIndex`] without
/// rebuilding from scratch. Used by the project-wide-merge path to
/// build a single resolver index spanning both the cached project
/// graph and the freshly-parsed in-scope graphs: the handler first
/// calls [`build_symbol_index`] against the cache snapshot, then
/// extends with the fresh `FileGraph`s.
///
/// Entries are pushed under the same four keys as `build_symbol_index`
/// (bare name; `Parent::Name`; `Namespace::Name`;
/// `Namespace::Parent::Name`). Duplicate keys accumulate in the
/// `Vec<SymbolEntry>` value — the resolver's scope-aware heuristic
/// disambiguates at lookup time, so layering does not need to
/// distinguish "cached" from "fresh" at index-build time.
pub fn extend_symbol_index(index: &mut SymbolIndex, graphs: &[FileGraph]) {
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
            push(index, fg.language, &s.name, entry.clone());

            // Parent::Name for methods.
            if !s.parent.is_empty() {
                let qualified = format!("{}::{}", s.parent, s.name);
                push(index, fg.language, &qualified, entry.clone());
            }

            // Namespace::Name and Namespace::Parent::Name.
            if !s.namespace.is_empty() {
                let ns_qualified = format!("{}::{}", s.namespace, s.name);
                push(index, fg.language, &ns_qualified, entry.clone());
                if !s.parent.is_empty() {
                    let full = format!("{}::{}::{}", s.namespace, s.parent, s.name);
                    push(index, fg.language, &full, entry.clone());
                }
            }
        }
    }
}

/// Build the basename → absolute-path file index used by the include
/// resolver. Mirrors the Go `buildFileIndex` in `internal/tools/analyze.go`.
///
/// One entry per discovered file path; multiple paths sharing a basename
/// all live in the same `Vec`, and the resolver disambiguates at lookup
/// time via suffix matching.
pub fn build_file_index(graphs: &[FileGraph]) -> FileIndex {
    let mut index = FileIndex::new();
    extend_file_index(&mut index, graphs);
    index
}

/// Layer additional `graphs` onto an existing [`FileIndex`] without
/// rebuilding from scratch. Counterpart to [`extend_symbol_index`]
/// used by the project-wide-merge path.
pub fn extend_file_index(index: &mut FileIndex, graphs: &[FileGraph]) {
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
}

fn push(index: &mut SymbolIndex, lang: code_graph_core::Language, key: &str, entry: SymbolEntry) {
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
/// scope-aware / basename heuristics from `code-graph-lang`. Plugins that
/// override the defaults (e.g. a future Python plugin doing dotted-import
/// resolution) get free dispatch through this function.
///
/// `Inherits` edges are left alone (the bare derived class name is the
/// canonical form). Unknown edge kinds and edges from files whose plugin
/// is no longer in the registry are silently skipped.
///
/// `progress` receives one event per file in the form
/// `(n, total, "Resolving edges: <path>")`, mirroring the per-file
/// granularity of the parse loop. Files whose plugin is missing from
/// the registry still count toward the progress counter so the total
/// stays consistent with `graphs.len()`.
pub fn resolve_all_edges(
    graphs: &mut [FileGraph],
    registry: &LanguageRegistry,
    progress: &dyn ProgressSink,
) {
    let symbol_index = build_symbol_index(graphs);
    let file_index = build_file_index(graphs);
    resolve_edges_with_indexes(graphs, &symbol_index, &file_index, registry, progress);
}

/// Resolve edges using PRE-BUILT `symbol_index` / `file_index` instead
/// of indexes derived from `graphs` alone.
///
/// Used by the project-wide-merge path in `analyze_codebase`: the
/// handler builds the indexes from the union of cached symbols (from a
/// prior invocation that touched a sibling subtree) and freshly-parsed
/// symbols (from the current invocation's scope), then resolves only
/// the FRESH edges against that combined index. Net effect: an edge
/// from a fresh file to a symbol cached during an earlier invocation
/// resolves correctly even though the cached symbol's source was not
/// re-parsed this time.
///
/// **Asymmetry by design.** Cached EDGES (from prior invocations) are
/// not re-resolved here — only their endpoints contribute to the
/// indexes. A cached edge from a previously-indexed file that
/// originally failed to resolve (because the target subtree wasn't
/// indexed yet) does NOT spontaneously resolve when the target subtree
/// later becomes part of the cache; the user would need
/// `force=true` at the originating subtree to re-parse and re-resolve.
/// This asymmetry keeps the resolve phase's cost bounded by the size
/// of `graphs` (the fresh set), not the size of the full project, and
/// matches the lazy/scoped indexing contract — out-of-scope state
/// updates lazily, never in the background.
pub fn resolve_edges_with_indexes(
    graphs: &mut [FileGraph],
    symbol_index: &SymbolIndex,
    file_index: &FileIndex,
    registry: &LanguageRegistry,
    progress: &dyn ProgressSink,
) {
    let total = graphs.len() as u32;
    let counter = AtomicU32::new(0);

    // Parallelized per-file. Per-file resolution only mutates the
    // current `fg.edges` and reads from the shared (immutable)
    // `symbol_index`, `file_index`, and `registry`. `LanguagePlugin`
    // already carries `Send + Sync` (the parse pool relies on it) and
    // `ProgressSink` is `Send + Sync` by trait bound, so the closure
    // captures everything it needs without lock or clone. On a
    // 72k-file, 4.7M-edge codebase this halves the resolve phase
    // wall-time vs the prior single-threaded loop. Progress events
    // arrive in rayon-worker-completion order rather than file-index
    // order — already the contract for the parse phase (see
    // `index_directory`), so the channel sink is shape-compatible.
    graphs.par_iter_mut().for_each(|fg| {
        let n = counter.fetch_add(1, Ordering::Relaxed) + 1;
        progress.report(n, total, &format!("Resolving edges: {}", fg.path));

        let plugin = match registry.plugin_for(fg.language) {
            Some(p) => p,
            None => return,
        };
        let path_for_ctx = PathBuf::from(&fg.path);
        fg.edges.retain_mut(|edge| {
            match edge.kind {
                EdgeKind::Includes => {
                    match plugin.resolve_include(&edge.to, file_index) {
                        Some((resolved, confidence))
                            if registry.language_for_path(&resolved).is_some() =>
                        {
                            edge.to = resolved.to_string_lossy().into_owned();
                            edge.confidence = confidence;
                        }
                        // Unresolved, or resolved to a non-source target: this
                        // include does not point at an indexed source file
                        // (system headers like `stdio.h`, external paths never
                        // in the FileIndex, config files like `.ini`/`.cfg`,
                        // plain `.txt`). It is not a graph edge — drop it
                        // rather than leak a raw/unresolvable string into the
                        // dependency graph. Not logged: this fires constantly
                        // in real C++ codebases and would flood stderr.
                        _ => return false,
                    }
                }
                EdgeKind::Calls => {
                    let ctx = CallContext {
                        caller_id: &edge.from,
                        caller_file: &path_for_ctx,
                        language: fg.language,
                    };
                    if let Some((id, confidence)) =
                        plugin.resolve_call(&edge.to, &ctx, symbol_index)
                    {
                        edge.to = id;
                        edge.confidence = confidence;
                    }
                    // Unresolved bare-token calls keep their pre-resolve
                    // `Confidence::Resolved` mark; they're filtered at
                    // BFS time via `is_resolved_node` and never surface
                    // to agent queries, so the confidence on them is
                    // observable only through cache introspection.
                }
                // Bare derived class names are the canonical form; the
                // graph engine resolves them to a concrete class node only
                // at hierarchy-query time.
                EdgeKind::Inherits => {}
                EdgeKind::Overrides => {
                    // Override edges share `resolve_call`'s lookup
                    // mechanism: both target a method by
                    // `Parent::name`-shaped bare token and benefit
                    // from the same scope-aware resolver. An
                    // unresolved Override edge survives with its
                    // bare `to` — `find_overrides` filters via
                    // `is_resolved_node` so unresolved edges don't
                    // surface to the agent.
                    let ctx = CallContext {
                        caller_id: &edge.from,
                        caller_file: &path_for_ctx,
                        language: fg.language,
                    };
                    if let Some((id, confidence)) =
                        plugin.resolve_call(&edge.to, &ctx, symbol_index)
                    {
                        edge.to = id;
                        edge.confidence = confidence;
                    }
                }
                _ => {}
            }
            true
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_graph_core::{
        Confidence, DiscoveryConfig, Edge, FileGraph, Language, ParsingConfig, RootConfig, Symbol,
        SymbolKind,
    };
    use code_graph_lang::{LanguagePlugin, LanguageRegistry, ParseError};
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
            code_graph_lang_cpp::CppParser::new().expect("CppParser::new"),
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
        // Phase markers from discovery (2 events) + per-file parse events
        // (n events). Discovery emits a "Discovering files..." start event
        // and a "Discovered N files" end event before parsing begins.
        let discover_start = events
            .iter()
            .filter(|e| e.message == "Discovering files...")
            .count();
        let discover_end = events
            .iter()
            .filter(|e| e.message.starts_with("Discovered "))
            .count();
        let parse_events: Vec<&ProgressEvent> = events
            .iter()
            .filter(|e| e.message.starts_with("Parsing: "))
            .collect();
        assert_eq!(discover_start, 1, "one 'Discovering files...' event");
        assert_eq!(discover_end, 1, "one 'Discovered N files' event");
        assert_eq!(
            parse_events.len(),
            n as usize,
            "one parse event per parsed file"
        );
        // The last reported parse progress must equal the total. Order of
        // intermediate parse events is rayon-dependent; only the high-water
        // mark is guaranteed.
        let max_progress = parse_events.iter().map(|e| e.progress).max().unwrap();
        assert_eq!(max_progress, n);
        for e in parse_events.iter() {
            assert_eq!(e.total, n);
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
                    confidence: Confidence::Resolved,
                },
                Edge {
                    from: format!("{main_path}:main"),
                    // Bare callee name — should resolve to the symbol ID.
                    to: "do_thing".to_string(),
                    kind: EdgeKind::Calls,
                    file: main_path.clone(),
                    line: 6,
                    confidence: Confidence::Resolved,
                },
            ],
        };

        let mut graphs = vec![header, main];
        let reg = cpp_only_registry();
        resolve_all_edges(&mut graphs, &reg, &NoopProgressSink);

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
        // Both edges had a sole candidate (one `foo.h`, one `do_thing`)
        // — the resolver must mark them Resolved, not Heuristic. A
        // regression that flipped the resolver to "always Heuristic
        // when downgrading from raw token" would show up here.
        assert_eq!(
            include_edge.confidence,
            Confidence::Resolved,
            "sole-candidate include must stay Resolved"
        );
        assert_eq!(
            call_edge.confidence,
            Confidence::Resolved,
            "sole-candidate call must stay Resolved"
        );
    }

    /// Multi-candidate call resolution downgrades the edge to
    /// [`Confidence::Heuristic`]. The fixture stages two C++ functions
    /// named `helper` in separate files and a `caller` that invokes
    /// `helper`; the scope rule picks the same-file one, but the
    /// confidence ride-along must mark it Heuristic regardless.
    /// This is the end-to-end through-the-resolve-loop counterpart
    /// of the unit-level
    /// `default_scope_aware_resolve_picks_same_file_over_global`
    /// assertion in `code-graph-lang`.
    #[test]
    fn resolve_all_edges_marks_multi_candidate_call_heuristic() {
        fn func_sym(name: &str, file: &str) -> Symbol {
            Symbol {
                name: name.to_string(),
                kind: SymbolKind::Function,
                file: file.to_string(),
                line: 1,
                column: 0,
                end_line: 1,
                signature: format!("void {name}()"),
                namespace: String::new(),
                parent: String::new(),
                language: Language::Cpp,
            }
        }
        let path_a = "/proj/a.cpp".to_string();
        let path_b = "/proj/b.cpp".to_string();
        let a = FileGraph {
            path: path_a.clone(),
            language: Language::Cpp,
            symbols: vec![func_sym("helper", &path_a), func_sym("caller", &path_a)],
            edges: vec![Edge {
                from: format!("{path_a}:caller"),
                to: "helper".to_string(),
                kind: EdgeKind::Calls,
                file: path_a.clone(),
                line: 2,
                confidence: Confidence::Resolved,
            }],
        };
        let b = FileGraph {
            path: path_b.clone(),
            language: Language::Cpp,
            symbols: vec![func_sym("helper", &path_b)],
            edges: Vec::new(),
        };
        let mut graphs = vec![a, b];
        let reg = cpp_only_registry();
        resolve_all_edges(&mut graphs, &reg, &NoopProgressSink);

        let call_edge = graphs
            .iter()
            .find(|g| g.path == path_a)
            .and_then(|g| g.edges.iter().find(|e| matches!(e.kind, EdgeKind::Calls)))
            .expect("caller's Calls edge must survive resolve");
        assert_eq!(
            call_edge.to,
            format!("{path_a}:helper"),
            "scope rule picks same-file candidate"
        );
        assert_eq!(
            call_edge.confidence,
            Confidence::Heuristic,
            "multi-candidate match must be marked Heuristic — the per-tool \
             min_confidence filter relies on this"
        );
    }

    /// An include whose resolved target is not a file any language plugin
    /// claims (here a `.ini` config file: the StubPlugin owns only `.cpp`
    /// and `.h`) must be dropped during edge resolution. The sibling
    /// include to a real `.h` survives. The watch reindex path
    /// (`handlers/watch.rs::try_reindex_file`) applies the textually
    /// identical `registry.language_for_path(...).is_none()` filter on its
    /// own copy of this loop; it has no separately-testable resolve seam,
    /// so this indexer-layer test is the canonical regression target.
    #[test]
    fn resolve_all_edges_drops_include_to_non_source_target() {
        // The .ini file gets its own FileGraph so build_file_index puts its
        // basename in the FileIndex — that's what makes the default
        // basename resolver return Some(.../config.ini), which the new
        // language_for_path filter must then reject.
        let ini_path = "/proj/config/config.ini".to_string();
        let ini = FileGraph {
            path: ini_path.clone(),
            language: Language::Cpp,
            symbols: Vec::new(),
            edges: Vec::new(),
        };
        let header_path = "/proj/inc/sibling.h".to_string();
        let header = FileGraph {
            path: header_path.clone(),
            language: Language::Cpp,
            symbols: Vec::new(),
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
                    from: main_path.clone(),
                    // Resolves (basename) to /proj/config/config.ini, whose
                    // extension no plugin claims → must be dropped.
                    to: "config.ini".to_string(),
                    kind: EdgeKind::Includes,
                    file: main_path.clone(),
                    line: 1,
                    confidence: Confidence::Resolved,
                },
                Edge {
                    from: main_path.clone(),
                    // Resolves to the real .h source → must survive.
                    to: "sibling.h".to_string(),
                    kind: EdgeKind::Includes,
                    file: main_path.clone(),
                    line: 2,
                    confidence: Confidence::Resolved,
                },
            ],
        };

        let mut graphs = vec![ini, header, main];
        let reg = cpp_only_registry();
        resolve_all_edges(&mut graphs, &reg, &NoopProgressSink);

        let main_after = graphs.iter().find(|g| g.path == main_path).unwrap();
        let include_edges: Vec<&Edge> = main_after
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Includes))
            .collect();
        assert_eq!(
            include_edges.len(),
            1,
            "only the .h include must survive; got: {:?}",
            include_edges.iter().map(|e| &e.to).collect::<Vec<_>>()
        );
        assert_eq!(
            include_edges[0].to, header_path,
            "the surviving include must be the resolved .h path"
        );
        assert!(
            !main_after.edges.iter().any(|e| e.to.ends_with(".ini")),
            "no include edge may point at a .ini target"
        );

        // End-to-end: the filtered .ini must not reach Graph::includes
        // either. This is the whole reason the filter physically removes
        // the edge rather than just skipping the rewrite —
        // merge_file_graph pushes every surviving Includes edge's target
        // into the graph unconditionally, so a survived-but-unrewritten
        // edge would still leak in. Asserting on FileGraph.edges alone
        // would not catch a regression that re-introduced that leak.
        use code_graph_graph::Graph;
        let mut g = Graph::new();
        g.merge_file_graph(main_after.clone());
        let deps = g.file_dependencies(Path::new(&main_path));
        assert!(
            deps.iter()
                .any(|d| d.path.as_path() == Path::new(&header_path)),
            "resolved .h include must reach Graph::includes: {deps:?}"
        );
        assert!(
            !deps
                .iter()
                .any(|d| d.path.to_string_lossy().ends_with(".ini")),
            "filtered .ini include must NOT reach Graph::includes: {deps:?}"
        );
    }

    // -- post_index hook -------------------------------------------------

    use crate::test_recording_plugin::RecordingPlugin;
    use std::sync::Arc;

    /// Analyze-path call site: `index_directory` must invoke
    /// `post_index` exactly once, over the full set of freshly-parsed
    /// FileGraphs, before returning. Covers the "every analyze re-index
    /// runs the hook over the complete graph set" contract that
    /// crate-aware plugins (e.g. Rust's namespace rewrite) rely on.
    #[test]
    fn index_directory_invokes_post_index_over_full_graph_set() {
        let dir = TempDir::new().unwrap();
        // Three files so a missed iteration or a per-file-instead-of-
        // whole-set call would be observable.
        for i in 0..3 {
            touch(
                &dir.path().join(format!("p{i}.fake")),
                b"// placeholder content\n",
            );
        }

        let calls = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let mut reg = LanguageRegistry::new();
        reg.register(Box::new(RecordingPlugin::new(
            Language::Cpp,
            &[".fake"],
            Arc::clone(&calls),
        )))
        .unwrap();

        let cfg = cfg_with_threads(2);
        let sink = NoopProgressSink;
        let (graphs, warnings) = index_directory(dir.path(), &reg, &cfg, &sink).unwrap();
        assert_eq!(graphs.len(), 3, "all three files parsed");
        assert!(warnings.is_empty(), "no parse warnings: {warnings:?}");

        let log = calls.lock().unwrap();
        assert_eq!(
            log.len(),
            1,
            "post_index must fire exactly once per index_directory call, got {} invocations",
            log.len()
        );
        let observed = &log[0];
        assert_eq!(
            observed.len(),
            3,
            "post_index must see all three freshly-parsed FileGraphs: {observed:?}"
        );
        for i in 0..3 {
            let want = dir
                .path()
                .join(format!("p{i}.fake"))
                .to_string_lossy()
                .into_owned();
            assert!(
                observed.iter().any(|p| p == &want),
                "post_index must include {want:?}; got {observed:?}"
            );
        }
    }

    /// Non-Rust plugins inherit the trait's no-op `post_index` default.
    /// This guards against a regression where the default body silently
    /// starts mutating the FileGraph slice, which would corrupt every
    /// other language's indexed output without the changed behavior being
    /// visible at the call site.
    #[test]
    fn index_directory_default_post_index_does_not_mutate_graphs() {
        let dir = TempDir::new().unwrap();
        touch(&dir.path().join("a.cpp"), b"void a() {}\n");
        touch(&dir.path().join("b.cpp"), b"void b() {}\n");

        let reg = cpp_only_registry();
        let cfg = cfg_with_threads(2);
        let sink = NoopProgressSink;
        let (graphs, _warnings) = index_directory(dir.path(), &reg, &cfg, &sink).unwrap();

        // The stub plugin uses the default no-op `post_index`. Symbols
        // and edges must match what `parse_file` produced — no rewrites
        // sneaking in.
        assert_eq!(graphs.len(), 2);
        for g in &graphs {
            assert_eq!(
                g.symbols.len(),
                1,
                "each stub file produces exactly one symbol"
            );
            assert!(g.edges.is_empty(), "stub edges must be empty post-hook");
            // The provisional empty namespace from StubPlugin survives.
            assert_eq!(g.symbols[0].namespace, "");
        }
    }
}
