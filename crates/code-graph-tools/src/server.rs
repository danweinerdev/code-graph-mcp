//! MCP server that exposes the code graph as 23 rmcp tools over stdio.
//!
//! Phase 3.1 shipped the scaffold: [`CodeGraphServer`] with all tools
//! wired through `#[tool_router]` plus the `ServerInner` state struct.
//! Every tool handler is now implemented; query handlers gate on
//! `ServerInner::require_indexed` before touching the graph.
//!
//! ## Tool wire format
//!
//! Tool descriptions are copied byte-for-byte from the Go reference at
//! `internal/tools/tools.go` for the Go-derived tools. Two are updated for
//! the multi-language Rust port:
//!
//! - `analyze_codebase`: widened from "Index a C/C++ codebase…" to "Index a
//!   codebase (C/C++, Rust, Go, Python, C#, Java) and build the code graph.
//!   Must be called before any query tools."
//! - `search_symbols`: keeps the existing description but gains a `language`
//!   parameter ("Filter by source language: cpp, rust, go, python, csharp, or
//!   java").
//!
//! These strings are captured as wire-format snapshots; any future
//! divergence triggers `cargo insta review`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, OnceLock};

#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;

use code_graph_core::RootConfig;
use code_graph_graph::Graph;
use code_graph_lang::LanguageRegistry;
use notify_debouncer_full::notify::RecommendedWatcher;
use notify_debouncer_full::{Debouncer, RecommendedCache};
#[cfg(test)]
use parking_lot::Mutex as PlMutex;
use parking_lot::RwLock as PlRwLock;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, Meta};
use rmcp::service::RoleServer;
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, Peer, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::{oneshot, Notify};
use tokio::task::JoinHandle;

use crate::analyze_job::AnalyzeSlot;
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
    /// Running watch-loop task. Graceful daemon shutdown awaits this after
    /// closing the debouncer and sending cancellation, so no queued batch can
    /// mutate the graph after its final cache save.
    pub task: JoinHandle<()>,
}

/// Coordinates cache writes with daemon shutdown without coupling persistence
/// correctness to analyze-job phase reporting.
pub struct PersistCoordinator {
    state: parking_lot::Mutex<PersistState>,
    changed: Notify,
}

struct PersistState {
    analyze_closed: bool,
    connection_closed: bool,
    persist_closed: bool,
    watch_cleanup_closed: bool,
    /// Set exactly once by the idle waiter that won the expiry race. It closes
    /// both connection and analyze admission before the daemon starts its
    /// graceful drain.
    idle_claimed: bool,
    /// Every connection/analyze begin and drop advances this value. The idle
    /// waiter uses it as an epoch so any activity restarts its full interval.
    lifecycle_generation: u64,
    analyses: u32,
    connections: u32,
    persists: u32,
    watch_cleanups: u32,
}

impl PersistCoordinator {
    pub fn new() -> Self {
        Self {
            state: parking_lot::Mutex::new(PersistState {
                analyze_closed: false,
                connection_closed: false,
                persist_closed: false,
                watch_cleanup_closed: false,
                idle_claimed: false,
                lifecycle_generation: 0,
                analyses: 0,
                connections: 0,
                persists: 0,
                watch_cleanups: 0,
            }),
            changed: Notify::new(),
        }
    }

    /// Atomically admit an analyze pipeline. Detached workers retain the
    /// owned guard until their full parse/resolve/persist pipeline ends.
    pub fn begin_analyze(self: &Arc<Self>) -> Result<AnalyzeGuard, &'static str> {
        let mut state = self.state.lock();
        if state.analyze_closed {
            return Err("daemon shutdown in progress; new analyze jobs are not accepted");
        }
        state.analyses += 1;
        state.lifecycle_generation = state.lifecycle_generation.wrapping_add(1);
        self.changed.notify_waiters();
        Ok(AnalyzeGuard {
            coordinator: Arc::clone(self),
        })
    }

    /// Atomically admit an MCP connection. The returned guard spans the
    /// accepted transport, any TCP authentication prelude, and the complete
    /// rmcp service lifetime so idle accounting never relies on permits.
    pub fn begin_connection(self: &Arc<Self>) -> Result<ConnectionGuard, &'static str> {
        let mut state = self.state.lock();
        if state.connection_closed {
            return Err("daemon shutdown in progress; new connections are not accepted");
        }
        state.connections += 1;
        state.lifecycle_generation = state.lifecycle_generation.wrapping_add(1);
        self.changed.notify_waiters();
        Ok(ConnectionGuard {
            coordinator: Arc::clone(self),
        })
    }

    /// Atomically admit one cache save unless graceful shutdown has closed
    /// persistence admission. The guard releases on normal return, error, or
    /// unwind.
    pub fn begin_persist(self: &Arc<Self>) -> Result<PersistGuard, &'static str> {
        let mut state = self.state.lock();
        if state.persist_closed {
            return Err("cache persistence is closed during daemon shutdown");
        }
        state.persists += 1;
        Ok(PersistGuard {
            coordinator: Arc::clone(self),
        })
    }

    /// Admit detached cooperative watch cleanup. `watch_stop` obtains this
    /// while it still holds the shared watch-state lock, so daemon shutdown
    /// either takes the live handle itself or waits for this cleanup.
    pub fn begin_watch_cleanup(self: &Arc<Self>) -> Result<WatchCleanupGuard, &'static str> {
        let mut state = self.state.lock();
        if state.watch_cleanup_closed {
            return Err("watch cleanup is closed during daemon shutdown");
        }
        state.watch_cleanups += 1;
        Ok(WatchCleanupGuard {
            coordinator: Arc::clone(self),
        })
    }

    /// Whether graceful shutdown has closed admission for analyze pipelines.
    /// `watch_start` checks this while holding its watch-state write lock so
    /// shutdown can close admission before taking that lock.
    pub fn analyze_admission_closed(&self) -> bool {
        self.state.lock().analyze_closed
    }

    /// Close connection and analyze admission without waiting for work that is
    /// already in flight. Daemon ownership-path loss uses this before closing
    /// its listener; the normal graceful-shutdown drain performs the waits.
    pub fn close_admission(&self) {
        let mut state = self.state.lock();
        state.analyze_closed = true;
        state.connection_closed = true;
        self.changed.notify_waiters();
    }

    /// Wait for an uninterrupted idle interval, then atomically claim daemon
    /// shutdown. `Duration::ZERO` is the documented never-exit sentinel and
    /// remains pending even when the daemon is otherwise idle.
    ///
    /// The generation is captured only after both counts are zero. Every
    /// analyze/connection begin or drop changes it and wakes this waiter, so a
    /// return to zero always receives a fresh full interval. Expiry rechecks
    /// under the same mutex before closing admission, making attach-vs-expiry
    /// deterministic: whichever operation gets the mutex first wins.
    pub async fn wait_for_idle_shutdown(&self, idle_timeout: std::time::Duration) -> bool {
        if idle_timeout.is_zero() {
            std::future::pending::<()>().await;
            unreachable!("the zero idle timeout sentinel never resolves")
        }

        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let generation = {
                let state = self.state.lock();
                if state.idle_claimed {
                    return false;
                }
                if state.connections != 0 || state.analyses != 0 {
                    None
                } else {
                    Some(state.lifecycle_generation)
                }
            };
            let Some(generation) = generation else {
                notified.await;
                continue;
            };

            tokio::select! {
                _ = &mut notified => continue,
                _ = tokio::time::sleep(idle_timeout) => {
                    let mut state = self.state.lock();
                    if !state.idle_claimed
                        && state.connections == 0
                        && state.analyses == 0
                        && state.lifecycle_generation == generation
                    {
                        state.idle_claimed = true;
                        state.connection_closed = true;
                        state.analyze_closed = true;
                        self.changed.notify_waiters();
                        return true;
                    }
                }
            }
        }
    }

    /// Close new analyze admission and wait for every previously-admitted
    /// pipeline. A notified future is created before inspecting state so a
    /// final guard drop cannot be lost between the check and await.
    pub async fn close_analyze_and_wait(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut state = self.state.lock();
                state.analyze_closed = true;
                state.connection_closed = true;
                if state.analyses == 0 {
                    return;
                }
            }
            notified.await;
        }
    }

    /// Close new ordinary saves and wait for already-active saves. The daemon
    /// performs its final exclusive save only after this returns.
    pub async fn close_persist_and_wait(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut state = self.state.lock();
                state.persist_closed = true;
                if state.persists == 0 {
                    return;
                }
            }
            notified.await;
        }
    }

    /// Close detached watch-cleanup admission and wait for cleanups already
    /// detached by `watch_stop`. The notified future is armed before checking
    /// the count so the final guard drop cannot be lost.
    pub async fn close_watch_cleanup_and_wait(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut state = self.state.lock();
                state.watch_cleanup_closed = true;
                if state.watch_cleanups == 0 {
                    return;
                }
            }
            notified.await;
        }
    }
}

impl Default for PersistCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII admission for an entire analyze pipeline.
pub struct AnalyzeGuard {
    coordinator: Arc<PersistCoordinator>,
}

impl Drop for AnalyzeGuard {
    fn drop(&mut self) {
        let mut state = self.coordinator.state.lock();
        debug_assert!(state.analyses > 0, "analyze guard must hold an active slot");
        state.analyses -= 1;
        state.lifecycle_generation = state.lifecycle_generation.wrapping_add(1);
        self.coordinator.changed.notify_waiters();
    }
}

/// RAII admission for one accepted daemon connection.
pub struct ConnectionGuard {
    coordinator: Arc<PersistCoordinator>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        let mut state = self.coordinator.state.lock();
        debug_assert!(
            state.connections > 0,
            "connection guard must hold an active connection slot"
        );
        state.connections -= 1;
        state.lifecycle_generation = state.lifecycle_generation.wrapping_add(1);
        self.coordinator.changed.notify_waiters();
    }
}

/// RAII admission for one cache save.
pub struct PersistGuard {
    coordinator: Arc<PersistCoordinator>,
}

impl Drop for PersistGuard {
    fn drop(&mut self) {
        let mut state = self.coordinator.state.lock();
        debug_assert!(state.persists > 0, "persist guard must hold an active slot");
        state.persists -= 1;
        if state.persists == 0 {
            self.coordinator.changed.notify_waiters();
        }
    }
}

/// RAII admission for a detached `watch_stop` cleanup task.
pub struct WatchCleanupGuard {
    coordinator: Arc<PersistCoordinator>,
}

impl Drop for WatchCleanupGuard {
    fn drop(&mut self) {
        let mut state = self.coordinator.state.lock();
        debug_assert!(
            state.watch_cleanups > 0,
            "watch cleanup guard must hold an active slot"
        );
        state.watch_cleanups -= 1;
        if state.watch_cleanups == 0 {
            self.coordinator.changed.notify_waiters();
        }
    }
}

