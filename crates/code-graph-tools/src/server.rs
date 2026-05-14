//! MCP server that exposes the code graph as 15 rmcp tools over stdio.
//!
//! Phase 3.1 ships the scaffold: [`CodeGraphServer`] with all 15 tools wired
//! through `#[tool_router]` plus the `ServerInner` state struct that future
//! tasks will read from. Every tool handler currently returns
//! `McpError::internal_error("not yet implemented (Phase 3.X)", None)` —
//! Phase 3.4 fills in the eight P0 handlers and Phase 3.5 fills in the
//! remaining seven.
//!
//! ## Tool wire format
//!
//! Tool descriptions are copied byte-for-byte from the Go reference at
//! `internal/tools/tools.go` for 13 of the 15 tools. Two are updated for
//! the multi-language Rust port:
//!
//! - `analyze_codebase`: widened from "Index a C/C++ codebase…" to "Index a
//!   codebase (C/C++, Rust, Go, Python, C#, Java) and build the code graph.
//!   Must be called before any query tools."
//! - `search_symbols`: keeps the existing description but gains a `language`
//!   parameter ("Filter by source language: cpp, rust, go, python, csharp, or
//!   java").
//!
//! Phase 3.7 captures these strings as wire-format snapshots; any future
//! divergence triggers `cargo insta review`.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use code_graph_core::RootConfig;
use code_graph_graph::Graph;
use code_graph_lang::LanguageRegistry;
use notify_debouncer_full::notify::RecommendedWatcher;
use notify_debouncer_full::{Debouncer, RecommendedCache};
use parking_lot::RwLock as PlRwLock;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, Meta};
use rmcp::service::RoleServer;
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, Peer, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::oneshot;
use tokio::sync::Mutex as TokioMutex;

use crate::handlers;

/// Active filesystem-watcher state stored on [`ServerInner::watch`].
///
/// Owns the running [`Debouncer`] (drop to stop the underlying
/// `RecommendedWatcher` on the OS) and a [`oneshot::Sender`] used to ask the
/// async watch_loop task to exit cleanly. Constructed by
/// [`crate::handlers::watch::watch_start`]; consumed by
/// [`crate::handlers::watch::watch_stop`].
///
/// Not `Clone` — the debouncer + sender ownership must move once into
/// `ServerInner.watch` and then back out for shutdown.
pub struct WatchHandle {
    /// Live debouncer over `notify`'s `RecommendedWatcher`. Dropping the
    /// debouncer tears down the underlying OS watch and closes its event
    /// channel.
    pub debouncer: Debouncer<RecommendedWatcher, RecommendedCache>,
    /// Cancel signal for the watch_loop task. `watch_stop` sends `()` so
    /// the loop's `tokio::select!` cancel arm fires and the task drops.
    pub cancel: oneshot::Sender<()>,
}

/// Shared state owned by the running MCP server.
///
/// Layout follows `Designs/RustRewrite/README.md` "State Management":
/// - [`Self::graph`] uses `parking_lot::RwLock` (the canonical lock type
///   re-exported from `code-graph-graph`); query handlers take a read lock
///   for the duration of the query and serialize the response.
/// - [`Self::index_lock`] uses `tokio::sync::Mutex` because
///   `analyze_codebase` is async and the lock guard must cross await
///   points. `try_lock` returns "indexing already in progress" matching
///   Go behavior.
/// - [`Self::indexed`] is an `AtomicBool` so [`CodeGraphServer::require_indexed`]
///   can check the flag with no lock acquisition.
pub struct ServerInner {
    /// In-memory code graph populated by `analyze_codebase`.
    pub graph: PlRwLock<Graph>,
    /// Plugin registry (one entry per registered language).
    pub registry: LanguageRegistry,
    /// `true` after at least one successful `analyze_codebase`. Read by
    /// [`CodeGraphServer::require_indexed`] without taking a lock.
    pub indexed: AtomicBool,
    /// Single-flight guard for `analyze_codebase`. `try_lock` returns
    /// "indexing already in progress" identical to the Go behavior; the
    /// watch loop's `reindex_file` (Phase 4) also acquires this lock to
    /// close the analyze-vs-watch merge race the Go implementation has.
    pub index_lock: TokioMutex<()>,
    /// Last indexed root directory; needed by `watch_start`.
    pub root_path: PlRwLock<Option<PathBuf>>,
    /// Active watcher, if any. Populated by
    /// [`crate::handlers::watch::watch_start`] and cleared by
    /// [`crate::handlers::watch::watch_stop`].
    pub watch: PlRwLock<Option<WatchHandle>>,
    /// Last-loaded `<root>/.code-graph.toml`. Defaults to
    /// [`RootConfig::default`] until `analyze_codebase` reads it from disk.
    pub config: PlRwLock<RootConfig>,
}

/// MCP server exposing the code graph through 15 tools.
///
/// Cloneable because rmcp's macro-generated dispatch table holds the server
/// by value (the `tool_router` field is a `ToolRouter<Self>` and dispatch
/// borrows `&self` from the cloned handle). All shared state lives behind
/// the `Arc<ServerInner>`, so cloning is cheap and lock-respecting.
#[derive(Clone)]
pub struct CodeGraphServer {
    pub inner: Arc<ServerInner>,
    /// `tool_router` snapshot used only by test helpers
    /// (`tool_count`, `tool_router_contains_every_expected_name`). The
    /// `#[tool_handler]` macro generates `call_tool` / `list_tools` bodies
    /// that invoke the static factory `Self::tool_router()` directly; this
    /// field is a detached copy and mutating it has no runtime effect.
    tool_router: ToolRouter<Self>,
}

impl CodeGraphServer {
    /// Construct a fresh server. The graph starts empty; the registry is
    /// taken by value (the registry is `!Clone` and is moved in here once
    /// at startup).
    pub fn new(registry: LanguageRegistry) -> Self {
        Self {
            inner: Arc::new(ServerInner {
                graph: PlRwLock::new(Graph::new()),
                registry,
                indexed: AtomicBool::new(false),
                index_lock: TokioMutex::new(()),
                root_path: PlRwLock::new(None),
                watch: PlRwLock::new(None),
                config: PlRwLock::new(RootConfig::default()),
            }),
            tool_router: Self::tool_router(),
        }
    }

    /// Number of registered tools. Used by the smoke test to confirm the
    /// `#[tool_router]` macro produced 15 entries before any IO loop runs.
    pub fn tool_count(&self) -> usize {
        self.tool_router.list_all().len()
    }

    /// Snapshot of every registered tool descriptor (`name`, `description`,
    /// `inputSchema`, …). Used by the Phase 3.7 wire-format snapshot suite
    /// — the macro-generated `Self::tool_router()` factory is private, so
    /// tests reach for this helper instead.
    pub fn tool_descriptors(&self) -> Vec<rmcp::model::Tool> {
        self.tool_router.list_all()
    }

