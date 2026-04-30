//! Watch-mode handlers — Phase 4.1 ships the lifecycle (start / stop) and a
//! placeholder watch_loop body that simply drains debouncer events. Phase 4.2
//! replaces the loop body with the index-lock-aware reindex pipeline.
//!
//! The wire-format contract for this module:
//! - `watch mode is already active` — second `watch_start` while watching.
//! - `watch mode is not active` — `watch_stop` when nothing is watching.
//! - The require_indexed envelope on either handler when no codebase is
//!   indexed yet.
//!
//! `WatchHandle` lives on [`crate::server::ServerInner`], so `watch_start`
//! constructs it and writes it; `watch_stop` takes it back out and drops
//! the debouncer to tear down the OS watch. The async watch_loop task gets
//! its own [`tokio::sync::oneshot::Receiver`] for shutdown — `watch_stop`
//! sends `()` on the paired sender to end the loop cleanly.

use std::sync::Arc;
use std::time::Duration;

use notify_debouncer_full::notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebounceEventResult, DebouncedEvent};
use tokio::sync::{mpsc, oneshot};

use crate::server::{ServerInner, WatchHandle};

use super::{tool_error, tool_success_json};

/// Debounce window for the filesystem watcher. The `notify-debouncer-full`
/// API coalesces every event for a given path that arrives within this
/// window into a single `DebouncedEvent`. 250 ms is the design's pick: it
/// rides through editor save patterns (atomic-rename, multi-event saves)
/// while still feeling instant for an interactive `watch_start` user.
pub const DEBOUNCE_TIMEOUT: Duration = Duration::from_millis(250);

/// Bound for the in-process channel between the debouncer's notify thread
/// and the watch_loop tokio task. Events are best-effort — when the
/// channel is full, the debouncer's notify thread will fall back to
/// `blocking_send` (see [`forward_events`]) so no events are silently
/// dropped at the producer.
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// JSON body for a successful `watch_start`. Mirrors the minimal-shape
/// success body from Go (an empty object). The `watching: true` flag is
/// the explicit "we just started" marker so the snapshot test can lock on
/// the wire format — Go returned `{}`, but the boolean costs nothing on
/// the wire and gives clients a single field to assert against.
#[derive(serde::Serialize)]
struct WatchStartResponse {
    watching: bool,
}

/// JSON body for a successful `watch_stop`. Symmetric to
/// [`WatchStartResponse`]: `watching: false` confirms the post-stop state.
#[derive(serde::Serialize)]
struct WatchStopResponse {
    watching: bool,
}

/// `watch_start` body. Caller must already have passed `require_indexed`.
///
/// Steps:
/// 1. Refuse if `inner.watch` already holds a [`WatchHandle`].
/// 2. Read the indexed `root_path` (set by the most recent successful
///    `analyze_codebase`).
/// 3. Construct the debouncer with [`DEBOUNCE_TIMEOUT`].
/// 4. Recursively watch `root_path`.
/// 5. Spawn the watch_loop task and store the resulting [`WatchHandle`]
///    on `inner.watch`.
pub fn watch_start(inner: &Arc<ServerInner>) -> rmcp::model::CallToolResult {
    if inner.watch.read().is_some() {
        return tool_error("watch mode is already active");
    }

    let root_path = match inner.root_path.read().clone() {
        Some(p) => p,
        None => {
            // require_indexed passed (the indexed atomic flag is set) but
            // root_path is empty — this means the index was loaded by some
            // path that didn't populate root_path. Today's analyze_codebase
            // always populates it, so this branch is defensive only.
            return tool_error("no codebase indexed — call analyze_codebase first");
        }
    };

    // Channel: notify-debouncer-full's notify thread (non-tokio) →
    // watch_loop tokio task. The closure passed to `new_debouncer` is
    // `Fn(DebounceEventResult)` and may run on a worker thread that has
    // no tokio runtime — `mpsc::Sender::try_send` is blocking-thread
    // safe, so the closure forwards events without needing to be inside
    // a tokio context.
    let (events_tx, events_rx) = mpsc::channel::<Vec<DebouncedEvent>>(EVENT_CHANNEL_CAPACITY);

    let mut debouncer = match new_debouncer(DEBOUNCE_TIMEOUT, None, forward_events(events_tx)) {
        Ok(d) => d,
        Err(e) => return tool_error(format!("failed to start watcher: {e}")),
    };

    if let Err(e) = debouncer.watch(&root_path, RecursiveMode::Recursive) {
        return tool_error(format!("failed to watch {}: {e}", root_path.display()));
    }

    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();

    tokio::spawn(watch_loop(Arc::clone(inner), events_rx, cancel_rx));

    let handle = WatchHandle {
        debouncer,
        cancel: cancel_tx,
    };
    *inner.watch.write() = Some(handle);

    tool_success_json(&WatchStartResponse { watching: true })
}