/// Shared state owned by the running MCP server.
///
/// Layout follows `Designs/RustRewrite/README.md` "State Management":
/// - [`Self::graph`] uses `parking_lot::RwLock` (the canonical lock type
///   re-exported from `code-graph-graph`); query handlers take a read lock
///   for the duration of the query and serialize the response.
/// - [`Self::index_lock`] uses `tokio::sync::Mutex` because
///   `analyze_codebase` is async and the lock guard must cross await
///   points. The analyze-only slot serializes analyze work; this lock only
///   serializes the active worker against watch reindexing.
/// - [`Self::indexed`] is an `AtomicBool` so [`CodeGraphServer::require_indexed`]
///   can check the flag with no lock acquisition.
/// - [`Self::status_publication`] makes the graph and its status-facing
///   provenance one publication unit. It is always the outermost lock when
///   combined with graph, applied-index, or operational-config locks; never
///   hold it across an `.await`. `get_status` takes it for read, successful
///   analyze publication and watch graph mutations take it for write.
pub struct ServerInner {
    /// In-memory code graph populated by `analyze_codebase`.
    pub graph: PlRwLock<Graph>,
    /// Plugin registry (one entry per registered language).
    pub registry: LanguageRegistry,
    /// Version-control providers for the history tools. Empty by default —
    /// history tools then report unavailability as a success (FR-36). The
    /// binary injects the git provider via
    /// [`CodeGraphServer::with_vcs_registry`]; this crate depends on the
    /// trait crate only (AC-23).
    pub vcs: Arc<code_graph_vcs::VcsRegistry>,
    /// `true` after at least one successful `analyze_codebase`. Read by
    /// [`CodeGraphServer::require_indexed`] without taking a lock.
    pub indexed: AtomicBool,
    /// Serializes publication of graph contents and the metadata reported with
    /// them by `get_status`. See the lock-order rule on [`ServerInner`].
    pub(crate) status_publication: PlRwLock<()>,
    /// Serializes the active analyze worker against the watch loop's
    /// `reindex_file` to close their merge race. The analyze slot, not this
    /// lock, serializes one running worker plus pending analyzes.
    pub index_lock: TokioMutex<()>,
    /// Last indexed root directory; needed by `watch_start`.
    pub root_path: PlRwLock<Option<PathBuf>>,
    /// Status-facing snapshot of the root and configuration that produced the
    /// active indexed state. Analyze replaces this only after it has
    /// successfully applied its admitted configuration; watch reindexing keeps
    /// the snapshot intact because it uses that cached configuration.
    pub(crate) applied_index: PlRwLock<AppliedIndexState>,
    /// Project root owning the active cache. Unlike `root_path`, this is not
    /// the invocation/watch scope and may be an ancestor with nested config.
    pub cache_root: PlRwLock<Option<PathBuf>>,
    /// Optional daemon-only I/O capability for the active cache root. It is
    /// distinct from [`Self::cache_root`]: responses, config discovery, and
    /// daemon root binding retain the canonical semantic path, while Linux
    /// daemon saves use a retained-root procfd alias that survives pathname
    /// replacement.
    pub cache_io_root: PlRwLock<Option<PathBuf>>,
    /// Daemon-only immutable project-root boundary. Direct in-process servers
    /// leave this unset and may analyze any discovered project root.
    pub daemon_project_root: OnceLock<PathBuf>,
    /// Linux-only identity of the project-root directory descriptor retained
    /// by the daemon.  Keep only portable device/inode values here: query and
    /// indexing paths remain the logical project-root path.
    #[cfg(target_os = "linux")]
    pub daemon_root_identity: OnceLock<DaemonRootIdentity>,
    /// Active watcher, if any. Populated by
    /// [`crate::handlers::watch::watch_start`] and cleared by
    /// [`crate::handlers::watch::watch_stop`].
    pub watch: PlRwLock<Option<WatchHandle>>,
    /// Last-loaded `<root>/.code-graph.toml`. Defaults to
    /// [`RootConfig::default`] until `analyze_codebase` reads it from disk.
    pub config: PlRwLock<RootConfig>,
    /// Nanoseconds since UNIX_EPOCH when the most recent
    /// `analyze_codebase` completed (whether via full re-index or the
    /// cache fast-path). `0` means "never indexed". Read by the
    /// `get_status` tool to expose operational visibility into when
    /// the in-memory graph state was established. AtomicU64 so reads
    /// are lock-free.
    pub index_built_at: AtomicU64,
    /// Whether the most recent `analyze_codebase` was called with
    /// `force=true`. Read by `get_status` so the user can tell
    /// "this index is the result of a force-rebuild" vs "this is the
    /// incremental result". `false` until the first analyze completes.
    pub index_force_built: AtomicBool,
    /// Admission gate for cache writes during graceful daemon replacement.
    /// Direct-mode servers never close it, preserving their existing behavior.
    pub persist: Arc<PersistCoordinator>,
    /// Single-flight slot for analyze jobs (sync + async). Holds at
    /// most one `Running` job plus the previous terminal job for the
    /// grace-window read pattern. Read by 1.2's worker (next commit).
    #[allow(dead_code)]
    pub(crate) analyze_slot: PlRwLock<AnalyzeSlot>,
    /// Test-only synchronous handoff performed inside analyze publication
    /// after graph replacement and before matching provenance is written.
    /// It proves `get_status` cannot observe an incomplete publication.
    #[cfg(test)]
    pub(crate) publication_hook: PlMutex<Option<PublicationHook>>,
    /// Test-only handoff after config discovery has selected its provenance
    /// and before analysis publishes the resulting graph.
    #[cfg(test)]
    pub(crate) config_discovery_hook: PlMutex<Option<ConfigDiscoveryHook>>,
    /// Test-only pause after the blocking async-admission probe. It makes the
    /// single-worker Tokio responsiveness regression deterministic.
    #[cfg(test)]
    pub(crate) admission_probe_hook: PlMutex<Option<AdmissionProbeHook>>,
    /// Test-only pause at analyze-worker entry for deterministic queue/panic
    /// supervision coverage.
    #[cfg(test)]
    pub(crate) worker_start_hook: PlMutex<Option<WorkerStartHook>>,
}

/// Test-only barrier proving that `get_status` cannot observe a newly
/// replaced graph before its applied-index metadata is published.
#[cfg(test)]
pub(crate) struct PublicationHook {
    pub(crate) reached: oneshot::Sender<()>,
    pub(crate) proceed: Arc<std::sync::Barrier>,
}

/// Test-only barrier proving config provenance comes from discovery rather
/// than a second filesystem probe after discovery returns.
#[cfg(test)]
pub(crate) struct ConfigDiscoveryHook {
    pub(crate) reached: oneshot::Sender<()>,
    pub(crate) proceed: Arc<std::sync::Barrier>,
}

/// Test-only handoff proving async admission probes run on Tokio's blocking
/// pool rather than occupying a runtime worker.
#[cfg(test)]
pub(crate) struct AdmissionProbeHook {
    pub(crate) reached: std::sync::mpsc::Sender<()>,
    pub(crate) proceed: Arc<std::sync::Barrier>,
}

/// Test-only handoff before an analyze worker performs its first action.
#[cfg(test)]
pub(crate) struct WorkerStartHook {
    pub(crate) reached: std::sync::mpsc::Sender<()>,
    pub(crate) proceed: Arc<std::sync::Barrier>,
}

/// MCP server exposing the code graph through 23 tools.
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
    /// at startup). The VCS registry starts empty, so history tools report
    /// unavailability; the binary uses [`Self::with_vcs_registry`].
    pub fn new(registry: LanguageRegistry) -> Self {
        Self::with_vcs_registry(registry, code_graph_vcs::VcsRegistry::new())
    }

    /// Construct a server with version-control providers for the history
    /// tools. Kept separate from [`Self::new`] so the ~40 existing test
    /// construction sites stay untouched and provider injection remains a
    /// binary-crate decision (AC-23).
    pub fn with_vcs_registry(registry: LanguageRegistry, vcs: code_graph_vcs::VcsRegistry) -> Self {
        Self {
            inner: Arc::new(ServerInner {
                graph: PlRwLock::new(Graph::new()),
                registry,
                vcs: Arc::new(vcs),
                indexed: AtomicBool::new(false),
                status_publication: PlRwLock::new(()),
                index_lock: TokioMutex::new(()),
                root_path: PlRwLock::new(None),
                applied_index: PlRwLock::new(AppliedIndexState::default()),
                cache_root: PlRwLock::new(None),
                cache_io_root: PlRwLock::new(None),
                daemon_project_root: OnceLock::new(),
                #[cfg(target_os = "linux")]
                daemon_root_identity: OnceLock::new(),
                watch: PlRwLock::new(None),
                config: PlRwLock::new(RootConfig::default()),
                index_built_at: AtomicU64::new(0),
                index_force_built: AtomicBool::new(false),
                persist: Arc::new(PersistCoordinator::new()),
                analyze_slot: PlRwLock::new(AnalyzeSlot::default()),
                #[cfg(test)]
                publication_hook: PlMutex::new(None),
                #[cfg(test)]
                config_discovery_hook: PlMutex::new(None),
                #[cfg(test)]
                admission_probe_hook: PlMutex::new(None),
                #[cfg(test)]
                worker_start_hook: PlMutex::new(None),
            }),
            tool_router: Self::tool_router(),
        }
    }

    /// Number of registered tools. Used by the smoke test to confirm the
    /// `#[tool_router]` macro produced the expected entries before any IO
    /// loop runs.
    pub fn tool_count(&self) -> usize {
        self.tool_router.list_all().len()
    }

    /// Snapshot of every registered tool descriptor (`name`, `description`,
    /// `inputSchema`, …). Used by the wire-format snapshot suite
    /// — the macro-generated `Self::tool_router()` factory is private, so
    /// tests reach for this helper instead.
    pub fn tool_descriptors(&self) -> Vec<rmcp::model::Tool> {
        self.tool_router.list_all()
    }

    /// Bind this server instance to one daemon project root for its lifetime.
    /// Daemon startup calls this once before accepting connections; direct
    /// stdio servers intentionally leave the optional boundary unset.
    pub fn bind_daemon_project_root(&self, root: PathBuf) -> Result<(), PathBuf> {
        self.inner.daemon_project_root.set(root)
    }

    /// Bind the Linux root identity from daemon startup's already-verified
    /// retained directory descriptor.  This is intentionally separate from
    /// the logical root path: only filesystem safety checks use the inode.
    #[cfg(target_os = "linux")]
    pub fn bind_daemon_retained_root(
        &self,
        metadata: &std::fs::Metadata,
    ) -> Result<(), DaemonRootIdentity> {
        self.inner.daemon_root_identity.set(DaemonRootIdentity {
            dev: metadata.dev(),
            ino: metadata.ino(),
        })
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
    /// the em-dash is U+2014 (not a hyphen-minus) and the snapshot
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

    /// Validate a user-supplied `subtree` argument and return either
    /// the normalized prefix to pass into the handler or a
    /// `CallToolResult` error envelope ready to return.
    ///
    /// `Ok(None)` ↔ caller passed no subtree (or empty string) — the
    /// handler should run without a subtree filter.
    /// `Ok(Some(prefix))` ↔ the prefix canonicalized and is at or
    /// under the indexed project root.
    /// `Err(envelope)` ↔ the prefix canonicalized to a path outside
    /// the indexed root. Without this guard,
    /// `subtree="../.."` would canonicalize to an ancestor and the
    /// downstream `starts_with(prefix)` walk would silently leak
    /// results from outside the user's intended scope. Validating
    /// here, at the single MCP-dispatch boundary, keeps the three
    /// `subtree=`-taking handlers (`get_orphans`, `detect_cycles`,
    /// `search_symbols`) consistent without each handler having to
    /// thread `root_path` and an error branch through its arg list.
    ///
    /// The returned `String` is the normalized prefix; handlers
    /// re-normalize via `paths::normalize_user_path` (idempotent on
    /// an already-canonical input), so passing the string form keeps
    /// the existing handler signatures untouched.
    pub fn validate_subtree(&self, raw: Option<&str>) -> Result<Option<String>, CallToolResult> {
        let Some(s) = raw.filter(|s| !s.is_empty()) else {
            return Ok(None);
        };
        let root_guard = self.inner.root_path.read();
        let root = root_guard.as_deref();
        // Resolve a RELATIVE subtree prefix against the indexed project
        // root, NOT the MCP server's process CWD. A client that indexed
        // `/proj` and passes `subtree="src"` means `/proj/src`; but
        // `normalize_user_path("src")` would canonicalize against the
        // server's launch directory (typically invisible to the client),
        // or leave a lexical relative path — either way failing the
        // absolute-prefix `starts_with(root)` guard below. Joining to the
        // root first makes project-relative prefixes behave as clients
        // expect. Absolute inputs pass through untouched. With no indexed
        // root yet (no analyze has run) there is nothing to scope against,
        // so a relative path normalizes lexically as before.
        let candidate = match root {
            Some(root) if Path::new(s).is_relative() => {
                // A project-relative subtree must resolve INSIDE the root.
                // Reject any `..` ascent up front: legitimate prefixes
                // ("src", "src/handlers") never need it, and joining
                // `../escape` to the root would otherwise land outside it —
                // and for a NON-EXISTENT target it would even slip past the
                // component-wise `starts_with` guard below, because
                // `normalize_user_path` does not collapse `..` on a path
                // that isn't on disk.
                if Path::new(s)
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                {
                    return Err(CallToolResult::error(vec![Content::text(format!(
                        "subtree {s:?} escapes the indexed project root {} via a \
                         parent-directory (`..`) segment",
                        root.display(),
                    ))]));
                }
                root.join(s)
            }
            _ => PathBuf::from(s),
        };
        let candidate_str = candidate.to_string_lossy();
        let normalized = code_graph_core::paths::normalize_user_path(&candidate_str);
        if let Some(root) = root {
            if !normalized.starts_with(root) {
                return Err(CallToolResult::error(vec![Content::text(format!(
                    "subtree {:?} resolves to {} which is outside the \
                     indexed project root {}",
                    s,
                    normalized.display(),
                    root.display(),
                ))]));
            }
        }
        Ok(Some(normalized.to_string_lossy().into_owned()))
    }
}