    /// Returns `Ok(())` if a codebase has been indexed; otherwise returns
    /// `Err(CallToolResult)` carrying the tool-level error envelope so the
    /// caller can hand it straight back to the MCP runtime.
    ///
    /// Handlers use the early-return pattern:
    ///
    /// ```ignore
    /// async fn my_handler(...) -> Result<CallToolResult, McpError> {
    ///     if let Err(r) = self.require_indexed() {
    ///         return Ok(r);
    ///     }
    ///     // ... happy path ...
    /// }
    /// ```
    ///
    /// The wire envelope mirrors `mcp.NewToolResultError` from the Go
    /// binary exactly: a `CallToolResult` with `is_error: true` and a
    /// single text content. Returning the error this way (instead of via
    /// `?` on a `McpError`) keeps `{"result":{"content":[…],"isError":true}}`
    /// on the wire instead of the JSON-RPC protocol-error envelope
    /// (`{"error":{"code":-32603,…}}`) that `McpError` propagates to.
    ///
    /// The error message itself matches the Go reference byte-for-byte;
    /// the em-dash is U+2014 (not a hyphen-minus) and Phase 3.7's snapshot
    /// suite locks the byte sequence in across all error paths.
    pub fn require_indexed(&self) -> Result<(), CallToolResult> {
        if self
            .inner
            .indexed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            Ok(())
        } else {
            Err(CallToolResult::error(vec![Content::text(
                "no codebase indexed — call analyze_codebase first".to_string(),
            )]))
        }
    }
}

// Argument structs ---------------------------------------------------------
//
// One struct per tool; `Parameters<T>` extracts `T` from the JSON-RPC
// request's `arguments` field. `Option<T>` fields mean "absent in the
// request"; required fields are `T` directly. Validation (e.g. "at least
// one of query/kind/namespace/language") is the handler's job — Phase 3.4
// adds it.

/// Empty parameter struct for tools that take no arguments
/// (`detect_cycles`, `watch_start`, `watch_stop`). The empty `{}` JSON
/// object deserializes into this without error.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct EmptyParams {}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AnalyzeCodebaseArgs {
    /// Absolute path to the directory to index.
    #[schemars(description = "Absolute path to the directory to index")]
    pub path: String,
    /// Force full re-index, ignoring any cache.
    #[schemars(description = "Force full re-index, ignoring any cache (default false)")]
    #[serde(default)]
    pub force: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetFileSymbolsArgs {
    #[schemars(description = "Absolute path to the source file")]
    pub file: String,
    #[schemars(
        description = "Only return top-level symbols (no nested methods/types) (default false)"
    )]
    #[serde(default)]
    pub top_level_only: Option<bool>,
    #[schemars(description = "Omit signature, column, end_line for compact output (default true)")]
    #[serde(default)]
    pub brief: Option<bool>,
    #[schemars(description = "Maximum results to return (default 100, max 1000)")]
    #[serde(default)]
    pub limit: Option<u32>,
    #[schemars(description = "Skip first N matches for pagination (default 0)")]
    #[serde(default)]
    pub offset: Option<u32>,
    #[schemars(
        description = "Return only the match count, no records (default false). Bounded response < 1KB regardless of match scale."
    )]
    #[serde(default)]
    pub count_only: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchSymbolsArgs {
    #[schemars(description = "Substring or regex pattern to match symbol names")]
    #[serde(default)]
    pub query: Option<String>,
    #[schemars(
        description = "Filter by symbol kind: function, method, class, struct, enum, typedef, interface, trait"
    )]
    #[serde(default)]
    pub kind: Option<String>,
    #[schemars(
        description = "Filter by namespace (substring match, e.g., 'Nfs' matches 'Ark::Nfs::V4')"
    )]
    #[serde(default)]
    pub namespace: Option<String>,
    #[schemars(description = "Filter by source language: cpp, rust, go, python, csharp, or java")]
    #[serde(default)]
    pub language: Option<String>,
    #[schemars(description = "Maximum results to return (default 20)")]
    #[serde(default)]
    pub limit: Option<u32>,
    #[schemars(description = "Skip first N matches for pagination (default 0)")]
    #[serde(default)]
    pub offset: Option<u32>,
    #[schemars(description = "Omit signature, column, end_line (default true)")]
    #[serde(default)]
    pub brief: Option<bool>,
    #[schemars(
        description = "Return only the match count, no records (default false). Bounded response < 1KB regardless of match scale."
    )]
    #[serde(default)]
    pub count_only: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetSymbolDetailArgs {
    #[schemars(
        description = "Symbol ID in format file:name as returned by get_file_symbols or search_symbols"
    )]
    pub symbol: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetSymbolSummaryArgs {
    #[schemars(description = "Optional absolute path: scope counts to a single file")]
    #[serde(default)]
    pub file: Option<String>,
    #[schemars(description = "Maximum rows to return (default 100, max 1000)")]
    #[serde(default)]
    pub limit: Option<u32>,
    #[schemars(description = "Skip first N rows for pagination (default 0)")]
    #[serde(default)]
    pub offset: Option<u32>,
    #[schemars(
        description = "Return only the row count, no records (default false). Bounded response < 1KB regardless of row scale. `total` is the (namespace, kind) pair count, NOT the sum of symbol counts."
    )]
    #[serde(default)]
    pub count_only: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetCallersArgs {
    #[schemars(description = "Symbol ID in format file:name")]
    pub symbol: String,
    #[schemars(description = "Maximum traversal depth (default 1)")]
    #[serde(default)]
    pub depth: Option<u32>,
    #[schemars(description = "Maximum callers to return per page (default 100, max 1000)")]
    #[serde(default)]
    pub limit: Option<u32>,
    #[schemars(description = "Skip first N callers for pagination (default 0)")]
    #[serde(default)]
    pub offset: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetCalleesArgs {
    #[schemars(description = "Symbol ID in format file:name")]
    pub symbol: String,
    #[schemars(description = "Maximum traversal depth (default 1)")]
    #[serde(default)]
    pub depth: Option<u32>,
    #[schemars(description = "Maximum callees to return per page (default 100, max 1000)")]
    #[serde(default)]
    pub limit: Option<u32>,
    #[schemars(description = "Skip first N callees for pagination (default 0)")]
    #[serde(default)]
    pub offset: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetDependenciesArgs {
    #[schemars(description = "Absolute path to the source file")]
    pub file: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct DetectCyclesArgs {
    #[schemars(description = "Maximum cycles to return (default 20, max 1000)")]
    #[serde(default)]
    pub limit: Option<u32>,
    #[schemars(description = "Skip first N cycles for pagination (default 0)")]
    #[serde(default)]
    pub offset: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetOrphansArgs {
    #[schemars(description = "Filter by symbol kind: function, method (default: all callables)")]
    #[serde(default)]
    pub kind: Option<String>,
    #[schemars(description = "Maximum results to return (default 20, max 1000)")]
    #[serde(default)]
    pub limit: Option<u32>,
    #[schemars(description = "Skip first N matches for pagination (default 0)")]
    #[serde(default)]
    pub offset: Option<u32>,
    #[schemars(description = "Omit signature, column, end_line (default true)")]
    #[serde(default)]
    pub brief: Option<bool>,
    #[schemars(
        description = "Return only the match count, no records (default false). Bounded response < 1KB regardless of match scale."
    )]
    #[serde(default)]
    pub count_only: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetClassHierarchyArgs {
    #[schemars(description = "Class name to look up")]
    pub class: String,
    #[schemars(
        description = "Traversal depth for transitive inheritance (default 1 = direct only)"
    )]
    #[serde(default)]
    pub depth: Option<u32>,
    #[schemars(
        description = "Maximum unique class names to include in the returned tree \
                       (default 250, max 1000). Each unique name counts once even \
                       when reached via multiple inheritance paths (diamonds), so \
                       a shared ancestor doesn't burn the budget twice. The \
                       response includes `truncated: true` and the partial tree \
                       when the budget is hit; raise this for deep hierarchies \
                       (e.g. UE's UObject)."
    )]
    #[serde(default)]
    pub max_nodes: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetCouplingArgs {
    #[schemars(description = "Absolute path to the source file")]
    pub file: String,
    #[schemars(description = "'outgoing' (default), 'incoming', or 'both'")]
    #[serde(default)]
    pub direction: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GenerateDiagramArgs {
    #[schemars(description = "Symbol ID to center the call graph on (format: file:name)")]
    #[serde(default)]
    pub symbol: Option<String>,
    #[schemars(description = "File path to center the dependency graph on")]
    #[serde(default)]
    pub file: Option<String>,
    #[schemars(description = "Class name to center the inheritance diagram on")]
    #[serde(default)]
    pub class: Option<String>,
    #[schemars(description = "BFS traversal depth (default 1)")]
    #[serde(default)]
    pub depth: Option<u32>,
    #[schemars(description = "Maximum nodes in diagram (default 30)")]
    #[serde(default)]
    pub max_nodes: Option<u32>,
    #[schemars(
        description = "Output format: 'edges' (default, minimal JSON) or 'mermaid' (Mermaid flowchart syntax)"
    )]
    #[serde(default)]
    pub format: Option<String>,
    #[schemars(
        description = "When format=mermaid, add CSS styling and center node highlighting (default false)"
    )]
    #[serde(default)]
    pub styled: Option<bool>,
}

