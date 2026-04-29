//! Watch-mode handlers — Phase 3.5 ships stubs only.
//!
//! Phase 4 will replace these with the real fsnotify-debouncer-full
//! implementation. The stub error string is locked here so the snapshot
//! suite in 3.7 picks it up exactly; flipping it later requires an
//! intentional snapshot review.

use rmcp::model::CallToolResult;

use super::tool_error;

/// Stub error string for both watch handlers. Centralized so a future
/// edit of the message updates both sites at once and so the snapshot
/// suite has a single source of truth to assert against.
pub const NOT_IMPLEMENTED_MESSAGE: &str = "watch mode not yet implemented in this build";

/// `watch_start` Phase 3.5 stub. Returns the tool-level error envelope
/// (`is_error: true`) — wire-shape consistent with every other tool
/// error and keeps the rmcp protocol-error path reserved for genuine
/// protocol failures.
pub fn watch_start_stub() -> CallToolResult {
    tool_error(NOT_IMPLEMENTED_MESSAGE)
}

/// `watch_stop` Phase 3.5 stub. Same shape as [`watch_start_stub`].
pub fn watch_stop_stub() -> CallToolResult {
    tool_error(NOT_IMPLEMENTED_MESSAGE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_text(r: &CallToolResult) -> String {
        r.content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default()
    }

    #[test]
    fn watch_start_stub_returns_not_implemented_error() {
        let r = watch_start_stub();
        assert_eq!(r.is_error, Some(true));
        assert_eq!(
            body_text(&r),
            "watch mode not yet implemented in this build"
        );
    }

    #[test]
    fn watch_stop_stub_returns_not_implemented_error() {
        let r = watch_stop_stub();
        assert_eq!(r.is_error, Some(true));
        assert_eq!(
            body_text(&r),
            "watch mode not yet implemented in this build"
        );
    }
}