/// Root and configuration provenance atomically published to `get_status`.
///
/// This is deliberately separate from the operational `root_path` and
/// `config` locks: those fields are consumed by watch and query paths, while
/// status must report one self-consistent successful-analyze snapshot without
/// probing the filesystem again.
#[derive(Default)]
pub(crate) struct AppliedIndexState {
    pub(crate) root_path: Option<PathBuf>,
    pub(crate) config_path: Option<PathBuf>,
    pub(crate) config: RootConfig,
}

/// Portable Linux directory identity retained by a daemon for its lifetime.
/// It deliberately stores no file descriptor, so code-graph-tools remains
/// free of platform I/O dependencies and all normal paths stay logical.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DaemonRootIdentity {
    dev: u64,
    ino: u64,
}

impl ServerInner {
    /// Reject a daemon operation once the visible project-root path no longer
    /// names the directory inode captured at daemon startup. Direct servers,
    /// non-Linux targets, and unbound daemon instances intentionally remain
    /// no-ops.
    pub fn ensure_daemon_root_current(&self) -> Result<(), String> {
        #[cfg(target_os = "linux")]
        {
            let (Some(root), Some(identity)) = (
                self.daemon_project_root.get(),
                self.daemon_root_identity.get(),
            ) else {
                return Ok(());
            };
            let metadata = std::fs::symlink_metadata(root).map_err(|error| {
                format!(
                    "daemon project root was replaced or is unavailable at {}: {error}",
                    root.display()
                )
            })?;
            if metadata.is_dir() && metadata.dev() == identity.dev && metadata.ino() == identity.ino
            {
                Ok(())
            } else {
                Err(format!(
                    "daemon project root was replaced at {}; refusing to publish into the replacement namespace",
                    root.display()
                ))
            }
        }
        #[cfg(not(target_os = "linux"))]
        Ok(())
    }
}

// Argument structs ---------------------------------------------------------
//
// One struct per tool; `Parameters<T>` extracts `T` from the JSON-RPC
// request's `arguments` field. `Option<T>` fields mean "absent in the
// request"; required fields are `T` directly. Validation (e.g. "at least
// one of query/kind/namespace/language") is the handler's job.

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
pub struct AnalyzeCodebaseAsyncArgs {
    /// Absolute path to the directory to index.
    #[schemars(description = "Absolute path to the directory to index")]
    pub path: String,
    /// Force full re-index, ignoring any cache.
    #[schemars(description = "Force full re-index, ignoring any cache (default false)")]
    #[serde(default)]
    pub force: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetAnalyzeStatusArgs {
    #[schemars(description = "Async analyze request job ID returned by analyze_codebase_async")]
    pub job_id: String,
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
    #[schemars(
        description = "Restrict matches to symbols whose file path is at or under this \
                       directory prefix. Uses the path-trie's subtree iterator so the \
                       file-set build cost is proportional to the subtree, NOT the whole \
                       graph. Absent / empty value matches across the whole graph."
    )]
    #[serde(default)]
    pub subtree: Option<String>,
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
    #[schemars(
        description = "Edit-distance (fuzzy) search mode. When true, `query` is matched by \
                       Levenshtein distance against symbol names rather than regex/substring. \
                       `query` must be a plain identifier (no regex metacharacters). \
                       `max_distance` controls the threshold (or length-adaptive default). \
                       Results sorted by closest match first. Incompatible with count_only."
    )]
    #[serde(default)]
    pub near: Option<bool>,
    #[schemars(
        description = "Max edit distance for near=true mode (default: length-adaptive — 1 edit \
                       at length 2-11, 2 at 12-17, 3 at 18+). Clamped to 8. Ignored when \
                       near=false."
    )]
    #[serde(default)]
    pub max_distance: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetSymbolDetailArgs {
    #[schemars(
        description = "Symbol ID in format file:name as returned by get_file_symbols or search_symbols"
    )]
    pub symbol: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BlameSymbolArgs {
    #[schemars(
        description = "Symbol ID in format file:name as returned by get_file_symbols or search_symbols"
    )]
    pub symbol: String,
    #[schemars(
        description = "Optional revision specifier to blame at (e.g. a commit hash, tag, or \
                       HEAD~2 for git). Default: the provider's default revision (git: HEAD). \
                       Attribution always reflects committed state at this revision, never \
                       uncommitted working-tree edits."
    )]
    pub at: Option<String>,
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
pub struct GetSymbolAtArgs {
    #[schemars(description = "Absolute path to the source file")]
    pub file: String,
    #[schemars(
        description = "1-based source line. 0 is rejected as a tool error (likely off-by-one)."
    )]
    pub line: u32,
    #[schemars(description = "Maximum enclosing symbols to return (default 100, max 1000)")]
    #[serde(default)]
    pub limit: Option<u32>,
    #[schemars(description = "Skip first N matches for pagination (default 0)")]
    #[serde(default)]
    pub offset: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindPathArgs {
    #[schemars(description = "Source symbol ID in format file:name or file:Parent::name")]
    pub from: String,
    #[schemars(description = "Target symbol ID in format file:name or file:Parent::name")]
    pub to: String,
    #[schemars(
        description = "Maximum nodes to examine before giving up (default 100000, max \
                       5000000). Lower to bound search cost on huge graphs; a search that \
                       hits the cap returns `found: false` with `cap_reached: true`, \
                       distinct from a genuine no-path result (`cap_reached: false`)."
    )]
    #[serde(default)]
    pub node_cap: Option<u32>,
    #[schemars(
        description = "Minimum resolver confidence required for an edge to be traversable. \
                       \"any\" (default) allows every resolved edge; \"resolved\" restricts \
                       the search to edges the resolver was unambiguous about, dropping \
                       Heuristic edges (picked from ≥ 2 same-name candidates via the scope \
                       rule)."
    )]
    #[serde(default)]
    pub min_confidence: Option<String>,
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
    #[schemars(
        description = "Minimum resolver confidence required for a hop to appear. \"any\" \
                       (default) includes every resolved edge; \"resolved\" drops edges \
                       the resolver picked from N same-name candidates (Heuristic) and \
                       returns only chains the resolver was sure about. Heuristic edges \
                       arise when ≥ 2 symbols share a callee name and the scope rule \
                       (same file > same parent > same namespace > global) picked one \
                       — useful filter when several unrelated classes have a method \
                       with the same name (e.g. `init`). Filter applies at each hop, \
                       so a depth-2 walk through a Heuristic intermediate is pruned \
                       entirely."
    )]
    #[serde(default)]
    pub min_confidence: Option<String>,
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
    #[schemars(
        description = "Minimum resolver confidence required for a hop to appear. \"any\" \
                       (default) includes every resolved edge; \"resolved\" drops edges \
                       the resolver picked from N same-name candidates (Heuristic) and \
                       returns only chains the resolver was sure about. Same semantics \
                       as on `get_callers`."
    )]
    #[serde(default)]
    pub min_confidence: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindClassCandidatesArgs {
    #[schemars(
        description = "Short class name to look up (exact match, case-sensitive). E.g. \
                       'UObject' or 'Base'. Returns every class-like symbol with this \
                       name across the indexed graph."
    )]
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindOverridesArgs {
    #[schemars(
        description = "Symbol ID of the virtual or pure-virtual method to find overrides of \
                       (format file:Parent::name)"
    )]
    pub symbol: String,
    #[schemars(description = "Maximum overrides to return per page (default 100, max 1000)")]
    #[serde(default)]
    pub limit: Option<u32>,
    #[schemars(description = "Skip first N overrides for pagination (default 0)")]
    #[serde(default)]
    pub offset: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetDependenciesArgs {
    #[schemars(description = "Absolute path to the source file")]
    pub file: String,
    #[schemars(description = "Max dependency rows to return (default 100, max 1000)")]
    #[serde(default)]
    pub limit: Option<u32>,
    #[schemars(description = "Skip first N rows for pagination (default 0)")]
    #[serde(default)]
    pub offset: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct DetectCyclesArgs {
    #[schemars(
        description = "Restrict reported cycles to those whose every file is at or under \
                       this directory prefix. Cycles touching files outside the prefix \
                       are dropped from the response (cycle DETECTION is still global — \
                       a cycle passing through an outside file is still detected, just \
                       not reported). Absent / empty value reports all cycles."
    )]
    #[serde(default)]
    pub subtree: Option<String>,
    #[schemars(description = "Maximum cycles to return (default 20, max 1000)")]
    #[serde(default)]
    pub limit: Option<u32>,
    #[schemars(description = "Skip first N cycles for pagination (default 0)")]
    #[serde(default)]
    pub offset: Option<u32>,
    #[schemars(
        description = "Maximum file paths to keep per cycle (default 50, max 500). \
                       Cycles longer than this have their `files` list truncated; \
                       that cycle's own `Cycle.truncated` field is then `true` and \
                       its `original_len` = the pre-truncation file count (this is \
                       per-cycle, distinct from the envelope's `truncated`)."
    )]
    #[serde(default)]
    pub max_cycle_size: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetOrphansArgs {
    #[schemars(description = "Filter by symbol kind: function, method (default: all callables)")]
    #[serde(default)]
    pub kind: Option<String>,
    #[schemars(
        description = "Restrict the search to symbols whose file path is at or under this \
                       directory prefix. Walks the path-trie's subtree iterator — O(symbols \
                       under prefix), independent of total graph size. Absent / empty value \
                       searches the whole graph."
    )]
    #[serde(default)]
    pub subtree: Option<String>,
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
    #[schemars(
        description = "Reliability filter: 'all' (default) returns every orphan including known \
                       false-positive classes; 'high' drops virtual methods (often reached via \
                       dynamic dispatch the resolver doesn't track) and macro-synthesized \
                       symbols (the macro IS the call site by construction); 'very_high' \
                       additionally drops (a) templated definitions (C++ functions/methods nested \
                       under a `template_declaration` ancestor, including methods of a templated \
                       class — the symbol carries a `/* template */` signature prefix), \
                       (b) any symbol with at least one incoming Calls edge of any confidence \
                       (catches candidates whose only callers are heuristic-resolution or \
                       bare-token unresolved-source edges), \
                       (c) C++ operator overloads (`operator<`, `operator==`, `operator()`, \
                       etc. — invoked implicitly by STL containers and language built-ins; \
                       gated on Language::Cpp so a Python or Rust `operator` identifier \
                       doesn't trip it), and \
                       (d) constructors in languages whose syntactic constructor name equals \
                       the class name (C++/Java/C#; Rust uses `new()` by convention and is NOT \
                       gated, Python uses `__init__`). 'high' typically cuts the orphan count by \
                       ~half on engine-style codebases; 'very_high' commonly cuts by 80-95%. \
                       Use 'very_high' for dead-code investigations on UE-scale codebases. \
                       Invalid values are rejected with a tool error."
    )]
    #[serde(default)]
    pub reliability: Option<String>,
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
    #[schemars(description = "Skip first N matches for pagination (default 0)")]
    #[serde(default)]
    pub offset: Option<u32>,
    #[schemars(description = "Maximum results to return per side (default 50, max 1000)")]
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DetectCommunitiesArgs {
    #[schemars(
        description = "Aggregation granularity for community detection. Only \"file\" is \
                       currently supported and is the default (absent/empty also resolves \
                       to \"file\"); any other non-empty value is a tool error."
    )]
    #[serde(default)]
    pub granularity: Option<String>,
    #[schemars(
        description = "Maximum label-propagation sweeps before giving up (default 50, max \
                       500; 0 resolves to the default). The response's `termination` field \
                       reports whether propagation converged before this ceiling or was cut \
                       off by it."
    )]
    #[serde(default)]
    pub max_iterations: Option<u32>,
    #[schemars(
        description = "Maximum member file paths kept per community (default 10, max 100; \
                       0 resolves to the default). Independent of `limit`/`offset`, which \
                       page over the LIST of communities — this caps the member list INSIDE \
                       each community. A community whose members are cut by this cap sets \
                       that community's own `truncated: true` and `original_len` to the \
                       pre-truncation member count; raise `members_per_community` to see \
                       fuller member lists, not `limit`."
    )]
    #[serde(default)]
    pub members_per_community: Option<u32>,
    #[schemars(description = "Maximum communities to return (default 100, max 1000)")]
    #[serde(default)]
    pub limit: Option<u32>,
    #[schemars(description = "Skip first N communities for pagination (default 0)")]
    #[serde(default)]
    pub offset: Option<u32>,
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
        description = "Call-graph direction (symbol mode only): 'callees', 'callers', or 'both' (default)"
    )]
    #[serde(default)]
    pub direction: Option<String>,
    #[schemars(
        description = "Minimum resolver confidence required for an edge to appear (symbol \
                       mode only): \"any\" (default) admits Heuristic edges, \"resolved\" \
                       drops edges the resolver picked from N same-name candidates. Same \
                       wire spelling and semantics as on `get_callers`/`get_callees`. \
                       Ignored by file and class modes — file dependencies and \
                       inheritance edges don't carry confidence today."
    )]
    #[serde(default)]
    pub min_confidence: Option<String>,
    #[schemars(
        description = "When format=mermaid, add CSS styling and center node highlighting (default false)"
    )]
    #[serde(default)]
    pub styled: Option<bool>,
}

