//! Standalone backend (Designs/CommandLineInterface Decisions 1, 2, 4).
//!
//! Bootstraps a server the way the MCP binary does — same registry, same
//! git-provider composition — then loads the cache READ-ONLY (no analyze,
//! no mtime revalidation, no sweep, no cache write) and calls `core::`
//! typed functions with an HONEST `indexed` flag computed from whether the
//! cache actually loaded (gate artifact 17 follow-up). Machine output is
//! produced by the same `serde_json::to_string` call the MCP adapter's
//! `tool_success_json` uses, so payloads are byte-identical by
//! construction, not by test luck.
//!
//! The `unwrap_or` default resolutions in the dispatch below mirror the
//! MCP adapter layer in `server.rs` VERBATIM (`top_level_only` false,
//! `brief` true, `count_only` false, `near` false, `styled` false,
//! `force` false). They are the adapter half of this front-end; every
//! other default lives in `core::` and is reached by passing `None`.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use code_graph_core::RootConfig;
use code_graph_lang::LanguageRegistry;
use code_graph_tools::core;
use code_graph_tools::core::query::CallDirection;
use code_graph_tools::core::structure::DiagramInput;
use code_graph_tools::core::symbols::SearchInput;
use code_graph_tools::core::{ToolOk, ToolResult};
use code_graph_tools::indexer::NoopProgressSink;
use code_graph_tools::CodeGraphServer;

use crate::args::Command;
use crate::{CliError, Outcome};

/// A bootstrapped standalone application: the same server shape the MCP
/// binary builds, plus the honest indexed state.
pub struct App {
    pub server: CodeGraphServer,
    pub indexed: bool,
    /// Canonicalized invocation root (config/cache/daemon discovery base).
    pub root: PathBuf,
}

/// Builds the language registry + git provider composition the MCP binary
/// uses (`code-graph-mcp/src/main.rs`), bound at `root`.
fn make_server(root: &Path) -> Result<CodeGraphServer, CliError> {
    let mut registry = LanguageRegistry::new();
    let plugins: Vec<Box<dyn code_graph_lang::LanguagePlugin>> = vec![
        Box::new(
            code_graph_lang_cpp::CppParser::new()
                .map_err(|e| CliError::Operational(format!("initialize C++ plugin: {e}")))?,
        ),
        Box::new(
            code_graph_lang_rust::RustParser::new()
                .map_err(|e| CliError::Operational(format!("initialize Rust plugin: {e}")))?,
        ),
        Box::new(
            code_graph_lang_go::GoParser::new()
                .map_err(|e| CliError::Operational(format!("initialize Go plugin: {e}")))?,
        ),
        Box::new(
            code_graph_lang_python::PythonParser::new()
                .map_err(|e| CliError::Operational(format!("initialize Python plugin: {e}")))?,
        ),
        Box::new(
            code_graph_lang_csharp::CSharpParser::new()
                .map_err(|e| CliError::Operational(format!("initialize C# plugin: {e}")))?,
        ),
        Box::new(
            code_graph_lang_java::JavaParser::new()
                .map_err(|e| CliError::Operational(format!("initialize Java plugin: {e}")))?,
        ),
    ];
    for plugin in plugins {
        registry
            .register(plugin)
            .map_err(|e| CliError::Operational(format!("register language plugin: {e}")))?;
    }

    let mut vcs = code_graph_vcs::VcsRegistry::new();
    if let Ok(provider) = code_graph_vcs_git::GitProvider::open(root) {
        if let Err(error) = vcs.register(Box::new(provider)) {
            eprintln!("code-graph: register git provider: {error}");
        }
    }

    Ok(CodeGraphServer::with_vcs_registry(registry, vcs))
}