// Tool router --------------------------------------------------------------

#[tool_router]
impl CodeGraphServer {
    // -- P0 (Phase 3.4) ----------------------------------------------------

    #[tool(
        description = "Index a codebase (C/C++, Rust, Go, Python, C#, Java) and build the code graph. Must be called before any query tools."
    )]
    async fn analyze_codebase(
        &self,
        Parameters(args): Parameters<AnalyzeCodebaseArgs>,
        peer: Peer<RoleServer>,
        meta: Meta,
    ) -> Result<CallToolResult, McpError> {
        let progress_token = meta.get_progress_token();
        Ok(handlers::analyze::analyze_codebase(
            self.inner.clone(),
            args.path,
            args.force.unwrap_or(false),
            Some(peer),
            progress_token,
        )
        .await)
    }

    #[tool(
        description = "List all symbols (functions, classes, etc.) defined in a file. \
                       Returns the {results, total, offset, limit, truncated, next_offset} \
                       envelope, sorted by symbol_id ascending. `limit` defaults to 100 \
                       (max 1000, clamped silently — the echoed `limit` reflects the \
                       resolved value); raise `limit` for wider pages on large files (UE \
                       generated headers can exceed 100), and use `offset` to advance \
                       through results. `top_level_only` (default false) drops nested \
                       methods/types. `brief` (default true) omits signature, column, \
                       and end_line; set false for full detail when investigating a \
                       specific symbol. `count_only=true` returns the match total with \
                       an empty `results` array in a < 1KB bounded response — use it for \
                       sizing queries before paging. Responses are also capped by \
                       `[response].max_bytes` (default 100KB); when the byte budget bites, \
                       `truncated` is true and `next_offset` points at the first \
                       un-emitted record — re-call with `offset = next_offset` to resume. \
                       `truncated=false` plus `next_offset=null` means the page is \
                       complete. `results.length` may be less than `limit` when the byte \
                       cap fires, so consult `truncated`, not length, to detect partial \
                       pages. Path is resolved against the indexed graph; `\\\\?\\` \
                       extended-path prefix is handled automatically, and relative \
                       segments (`.` / `..`) resolve against the on-disk file when it \
                       exists (otherwise lexical-only)."
    )]
    async fn get_file_symbols(
        &self,
        Parameters(args): Parameters<GetFileSymbolsArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(r) = self.require_indexed() {
            return Ok(r);
        }
        // The handler internally calls `paths::normalize_user_path`, which
        // wraps `dunce::canonicalize` — a blocking filesystem stat. On a
        // local-disk repo this is microseconds, but on NFS / sshfs / a
        // stale Windows share it can park the Tokio worker for hundreds of
        // ms to seconds. Offload the whole sync handler (which also
        // acquires the graph RwLock and serializes JSON) to the blocking
        // pool. Same idiom `analyze_codebase` uses for its indexing work.
        let inner = self.inner.clone();
        let max_bytes = inner.config.read().response.max_bytes;
        let result = tokio::task::spawn_blocking(move || {
            handlers::symbols::get_file_symbols(
                &inner.graph,
                &args.file,
                args.top_level_only.unwrap_or(false),
                args.brief.unwrap_or(true),
                args.limit,
                args.offset,
                args.count_only.unwrap_or(false),
                max_bytes,
            )
        })
        .await;
        Ok(match result {
            Ok(r) => r,
            Err(e) => handlers::tool_error(format!("get_file_symbols task panicked: {e}")),
        })
    }

    #[tool(
        description = "Search for symbols by name pattern across the indexed codebase. \
                       Returns the {results, total, offset, limit, truncated, next_offset} \
                       envelope, sorted by symbol_id ascending. At least one filter is \
                       expected: `query` (substring or regex on the symbol name), `kind` \
                       (function, method, class, struct, enum, typedef, interface, \
                       trait), `namespace` (substring match against the symbol's \
                       namespace path, e.g. 'Nfs' matches 'Ark::Nfs::V4'), and/or \
                       `language` (cpp, rust, go, python, csharp, java). `limit` defaults \
                       to 20 (max 1000, clamped silently — the echoed `limit` reflects \
                       the resolved value); raise `limit` for broad searches expected to \
                       return many hits, and use `offset` to advance through the \
                       remainder. `brief` (default true) omits signature, column, and \
                       end_line; set false for full detail. `count_only=true` returns \
                       the match total with an empty `results` array in a < 1KB bounded \
                       response — use it to size a search before committing to paging. \
                       Responses are also capped by `[response].max_bytes` (default \
                       100KB); when the byte budget bites, `truncated` is true and \
                       `next_offset` points at the first un-emitted record — re-call \
                       with `offset = next_offset` to resume. `truncated=false` plus \
                       `next_offset=null` means the page is complete. `results.length` \
                       may be less than `limit` when the byte cap fires, so consult \
                       `truncated`, not length, to detect partial pages."
    )]
    async fn search_symbols(
        &self,
        Parameters(args): Parameters<SearchSymbolsArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(r) = self.require_indexed() {
            return Ok(r);
        }
        let input = handlers::symbols::SearchSymbolsInput {
            query: args.query.as_deref(),
            kind: args.kind.as_deref(),
            namespace: args.namespace.as_deref(),
            language: args.language.as_deref(),
            limit: args.limit,
            offset: args.offset,
            brief: args.brief.unwrap_or(true),
            count_only: args.count_only.unwrap_or(false),
        };
        let max_bytes = self.inner.config.read().response.max_bytes;
        Ok(handlers::symbols::search_symbols(
            &self.inner.graph,
            input,
            max_bytes,
        ))
    }

    #[tool(description = "Get full details for a symbol by its ID")]
    async fn get_symbol_detail(
        &self,
        Parameters(args): Parameters<GetSymbolDetailArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(r) = self.require_indexed() {
            return Ok(r);
        }
        Ok(handlers::symbols::get_symbol_detail(
            &self.inner.graph,
            &args.symbol,
        ))
    }

    #[tool(
        description = "Get symbol counts grouped by namespace and kind — useful for codebase \
                       orientation. Returns a `Page<SummaryRow>` envelope: {results, total, \
                       offset, limit, truncated, next_offset}, where each row is \
                       `{namespace, kind, count}`. Rows are sorted by `(namespace, kind)` \
                       ascending so paging is deterministic across calls. `limit` defaults \
                       to 100 (max 1000, clamped silently — the echoed `limit` reflects the \
                       resolved value); raise `limit` for more rows per page on large \
                       codebases with many distinct namespaces. `offset` defaults to 0; \
                       raise `offset` to skip past previous results. `count_only=true` \
                       returns the sentinel page with `total` = the `(namespace, kind)` \
                       pair count (NOT the sum of individual symbols) and an empty \
                       `results` array in a < 1KB bounded response — use it to size the \
                       row set before paging. Responses are also capped by \
                       `[response].max_bytes` (default 100KB); when the byte budget bites, \
                       `truncated` is true and `next_offset` points at the first \
                       un-emitted row — re-call with `offset = next_offset` to resume. \
                       `truncated=false` plus `next_offset=null` means the page is \
                       complete. `results.length` may be less than `limit` when the byte \
                       cap fires, so consult `truncated`, not length, to detect partial \
                       pages. NOTE on `<global>`: rows with `namespace == \"<global>\"` \
                       are a display label for the empty namespace (symbols defined at \
                       global scope). `search_symbols` cannot currently filter to \
                       global-scope symbols only — its `namespace` field is a \
                       case-insensitive substring filter where the empty string means \
                       \"no filter\", so `search_symbols(namespace=\"\")` returns all \
                       symbols rather than only global-scope ones. To investigate \
                       global-scope symbols, use this tool to confirm they exist, then \
                       inspect them via `search_symbols` with other filters (e.g., by \
                       `kind` or `query`)."
    )]
    async fn get_symbol_summary(
        &self,
        Parameters(args): Parameters<GetSymbolSummaryArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(r) = self.require_indexed() {
            return Ok(r);
        }
        let max_bytes = self.inner.config.read().response.max_bytes;
        Ok(handlers::symbols::get_symbol_summary(
            &self.inner.graph,
            args.file.as_deref(),
            args.limit,
            args.offset,
            args.count_only.unwrap_or(false),
            max_bytes,
        ))
    }

    #[tool(
        description = "Find functions that call the given symbol (upstream call chain). \
                       `symbol` is a Symbol ID in the `file:name` or `file:Parent::name` \
                       format returned by get_file_symbols/search_symbols. Returns the \
                       {results, total, offset, limit, truncated, next_offset} envelope, \
                       sorted by (depth, symbol_id) ascending so the closest callers \
                       appear first. `depth` defaults to 1 (direct callers only); raise \
                       it to walk further upstream. `limit` defaults to 100 (max 1000, \
                       clamped silently — the echoed `limit` reflects the resolved \
                       value); raise `limit` for hot symbols with high fan-in (e.g. \
                       UObject::Serialize), use `offset` to page through the remainder, \
                       or narrow by lowering `depth`. Responses are also capped by \
                       `[response].max_bytes` (default 100KB); when the byte budget \
                       bites, `truncated` is true and `next_offset` points at the first \
                       un-emitted record — re-call with `offset = next_offset` to \
                       resume. `truncated=false` plus `next_offset=null` means the page \
                       is complete. `results.length` may be less than `limit` when the \
                       byte cap fires, so consult `truncated`, not length, to detect \
                       partial pages."
    )]
    async fn get_callers(
        &self,
        Parameters(args): Parameters<GetCallersArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(r) = self.require_indexed() {
            return Ok(r);
        }
        let max_bytes = self.inner.config.read().response.max_bytes;
        Ok(handlers::query::callers_or_callees(
            &self.inner.graph,
            &args.symbol,
            args.depth,
            handlers::query::Direction::Callers,
            args.limit,
            args.offset,
            max_bytes,
        ))
    }

    #[tool(
        description = "Find functions called by the given symbol (downstream call \
                       chain). `symbol` is a Symbol ID in the `file:name` or \
                       `file:Parent::name` format returned by \
                       get_file_symbols/search_symbols. Returns the {results, total, \
                       offset, limit, truncated, next_offset} envelope, sorted by \
                       (depth, symbol_id) ascending so the closest callees appear \
                       first. `depth` defaults to 1 (direct callees only); raise it to \
                       walk further downstream. `limit` defaults to 100 (max 1000, \
                       clamped silently — the echoed `limit` reflects the resolved \
                       value); raise `limit` for symbols with wide fan-out, use \
                       `offset` to page through the remainder, or narrow by lowering \
                       `depth` to scope a specific subtree. Responses are also capped \
                       by `[response].max_bytes` (default 100KB); when the byte budget \
                       bites, `truncated` is true and `next_offset` points at the \
                       first un-emitted record — re-call with `offset = next_offset` \
                       to resume. `truncated=false` plus `next_offset=null` means the \
                       page is complete. `results.length` may be less than `limit` \
                       when the byte cap fires, so consult `truncated`, not length, \
                       to detect partial pages."
    )]
    async fn get_callees(
        &self,
        Parameters(args): Parameters<GetCalleesArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(r) = self.require_indexed() {
            return Ok(r);
        }
        let max_bytes = self.inner.config.read().response.max_bytes;
        Ok(handlers::query::callers_or_callees(
            &self.inner.graph,
            &args.symbol,
            args.depth,
            handlers::query::Direction::Callees,
            args.limit,
            args.offset,
            max_bytes,
        ))
    }

    #[tool(
        description = "List files included/imported by the given file. Path is resolved \
                       against the indexed graph; `\\\\?\\` extended-path prefix is handled \
                       automatically, and relative segments (`.` / `..`) resolve against \
                       the on-disk file when it exists (otherwise lexical-only)."
    )]
    async fn get_dependencies(
        &self,
        Parameters(args): Parameters<GetDependenciesArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(r) = self.require_indexed() {
            return Ok(r);
        }
        // `paths::normalize_user_path` inside the handler may block on a
        // filesystem stat (see `get_file_symbols` comment).
        let inner = self.inner.clone();
        let result = tokio::task::spawn_blocking(move || {
            handlers::query::get_dependencies(&inner.graph, &args.file)
        })
        .await;
        Ok(match result {
            Ok(r) => r,
            Err(e) => handlers::tool_error(format!("get_dependencies task panicked: {e}")),
        })
    }

    // -- P1+P2 + watch (Phase 3.5) -----------------------------------------

    #[tool(
        description = "Detect circular include dependencies (strongly-connected components of the include graph). Returns the {results, total, offset, limit, truncated, next_offset} envelope; each `results[i]` is a Vec<String> of file paths in one cycle. Cycles are sorted internally by path; the outer list is sorted by each cycle's first path so pagination is deterministic across calls. Default limit 20 (max 1000); raise `limit` for more cycles per page (toward the 1000 cap), or use `offset` to page through (when `truncated=true`, re-call with `offset = next_offset`). Cycles are rare in well-maintained codebases, so the default is small. NOTE: the byte budget at [response].max_bytes does not truncate cycle lists — `detect_cycles` builds the full result set before slicing, so `truncated` is always `false` and `next_offset` is always `null` here. Consult `truncated` rather than `results.length` to detect partial pages."
    )]
    async fn detect_cycles(
        &self,
        Parameters(args): Parameters<DetectCyclesArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(r) = self.require_indexed() {
            return Ok(r);
        }
        Ok(handlers::structure::detect_cycles(
            &self.inner.graph,
            args.limit,
            args.offset,
        ))
    }

    #[tool(description = "Find symbols with no incoming call edges (uncalled \
                       functions/methods). Returns the {results, total, offset, limit, \
                       truncated, next_offset} envelope, sorted by symbol_id \
                       ascending. `limit` defaults to 20 (max 1000, clamped silently — \
                       the echoed `limit` reflects the resolved value); the default is \
                       small because orphan lists are typically explored \
                       interactively. Raise `limit` for wider scans, use `offset` to \
                       advance through pages, or filter by `kind` (function, method, \
                       class, struct, enum, typedef, interface, trait) to narrow the \
                       scope. `brief` (default true) omits signature, column, and \
                       end_line for token efficiency; set false for full detail. \
                       `count_only=true` returns the orphan total with an empty \
                       `results` array in a < 1KB bounded response — use it to size \
                       the orphan set before paging. Responses are also capped by \
                       `[response].max_bytes` (default 100KB); when the byte budget \
                       bites, `truncated` is true and `next_offset` points at the \
                       first un-emitted record — re-call with `offset = next_offset` \
                       to resume. `truncated=false` plus `next_offset=null` means the \
                       page is complete. `results.length` may be less than `limit` \
                       when the byte cap fires, so consult `truncated`, not length, \
                       to detect partial pages.")]
    async fn get_orphans(
        &self,
        Parameters(args): Parameters<GetOrphansArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(r) = self.require_indexed() {
            return Ok(r);
        }
        let max_bytes = self.inner.config.read().response.max_bytes;
        Ok(handlers::structure::get_orphans(
            &self.inner.graph,
            args.kind.as_deref(),
            args.limit,
            args.offset,
            args.brief,
            args.count_only.unwrap_or(false),
            max_bytes,
        ))
    }

    #[tool(
        description = "Get the inheritance tree for a class (base classes and derived classes). \
                       Returns the {hierarchy, truncated, max_nodes, total_nodes_seen} envelope: \
                       `hierarchy` is the tree rooted at the queried class; `truncated` flags \
                       whether the `max_nodes` budget cut off any children; `total_nodes_seen` \
                       is the count of unique class names actually walked. Diamond inheritance \
                       counts each shared ancestor once in the budget, regardless of how many \
                       paths reach it. Default `max_nodes` is 250 (max 1000) — sized to fit \
                       most hierarchies under the MCP token ceiling, but a single deep \
                       inheritance tree (e.g. UE's UObject) can exceed it. Watch for \
                       `truncated: true` and raise `max_nodes` (or narrow `depth`) when it \
                       fires."
    )]
    async fn get_class_hierarchy(
        &self,
        Parameters(args): Parameters<GetClassHierarchyArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(r) = self.require_indexed() {
            return Ok(r);
        }
        Ok(handlers::structure::get_class_hierarchy(
            &self.inner.graph,
            &args.class,
            args.depth,
            args.max_nodes,
        ))
    }

    #[tool(
        description = "Get cross-file dependency counts for a file. Path is resolved \
                       against the indexed graph; `\\\\?\\` extended-path prefix is handled \
                       automatically, and relative segments (`.` / `..`) resolve against \
                       the on-disk file when it exists (otherwise lexical-only)."
    )]
    async fn get_coupling(
        &self,
        Parameters(args): Parameters<GetCouplingArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(r) = self.require_indexed() {
            return Ok(r);
        }
        // `paths::normalize_user_path` inside the handler may block on a
        // filesystem stat (see `get_file_symbols` comment).
        let inner = self.inner.clone();
        let result = tokio::task::spawn_blocking(move || {
            handlers::structure::get_coupling(&inner.graph, &args.file, args.direction.as_deref())
        })
        .await;
        Ok(match result {
            Ok(r) => r,
            Err(e) => handlers::tool_error(format!("get_coupling task panicked: {e}")),
        })
    }

    #[tool(
        description = "Generate a graph diagram: call graph (symbol), file dependencies (file), or inheritance tree (class). Returns edges as JSON by default, or Mermaid syntax when format=mermaid. When `file=` is used, the path is resolved against the indexed graph; `\\\\?\\` extended-path prefix is handled automatically, and relative segments (`.` / `..`) resolve against the on-disk file when it exists (otherwise lexical-only). The `symbol=` and `class=` modes take identifiers, not paths, and are NOT normalized — pass them exactly as they appear in the index."
    )]
    async fn generate_diagram(
        &self,
        Parameters(args): Parameters<GenerateDiagramArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(r) = self.require_indexed() {
            return Ok(r);
        }
        // Only the `file=` branch calls `paths::normalize_user_path` (the
        // blocking stat); `symbol=` and `class=` are in-memory. Wrap
        // unconditionally for the simpler dispatcher — the spawn_blocking
        // overhead for the non-file branches is ~5µs of scheduling, well
        // below JSON-serialization cost.
        let inner = self.inner.clone();
        let result = tokio::task::spawn_blocking(move || {
            let input = handlers::structure::GenerateDiagramInput {
                symbol: args.symbol.as_deref(),
                file: args.file.as_deref(),
                class: args.class.as_deref(),
                depth: args.depth,
                max_nodes: args.max_nodes,
                format: args.format.as_deref(),
                styled: args.styled.unwrap_or(false),
            };
            handlers::structure::generate_diagram(&inner.graph, input)
        })
        .await;
        Ok(match result {
            Ok(r) => r,
            Err(e) => handlers::tool_error(format!("generate_diagram task panicked: {e}")),
        })
    }

    #[tool(
        description = "Start watching the indexed directory for file changes and auto-reindex modified files"
    )]
    async fn watch_start(
        &self,
        Parameters(_args): Parameters<EmptyParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(r) = self.require_indexed() {
            return Ok(r);
        }
        Ok(handlers::watch::watch_start(&self.inner))
    }

    #[tool(description = "Stop watching for file changes")]
    async fn watch_stop(
        &self,
        Parameters(_args): Parameters<EmptyParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(r) = self.require_indexed() {
            return Ok(r);
        }
        Ok(handlers::watch::watch_stop(&self.inner))
    }
}