// Tool router --------------------------------------------------------------

#[tool_router]
impl CodeGraphServer {
    // -- P0 -----------------------------------------------------------------

    #[tool(
        description = "Index a codebase (C/C++, Rust, Go, Python, C#, Java) and build the code graph. Must be called before any query tools. `path` is required; `force` is optional (default false) and bypasses the cache. Concurrent analyzes are serialized: the running scan is unchanged, while pending canonical paths compact by containment and OR `force`; this synchronous call waits for its satisfying terminal `{ files, symbols, edges, root_path, warnings }` result or error."
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
        description = "Kick off `analyze_codebase` on a background task and return \
                       immediately (< 1KB, sub-second) with a job handle. The indexing \
                       pipeline runs detached on the tokio runtime; agents observe \
                        progress and the terminal result by polling `get_analyze_status`. \
                       Prefer this over the sync `analyze_codebase` for large \
                       codebases (UE / LLVM-scale, ~130-200s wall time) or any case \
                       where the client's `MCP_TOOL_TIMEOUT` could fire on a long \
                       sync call — every individual tool call is sub-second, so the \
                       per-call timer never fires. Args: `path` (required, absolute \
                       path to index — same wording as `analyze_codebase`), `force` \
                       (optional, default false — full re-index ignoring the cache). \
                       Response shape: \
                       `{ job_id, status, started_at, existing, note }`. `job_id` is \
                         a distinct 20-char zero-padded decimal async-request handle, including \
                         when pending work is absorbed; `status` is always the literal \
                        `\"running\"` at kickoff; `started_at` is RFC3339 UTC; \
                        `existing` is false for every accepted request; \
                       `note` is a short human-readable hint. **Polling pattern:** \
                        call `get_analyze_status(job_id)` with this response's handle. It \
                        resolves an alias to the satisfying canonical `AnalyzeJobView`; poll \
                        while `status == \"running\"`; once it flips to \
                        `\"completed\"` read `result` (byte-identical \
                       shape to `analyze_codebase`'s success body — `{ files, \
                       symbols, edges, root_path, warnings }`); on `\"failed\"` \
                        read `error`. A poll cadence of 250-1000ms is \
                       a reasonable starting point — the slot and inner state \
                       locks are held only for `Arc::clone` and a small struct \
                       read on each poll, so polling does not contend with the \
                       worker meaningfully. **Grace window:** after a job \
                       terminates its result is preserved in \
                       `analyze_job_previous_terminal` for exactly ONE additional \
                       kickoff — if you start a new analyze before reading the \
                       prior terminal, the prior result is still recoverable for \
                        that one rotation, after which it is gone. **Concurrent \
                        kickoff:** every arrival while a scan is running enters \
                        the pending analyze-only FIFO; only pending canonical \
                        paths compact, never the running scan. An equal or \
                        descendant request follows its pending coverer; an \
                        incoming ancestor replaces pending descendants at the \
                        earliest displaced FIFO position; disjoint paths keep \
                        FIFO order. The response still returns immediately and \
                         callers poll their own handle; `force` is ORed across compacted \
                         pending work. The queue permits 32 non-terminal pending requests, \
                         including synchronous and asynchronous followers; a further request \
                         returns a retryable queue-full tool error even if it is covered."
    )]
    async fn analyze_codebase_async(
        &self,
        Parameters(args): Parameters<AnalyzeCodebaseAsyncArgs>,
    ) -> Result<CallToolResult, McpError> {
        Ok(handlers::analyze::analyze_codebase_async(
            self.inner.clone(),
            args.path,
            args.force.unwrap_or(false),
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
                        `[response].max_bytes` (default 100KB); when either `limit` or the \
                        byte budget leaves records un-emitted, `truncated` is true and \
                        `next_offset` points at the first un-emitted record — re-call with \
                        `offset = next_offset` to resume. If a byte-starved page is empty and \
                        `next_offset` equals the requested `offset`, that is a start-fresh marker, \
                        not a continuation: do not retry unchanged; raise `[response].max_bytes`, \
                        re-run `analyze_codebase`, then retry. \
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
                       Returns the `Page<SymbolResult>` envelope {results, total, \
                       offset, limit, truncated, next_offset} (flattened on the wire), \
                       sorted by symbol_id ascending, PLUS an optional `suggestions: \
                       string[]` field — see the suggestions block below. At least one \
                       filter is expected: `query` (substring or regex on the symbol \
                       name), `kind` (function, method, class, struct, enum, typedef, \
                       interface, trait), `namespace` (substring match against the \
                       symbol's namespace path, e.g. 'Nfs' matches 'Ark::Nfs::V4'), \
                       and/or `language` (cpp, rust, go, python, csharp, java). `limit` \
                       defaults to 20 (max 1000, clamped silently — the echoed `limit` \
                       reflects the resolved value); raise `limit` for broad searches \
                       expected to return many hits, and use `offset` to advance \
                       through the remainder. `offset` defaults to 0; raise `offset` \
                       to skip past previous results, or set `offset = next_offset` to \
                       resume after a truncated page. `brief` (default true) omits \
                       signature, column, and end_line; set false for full detail. \
                       `count_only=true` returns the match total with an empty \
                       `results` array in a < 1KB bounded response — use it to size a \
                       search before committing to paging. **`suggestions: string[]` \
                       (did-you-mean field):** included in the response ONLY when (a) \
                       `query` is anchored as `^…$` with length ≥ 2 (and the inner \
                       pattern is non-empty), AND (b) `total == 0` (no matches under \
                       the anchored query). Holds up to 5 candidate symbol-id strings. \
                       When the inner pattern is a plain identifier (no regex \
                       metacharacters), candidates are sorted by ascending Levenshtein \
                       distance to the inner pattern (length-adaptive threshold: 1 edit \
                       at length 2-11, 2 at 12-17, 3 at 18+), then by common-prefix \
                       length DESC (so obvious-intent names like `Actor` surface for \
                       a `^Actr$` typo instead of getting buried under alphabetically- \
                       earlier same-length candidates), then by `is_shorter_than_query` \
                       asc (insert-typo bias — when a delete-typo and an insert-typo \
                       tie on every other key, the insert-typo wins because users \
                       under-type more often than over-type), then by length-closeness, \
                       then name. Falls back to a broad substring match when no symbols are \
                       within the edit-distance threshold. When the inner pattern \
                       contains regex metacharacters, candidates come from the broad \
                       substring match alone. **Absent from the wire entirely when empty** (no \
                       `\"suggestions\": []` key — serialization skips empty lists), \
                       so a present `suggestions` field is itself a signal that the \
                       anchored query missed. **Never emitted on `count_only=true`** \
                       (the count_only path short-circuits before the suggestion \
                       block), so clients counting matches must use plain \
                       `query` (no anchors) or call again without `count_only` to \
                       receive suggestions. Responses are also capped by \
                        `[response].max_bytes` (default 100KB); when either `limit` or the \
                        byte budget leaves records un-emitted, `truncated` is true and \
                         `next_offset` points at the first un-emitted record — re-call with \
                         `offset = next_offset` to resume. If a byte-starved page is empty and \
                        `next_offset` equals the requested `offset`, that is a start-fresh marker, \
                        not a continuation: do not retry unchanged; raise `[response].max_bytes`, \
                        re-run `analyze_codebase`, then retry. `truncated=false` plus \
                        `next_offset=null` means the page \
                       is complete. `results.length` may be less than `limit` when the \
                       byte cap fires, so consult `truncated`, not length, to detect \
                       partial pages."
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
            subtree: args.subtree.as_deref(),
            limit: args.limit,
            offset: args.offset,
            brief: args.brief.unwrap_or(true),
            count_only: args.count_only.unwrap_or(false),
            near: args.near.unwrap_or(false),
            max_distance: args.max_distance,
        };
        let max_bytes = self.inner.config.read().response.max_bytes;
        let validated_subtree = match self.validate_subtree(input.subtree) {
            Ok(v) => v,
            Err(r) => return Ok(r),
        };
        let input = handlers::symbols::SearchSymbolsInput {
            subtree: validated_subtree.as_deref(),
            ..input
        };
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
        description = "Who last changed a symbol: per-line authorship for the symbol's line \
                       span, from version control. `symbol` (required) is the symbol ID as \
                       returned by get_file_symbols/search_symbols. `at` (optional) is a \
                       revision specifier (commit hash, tag, HEAD~2); default is the \
                       provider's default revision (git: HEAD) — attribution always reflects \
                       committed state at that revision, never uncommitted edits. Returns a \
                       single JSON object (NOT a Page envelope): {available, reason?, \
                       symbol_id, file, start_line, end_line, rev, stale, stale_reason?, \
                       hunks}, where each hunk is {rev, author, timestamp_utc, start_line, \
                       line_count} clipped to the span. `available: false` with `reason` is a \
                       SUCCESS, not an error — it means no supported VCS covers the indexed \
                       root or this path has no history at the blamed revision; every other \
                       tool works normally either way. `stale: true` means the on-disk file \
                       (which the span's line numbers come from) differs from the blamed \
                       revision (line-ending-insensitive compare, so autocrlf checkouts stay \
                       clean), so attributed line numbers may misalign — commit the edits \
                       or pass `at` naming a revision matching the indexed state, then \
                       re-call. `stale_reason` may also accompany `stale: false` when the \
                       comparison could not be performed or the span has no attributable \
                       lines at the blamed revision; absent `stale_reason` means verified \
                       clean. Attribution is line-granular: a symbol sharing its first or \
                       last line with another symbol has that shared line attributed to \
                       both. Unknown `symbol` is a tool error with did-you-mean suggestions; \
                       an unresolvable `at` is a tool error naming the bad specifier."
    )]
    async fn blame_symbol(
        &self,
        Parameters(args): Parameters<BlameSymbolArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(r) = self.require_indexed() {
            return Ok(r);
        }
        let root = self.inner.root_path.read().clone();
        Ok(handlers::history::blame_symbol(
            &self.inner.graph,
            &self.inner.vcs,
            root,
            &args.symbol,
            args.at.as_deref(),
        )
        .await)
    }

    #[tool(
        description = "Get symbol counts grouped by namespace and kind — useful for codebase \
                       orientation. Returns a `Page<SummaryRow>` envelope: {results, total, \
                       offset, limit, truncated, next_offset}, where each row is \
                       `{namespace, kind, count}`. NOTE on `<global>`: rows with \
                       `namespace == \"<global>\"` are a display label for the empty \
                       namespace (symbols defined at global scope). `search_symbols` \
                       cannot currently filter to global-scope symbols only — its \
                       `namespace` field is a case-insensitive substring filter where \
                       the empty string means \"no filter\", so \
                       `search_symbols(namespace=\"\")` returns all symbols rather than \
                       only global-scope ones. To investigate global-scope symbols, use \
                       this tool to confirm they exist, then inspect them via \
                       `search_symbols` with other filters (e.g., by `kind` or `query`). \
                       Rows are sorted by `(namespace, kind)` ascending so paging is \
                       deterministic across calls. `limit` defaults to 100 (max 1000, \
                       clamped silently — the echoed `limit` reflects the resolved \
                       value); raise `limit` for more rows per page on large codebases \
                       with many distinct namespaces. `offset` defaults to 0; raise \
                       `offset` to skip past previous results. `count_only=true` returns \
                       the sentinel page with `total` = the `(namespace, kind)` pair \
                       count (NOT the sum of individual symbols) and an empty `results` \
                       array in a < 1KB bounded response — use it to size the row set \
                       before paging. Responses are also capped by \
                        `[response].max_bytes` (default 100KB); when either `limit` or the \
                        byte budget leaves rows un-emitted, `truncated` is true and \
                         `next_offset` points at the first un-emitted row — re-call with \
                         `offset = next_offset` to resume. If a byte-starved page is empty and \
                        `next_offset` equals the requested `offset`, that is a start-fresh marker, \
                        not a continuation: do not retry unchanged; raise `[response].max_bytes`, \
                        re-run `analyze_codebase`, then retry. \
                       `truncated=false` plus `next_offset=null` means the page is \
                       complete. `results.length` may be less than `limit` when the byte \
                       cap fires, so consult `truncated`, not length, to detect partial \
                       pages."
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
        description = "Find the symbol(s) whose source span encloses a given file+line — \
                       e.g. 'what function/method is at file X, line N'. NOT goto-definition: \
                       it does not resolve the identifier written at that position to its \
                       binding (scope resolution is out of scope for this tool); it answers \
                       'what encloses this line' purely by span containment (line is between \
                       a symbol's `line` and `end_line`). Enclosure is LINE-granular, not \
                       column-granular — symbols carry no end column, so if two symbols share \
                       the same starting line (e.g. a one-line method and its enclosing \
                       one-line class), both match. Returns the `Page<EnclosingSymbol>` \
                       envelope {results, total, offset, limit, truncated, next_offset}, \
                       where each row is {symbol_id, name, kind, line, end_line, span_lines, \
                       parent, namespace}. Results are ordered innermost-first (smallest \
                       `span_lines` ascending, then `line` descending, then `symbol_id` \
                       ascending) — a method nested in a class returns the method before the \
                       class. `file` is an absolute path resolved against the indexed graph \
                       (`\\\\?\\` extended-path prefix handled automatically, relative `.`/`..` \
                       segments resolved against the on-disk file when it exists); an unknown \
                       file is a tool error naming the path. `line` is 1-based and REQUIRED; \
                       `line = 0` is a tool error (very likely an off-by-one bug, not silently \
                       clamped). A line enclosed by no symbol returns SUCCESS with an empty \
                       `Page` (results: [], total: 0) — never an error and never a \
                       nearest-symbol guess. `limit` defaults to 100 (max 1000, clamped \
                       silently — the echoed `limit` reflects the resolved value); raise \
                       `limit` for deeply-nested spans, or use `offset` to page through. \
                        Responses are also capped by `[response].max_bytes` (default 100KB); \
                        when either `limit` or the byte budget leaves records un-emitted, \
                        `truncated` is true and `next_offset` points at the first un-emitted \
                         record — re-call with `offset = next_offset` to resume. If a byte-starved \
                        page is empty and `next_offset` equals the requested `offset`, that is a \
                        start-fresh marker, not a continuation: do not retry unchanged; raise \
                        `[response].max_bytes`, re-run `analyze_codebase`, then retry. \
                        `truncated=false` plus `next_offset=null` \
                       means the page is complete."
    )]
    async fn get_symbol_at(
        &self,
        Parameters(args): Parameters<GetSymbolAtArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(r) = self.require_indexed() {
            return Ok(r);
        }
        let max_bytes = self.inner.config.read().response.max_bytes;
        // `paths::normalize_user_path` inside the handler may block on a
        // filesystem stat (see `get_file_symbols` comment).
        let inner = self.inner.clone();
        let result = tokio::task::spawn_blocking(move || {
            handlers::symbols::get_symbol_at(
                &inner.graph,
                &args.file,
                args.line,
                args.limit,
                args.offset,
                max_bytes,
            )
        })
        .await;
        Ok(match result {
            Ok(r) => r,
            Err(e) => handlers::tool_error(format!("get_symbol_at task panicked: {e}")),
        })
    }

    #[tool(
        description = "Find functions that call the given symbol (upstream call chain). \
                       `symbol` is a Symbol ID in the `file:name` or `file:Parent::name` \
                       format returned by get_file_symbols/search_symbols. Returns the \
                       `Page<CallChain>` envelope {results, total, offset, limit, \
                       truncated, next_offset}, where each `CallChain` is \
                       `{symbol_id, file, line, depth}` and is sorted by \
                       (depth, symbol_id) ascending so the closest callers appear first. \
                       **CallChain field semantics:** `symbol_id` is the DEFINITION site \
                       (the caller being reported, in `file:name`/`file:Parent::name` \
                       form); `file` and `line` are the CALL site — the source file and \
                       line of the `Calls` edge that reached this hop. At depth 1 the \
                       call site lives in the caller's own file by definition; at depth \
                       ≥ 2 `file` and the file segment of `symbol_id` routinely diverge \
                       across crates (a caller defined in crate `foo` may be reached \
                       through a call site that lives in crate `baz`). To recover \
                       \"where is this defined?\" split `symbol_id` on the rightmost `:` \
                       not part of `::` — do NOT read `file`. **Resolved-only filter \
                       (parity with `generate_diagram`):** hops whose target is not a \
                       resolved project symbol are dropped at BFS time and never appear \
                       — bare-token unresolved callers (e.g. `Ok`, `printf`, \
                       `to_string`, language-builtin macros/stdlib calls) are filtered \
                       out uniformly across all six languages, so a function whose only \
                       incoming callers are unresolved tokens returns an empty page, \
                       not a page of bare-token rows. **Non-callable soft-hint \
                       (success, not error):** calling `get_callers` on a symbol whose \
                       kind is `struct`, `enum`, `trait`, `typedef`, or `interface` \
                       returns a `CallToolResult` SUCCESS (`is_error: false`) whose body \
                       is a plain-text advisory naming the symbol + kind and routing to \
                       `get_class_hierarchy` or `get_symbol_detail` (structural kinds: \
                       struct/enum/trait/interface — both tools offered), or to \
                       `get_symbol_detail` only (typedef) — NOT the `Page<CallChain>` \
                       envelope, so a JSON parse of the body will fail; clients \
                       pattern-matching the envelope must try plain-text first. A \
                       callable symbol with zero resolved callers still returns the \
                       empty `Page<CallChain>` envelope, \
                       preserving the trichotomy (wrong symbol → tool error; wrong tool \
                       for the kind → soft-hint success; callable with no resolved \
                       callers → empty envelope). `depth` defaults to 1 (direct callers \
                       only); raise it to walk further upstream. `limit` defaults to \
                       100 (max 1000, clamped silently — the echoed `limit` reflects \
                       the resolved value); raise `limit` for hot symbols with high \
                       fan-in (e.g. UObject::Serialize), use `offset` to page through \
                       the remainder, or narrow by lowering `depth`. `offset` defaults \
                       to 0; raise `offset` to skip past previous results, or set \
                       `offset = next_offset` to resume. Responses are also capped by \
                        `[response].max_bytes` (default 100KB); when either `limit` or the \
                        byte budget leaves records un-emitted, `truncated` is true and \
                         `next_offset` points at the first un-emitted record — re-call with \
                         `offset = next_offset` to resume. If a byte-starved page is empty and \
                        `next_offset` equals the requested `offset`, that is a start-fresh marker, \
                        not a continuation: do not retry unchanged; raise `[response].max_bytes`, \
                        re-run `analyze_codebase`, then retry. `truncated=false` plus \
                        `next_offset=null` means the page \
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
            args.min_confidence.as_deref(),
        ))
    }

    #[tool(
        description = "Find functions called by the given symbol (downstream call \
                       chain). `symbol` is a Symbol ID in the `file:name` or \
                       `file:Parent::name` format returned by \
                       get_file_symbols/search_symbols. Returns the `Page<CallChain>` \
                       envelope {results, total, offset, limit, truncated, \
                       next_offset}, where each `CallChain` is \
                       `{symbol_id, file, line, depth}` and is sorted by \
                       (depth, symbol_id) ascending so the closest callees appear \
                       first. **CallChain field semantics:** `symbol_id` is the \
                       DEFINITION site (the callee being reported, in \
                       `file:name`/`file:Parent::name` form); `file` and `line` are the \
                       CALL site — the source file and line of the `Calls` edge that \
                       reached this hop. The call site (`file`) is always in the \
                       queried symbol's file — the function making the call — never \
                       in the callee's definition file. So `file` and the file segment \
                       of `symbol_id` diverge whenever the callee is defined outside \
                       the queried symbol's file, including at depth 1 (any cross-file \
                       call); at depth ≥ 2 the same asymmetry compounds as the BFS \
                       hops through intermediate frames. To recover \"where is this \
                       defined?\" split `symbol_id` on the rightmost `:` not part of \
                       `::` — do NOT read `file`; that rule applies uniformly at all \
                       depths. **Resolved-only filter (parity with \
                       `generate_diagram`):** hops whose target is not a resolved \
                       project symbol are dropped at BFS time and never appear — \
                       bare-token unresolved callees (e.g. `Ok`, `printf`, \
                       `to_string`, language-builtin macros/stdlib calls) are filtered \
                       out uniformly across all six languages, so a function whose only \
                       outgoing calls hit unresolved tokens returns an empty page, not \
                       a page of bare-token rows. **Non-callable soft-hint (success, \
                       not error):** calling `get_callees` on a symbol whose kind is \
                       `struct`, `enum`, `trait`, `typedef`, or `interface` returns a \
                       `CallToolResult` SUCCESS (`is_error: false`) whose body is a \
                       plain-text advisory naming the symbol + kind and routing to \
                       `get_class_hierarchy` or `get_symbol_detail` (structural kinds: \
                       struct/enum/trait/interface — both tools offered), or to \
                       `get_symbol_detail` only (typedef) — NOT the `Page<CallChain>` \
                       envelope, so a JSON parse of the body will fail; clients \
                       pattern-matching the envelope must try plain-text first. A \
                       callable symbol with zero resolved callees still returns the \
                       empty `Page<CallChain>` envelope, \
                       preserving the trichotomy (wrong symbol → tool error; wrong \
                       tool for the kind → soft-hint success; callable with no \
                       resolved callees → empty envelope). `depth` defaults to 1 \
                       (direct callees only); raise it to walk further downstream. \
                       `limit` defaults to 100 (max 1000, clamped silently — the echoed \
                       `limit` reflects the resolved value); raise `limit` for symbols \
                       with wide fan-out, use `offset` to page through the remainder, \
                       or narrow by lowering `depth` to scope a specific subtree. \
                       `offset` defaults to 0; raise `offset` to skip past previous \
                       results, or set `offset = next_offset` to resume. Responses are \
                        also capped by `[response].max_bytes` (default 100KB); when either \
                        `limit` or the byte budget leaves records un-emitted, `truncated` is \
                         true and `next_offset` points at the first un-emitted record — re-call \
                         with `offset = next_offset` to resume. If a byte-starved page is empty \
                        and `next_offset` equals the requested `offset`, that is a start-fresh \
                        marker, not a continuation: do not retry unchanged; raise \
                        `[response].max_bytes`, re-run `analyze_codebase`, then retry. \
                        `truncated=false` plus `next_offset=null` \
                       means the page is complete. `results.length` may be less than \
                       `limit` when the byte cap fires, so consult `truncated`, not \
                       length, to detect partial pages."
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
            args.min_confidence.as_deref(),
        ))
    }

    #[tool(
        description = "Find the shortest call-path from one symbol to another — a chain of \
                       `Calls` edges answering 'how does A reach B', without walking \
                       `get_callers`/`get_callees` by hand. Returns a SINGLE JSON OBJECT \
                       {found, hops, hop_count, heuristic_hops, nodes_examined, node_cap, \
                       cap_reached} — NOT a `Page` envelope, since this is one shortest-path \
                       answer, not a list. `hops` is an array of `{symbol_id, file, line, \
                       entered_by}`: `hops[0]` is `from` (its `entered_by` is null, since no \
                       edge reached it), `hops[last]` is `to`, and every adjacent pair is a \
                       real `Calls` edge; `hop_count = hops.length - 1` (the edge count, not \
                       the node count). IMPORTANT CAVEAT: call resolution is a syntactic \
                       heuristic (same file > same parent > same namespace > global, per \
                       CLAUDE.md), so a returned path is EVIDENCE that a call chain likely \
                       exists, not a proof — treat `heuristic_hops` (the count of edges on \
                       the path whose resolver confidence was Heuristic rather than Resolved) \
                       as a trust signal; a path with `heuristic_hops: 0` is fully resolved. \
                       Among equal-hop-count paths the search prefers the one with fewer \
                       Heuristic edges. `found: false` is SUCCESS, not an error — it covers \
                       two distinct cases distinguished ONLY by `cap_reached`: `cap_reached: \
                       false` means no path exists in the graph at all; `cap_reached: true` \
                       means the search gave up after examining `node_cap` nodes without \
                       finding one — raise `node_cap` to search further, don't treat it as \
                       'no path'. `from == to` is success with `hop_count: 0` and a \
                       single-element `hops`, not an error. Args: `from`/`to` (required \
                       Symbol IDs in `file:name`/`file:Parent::name` form; either being \
                       unknown is a tool error with did-you-mean suggestions). `node_cap` \
                       defaults to 100000 (max 5000000, clamped silently — 0 resolves to the \
                       default; the echoed `node_cap` reflects the resolved value); lower it \
                       to bound search cost, raise it if `cap_reached` fires before finding a \
                       path. `min_confidence` — \"any\" (default) traverses every resolved \
                       edge; \"resolved\" restricts the search to edges the resolver was \
                       unambiguous about, dropping Heuristic edges entirely from the search \
                       space (not merely from the result)."
    )]
    async fn find_path(
        &self,
        Parameters(args): Parameters<FindPathArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(r) = self.require_indexed() {
            return Ok(r);
        }
        // The Dijkstra search holds a parking_lot read guard with no await
        // point across the whole traversal; on the multi-thread runtime that
        // would occupy a worker and block a concurrent watch reindex needing
        // the write lock (see `get_coupling`).
        let inner = self.inner.clone();
        let from = args.from.clone();
        let to = args.to.clone();
        let min_confidence = args.min_confidence.clone();
        let result = tokio::task::spawn_blocking(move || {
            handlers::query::find_path(
                &inner.graph,
                &from,
                &to,
                args.node_cap,
                min_confidence.as_deref(),
            )
        })
        .await;
        Ok(match result {
            Ok(r) => r,
            Err(e) => handlers::tool_error(format!("find_path task panicked: {e}")),
        })
    }

    #[tool(
        description = "Find every method that overrides the given (virtual or pure-virtual) \
                       method. Returns the standard `Page<CallChain>` envelope \
                       {results, total, offset, limit, truncated, next_offset}, where each \
                       row is `{symbol_id, file, line, depth}`. `depth` is always 1 — \
                       override is a single-step language relation by design; for transitive \
                       analysis compose `find_overrides` with `get_callers`. Sort is by \
                       `symbol_id` ascending. Unknown symbols return the standard \
                       'symbol not found' error with did-you-mean suggestions; a known \
                       method with no overrides returns the empty `Page<CallChain>` \
                       envelope. Currently only C++ extracts `EdgeKind::Overrides` edges \
                       (detection: `virtual` declarator on a method whose parent class has \
                       Inherits edges); other languages always return empty until their \
                       extractors are extended. `limit` defaults to 100 (max 1000, clamped \
                       silently); `offset` defaults to 0. Response capped at \
                        `[response].max_bytes` (default 100KB). Re-call with `offset = \
                        next_offset` to resume a truncated page, except when a byte-starved page \
                        is empty and `next_offset` equals the requested `offset`: that is a \
                        start-fresh marker, not a continuation. Do not retry unchanged; raise \
                        `[response].max_bytes`, re-run `analyze_codebase`, then retry."
    )]
    async fn find_overrides(
        &self,
        Parameters(args): Parameters<FindOverridesArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(r) = self.require_indexed() {
            return Ok(r);
        }
        let max_bytes = self.inner.config.read().response.max_bytes;
        Ok(handlers::query::find_overrides(
            &self.inner.graph,
            &args.symbol,
            args.limit,
            args.offset,
            max_bytes,
        ))
    }

    #[tool(
        description = "List files included/imported by the given file. Path is resolved \
                       against the indexed graph; `\\\\?\\` extended-path prefix is handled \
                       automatically, and relative segments (`.` / `..`) resolve against \
                       the on-disk file when it exists (otherwise lexical-only). Returns \
                       the {results, total, offset, limit, truncated, next_offset} \
                       envelope; each `results[i]` is {file, kind, line} where `file` is \
                       the included path, `kind` is \"includes\", and `line` is the source \
                       line of the include/import directive. Only includes that resolve \
                       to an indexed source file appear: targets that do not \
                       (system/external headers, `.ini`/`.cfg`, `.txt`, anything no \
                       language \
                       plugin claims) are filtered at index time and are absent from \
                       dependencies. Rows are sorted by (file, \
                       line) ascending so pagination is deterministic. An unknown file is \
                       not an error — it returns an empty page (results: [], total: 0). \
                       `limit` defaults to 100 (max 1000, clamped silently — the echoed \
                       `limit` reflects the resolved value); raise `limit` for files with \
                        many includes, or use `offset` to page through. Responses are also \
                        capped by `[response].max_bytes` (default 100KB); when either `limit` \
                        or the byte budget leaves records un-emitted, `truncated` is true and \
                         `next_offset` points at the first un-emitted record — re-call with \
                         `offset = next_offset` to resume. If a byte-starved page is empty and \
                        `next_offset` equals the requested `offset`, that is a start-fresh marker, \
                        not a continuation: do not retry unchanged; raise `[response].max_bytes`, \
                        re-run `analyze_codebase`, then retry. `truncated=false` plus \
                        `next_offset=null` means the page \
                       is complete. `results.length` may be less than `limit` when the \
                       byte cap fires, so consult `truncated`, not length, to detect \
                       partial pages."
    )]
    async fn get_dependencies(
        &self,
        Parameters(args): Parameters<GetDependenciesArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(r) = self.require_indexed() {
            return Ok(r);
        }
        let max_bytes = self.inner.config.read().response.max_bytes;
        // `paths::normalize_user_path` inside the handler may block on a
        // filesystem stat (see `get_file_symbols` comment).
        let inner = self.inner.clone();
        let result = tokio::task::spawn_blocking(move || {
            handlers::query::get_dependencies(
                &inner.graph,
                &args.file,
                args.limit,
                args.offset,
                max_bytes,
            )
        })
        .await;
        Ok(match result {
            Ok(r) => r,
            Err(e) => handlers::tool_error(format!("get_dependencies task panicked: {e}")),
        })
    }

    // -- P1+P2 + watch ------------------------------------------------------

    #[tool(
        description = "Detect circular include dependencies (strongly-connected components of the include graph). Returns the {results, total, offset, limit, truncated, next_offset} envelope; each `results[i]` is a `Cycle` {files, truncated, original_len?} — `files` is the file paths in one cycle in canonical sorted order, `truncated` is that cycle's own per-file-list flag, and `original_len` (the pre-truncation file count) is present ONLY when that cycle's file list was shortened. There are TWO independent `truncated` notions: the ENVELOPE's `truncated` means there are more cycles in further pages; each `Cycle.truncated` means that one cycle's `files` list was capped — they do not imply each other. The envelope's `truncated`/`next_offset` are honest and by-COUNT: `truncated=true` with a non-null `next_offset` means more cycles remain — re-call with `offset = next_offset` to resume; `truncated=false` plus `next_offset=null` means this page is the complete/last set of cycles. Cycles are sorted internally by path and the outer list by each cycle's first path, so pagination is deterministic across calls. The byte budget at [response].max_bytes does NOT apply to `detect_cycles`: cycle pagination is purely by-count via `limit`/`offset`, never by serialized size. `limit` defaults to 20 (0 resolves to 20; clamped at 1000, the echoed `limit` reflects the resolved value); the default is small because cycles are rare in well-maintained codebases. `offset` defaults to 0; to advance through pages set `offset = next_offset` from the truncated envelope (not an arbitrary increment). `max_cycle_size` defaults to 50 (0 resolves to 50; clamped at 500) and caps each returned cycle's `files` list: a cycle longer than the cap is shortened in place and reports `Cycle.truncated: true` with `Cycle.original_len` = the pre-truncation file count; cycles at or under the cap keep `truncated: false` with `original_len` absent. Raise `max_cycle_size` to see fuller file lists for large SCCs. Consult the envelope `truncated`, not `results.length`, to detect whether more cycle pages remain."
    )]
    async fn detect_cycles(
        &self,
        Parameters(args): Parameters<DetectCyclesArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(r) = self.require_indexed() {
            return Ok(r);
        }
        let validated_subtree = match self.validate_subtree(args.subtree.as_deref()) {
            Ok(v) => v,
            Err(r) => return Ok(r),
        };
        Ok(handlers::structure::detect_cycles(
            &self.inner.graph,
            validated_subtree.as_deref(),
            args.limit,
            args.offset,
            args.max_cycle_size,
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
                        `[response].max_bytes` (default 100KB); when either `limit` or the \
                        byte budget leaves records un-emitted, `truncated` is true and \
                         `next_offset` points at the first un-emitted record — re-call with \
                         `offset = next_offset` to resume. If a byte-starved page is empty and \
                        `next_offset` equals the requested `offset`, that is a start-fresh marker, \
                        not a continuation: do not retry unchanged; raise `[response].max_bytes`, \
                        re-run `analyze_codebase`, then retry. `truncated=false` plus \
                        `next_offset=null` means the \
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
        let validated_subtree = match self.validate_subtree(args.subtree.as_deref()) {
            Ok(v) => v,
            Err(r) => return Ok(r),
        };
        Ok(handlers::structure::get_orphans(
            &self.inner.graph,
            args.kind.as_deref(),
            validated_subtree.as_deref(),
            args.limit,
            args.offset,
            args.brief,
            args.count_only.unwrap_or(false),
            args.reliability.as_deref(),
            max_bytes,
        ))
    }

    #[tool(
        description = "Get the inheritance tree for a class. The tree always walks BOTH \
                       directions from the queried class: ancestors (its `bases`, via forward \
                       `Inherits` edges) and descendants (its `derived`, via reverse `Inherits` \
                       edges). There is no direction argument. Returns the \
                       {hierarchy, truncated, max_nodes, total_nodes_seen} envelope: \
                       `hierarchy` is the `HierarchyNode` tree rooted at the queried class; \
                       `truncated: true` flags that the `max_nodes` budget cut off children \
                       — raise `max_nodes` (≤ 1000) to lift the budget, or lower `depth` to \
                       request a smaller tree that fits the current budget; \
                       `total_nodes_seen` is the count of UNIQUE class names walked — a \
                       diamond ancestor reachable via N arms counts as 1, not N, so this can \
                       be far smaller than the number of nodes rendered when ref-stubs are \
                       present. Args: `class` (required, exact name). `depth` (default 1 = \
                       direct bases/derived only; 0 or omitted resolves to 1) bounds transitive \
                       traversal. `max_nodes` (default 250, max 1000; values > 1000 clamp to \
                       1000, 0 resolves to the 250 default) bounds unique class names in the \
                       tree — 250 fits most hierarchies under the MCP token ceiling, but a \
                       single deep tree (e.g. UE's UObject) can exceed it; raise `max_nodes` \
                       (not `depth`) to render more of a truncated tree. Each `HierarchyNode` \
                       is {name, bases?, derived?, ref?}; empty `bases`/`derived` are omitted, \
                       and `ref` is present only when `true` (it is never emitted as false). \
                       SHAPE: in multi-inheritance (diamond) graphs \
                       a shared base/derived appears once in canonical form and then as \
                       ref-stubs. The FIRST occurrence of a name in DFS pre-order is the \
                       canonical node carrying the full `bases`/`derived` subtree; every \
                       SUBSEQUENT occurrence is a {name, ref: true} stub with empty \
                       `bases`/`derived`. To reconstruct the full tree, maintain a \
                       `name -> node` map keyed on the first NON-ref occurrence of each name \
                       and treat a `ref: true` node as a pointer back to that canonical entry, \
                       NOT as a leaf. A `{name}` node with NO `ref` field is walk-terminal: \
                       it is either a natural leaf or a cycle-guard halt (the two are \
                       JSON-indistinguishable) — do not resolve it against the map. A \
                       `ref: true` stub is the ONLY node you follow back to a canonical entry."
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
        description = "Disambiguation tool: list every class-like symbol (Class / Struct / \
                       Interface / Trait) whose short name exactly matches `name`. Returns a \
                       JSON array of `SymbolResult` records sorted by (file, line). Used to \
                       answer the question raised by `get_class_hierarchy`'s ambiguity \
                       error: 'this name has N candidates, here are their fully-qualified \
                       symbol_ids — pick one and use get_symbol_detail / get_callers on it \
                       instead of the bare name.' Empty result is `[]`, not an error: zero \
                       candidates is meaningful (the name doesn't exist as a class-like \
                       symbol) and lets clients building UI on top treat it as 'nothing to \
                       disambiguate' without special-casing."
    )]
    async fn find_class_candidates(
        &self,
        Parameters(args): Parameters<FindClassCandidatesArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(r) = self.require_indexed() {
            return Ok(r);
        }
        Ok(handlers::structure::find_class_candidates(
            &self.inner.graph,
            &args.name,
        ))
    }

    #[tool(
        description = "Cross-file coupling counts for a file: how many call+include \
                       edges connect it to each other indexed source file. `direction` \
                       selects the response SHAPE: `\"outgoing\"` (DEFAULT — edges \
                       leaving `file`) or `\"incoming\"` (edges pointing into `file`) \
                       each return the {results, total, offset, limit, truncated, \
                       next_offset} envelope where each `results[i]` is {file, count} \
                       (count = number of edges between the two files). `\"both\"` \
                       returns a DIFFERENT shape: {incoming, outgoing} where each value \
                       is its own independent {results, total, offset, limit, \
                       truncated, next_offset} page — there is NO top-level `results` \
                       array in `both` mode. Accepted `direction` values are \
                       `outgoing`, `incoming`, `both`; absent/empty resolves to \
                       `outgoing`; any other spelling is a tool error. Rows are sorted \
                       by `count` descending, then `file` ascending, so the most \
                       tightly-coupled files page first and pagination is deterministic \
                       across calls. `limit` defaults to 50 per side (max 1000, clamped \
                       silently — the echoed `limit` reflects the resolved value; 0 \
                       resolves to the default); raise `limit` for highly-coupled \
                       files, or use `offset` (default 0) to page through. An unknown \
                       file is not an error — it returns empty page(s) (results: [], \
                       total: 0). Responses are capped by `[response].max_bytes` \
                        (default 100KB). For any byte-starved empty page whose `next_offset` \
                        equals the requested `offset`, treat it as a start-fresh marker, not a \
                        continuation: do not retry unchanged; raise `[response].max_bytes`, \
                        re-run `analyze_codebase`, then retry. In `both` mode the budget is allocated \
                       SEQUENTIALLY: the incoming page is sized first against the full \
                       budget, then the outgoing page receives only what remains after \
                       the incoming page plus a fixed wrapper reserve; if incoming \
                        consumes the whole budget the outgoing page comes back empty \
                        with `truncated:true` and `next_offset` equal to the request's \
                        resolved `offset` (a start-fresh \
                        marker, not a continuation): do not retry it unchanged; raise \
                        `[response].max_bytes`, re-run `analyze_codebase`, then retry. When \
                        `truncated` is true on either side and `next_offset` differs from the \
                        requested `offset`, that side has un-emitted rows because `limit` or the \
                        byte budget cut it — re-call with that single `direction=\"incoming\"` \
                        (or `\"outgoing\"`) and `offset = next_offset` from the truncated \
                        page to resume; `truncated=false` \
                       plus `next_offset=null` means that page is complete. \
                       `results.length` may be less than `limit` when the byte cap \
                       fires, so consult `truncated`, not length, to detect partial \
                       pages. Path is resolved against the indexed graph; `\\\\?\\` \
                       extended-path prefix is handled automatically, and relative \
                       segments (`.` / `..`) resolve against the on-disk file when it \
                       exists (otherwise lexical-only). Counts cover only edges between \
                       indexed source files: targets that do not resolve to an indexed \
                       source file (system/external headers, `.ini`/`.cfg`, `.txt`) are \
                       filtered at index time and never contribute to a count."
    )]
    async fn get_coupling(
        &self,
        Parameters(args): Parameters<GetCouplingArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(r) = self.require_indexed() {
            return Ok(r);
        }
        let max_bytes = self.inner.config.read().response.max_bytes;
        // `paths::normalize_user_path` inside the handler may block on a
        // filesystem stat (see `get_file_symbols` comment).
        let inner = self.inner.clone();
        let result = tokio::task::spawn_blocking(move || {
            handlers::structure::get_coupling(
                &inner.graph,
                &args.file,
                args.direction.as_deref(),
                args.offset,
                args.limit,
                max_bytes,
            )
        })
        .await;
        Ok(match result {
            Ok(r) => r,
            Err(e) => handlers::tool_error(format!("get_coupling task panicked: {e}")),
        })
    }

    #[tool(
        description = "Detect the codebase's de-facto module structure via label \
                       propagation over the file-level call+include graph — answers 'what \
                       are the clusters here' for an unfamiliar codebase, which pairwise \
                       `get_coupling` alone cannot. Returns the flattened envelope \
                       {results, total, offset, limit, truncated, next_offset, granularity, \
                       termination, iterations, node_count, edge_count, degenerate} — the \
                       `Page<Community>` fields are flattened onto the top level (same \
                       precedent as `search_symbols`), where each `results[i]` is a \
                       `Community` {label, size, members, truncated, original_len?}: `label` \
                       is a derived directory-prefix name, `size` is the community's true \
                       total member-file count, and `members` is the (possibly capped) file \
                       list. TWO INDEPENDENT CAPS, do not conflate them: `limit`/`offset` \
                       page over the LIST of communities against the byte budget (standard \
                       envelope pagination, like any other `Page<T>` tool); \
                       `members_per_community` separately caps the member file list WITHIN \
                       each community. A community whose member list was cut by \
                       `members_per_community` carries `truncated: true` and `original_len` \
                       = its true pre-cap member count on THAT community — this mirrors \
                       `Cycle.truncated`/`Cycle.original_len` exactly, and as with `Cycle`, \
                       neither the per-community `truncated` nor the envelope's `truncated` \
                       implies the other. `results` are sorted by descending `size`. \
                       `granularity` (default \"file\", the only supported value today; any \
                       other non-empty value is a tool error) is echoed back. `termination` \
                       is `\"converged\"` (propagation reached a stable partition) or \
                       `\"iteration_ceiling\"` (cut off at `iterations` sweeps without \
                       converging — no tuning is required to get a result either way). \
                       `node_count`/`edge_count` describe the aggregated file graph itself \
                       (unaffected by `limit`/`offset` pagination). `degenerate` is `null` \
                       for an ordinary partition; when the partition is degenerate it is \
                       FLAGGED rather than silently returned as an ordinary result — \
                       `{kind: \"giant\", share_permille}` when one community holds >= 90% \
                       of nodes (`node_count >= 10`), or `{kind: \"atomized\"}` when every \
                       node is its own singleton community — either signal means the \
                       result is likely not a useful module breakdown as-is. \
                       `max_iterations` defaults to 50 (max 500, 0 resolves to the default). \
                       `members_per_community` defaults to 10 (max 100, 0 resolves to the \
                       default); raise it for fuller member lists per community, not \
                       `limit`. `limit` defaults to 100 (max 1000, clamped silently); raise \
                       `limit` for more communities per page, use `offset` to page through \
                       the rest. Responses are capped by `[response].max_bytes` (default \
                        100KB). Re-call with `offset = next_offset` to resume a truncated page, \
                        except when a byte-starved page is empty and `next_offset` equals the \
                        requested `offset`: that is a start-fresh marker, not a continuation. Do \
                        not retry unchanged; raise `[response].max_bytes`, re-run \
                        `analyze_codebase`, then retry."
    )]
    async fn detect_communities(
        &self,
        Parameters(args): Parameters<DetectCommunitiesArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(r) = self.require_indexed() {
            return Ok(r);
        }
        // Label propagation runs over the whole aggregated file graph and
        // holds a parking_lot read guard for the duration (see `find_path`
        // above) — a concurrent watch-driven reindex needing the write
        // lock waits. `spawn_blocking` protects the tokio scheduler, NOT
        // the client's `MCP_TOOL_TIMEOUT` (CLAUDE.md documents this for
        // `analyze_codebase`): that timeout is wall-clock and unaffected
        // by which thread pool runs the work. Measured cost: 177ms on an
        // 841-file corpus; no larger corpus has been measured — see
        // `.plans/Plans/GraphPlatformExpansion/notes/phase-1-query-performance.md`.
        let max_bytes = self.inner.config.read().response.max_bytes;
        let inner = self.inner.clone();
        let result = tokio::task::spawn_blocking(move || {
            handlers::structure::detect_communities(
                &inner.graph,
                args.granularity.as_deref(),
                args.max_iterations,
                args.members_per_community,
                args.limit,
                args.offset,
                max_bytes,
            )
        })
        .await;
        Ok(match result {
            Ok(r) => r,
            Err(e) => handlers::tool_error(format!("detect_communities task panicked: {e}")),
        })
    }

    #[tool(
        description = "Generate a graph diagram: call graph (`symbol=`), file dependencies \
                       (`file=`), or inheritance tree (`class=`). Provide EXACTLY ONE of \
                       `symbol`/`file`/`class` (empty strings count as absent); zero or more \
                       than one is an error. `direction` (symbol mode only) is one of \
                       `\"callees\"` (only what the symbol calls), `\"callers\"` (only what \
                       calls the symbol), or `\"both\"` (default — callees and callers in one \
                       diagram); set `direction=\"callees\"` to show only outgoing calls. \
                       `direction` is IGNORED for `file=`/`class=` modes — passing it there is \
                       silently accepted, NOT rejected; an unrecognized spelling IS rejected, \
                       but only in symbol mode. LOSSY DEDUPE: edges that render to the same \
                       (from_label, to_label) pair collapse into ONE — two distinct symbols \
                       whose display labels collide (e.g. same `parent::name` in different \
                       files) become a single diagram edge by design (visual coherence over \
                       ID-level fidelity). Clients needing ID-level fidelity should call \
                       `get_callers`/`get_callees` instead. UNRESOLVED-TARGET FILTER: calls to \
                       symbols not in the index are dropped from the diagram — they no longer \
                       appear as file-basename pseudo-nodes (an absent edge is a truer signal \
                       than a synthetic node with no symbol behind it). `format` is `\"edges\"` \
                       (default; JSON array of `{from, to, label, direction}` objects, `[]` \
                       when empty) or `\"mermaid\"` (Mermaid flowchart text). Every edge carries \
                       `direction`: `\"calls\"` (outgoing — the `from` endpoint calls `to`) or \
                       `\"called_by\"` (incoming — `from` is an inbound caller of `to`). In \
                       call-graph (`symbol=`) Mermaid output `\"calls\"` renders as a solid \
                       `-->|calls|` arrow and `\"called_by\"` as a dashed `-.->|called by|`; \
                       `file=` and `class=` modes always emit solid arrows with their own \
                       edge labels (`-->|includes|`, `-->|inherits|`). In `both` mode a \
                       single underlying call reachable from both traversal arms is emitted \
                       ONCE, tagged by whichever arm reached it first in BFS order; a \
                       genuinely bidirectional pair (A→B and B→A) survives as two edges. \
                       `depth` (default 1; 0 or omitted resolves to 1) bounds BFS \
                       traversal distance. `max_nodes` (default 30; 0 or omitted resolves to \
                       30; no upper clamp) bounds nodes visited by the BFS in every mode; \
                       raise it when a symbol/file/class neighborhood exceeds 30. `styled` (default \
                       false, mermaid format only) highlights the center node. On a `symbol=` \
                       or `class=` miss the error carries did-you-mean suggestions; a `file=` \
                       miss returns a bare not-found (no suggestion source for filenames). When \
                       `file=` is used, the path is resolved against the indexed graph; \
                       `\\\\?\\` extended-path prefix is handled automatically, and relative \
                       segments (`.` / `..`) resolve against the on-disk file when it exists \
                       (otherwise lexical-only). The `symbol=` and `class=` modes take \
                       identifiers, not paths, and are NOT normalized — pass them exactly as \
                       they appear in the index."
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
                direction: args.direction.as_deref(),
                min_confidence: args.min_confidence.as_deref(),
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
        description = "Start watching the indexed directory for file changes and auto-reindex modified files. In daemon mode this watch is shared by every attached session; if another session already started it, the existing watch remains active."
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

    #[tool(
        description = "Operational diagnostic — returns a JSON object describing the running server: \
                       binary git SHA (with `-dirty` suffix for working-tree builds), package version, \
                       release-vs-debug build flag, discovered `.code-graph.toml` path (or null), \
                       active `[cpp].macro_strip` / `[cpp].macro_strip_with_args` counts, indexed project root, \
                       graph stats (files/symbols/edges), and the timestamp of the most recent \
                       analyze_codebase + whether it was force-rebuilt. No side effects, no locks held across \
                       I/O — safe to call from any client at any time. Use this to verify which build is \
                       actually running before debugging behaviour, and to confirm config discovery picked \
                       up the toml you expected. \
                       \
                       When an analyze is in flight or recently terminated, `analyze_job` carries \
                       `{ job_id, status, path, force, started_at, finished_at, progress, progress_total, \
                       progress_message, error?, result?, current_phase }`. \
                       `current_phase` names the indexing phase the worker is currently in: \
                       `\"loading_cache\"` (rkyv archive deserialize — can take minutes on \
                       multi-GB caches; skipped when `force=true` and the invocation scope is \
                       the entire project root), `\"discovering\"` (file-walk), `\"parsing\"` \
                       (per-file tree-sitter parse + extract — the dominant phase), \
                       `\"resolving\"` (cross-file edge resolution), `\"persisting\"` (cache write), \
                       `\"completed\"` (terminal indicator stamped atomically with \
                       `status: \"completed\"` — a polling client can read either field alone \
                       to detect successful terminal). Failed jobs intentionally retain their \
                       last in-flight phase so `current_phase + error` together localize where \
                       the failure happened. It is independent of `status` and stays as a \
                       historical value on terminal jobs (the phase that was active when the worker \
                       reached terminal); for terminal liveness use `status` not `current_phase`. \
                       Emits explicit `null` until the worker enters its first phase. \
                       Poll this tool while `analyze_job.status == \
                       \"running\"` (a 250-1000ms cadence is reasonable — poll locks are constant-time); \
                       read `analyze_job.result` once `status` flips to `\"completed\"` (shape: \
                       `{ files, symbols, edges, root_path, warnings }`, byte-identical to \
                       `analyze_codebase`'s success body), or `analyze_job.error` on `\"failed\"`. If \
                       you've kicked off a new analyze before reading the previous terminal, \
                       `analyze_job_previous_terminal` carries the prior terminal job for exactly one \
                       grace-window kickoff (gone after the next rotation). Both fields are `null` \
                       before any analyze has ever run, and emit explicit `null` (not absent) so \
                       clients can distinguish \"no analyze ever\" from \"missing field on an old \
                       server\"."
    )]
    async fn get_status(
        &self,
        Parameters(_args): Parameters<EmptyParams>,
    ) -> Result<CallToolResult, McpError> {
        Ok(handlers::status::get_status(self.inner.clone()))
    }

    #[tool(
        description = "Poll one `analyze_codebase_async` request. `job_id` is required and may \
                       be either the original canonical async job ID or an absorbed/displaced \
                       follower alias. Returns the satisfying canonical `AnalyzeJobView`: \
                       `{ job_id, status, path, force, started_at, finished_at, progress, \
                       progress_total, progress_message, error, result, current_phase }`. The \
                       returned `job_id` identifies the satisfying canonical scan, so it may differ \
                       from an alias input. Poll while `status == \"running\"` (250-1000ms is a \
                       reasonable cadence); read `result` on `\"completed\"` or `error` on \
                       `\"failed\"`. Unknown IDs, and aliases whose canonical terminal has left \
                       the one-job grace window, return a user-visible tool error. `get_status` \
                       remains the shared current/previous diagnostic projection."
    )]
    async fn get_analyze_status(
        &self,
        Parameters(args): Parameters<GetAnalyzeStatusArgs>,
    ) -> Result<CallToolResult, McpError> {
        Ok(handlers::status::get_analyze_status(
            self.inner.clone(),
            args.job_id,
        ))
    }
}