/// Bootstrap for query subcommands: discover the project root, load the
/// cache read-only, set the honest `indexed` flag. `load_cache = false`
/// is the `analyze-codebase` path — analyze owns its own cache handling.
pub fn bootstrap(invocation_root: &Path, load_cache: bool) -> Result<App, CliError> {
    let root = code_graph_core::paths::canonicalize(invocation_root).map_err(|e| {
        CliError::Operational(format!(
            "cannot resolve root {}: {e}",
            invocation_root.display()
        ))
    })?;
    let (config, project_root) = RootConfig::load(&root)
        .map_err(|e| CliError::Operational(format!("configuration error: {e}")))?;

    let server = make_server(&project_root)?;
    let mut indexed = false;
    if load_cache {
        // Ok(false) covers absent, version/endian-mismatched, and structurally
        // invalid caches: all "not present", all honest unindexed (exit 1 at
        // the guard). Err covers genuine I/O failures and semantic corruption
        // in an otherwise bytecheck-valid main archive: operational (exit 2).
        match server.inner.graph.write().load(&project_root) {
            Ok(loaded) => indexed = loaded,
            Err(error) => {
                return Err(CliError::Operational(format!(
                    "cache unreadable under {}: {error}",
                    project_root.display()
                )))
            }
        }
    }
    *server.inner.config.write() = config;
    *server.inner.root_path.write() = Some(root.clone());
    *server.inner.cache_root.write() = Some(project_root);
    server.inner.indexed.store(indexed, Ordering::Release);

    Ok(App {
        server,
        indexed,
        root,
    })
}

/// Maps a typed core result onto the CLI outcome classes: `Value` becomes
/// the exact MCP payload (`serde_json::to_string`, compact — the same
/// serialization `tool_success_json` performs), `Text` passes through
/// verbatim, `ToolError` carries the same text the MCP error flag would.
fn out<T: serde::Serialize>(r: ToolResult<T>) -> Result<Outcome, CliError> {
    match r {
        Ok(ToolOk::Value(v)) => Ok(Outcome::Json(serde_json::to_string(&v).unwrap_or_default())),
        Ok(ToolOk::Text(s)) => Ok(Outcome::Text(s)),
        Err(e) => Err(CliError::Tool(e.0)),
    }
}