#[tool_handler]
impl ServerHandler for CodeGraphServer {}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_server() -> CodeGraphServer {
        CodeGraphServer::new(LanguageRegistry::new())
    }

    /// The Phase 3.1 acceptance gate: `tools/list` must surface exactly 15
    /// tools. If a future task adds or removes a `#[tool]`, this assertion
    /// is the first place a wire-format change shows up.
    #[test]
    fn tool_router_registers_fifteen_tools() {
        let server = empty_server();
        assert_eq!(
            server.tool_count(),
            15,
            "expected 15 registered tools, got {}",
            server.tool_count(),
        );
    }

    /// Confirms every expected tool name is present. Names are part of the
    /// MCP wire contract — a typo would silently break any agent dispatching
    /// by name.
    #[test]
    fn tool_router_contains_every_expected_name() {
        let server = empty_server();
        let names: std::collections::HashSet<_> = server
            .tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        for expected in [
            "analyze_codebase",
            "get_file_symbols",
            "search_symbols",
            "get_symbol_detail",
            "get_symbol_summary",
            "get_callers",
            "get_callees",
            "get_dependencies",
            "detect_cycles",
            "get_orphans",
            "get_class_hierarchy",
            "get_coupling",
            "generate_diagram",
            "watch_start",
            "watch_stop",
        ] {
            assert!(
                names.contains(expected),
                "tool {expected} missing from router; have {names:?}",
            );
        }
    }

    /// `require_indexed` must produce the tool-level error envelope (the
    /// same shape Go's `mcp.NewToolResultError` produces) and the Go
    /// reference's exact wording (em-dash U+2014, not a hyphen-minus).
    /// Snapshot tests in Phase 3.7 will lock the byte sequence in across
    /// all error paths.
    #[test]
    fn require_indexed_returns_exact_go_wording() {
        let server = empty_server();
        let result = server
            .require_indexed()
            .expect_err("fresh server must report not indexed");
        assert_eq!(
            result.is_error,
            Some(true),
            "tool error envelope must set is_error=true, got {:?}",
            result.is_error,
        );
        let text = result
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.as_str())
            .expect("first content block must be text");
        assert_eq!(
            text, "no codebase indexed — call analyze_codebase first",
            "require_indexed text must match Go reference byte-for-byte",
        );
    }

    /// `require_indexed` returns Ok once the atomic flag is set.
    #[test]
    fn require_indexed_succeeds_after_indexed_flag_set() {
        let server = empty_server();
        server
            .inner
            .indexed
            .store(true, std::sync::atomic::Ordering::Release);
        server.require_indexed().expect("indexed flag must pass");
    }

    /// Phase 3.4 P0 handlers must enforce the require_indexed gate before
    /// running. Each query handler returns the tool-level "no codebase
    /// indexed" error when the server has not yet been indexed.
    #[tokio::test]
    async fn get_file_symbols_requires_indexed_before_running() {
        let server = empty_server();
        let r = server
            .get_file_symbols(Parameters(GetFileSymbolsArgs {
                file: "/never.cpp".to_string(),
                top_level_only: None,
                brief: None,
                limit: None,
                offset: None,
                count_only: None,
            }))
            .await
            .expect("tool-level errors return Ok(CallToolResult)");
        assert_eq!(r.is_error, Some(true));
        let text = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        assert_eq!(text, "no codebase indexed — call analyze_codebase first");
    }

    #[tokio::test]
    async fn search_symbols_requires_indexed_before_running() {
        let server = empty_server();
        let r = server
            .search_symbols(Parameters(SearchSymbolsArgs {
                query: Some("foo".to_string()),
                kind: None,
                namespace: None,
                language: None,
                limit: None,
                offset: None,
                brief: None,
                count_only: None,
            }))
            .await
            .expect("Ok envelope on require_indexed failure");
        assert_eq!(r.is_error, Some(true));
        let text = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        assert_eq!(text, "no codebase indexed — call analyze_codebase first");
    }

    #[tokio::test]
    async fn get_symbol_detail_requires_indexed_before_running() {
        let server = empty_server();
        let r = server
            .get_symbol_detail(Parameters(GetSymbolDetailArgs {
                symbol: "/x.cpp:foo".to_string(),
            }))
            .await
            .expect("Ok envelope on require_indexed failure");
        assert_eq!(r.is_error, Some(true));
        let text = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        assert_eq!(text, "no codebase indexed — call analyze_codebase first");
    }

    #[tokio::test]
    async fn get_symbol_summary_requires_indexed_before_running() {
        let server = empty_server();
        let r = server
            .get_symbol_summary(Parameters(GetSymbolSummaryArgs {
                file: None,
                limit: None,
                offset: None,
                count_only: None,
            }))
            .await
            .expect("Ok envelope on require_indexed failure");
        assert_eq!(r.is_error, Some(true));
        let text = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        assert_eq!(text, "no codebase indexed — call analyze_codebase first");
    }

    #[tokio::test]
    async fn get_callers_requires_indexed_before_running() {
        let server = empty_server();
        let r = server
            .get_callers(Parameters(GetCallersArgs {
                symbol: "/x.cpp:foo".to_string(),
                depth: None,
                limit: None,
                offset: None,
            }))
            .await
            .expect("Ok envelope on require_indexed failure");
        assert_eq!(r.is_error, Some(true));
    }

    #[tokio::test]
    async fn get_callees_requires_indexed_before_running() {
        let server = empty_server();
        let r = server
            .get_callees(Parameters(GetCalleesArgs {
                symbol: "/x.cpp:foo".to_string(),
                depth: None,
                limit: None,
                offset: None,
            }))
            .await
            .expect("Ok envelope on require_indexed failure");
        assert_eq!(r.is_error, Some(true));
    }

    #[tokio::test]
    async fn get_dependencies_requires_indexed_before_running() {
        let server = empty_server();
        let r = server
            .get_dependencies(Parameters(GetDependenciesArgs {
                file: "/x.cpp".to_string(),
            }))
            .await
            .expect("Ok envelope on require_indexed failure");
        assert_eq!(r.is_error, Some(true));
    }

    /// Smoke test: with the indexed flag set, query handlers run their
    /// happy path through the in-module handler functions instead of
    /// short-circuiting on require_indexed. The detailed shape assertions
    /// live in the per-handler tests in `handlers/symbols.rs` and
    /// `handlers/query.rs`.
    #[tokio::test]
    async fn p0_handler_passes_require_indexed_when_flag_set() {
        let server = empty_server();
        server
            .inner
            .indexed
            .store(true, std::sync::atomic::Ordering::Release);
        // Empty graph + unknown file → handler-specific error wording, not
        // require_indexed wording.
        let r = server
            .get_file_symbols(Parameters(GetFileSymbolsArgs {
                file: "/never.cpp".to_string(),
                top_level_only: None,
                brief: None,
                limit: None,
                offset: None,
                count_only: None,
            }))
            .await
            .unwrap();
        assert_eq!(r.is_error, Some(true));
        let text = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        assert_eq!(text, "no symbols found in file: /never.cpp");
    }

    /// One representative description must preserve the em-dash (U+2014)
    /// — a hyphen-minus regression would slip past the count/name tests
    /// but would be caught here. `get_symbol_summary` is chosen because
    /// its description has carried an em-dash since the original Go
    /// reference. The full description-snapshot suite is the wire-
    /// format-of-record (`tests/snapshot_tools_list.rs`); this test
    /// guards just the em-dash byte sequence.
    ///
    /// ResponseShapePolish Phase 1 (Task 1.1) rewrote the description
    /// to mention the new `Page<SummaryRow>` envelope shape; the
    /// em-dash is preserved verbatim. Task 1.5 will rewrite the
    /// description again with the full envelope/sort/count_only
    /// wording — keep the em-dash check intact through that edit.
    #[test]
    fn tool_descriptions_match_go_reference_for_get_symbol_summary() {
        let tools = CodeGraphServer::tool_router().list_all();
        let tool = tools
            .iter()
            .find(|t| t.name == "get_symbol_summary")
            .expect("get_symbol_summary must be registered");
        let description = tool
            .description
            .as_ref()
            .map(|c| c.as_ref())
            .expect("get_symbol_summary must have a description");
        assert!(
            description.starts_with("Get symbol counts grouped by namespace and kind \u{2014} "),
            "get_symbol_summary description must keep the em-dash (U+2014) prefix; got {description:?}",
        );
    }

    // -- Phase 3.5 require_indexed gates -----------------------------------
    //
    // Each P1+P2 handler enforces the require_indexed gate before doing
    // any work. Same exact-text assertion as the P0 family above.

    #[tokio::test]
    async fn detect_cycles_requires_indexed_before_running() {
        let server = empty_server();
        let r = server
            .detect_cycles(Parameters(DetectCyclesArgs::default()))
            .await
            .expect("tool-level errors return Ok(CallToolResult)");
        assert_eq!(r.is_error, Some(true));
        let text = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        assert_eq!(text, "no codebase indexed — call analyze_codebase first");
    }

    #[tokio::test]
    async fn get_orphans_requires_indexed_before_running() {
        let server = empty_server();
        let r = server
            .get_orphans(Parameters(GetOrphansArgs {
                kind: None,
                limit: None,
                offset: None,
                brief: None,
                count_only: None,
            }))
            .await
            .expect("Ok envelope on require_indexed failure");
        assert_eq!(r.is_error, Some(true));
        let text = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        assert_eq!(text, "no codebase indexed — call analyze_codebase first");
    }

    #[tokio::test]
    async fn get_class_hierarchy_requires_indexed_before_running() {
        let server = empty_server();
        let r = server
            .get_class_hierarchy(Parameters(GetClassHierarchyArgs {
                class: "Anything".to_string(),
                depth: None,
                max_nodes: None,
            }))
            .await
            .expect("Ok envelope on require_indexed failure");
        assert_eq!(r.is_error, Some(true));
        let text = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        assert_eq!(text, "no codebase indexed — call analyze_codebase first");
    }

    #[tokio::test]
    async fn get_coupling_requires_indexed_before_running() {
        let server = empty_server();
        let r = server
            .get_coupling(Parameters(GetCouplingArgs {
                file: "/x.cpp".to_string(),
                direction: None,
            }))
            .await
            .expect("Ok envelope on require_indexed failure");
        assert_eq!(r.is_error, Some(true));
        let text = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        assert_eq!(text, "no codebase indexed — call analyze_codebase first");
    }

    #[tokio::test]
    async fn generate_diagram_requires_indexed_before_running() {
        let server = empty_server();
        let r = server
            .generate_diagram(Parameters(GenerateDiagramArgs {
                symbol: Some("/x.cpp:foo".to_string()),
                file: None,
                class: None,
                depth: None,
                max_nodes: None,
                format: None,
                styled: None,
            }))
            .await
            .expect("Ok envelope on require_indexed failure");
        assert_eq!(r.is_error, Some(true));
        let text = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        assert_eq!(text, "no codebase indexed — call analyze_codebase first");
    }

    // -- Phase 4.1 watch require_indexed gates -----------------------------
    //
    // Both watch handlers must short-circuit on require_indexed before
    // touching debouncer state. Phase 3.5's stubs deliberately skipped
    // this; Phase 4 restores it. Lifecycle tests live in
    // `handlers/watch.rs`.

    #[tokio::test]
    async fn watch_start_requires_indexed_before_running() {
        let server = empty_server();
        let r = server
            .watch_start(Parameters(EmptyParams::default()))
            .await
            .expect("Ok envelope on require_indexed failure");
        assert_eq!(r.is_error, Some(true));
        let text = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        assert_eq!(text, "no codebase indexed — call analyze_codebase first");
    }

    #[tokio::test]
    async fn watch_stop_requires_indexed_before_running() {
        let server = empty_server();
        let r = server
            .watch_stop(Parameters(EmptyParams::default()))
            .await
            .expect("Ok envelope on require_indexed failure");
        assert_eq!(r.is_error, Some(true));
        let text = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        assert_eq!(text, "no codebase indexed — call analyze_codebase first");
    }

    /// Smoke test: with the indexed flag set, a P1 handler runs its happy
    /// path through the in-module handler function instead of short-
    /// circuiting on require_indexed. Detailed assertions live in
    /// `handlers/structure.rs` tests.
    #[tokio::test]
    async fn p1_handler_passes_require_indexed_when_flag_set() {
        let server = empty_server();
        server
            .inner
            .indexed
            .store(true, std::sync::atomic::Ordering::Release);
        // Empty graph → detect_cycles returns the Page<Vec<String>>
        // envelope with results=[], total=0. Confirms require_indexed
        // doesn't block the call once the flag is set.
        let r = server
            .detect_cycles(Parameters(DetectCyclesArgs::default()))
            .await
            .unwrap();
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        let text = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["results"].as_array().unwrap().len(), 0);
        assert_eq!(parsed["total"].as_u64().unwrap(), 0);
    }

    // -- Phase 3.1 count_only Args deserialization -------------------------
    //
    // The `count_only: Option<bool>` field on GetOrphansArgs,
    // SearchSymbolsArgs, GetFileSymbolsArgs, and GetSymbolSummaryArgs must
    // deserialize via the same contract as the existing `Option<bool>`
    // fields (`brief`, `force`, …): absent → None, present-and-bool →
    // Some(value), wrong-type → deserialization error. The None default
    // is what lets handlers resolve it to `false` via `unwrap_or(false)`;
    // the test guards the pre-condition that flow depends on.
    //
    // We exercise the contract once per struct (across the four cases) so
    // that a future schema-level regression on any of the four is caught
    // independently.

    #[test]
    fn get_orphans_args_count_only_absent_is_none() {
        let args: GetOrphansArgs = serde_json::from_str("{}").expect("empty object deserializes");
        assert!(
            args.count_only.is_none(),
            "absent count_only must deserialize to None (handler resolves None to false via unwrap_or(false)); got {:?}",
            args.count_only,
        );
    }

    #[test]
    fn get_orphans_args_count_only_true_is_some_true() {
        let args: GetOrphansArgs =
            serde_json::from_str(r#"{"count_only": true}"#).expect("count_only=true deserializes");
        assert_eq!(args.count_only, Some(true));
    }

    #[test]
    fn get_orphans_args_count_only_false_is_some_false() {
        let args: GetOrphansArgs = serde_json::from_str(r#"{"count_only": false}"#)
            .expect("count_only=false deserializes");
        assert_eq!(args.count_only, Some(false));
    }

    #[test]
    fn get_orphans_args_count_only_malformed_string_rejected() {
        let result: Result<GetOrphansArgs, _> = serde_json::from_str(r#"{"count_only": "yes"}"#);
        assert!(
            result.is_err(),
            "string instead of bool must produce a deserialization error; got {result:?}",
        );
    }

    #[test]
    fn search_symbols_args_count_only_absent_is_none() {
        let args: SearchSymbolsArgs =
            serde_json::from_str("{}").expect("empty object deserializes");
        assert!(
            args.count_only.is_none(),
            "absent count_only must deserialize to None; got {:?}",
            args.count_only,
        );
    }

    #[test]
    fn search_symbols_args_count_only_true_is_some_true() {
        let args: SearchSymbolsArgs =
            serde_json::from_str(r#"{"count_only": true}"#).expect("count_only=true deserializes");
        assert_eq!(args.count_only, Some(true));
    }

    #[test]
    fn search_symbols_args_count_only_false_is_some_false() {
        let args: SearchSymbolsArgs = serde_json::from_str(r#"{"count_only": false}"#)
            .expect("count_only=false deserializes");
        assert_eq!(args.count_only, Some(false));
    }

    #[test]
    fn search_symbols_args_count_only_malformed_string_rejected() {
        let result: Result<SearchSymbolsArgs, _> = serde_json::from_str(r#"{"count_only": "yes"}"#);
        assert!(
            result.is_err(),
            "string instead of bool must produce a deserialization error; got {result:?}",
        );
    }

    #[test]
    fn get_file_symbols_args_count_only_absent_is_none() {
        // `file` is required on GetFileSymbolsArgs; supply a stub.
        let args: GetFileSymbolsArgs =
            serde_json::from_str(r#"{"file": "/x.cpp"}"#).expect("minimum-required deserializes");
        assert!(
            args.count_only.is_none(),
            "absent count_only must deserialize to None; got {:?}",
            args.count_only,
        );
    }

    #[test]
    fn get_file_symbols_args_count_only_true_is_some_true() {
        let args: GetFileSymbolsArgs =
            serde_json::from_str(r#"{"file": "/x.cpp", "count_only": true}"#)
                .expect("count_only=true deserializes");
        assert_eq!(args.count_only, Some(true));
    }

    #[test]
    fn get_file_symbols_args_count_only_false_is_some_false() {
        let args: GetFileSymbolsArgs =
            serde_json::from_str(r#"{"file": "/x.cpp", "count_only": false}"#)
                .expect("count_only=false deserializes");
        assert_eq!(args.count_only, Some(false));
    }

    #[test]
    fn get_file_symbols_args_count_only_malformed_string_rejected() {
        let result: Result<GetFileSymbolsArgs, _> =
            serde_json::from_str(r#"{"file": "/x.cpp", "count_only": "yes"}"#);
        assert!(
            result.is_err(),
            "string instead of bool must produce a deserialization error; got {result:?}",
        );
    }

    #[test]
    fn get_symbol_summary_args_count_only_absent_is_none() {
        let args: GetSymbolSummaryArgs =
            serde_json::from_str("{}").expect("empty object deserializes");
        assert!(
            args.count_only.is_none(),
            "absent count_only must deserialize to None; got {:?}",
            args.count_only,
        );
    }

    #[test]
    fn get_symbol_summary_args_count_only_true_is_some_true() {
        let args: GetSymbolSummaryArgs =
            serde_json::from_str(r#"{"count_only": true}"#).expect("count_only=true deserializes");
        assert_eq!(args.count_only, Some(true));
    }

    #[test]
    fn get_symbol_summary_args_count_only_false_is_some_false() {
        let args: GetSymbolSummaryArgs = serde_json::from_str(r#"{"count_only": false}"#)
            .expect("count_only=false deserializes");
        assert_eq!(args.count_only, Some(false));
    }

    #[test]
    fn get_symbol_summary_args_count_only_malformed_string_rejected() {
        let result: Result<GetSymbolSummaryArgs, _> =
            serde_json::from_str(r#"{"count_only": "yes"}"#);
        assert!(
            result.is_err(),
            "string instead of bool must produce a deserialization error; got {result:?}",
        );
    }
}
