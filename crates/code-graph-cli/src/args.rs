//! clap argument surface (Designs/CommandLineInterface, "Command surface").
//!
//! Subcommand names mirror MCP tool names exactly, kebab-cased. Required
//! tool args are positionals; optional args are `--kebab-case` flags
//! spelled like their MCP argument names. **clap declares no defaults** —
//! every optional flag is an `Option<_>` that flows to `core::` as `None`,
//! so defaults, clamps, and ceilings live in one place (the core layer and
//! its thin adapter unwraps, mirrored in `exec.rs` where the adapter owns
//! the resolution).
//!
//! Optional BOOLEAN flags take an optional explicit value: `--brief` means
//! `Some(true)`, `--brief false` means `Some(false)`, absent means `None`
//! (NOT false — several tools default a bool to `true`).

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "code-graph",
    version,
    about = "Query the code graph from the terminal: same typed core, same payloads, \
             same defaults as the MCP surface. Attaches to a repository daemon when \
             one is running; answers from the on-disk cache when not."
)]
pub struct Cli {
    /// Invocation path for config/cache/daemon discovery (default: current directory)
    #[arg(long, global = true)]
    pub root: Option<PathBuf>,
    /// Machine-readable output: print the exact MCP tool payload
    #[arg(long, global = true)]
    pub json: bool,
    /// Suppress stderr breadcrumbs (fallback notices); errors still print
    #[arg(long, global = true)]
    pub quiet: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Index a codebase (synchronous; blocks until done)
    AnalyzeCodebase {
        /// Directory to index (default: --root / current directory)
        path: Option<String>,
        /// Bypass the cache and re-index from scratch
        #[arg(long)]
        force: bool,
    },
    /// Diagnostic snapshot: binary, config, indexed root, graph stats
    GetStatus,
    /// List symbols defined in one file
    GetFileSymbols {
        file: String,
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        top_level_only: Option<bool>,
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        brief: Option<bool>,
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        count_only: Option<bool>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Search symbols by name pattern, kind, namespace, language, subtree
    SearchSymbols {
        /// Substring or regex pattern to match symbol names
        query: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        namespace: Option<String>,
        #[arg(long)]
        language: Option<String>,
        #[arg(long)]
        subtree: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        offset: Option<u32>,
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        brief: Option<bool>,
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        count_only: Option<bool>,
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        near: Option<bool>,
        #[arg(long)]
        max_distance: Option<u32>,
    },
    /// Full detail for one symbol ID
    GetSymbolDetail { symbol: String },
    /// Symbol counts grouped by namespace and kind
    GetSymbolSummary {
        #[arg(long)]
        file: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        offset: Option<u32>,
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        count_only: Option<bool>,
    },
    /// Symbols enclosing a 1-based line, innermost first (span containment)
    GetSymbolAt {
        file: String,
        line: u32,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Who calls this symbol (BFS over reverse call edges)
    GetCallers {
        symbol: String,
        #[arg(long)]
        depth: Option<u32>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        offset: Option<u32>,
        #[arg(long)]
        min_confidence: Option<String>,
    },
    /// What this symbol calls (BFS over forward call edges)
    GetCallees {
        symbol: String,
        #[arg(long)]
        depth: Option<u32>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        offset: Option<u32>,
        #[arg(long)]
        min_confidence: Option<String>,
    },
    /// Methods overriding this virtual/abstract method
    FindOverrides {
        symbol: String,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Classes with this exact name (disambiguation helper)
    FindClassCandidates { name: String },
    /// Shortest call chain between two symbols
    FindPath {
        from: String,
        to: String,
        #[arg(long)]
        node_cap: Option<u32>,
        #[arg(long)]
        min_confidence: Option<String>,
    },
    /// Files this file includes/imports (resolved to indexed files)
    GetDependencies {
        file: String,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Circular file dependencies
    DetectCycles {
        #[arg(long)]
        subtree: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        offset: Option<u32>,
        #[arg(long)]
        max_cycle_size: Option<u32>,
    },
    /// Callables with zero incoming call edges
    GetOrphans {
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        subtree: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        offset: Option<u32>,
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        brief: Option<bool>,
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        count_only: Option<bool>,
        #[arg(long)]
        reliability: Option<String>,
    },
    /// Inheritance tree around a class (both directions)
    GetClassHierarchy {
        class: String,
        #[arg(long)]
        depth: Option<u32>,
        #[arg(long)]
        max_nodes: Option<u32>,
    },
    /// Call+include coupling between this file and others
    GetCoupling {
        file: String,
        /// outgoing (default), incoming, or both
        #[arg(long)]
        direction: Option<String>,
        #[arg(long)]
        offset: Option<u32>,
        #[arg(long)]
        limit: Option<u32>,
    },
    /// File-granularity community detection over the aggregated graph
    DetectCommunities {
        #[arg(long)]
        granularity: Option<String>,
        #[arg(long)]
        max_iterations: Option<u32>,
        #[arg(long)]
        members_per_community: Option<u32>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        offset: Option<u32>,
    },
    /// Call-graph / file-dep / inheritance diagram (edges JSON or Mermaid)
    GenerateDiagram {
        #[arg(long)]
        symbol: Option<String>,
        #[arg(long)]
        file: Option<String>,
        #[arg(long)]
        class: Option<String>,
        #[arg(long)]
        depth: Option<u32>,
        #[arg(long)]
        max_nodes: Option<u32>,
        /// edges (default) or mermaid
        #[arg(long)]
        format: Option<String>,
        /// callees, callers, or both (symbol mode only)
        #[arg(long)]
        direction: Option<String>,
        #[arg(long)]
        min_confidence: Option<String>,
        #[arg(long, num_args = 0..=1, default_missing_value = "true")]
        styled: Option<bool>,
    },
    /// Who last changed each line of a symbol's span
    BlameSymbol {
        symbol: String,
        /// Revision spec (default: provider default revision, e.g. git HEAD)
        #[arg(long)]
        at: Option<String>,
    },
    /// When did this symbol's CONTENT change (transitions only)
    SymbolHistory {
        symbol: String,
        /// normalized (default); literal_insensitive arrives in a later phase
        #[arg(long)]
        mode: Option<String>,
        /// Revisions to examine (default 50, max 500)
        #[arg(long)]
        window: Option<u32>,
    },
}

impl Command {
    /// The MCP tool name this subcommand mirrors, plus its `tools/call`
    /// arguments with MCP field spellings. Consumed by the daemon backend;
    /// the standalone backend calls `core::` directly (`exec.rs`).
    /// `None` values serialize as JSON `null`, which the tool arg structs
    /// deserialize back to `None` — the tool's own defaults then apply,
    /// exactly as they do for an MCP client that omits the field.
    pub fn tool_call(&self, invocation_root: &str) -> (&'static str, serde_json::Value) {
        use serde_json::json;
        match self {
            Command::AnalyzeCodebase { path, force } => (
                "analyze_codebase",
                json!({
                    "path": path.clone().unwrap_or_else(|| invocation_root.to_string()),
                    "force": force,
                }),
            ),
            Command::GetStatus => ("get_status", json!({})),
            Command::GetFileSymbols {
                file,
                top_level_only,
                brief,
                count_only,
                limit,
                offset,
            } => (
                "get_file_symbols",
                json!({
                    "file": file,
                    "top_level_only": top_level_only,
                    "brief": brief,
                    "count_only": count_only,
                    "limit": limit,
                    "offset": offset,
                }),
            ),
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
            } => (
                "search_symbols",
                json!({
                    "query": query,
                    "kind": kind,
                    "namespace": namespace,
                    "language": language,
                    "subtree": subtree,
                    "limit": limit,
                    "offset": offset,
                    "brief": brief,
                    "count_only": count_only,
                    "near": near,
                    "max_distance": max_distance,
                }),
            ),
            Command::GetSymbolDetail { symbol } => {
                ("get_symbol_detail", json!({ "symbol": symbol }))
            }
            Command::GetSymbolSummary {
                file,
                limit,
                offset,
                count_only,
            } => (
                "get_symbol_summary",
                json!({
                    "file": file,
                    "limit": limit,
                    "offset": offset,
                    "count_only": count_only,
                }),
            ),
            Command::GetSymbolAt {
                file,
                line,
                limit,
                offset,
            } => (
                "get_symbol_at",
                json!({ "file": file, "line": line, "limit": limit, "offset": offset }),
            ),
            Command::GetCallers {
                symbol,
                depth,
                limit,
                offset,
                min_confidence,
            } => (
                "get_callers",
                json!({
                    "symbol": symbol,
                    "depth": depth,
                    "limit": limit,
                    "offset": offset,
                    "min_confidence": min_confidence,
                }),
            ),
            Command::GetCallees {
                symbol,
                depth,
                limit,
                offset,
                min_confidence,
            } => (
                "get_callees",
                json!({
                    "symbol": symbol,
                    "depth": depth,
                    "limit": limit,
                    "offset": offset,
                    "min_confidence": min_confidence,
                }),
            ),
            Command::FindOverrides {
                symbol,
                limit,
                offset,
            } => (
                "find_overrides",
                json!({ "symbol": symbol, "limit": limit, "offset": offset }),
            ),
            Command::FindClassCandidates { name } => {
                ("find_class_candidates", json!({ "name": name }))
            }
            Command::FindPath {
                from,
                to,
                node_cap,
                min_confidence,
            } => (
                "find_path",
                json!({
                    "from": from,
                    "to": to,
                    "node_cap": node_cap,
                    "min_confidence": min_confidence,
                }),
            ),
            Command::GetDependencies {
                file,
                limit,
                offset,
            } => (
                "get_dependencies",
                json!({ "file": file, "limit": limit, "offset": offset }),
            ),
            Command::DetectCycles {
                subtree,
                limit,
                offset,
                max_cycle_size,
            } => (
                "detect_cycles",
                json!({
                    "subtree": subtree,
                    "limit": limit,
                    "offset": offset,
                    "max_cycle_size": max_cycle_size,
                }),
            ),
            Command::GetOrphans {
                kind,
                subtree,
                limit,
                offset,
                brief,
                count_only,
                reliability,
            } => (
                "get_orphans",
                json!({
                    "kind": kind,
                    "subtree": subtree,
                    "limit": limit,
                    "offset": offset,
                    "brief": brief,
                    "count_only": count_only,
                    "reliability": reliability,
                }),
            ),
            Command::GetClassHierarchy {
                class,
                depth,
                max_nodes,
            } => (
                "get_class_hierarchy",
                json!({ "class": class, "depth": depth, "max_nodes": max_nodes }),
            ),
            Command::GetCoupling {
                file,
                direction,
                offset,
                limit,
            } => (
                "get_coupling",
                json!({
                    "file": file,
                    "direction": direction,
                    "offset": offset,
                    "limit": limit,
                }),
            ),
            Command::DetectCommunities {
                granularity,
                max_iterations,
                members_per_community,
                limit,
                offset,
            } => (
                "detect_communities",
                json!({
                    "granularity": granularity,
                    "max_iterations": max_iterations,
                    "members_per_community": members_per_community,
                    "limit": limit,
                    "offset": offset,
                }),
            ),
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
            } => (
                "generate_diagram",
                json!({
                    "symbol": symbol,
                    "file": file,
                    "class": class,
                    "depth": depth,
                    "max_nodes": max_nodes,
                    "format": format,
                    "direction": direction,
                    "min_confidence": min_confidence,
                    "styled": styled,
                }),
            ),
            Command::BlameSymbol { symbol, at } => {
                ("blame_symbol", json!({ "symbol": symbol, "at": at }))
            }
            Command::SymbolHistory {
                symbol,
                mode,
                window,
            } => (
                "symbol_history",
                json!({ "symbol": symbol, "mode": mode, "window": window }),
            ),
        }
    }

    /// Decision 7 eligibility: query subcommands retry standalone when the
    /// daemon answers "not indexed". `analyze-codebase` routes to the
    /// daemon so the shared graph gets the analysis; `get-status` is
    /// ungated and never produces the error.
    pub fn unindexed_fallback_eligible(&self) -> bool {
        !matches!(self, Command::AnalyzeCodebase { .. } | Command::GetStatus)
    }
}
