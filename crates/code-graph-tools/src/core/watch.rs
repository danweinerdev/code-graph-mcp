//! Typed core for `watch_start` / `watch_stop`.
//!
//! Both are GATED tools: `server.rs` already calls the wire-layer
//! `require_indexed` before dispatching to `handlers::watch::watch_start`
//! / `watch_stop`, but per Design Decision 8 that check is never moved —
//! it is a NEW call site at the top of each core function here, so a
//! caller reaching these functions directly (bypassing `server.rs`) still
//! gets the domain error rather than acting on an unindexed graph.

use std::sync::Arc;

use notify_debouncer_full::new_debouncer;
use notify_debouncer_full::notify::RecursiveMode;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

use crate::core::{require_indexed, ToolOk, ToolResult};
use crate::handlers::watch::{
    forward_events, watch_loop, WatchResponse, DEBOUNCE_TIMEOUT, EVENT_CHANNEL_CAPACITY,
};
use crate::server::{ServerInner, WatchHandle};

/// `watch_start` body. Body moved verbatim from
/// `handlers::watch::watch_start`, plus the core `require_indexed` call
/// at entry (Decision 8 — this call site is new, not moved).
pub(crate) fn watch_start(inner: &Arc<ServerInner>) -> ToolResult<WatchResponse> {
    require_indexed(inner.indexed.load(std::sync::atomic::Ordering::Acquire))?;

    let mut watch_guard = inner.watch.write();
    if watch_guard.is_some() {
        return Err(crate::core::ToolError(
            "watch mode is already active".to_string(),
        ));
    }

    let root_path = match inner.root_path.read().clone() {
        Some(p) => p,
        None => {
            // require_indexed passed (the indexed atomic flag is set) but
            // root_path is empty — this means the index was loaded by some
            // path that didn't populate root_path. Today's analyze_codebase
            // always populates it, so this branch is defensive only.
            return Err(crate::core::ToolError(
                "no codebase indexed — call analyze_codebase first".to_string(),
            ));
        }
    };

    // Channel: notify-debouncer-full's notify thread (non-tokio) →
    // watch_loop tokio task. The closure passed to `new_debouncer` is
    // `Fn(DebounceEventResult)` and may run on a worker thread that has
    // no tokio runtime — `mpsc::Sender::try_send` is blocking-thread
    // safe, so the closure forwards events without needing to be inside
    // a tokio context.
    let (events_tx, events_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);

    let mut debouncer = match new_debouncer(DEBOUNCE_TIMEOUT, None, forward_events(events_tx)) {
        Ok(d) => d,
        Err(e) => {
            return Err(crate::core::ToolError(format!(
                "failed to start watcher: {e}"
            )))
        }
    };

    if let Err(e) = debouncer.watch(&root_path, RecursiveMode::Recursive) {
        return Err(crate::core::ToolError(format!(
            "failed to watch {}: {e}",
            root_path.display()
        )));
    }

    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();

    tokio::spawn(watch_loop(Arc::clone(inner), events_rx, cancel_rx));

    *watch_guard = Some(WatchHandle {
        debouncer,
        cancel: cancel_tx,
    });

    Ok(ToolOk::Value(WatchResponse { watching: true }))
}

/// `watch_stop` body. Body moved verbatim from
/// `handlers::watch::watch_stop`, plus the core `require_indexed` call
/// at entry (Decision 8).
pub(crate) fn watch_stop(inner: &Arc<ServerInner>) -> ToolResult<WatchResponse> {
    require_indexed(inner.indexed.load(std::sync::atomic::Ordering::Acquire))?;

    let handle = match inner.watch.write().take() {
        Some(h) => h,
        None => {
            return Err(crate::core::ToolError(
                "watch mode is not active".to_string(),
            ))
        }
    };

    let WatchHandle { debouncer, cancel } = handle;
    // Best-effort: if the watch_loop task has already exited (e.g. its
    // future was cancelled at runtime shutdown), the receiver is gone and
    // `send` returns Err. That's fine — the goal is "stop watching", and
    // dropping the debouncer below achieves that regardless.
    let _ = cancel.send(());
    drop(debouncer);

    Ok(ToolOk::Value(WatchResponse { watching: false }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::analyze::analyze_codebase;
    use crate::server::CodeGraphServer;
    use code_graph_lang::LanguageRegistry;
    use code_graph_lang_cpp::CppParser;
    use std::sync::atomic::Ordering;
    use tempfile::TempDir;

    fn server_with_cpp_parser() -> CodeGraphServer {
        let mut reg = LanguageRegistry::new();
        reg.register(Box::new(CppParser::new().expect("CppParser::new")))
            .unwrap();
        CodeGraphServer::new(reg)
    }

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
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        assert!(server.inner.indexed.load(Ordering::Acquire));
        (server, dir)
    }

    /// AC-28: an unindexed `watch_start` returns `Err(ToolError)`,
    /// discriminable without ever going through serialization.
    #[test]
    fn watch_start_unindexed_returns_typed_error() {
        let server = server_with_cpp_parser();
        let err = match watch_start(&server.inner) {
            Err(e) => e,
            Ok(_) => panic!("unindexed watch_start must error"),
        };
        assert_eq!(err.0, "no codebase indexed — call analyze_codebase first");
    }

    /// AC-28 counterpart for `watch_stop`.
    #[test]
    fn watch_stop_unindexed_returns_typed_error() {
        let server = server_with_cpp_parser();
        let err = match watch_stop(&server.inner) {
            Err(e) => e,
            Ok(_) => panic!("unindexed watch_stop must error"),
        };
        assert_eq!(err.0, "no codebase indexed — call analyze_codebase first");
    }

    #[tokio::test]
    async fn watch_start_indexed_returns_typed_value() {
        let (server, dir) = indexed_server().await;
        let ok = watch_start(&server.inner).expect("indexed watch_start must succeed");
        match ok {
            ToolOk::Value(resp) => assert!(resp.watching),
            ToolOk::Text(_) => panic!("expected Value(WatchResponse)"),
        }
        let _ = watch_stop(&server.inner);
        drop(dir);
    }

    #[tokio::test]
    async fn watch_start_double_start_returns_typed_error() {
        let (server, dir) = indexed_server().await;
        let first = watch_start(&server.inner);
        assert!(first.is_ok(), "first watch_start must succeed");
        let err = match watch_start(&server.inner) {
            Err(e) => e,
            Ok(_) => panic!("second watch_start must error"),
        };
        assert_eq!(err.0, "watch mode is already active");
        let _ = watch_stop(&server.inner);
        drop(dir);
    }

    #[tokio::test]
    async fn watch_stop_when_not_watching_returns_typed_error() {
        let (server, dir) = indexed_server().await;
        let err = match watch_stop(&server.inner) {
            Err(e) => e,
            Ok(_) => panic!("watch_stop with no active watch must error"),
        };
        assert_eq!(err.0, "watch mode is not active");
        drop(dir);
    }

    #[tokio::test]
    async fn watch_stop_after_start_returns_typed_value() {
        let (server, dir) = indexed_server().await;
        let _ = watch_start(&server.inner).expect("watch_start must succeed");
        let ok = watch_stop(&server.inner).expect("watch_stop must succeed");
        match ok {
            ToolOk::Value(resp) => assert!(!resp.watching),
            ToolOk::Text(_) => panic!("expected Value(WatchResponse)"),
        }
        drop(dir);
    }
}