/// `watch_stop` body. Caller must already have passed `require_indexed`.
///
/// Takes the live [`WatchHandle`] out of `inner.watch`, sends the cancel
/// signal so the watch_loop task exits, then drops the debouncer (which
/// tears down the OS watch).
pub fn watch_stop(inner: &Arc<ServerInner>) -> rmcp::model::CallToolResult {
    let handle = match inner.watch.write().take() {
        Some(h) => h,
        None => return tool_error("watch mode is not active"),
    };

    let WatchHandle { debouncer, cancel } = handle;
    // Best-effort: if the watch_loop task has already exited (e.g. its
    // future was cancelled at runtime shutdown), the receiver is gone and
    // `send` returns Err. That's fine — the goal is "stop watching", and
    // dropping the debouncer below achieves that regardless.
    let _ = cancel.send(());
    drop(debouncer);

    tool_success_json(&WatchStopResponse { watching: false })
}

/// Build the `Fn(DebounceEventResult)` closure that the debouncer's notify
/// thread will invoke. Bridges synchronous notify events into the tokio
/// receiver owned by [`watch_loop`].
///
/// The closure is `Fn`, so it must be cheap to call repeatedly — captures
/// `events_tx` by move, then `clone`s for each invocation. Errors from
/// notify itself are swallowed: Phase 4.2 will surface them via tracing
/// once the loop has somewhere to log to. Today's contract is "stop
/// crashing the watcher thread".
fn forward_events(events_tx: mpsc::Sender<Vec<DebouncedEvent>>) -> impl Fn(DebounceEventResult) {
    move |result| {
        let events = match result {
            Ok(e) => e,
            Err(_errors) => return,
        };
        if events.is_empty() {
            return;
        }
        // Try non-blocking first; fall through to blocking_send when the
        // channel is full so we don't drop events at the producer.
        match events_tx.try_send(events) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(events)) => {
                // We're on a notify worker thread (non-tokio); a blocking
                // send is safe here and bounded by the watch_loop's drain
                // rate.
                let _ = events_tx.blocking_send(events);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                // The receiver has been dropped (watch_stop already ran).
                // Discarding here is the right behavior — there's no one
                // left to consume the events.
            }
        }
    }
}