#[tool_handler]
impl ServerHandler for CodeGraphServer {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    fn empty_server() -> CodeGraphServer {
        CodeGraphServer::new(LanguageRegistry::new())
    }

    #[tokio::test]
    async fn persist_coordinator_waits_for_active_save() {
        let coordinator = Arc::new(PersistCoordinator::new());
        let guard = coordinator.begin_persist().unwrap();
        let waiter = tokio::spawn({
            let coordinator = Arc::clone(&coordinator);
            async move { coordinator.close_persist_and_wait().await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiter.is_finished(), "shutdown waits for admitted save");
        drop(guard);
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("guard release wakes shutdown")
            .unwrap();
    }

    #[tokio::test]
    async fn persist_coordinator_has_no_lost_wake_race() {
        for _ in 0..32 {
            let coordinator = Arc::new(PersistCoordinator::new());
            let guard = coordinator.begin_persist().unwrap();
            let waiter = tokio::spawn({
                let coordinator = Arc::clone(&coordinator);
                async move { coordinator.close_persist_and_wait().await }
            });
            drop(guard);
            tokio::time::timeout(Duration::from_secs(1), waiter)
                .await
                .expect("close observes a concurrent final drop")
                .unwrap();
        }
    }

    #[tokio::test]
    async fn persist_coordinator_denies_saves_after_shutdown_begins() {
        let coordinator = Arc::new(PersistCoordinator::new());
        coordinator.close_persist_and_wait().await;
        assert!(coordinator.begin_persist().is_err());
    }

    #[tokio::test]
    async fn persist_coordinator_multi_close_and_analyze_admission_are_safe() {
        let coordinator = Arc::new(PersistCoordinator::new());
        let guard = coordinator.begin_analyze().unwrap();
        let first = tokio::spawn({
            let coordinator = Arc::clone(&coordinator);
            async move { coordinator.close_analyze_and_wait().await }
        });
        let second = tokio::spawn({
            let coordinator = Arc::clone(&coordinator);
            async move { coordinator.close_analyze_and_wait().await }
        });
        drop(guard);
        tokio::time::timeout(Duration::from_secs(1), async {
            first.await.unwrap();
            second.await.unwrap();
        })
        .await
        .expect("multiple close callers drain together");
        assert!(coordinator.begin_analyze().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn idle_shutdown_claims_after_an_uninterrupted_idle_interval() {
        let coordinator = Arc::new(PersistCoordinator::new());
        let waiter = tokio::spawn({
            let coordinator = Arc::clone(&coordinator);
            async move {
                coordinator
                    .wait_for_idle_shutdown(Duration::from_secs(5))
                    .await
            }
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(5)).await;
        assert!(waiter.await.unwrap());
        assert!(coordinator.begin_connection().is_err());
        assert!(coordinator.begin_analyze().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn idle_shutdown_waits_for_an_attached_connection() {
        let coordinator = Arc::new(PersistCoordinator::new());
        let connection = coordinator.begin_connection().unwrap();
        let waiter = tokio::spawn({
            let coordinator = Arc::clone(&coordinator);
            async move {
                coordinator
                    .wait_for_idle_shutdown(Duration::from_secs(5))
                    .await
            }
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(10)).await;
        assert!(
            !waiter.is_finished(),
            "attached client holds the daemon open"
        );
        drop(connection);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(5)).await;
        assert!(waiter.await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn idle_shutdown_waits_for_an_admitted_analyze() {
        let coordinator = Arc::new(PersistCoordinator::new());
        let analyze = coordinator.begin_analyze().unwrap();
        let waiter = tokio::spawn({
            let coordinator = Arc::clone(&coordinator);
            async move {
                coordinator
                    .wait_for_idle_shutdown(Duration::from_secs(5))
                    .await
            }
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(10)).await;
        assert!(
            !waiter.is_finished(),
            "active analyze holds the daemon open"
        );
        drop(analyze);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(5)).await;
        assert!(waiter.await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn idle_shutdown_restarts_the_full_interval_after_activity_returns_to_zero() {
        let coordinator = Arc::new(PersistCoordinator::new());
        let waiter = tokio::spawn({
            let coordinator = Arc::clone(&coordinator);
            async move {
                coordinator
                    .wait_for_idle_shutdown(Duration::from_secs(10))
                    .await
            }
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(9)).await;
        let connection = coordinator.begin_connection().unwrap();
        drop(connection);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(
            !waiter.is_finished(),
            "a return to zero must not resume the previous interval"
        );
        tokio::time::advance(Duration::from_secs(9)).await;
        assert!(waiter.await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn zero_idle_timeout_never_claims_shutdown() {
        let coordinator = Arc::new(PersistCoordinator::new());
        let waiter = tokio::spawn({
            let coordinator = Arc::clone(&coordinator);
            async move { coordinator.wait_for_idle_shutdown(Duration::ZERO).await }
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(86_400)).await;
        assert!(!waiter.is_finished(), "zero is the never-exit sentinel");
        waiter.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn attach_wins_the_idle_expiry_race_when_it_acquires_the_mutex_first() {
        let coordinator = Arc::new(PersistCoordinator::new());
        let waiter = tokio::spawn({
            let coordinator = Arc::clone(&coordinator);
            async move {
                coordinator
                    .wait_for_idle_shutdown(Duration::from_secs(5))
                    .await
            }
        });
        tokio::task::yield_now().await;
        // Advancing makes the sleep ready, but this task has not yielded, so
        // the admission takes the shared mutex before the expiry branch can
        // claim shutdown.
        tokio::time::advance(Duration::from_secs(5)).await;
        let connection = coordinator.begin_connection().unwrap();
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished(), "accepted attach prevents idle claim");
        drop(connection);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(5)).await;
        assert!(waiter.await.unwrap());
    }

    /// `tools/list` must surface exactly 24 tools. If a future change adds
    /// or removes a `#[tool]`, this assertion is the first place a
    /// wire-format change shows up.
    #[test]
    fn tool_router_registers_twenty_four_tools() {
        let server = empty_server();
        assert_eq!(
            server.tool_count(),
            24,
            "expected 24 registered tools, got {}",
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
            "analyze_codebase_async",
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
            "get_status",
            "get_analyze_status",
            "find_overrides",
            "find_class_candidates",
            "get_symbol_at",
            "find_path",
            "detect_communities",
            "blame_symbol",
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
    /// Snapshot tests lock the byte sequence in across all error paths.
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

    /// P0 query handlers must enforce the require_indexed gate before
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
                subtree: None,
                query: Some("foo".to_string()),
                kind: None,
                namespace: None,
                language: None,
                limit: None,
                offset: None,
                brief: None,
                count_only: None,
                near: None,
                max_distance: None,
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
                min_confidence: None,
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
                min_confidence: None,
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
                limit: None,
                offset: None,
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
    /// The `get_symbol_summary` description has been rewritten to mention
    /// the `Page<SummaryRow>` envelope shape (and its sort / count_only
    /// wording), but the em-dash is preserved verbatim. Keep this em-dash
    /// check intact through any future description edit.
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

    // -- require_indexed gates ---------------------------------------------
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
                subtree: None,
                limit: None,
                offset: None,
                brief: None,
                count_only: None,
                reliability: None,
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
                offset: None,
                limit: None,
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
                direction: None,
                min_confidence: None,
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

    // -- watch require_indexed gates ---------------------------------------
    //
    // Both watch handlers must short-circuit on require_indexed before
    // touching debouncer state. Lifecycle tests live in
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

    // -- count_only Args deserialization -----------------------------------
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
            "absent count_only must deserialize to None (handler resolves None to false via unwrap_or(false)); got {:?}",
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

    // -- max_cycle_size Args deserialization -------------------------------
    //
    // The `max_cycle_size: Option<u32>` field on DetectCyclesArgs must
    // deserialize via the same contract as the existing `Option<u32>`
    // fields (`limit`, `offset`): absent → None, present-and-number →
    // Some(value), wrong-type → deserialization error. The None default
    // is what lets the handler resolve it to 50 via the
    // `.filter(|&n| n != 0).unwrap_or(50).min(500)` idiom; the test
    // guards the pre-condition that flow depends on.

    #[test]
    fn detect_cycles_args_max_cycle_size_absent_is_none() {
        let args: DetectCyclesArgs = serde_json::from_str("{}").expect("empty object deserializes");
        assert!(
            args.max_cycle_size.is_none(),
            "absent max_cycle_size must deserialize to None (handler resolves None to the default 50); got {:?}",
            args.max_cycle_size,
        );
    }

    #[test]
    fn detect_cycles_args_max_cycle_size_number_is_some() {
        let args: DetectCyclesArgs = serde_json::from_str(r#"{"max_cycle_size": 50}"#)
            .expect("max_cycle_size=50 deserializes");
        assert_eq!(args.max_cycle_size, Some(50));
    }

    #[test]
    fn detect_cycles_args_max_cycle_size_malformed_string_rejected() {
        let result: Result<DetectCyclesArgs, _> =
            serde_json::from_str(r#"{"max_cycle_size": "fifty"}"#);
        assert!(
            result.is_err(),
            "string instead of integer must produce a deserialization error; got {result:?}",
        );
    }
}