/// Standalone dispatch: every arm calls exactly the `core::` function the
/// MCP adapter calls, with the same argument resolution and the honest
/// indexed state required by `code_graph_tools::core::require_indexed`.
pub async fn run(app: &App, command: &Command) -> Result<Outcome, CliError> {
    let inner = &app.server.inner;
    let indexed = app.indexed;
    let max_bytes = inner.config.read().response.max_bytes;

    match command {
        Command::AnalyzeCodebase { path, force } => {
            let path = path
                .clone()
                .unwrap_or_else(|| app.root.to_string_lossy().into_owned());
            out(core::analyze::analyze_codebase(
                Arc::clone(inner),
                path,
                *force,
                Arc::new(NoopProgressSink),
            )
            .await)
        }
        Command::GetStatus => out(core::status::get_status(Arc::clone(inner))),
        Command::GetFileSymbols {
            file,
            top_level_only,
            brief,
            count_only,
            limit,
            offset,
        } => out(core::symbols::get_file_symbols(
            &inner.graph,
            indexed,
            file,
            top_level_only.unwrap_or(false),
            brief.unwrap_or(true),
            *limit,
            *offset,
            count_only.unwrap_or(false),
            max_bytes,
        )),
        Command::SearchSymbols {
            query,
            kind,
            namespace,
            language,
            subtree,
            limit,
            offset,
            brief,
            count_only,
            near,
            max_distance,
        } => out(core::symbols::search_symbols(
            &inner.graph,
            indexed,
            SearchInput {
                query: query.as_deref(),
                kind: kind.as_deref(),
                namespace: namespace.as_deref(),
                language: language.as_deref(),
                subtree: subtree.as_deref(),
                limit: *limit,
                offset: *offset,
                brief: brief.unwrap_or(true),
                count_only: count_only.unwrap_or(false),
                near: near.unwrap_or(false),
                max_distance: *max_distance,
            },
            max_bytes,
        )),
        Command::GetSymbolDetail { symbol } => out(core::symbols::get_symbol_detail(
            &inner.graph,
            indexed,
            symbol,
        )),
        Command::GetSymbolSummary {
            file,
            limit,
            offset,
            count_only,
        } => out(core::symbols::get_symbol_summary(
            &inner.graph,
            indexed,
            file.as_deref(),
            *limit,
            *offset,
            count_only.unwrap_or(false),
            max_bytes,
        )),
        Command::GetSymbolAt {
            file,
            line,
            limit,
            offset,
        } => out(core::symbols::get_symbol_at(
            &inner.graph,
            indexed,
            file,
            *line,
            *limit,
            *offset,
            max_bytes,
        )),
        Command::GetCallers {
            symbol,
            depth,
            limit,
            offset,
            min_confidence,
        } => out(core::query::callers_or_callees(
            &inner.graph,
            indexed,
            symbol,
            *depth,
            CallDirection::Callers,
            *limit,
            *offset,
            max_bytes,
            min_confidence.as_deref(),
        )),
        Command::GetCallees {
            symbol,
            depth,
            limit,
            offset,
            min_confidence,
        } => out(core::query::callers_or_callees(
            &inner.graph,
            indexed,
            symbol,
            *depth,
            CallDirection::Callees,
            *limit,
            *offset,
            max_bytes,
            min_confidence.as_deref(),
        )),
        Command::FindOverrides {
            symbol,
            limit,
            offset,
        } => out(core::query::find_overrides(
            &inner.graph,
            indexed,
            symbol,
            *limit,
            *offset,
            max_bytes,
        )),
        Command::FindClassCandidates { name } => out(core::structure::find_class_candidates(
            &inner.graph,
            indexed,
            name,
        )),
        Command::FindPath {
            from,
            to,
            node_cap,
            min_confidence,
        } => out(core::query::find_path(
            &inner.graph,
            indexed,
            from,
            to,
            *node_cap,
            min_confidence.as_deref(),
        )),
        Command::GetDependencies {
            file,
            limit,
            offset,
        } => out(core::query::get_dependencies(
            &inner.graph,
            indexed,
            file,
            *limit,
            *offset,
            max_bytes,
        )),
        Command::DetectCycles {
            subtree,
            limit,
            offset,
            max_cycle_size,
        } => out(core::structure::detect_cycles(
            &inner.graph,
            indexed,
            subtree.as_deref(),
            *limit,
            *offset,
            *max_cycle_size,
        )),
        Command::GetOrphans {
            kind,
            subtree,
            limit,
            offset,
            brief,
            count_only,
            reliability,
        } => out(core::structure::get_orphans(
            &inner.graph,
            indexed,
            kind.as_deref(),
            subtree.as_deref(),
            *limit,
            *offset,
            *brief,
            count_only.unwrap_or(false),
            reliability.as_deref(),
            max_bytes,
        )),
        Command::GetClassHierarchy {
            class,
            depth,
            max_nodes,
        } => out(core::structure::get_class_hierarchy(
            &inner.graph,
            indexed,
            class,
            *depth,
            *max_nodes,
        )),
        Command::GetCoupling {
            file,
            direction,
            offset,
            limit,
        } => out(core::structure::get_coupling(
            &inner.graph,
            indexed,
            file,
            direction.as_deref(),
            *offset,
            *limit,
            max_bytes,
        )),
        Command::DetectCommunities {
            granularity,
            max_iterations,
            members_per_community,
            limit,
            offset,
        } => out(core::structure::detect_communities(
            &inner.graph,
            indexed,
            granularity.as_deref(),
            *max_iterations,
            *members_per_community,
            *limit,
            *offset,
            max_bytes,
        )),
        Command::GenerateDiagram {
            symbol,
            file,
            class,
            depth,
            max_nodes,
            format,
            direction,
            min_confidence,
            styled,
        } => out(core::structure::generate_diagram(
            &inner.graph,
            indexed,
            DiagramInput {
                symbol: symbol.as_deref(),
                file: file.as_deref(),
                class: class.as_deref(),
                depth: *depth,
                max_nodes: *max_nodes,
                format: format.as_deref(),
                styled: styled.unwrap_or(false),
                direction: direction.as_deref(),
                min_confidence: min_confidence.as_deref(),
            },
        )),
        Command::BlameSymbol { symbol, at } => {
            let root = inner.root_path.read().clone();
            out(core::history::blame_symbol(
                &inner.graph,
                &inner.vcs,
                indexed,
                root,
                symbol,
                at.as_deref(),
            )
            .await)
        }
        Command::SymbolHistory {
            symbol,
            mode,
            window,
        } => out(
            core::history::symbol_history(inner, indexed, symbol, mode.as_deref(), *window).await,
        ),
    }
}