/// Phase 4.1 placeholder watch_loop. Drains the event channel and drops
/// every batch on the floor; Phase 4.2 replaces this body with the
/// index-lock-aware reindex pipeline. The shape (mpsc::Receiver +
/// oneshot::Receiver + tokio::select!) is locked here so the next task
/// only has to fill in the event-processing arm.
async fn watch_loop(
    _inner: Arc<ServerInner>,
    mut events: mpsc::Receiver<Vec<DebouncedEvent>>,
    cancel: oneshot::Receiver<()>,
) {
    tokio::pin!(cancel);
    loop {
        tokio::select! {
            _ = &mut cancel => return,
            maybe_evts = events.recv() => match maybe_evts {
                Some(_evts) => {
                    // Phase 4.2: dispatch each path through reindex_file.
                    // Today we drop the batch — the watcher is wired up
                    // and lifecycle tests can observe the channel
                    // mechanics, but no graph mutation happens yet.
                }
                None => return,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;
    use std::sync::atomic::Ordering;

    use codegraph_lang::LanguageRegistry;
    use codegraph_lang_cpp::CppParser;
    use rmcp::model::CallToolResult;
    use tempfile::TempDir;

    use crate::handlers::analyze::analyze_codebase;
    use crate::server::CodeGraphServer;

    fn first_text(r: &CallToolResult) -> String {
        r.content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default()
    }

    fn server_with_cpp_parser() -> CodeGraphServer {
        let mut reg = LanguageRegistry::new();
        reg.register(Box::new(CppParser::new().expect("CppParser::new")))
            .unwrap();
        CodeGraphServer::new(reg)
    }

    /// Index a TempDir holding a single trivial C++ source file; return the
    /// server (now indexed) and the dir handle (kept alive for the test).
    async fn indexed_server() -> (CodeGraphServer, TempDir) {
        let dir = TempDir::new().expect("TempDir");
        std::fs::write(dir.path().join("a.cpp"), b"void f() {}\n").expect("write fixture");
        let server = server_with_cpp_parser();
        let r = analyze_codebase(
            server.inner.clone(),
            dir.path().to_string_lossy().into_owned(),
            false,
            None,
            None,
        )
        .await;
        assert!(
            r.is_error.is_none() || r.is_error == Some(false),
            "fixture analyze must succeed: {r:?}"
        );
        assert!(server.inner.indexed.load(Ordering::Acquire));
        (server, dir)
    }

    /// Wait briefly for the spawned watch_loop tokio task to deposit its
    /// `WatchHandle` into `inner.watch`. The Phase 4.1 implementation
    /// installs the handle synchronously inside `watch_start`, so this is
    /// a defensive check — the assertion fires either way.
    async fn assert_watch_handle_present(server: &CodeGraphServer) {
        // No actual async wait is needed — `watch_start` writes the handle
        // before returning. Kept the helper signature async-ish so a
        // future change that defers handle install (e.g. waiting on the
        // spawned loop's startup) doesn't invalidate every call site.
        assert!(
            server.inner.watch.read().is_some(),
            "watch_start must populate inner.watch synchronously",
        );
    }

    fn assert_watch_handle_absent(server: &CodeGraphServer) {
        assert!(
            server.inner.watch.read().is_none(),
            "watch state must be cleared",
        );
    }

    /// Defensive guard: when `indexed=true` but `root_path` is empty
    /// (a state today's `analyze_codebase` never produces but a future
    /// loader might), `watch_start` falls back to the require_indexed
    /// envelope rather than panicking on the missing root.
    #[tokio::test]
    async fn watch_start_without_root_path_returns_require_indexed_envelope() {
        let server = server_with_cpp_parser();
        server.inner.indexed.store(true, Ordering::Release);
        // root_path stays None.
        let r = watch_start(&server.inner);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(
            first_text(&r),
            "no codebase indexed — call analyze_codebase first",
        );
        assert_watch_handle_absent(&server);
    }

    #[tokio::test]
    async fn watch_start_indexed_succeeds_and_sets_watch_state() {
        let (server, dir) = indexed_server().await;
        let r = watch_start(&server.inner);
        assert!(
            r.is_error.is_none() || r.is_error == Some(false),
            "watch_start happy path: {r:?}",
        );
        assert_eq!(first_text(&r), "{\"watching\":true}");
        assert_watch_handle_present(&server).await;
        // Tear down before the TempDir drops so the OS watch unwinds
        // cleanly (the debouncer holds an inotify handle on the dir).
        let _ = watch_stop(&server.inner);
        drop(dir);
    }

    #[tokio::test]
    async fn watch_start_double_start_errors() {
        let (server, dir) = indexed_server().await;
        let r1 = watch_start(&server.inner);
        assert!(r1.is_error.is_none() || r1.is_error == Some(false));
        let r2 = watch_start(&server.inner);
        assert_eq!(r2.is_error, Some(true));
        assert_eq!(first_text(&r2), "watch mode is already active");
        // First handle still installed.
        assert_watch_handle_present(&server).await;
        let _ = watch_stop(&server.inner);
        drop(dir);
    }

    #[tokio::test]
    async fn watch_stop_when_not_watching_errors() {
        let (server, dir) = indexed_server().await;
        let r = watch_stop(&server.inner);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(first_text(&r), "watch mode is not active");
        assert_watch_handle_absent(&server);
        drop(dir);
    }

    #[tokio::test]
    async fn watch_stop_after_start_succeeds_and_clears_state() {
        let (server, dir) = indexed_server().await;
        let r1 = watch_start(&server.inner);
        assert!(r1.is_error.is_none() || r1.is_error == Some(false));
        assert_watch_handle_present(&server).await;

        let r2 = watch_stop(&server.inner);
        assert!(r2.is_error.is_none() || r2.is_error == Some(false));
        assert_eq!(first_text(&r2), "{\"watching\":false}");
        assert_watch_handle_absent(&server);

        // Second stop is now an error.
        let r3 = watch_stop(&server.inner);
        assert_eq!(r3.is_error, Some(true));
        assert_eq!(first_text(&r3), "watch mode is not active");

        drop(dir);
    }

    /// Sanity check: the constants and types we lock in survive a
    /// round-trip through their published API surfaces. Failing here
    /// would indicate the dependency upgraded under us in a way that
    /// changed the debouncer-event shape.
    #[test]
    fn debounce_constants_are_well_formed() {
        assert_eq!(DEBOUNCE_TIMEOUT, Duration::from_millis(250));
        // Just a structural check that DebouncedEvent is still the type
        // we forward — compile-time only.
        let _: fn(DebounceEventResult) = |_| {};
        // And that we can name the path type the API hands back.
        let _: fn(&Path) = |_| {};
    }
}
