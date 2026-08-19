//! Typed core layer beneath the MCP handlers (Phase 2, Typed Core Layering).
//!
//! `handlers/*.rs` speak `CallToolResult`, an rmcp wire type carrying
//! pre-serialized JSON. This module introduces a domain-typed result shape
//! so tool logic is reachable without any rmcp type in the signature — the
//! first step toward a CLI or socket front-end that never touches rmcp.
//!
//! Nothing in this module is wired up yet: no handler has been migrated
//! (that is tasks 2.2 through 2.6). This is scaffolding only —
//! `ToolOk`/`ToolError`/`ToolResult`, the single adapter that knows about
//! rmcp, and a core-level `require_indexed`.

use crate::handlers::{tool_error, tool_success_json};
use rmcp::model::{CallToolResult, Content};
use serde::Serialize;

pub mod analyze;
pub mod fingerprint_cache;
pub mod history;
pub mod query;
pub mod status;
pub mod structure;
pub mod symbols;
pub mod watch;

/// A successful outcome. Two variants because two tools legitimately
/// return prose rather than a structured document:
///
/// - The non-callable soft-hint from `get_callers`/`get_callees` (calling
///   either on a `Struct`/`Enum`/`Trait`/`Typedef`/`Interface` returns a
///   plain-text advisory, not the `Page<CallChain>` envelope).
/// - `generate_diagram(format="mermaid")`, which renders Mermaid
///   flowchart text rather than JSON.
///
/// Modeling `Text` around only the first case (as e.g. an `Advisory`
/// variant carrying a symbol and a kind) would leave the mermaid path with
/// nowhere to go when it migrates in task 2.5. One general `Text` variant
/// covers both.
pub enum ToolOk<T> {
    Value(T),
    Text(String),
}

/// A user-visible failure — the typed form of a human-readable error
/// message, NOT an operational error type.
///
/// The workspace invariant (see CLAUDE.md, "Tool handler return type") is
/// that user-visible errors travel as `CallToolResult` with the error flag
/// set, never as `Err(McpError)`. `ToolError` preserves that invariant at
/// the typed-core boundary: it wraps a plain `String` rather than deriving
/// from `thiserror`, precisely so it cannot be mistaken for — or drift
/// toward — a transport or panic failure. Conflating the two channels is
/// how the workspace invariant erodes.
#[derive(Debug)]
pub struct ToolError(pub String);

/// The result type every typed core function returns.
pub type ToolResult<T> = Result<ToolOk<T>, ToolError>;

/// The single place in the core layer that references rmcp (FR-02).
///
/// Maps each `ToolResult<T>` arm to exactly what the existing wire-layer
/// helpers in `handlers::mod` already produce, so the two paths cannot
/// drift apart:
///
/// - `Ok(ToolOk::Value(v))`  -> `tool_success_json(&v)`
/// - `Ok(ToolOk::Text(s))`   -> `CallToolResult::success(vec![Content::text(s)])`
/// - `Err(ToolError(msg))`  -> `tool_error(msg)`
pub fn to_call_tool_result<T: Serialize>(r: ToolResult<T>) -> CallToolResult {
    match r {
        Ok(ToolOk::Value(v)) => tool_success_json(&v),
        Ok(ToolOk::Text(s)) => CallToolResult::success(vec![Content::text(s)]),
        Err(ToolError(msg)) => tool_error(msg),
    }
}

/// Core-level indexed-state guard.
///
/// This is a NEW call site, not a moved one (design Decision 8): today
/// `require_indexed` is called only from `server.rs`, before a handler
/// runs — `handlers/*.rs` contain zero occurrences of it. A mechanical
/// "move the body into `core`" migration would therefore leave every
/// gated core function with no guard at all, because there was never one
/// in the handler to move. The invariant is set equality, not a fixed
/// count: the set of core functions that call `require_indexed` at their
/// own entry must equal the set of gated `#[tool]` call sites (all query
/// and watch tools; `get_status`, `analyze_codebase`, and
/// `analyze_codebase_async` are ungated by design). Verify this by
/// diffing the two sets directly — never by counting to a remembered
/// total, since a remembered total silently goes stale the moment either
/// side gains or loses a member (this comment has already done that
/// once).
///
/// The message is copied verbatim from
/// `CodeGraphServer::require_indexed` in `server.rs` so the two guards
/// (wire-layer and core) produce byte-identical text — the double check
/// on the MCP path is deliberate and cheap (one `Ordering::Acquire`
/// atomic load, no lock), not a bug to be "fixed" by removing either
/// side.
pub fn require_indexed(indexed: bool) -> Result<(), ToolError> {
    if indexed {
        Ok(())
    } else {
        Err(ToolError(
            "no codebase indexed — call analyze_codebase first".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    // AC-01, enforced mechanically. These imports are deliberately explicit
    // rather than `use super::*`: the parent module imports rmcp types for
    // the adapter, and a glob would pull them into scope here, quietly
    // weakening the very property this module exists to prove. If this
    // compiles, a structured result is constructible and readable with no
    // wire type in scope at all. Do not replace these with a glob, and do
    // not add an rmcp import "for convenience" — either silently defeats
    // the test.
    use super::{require_indexed, ToolError, ToolOk};
    #[test]
    fn tool_ok_value_reachable_without_rmcp() {
        let ok: ToolOk<u32> = ToolOk::Value(42);
        match ok {
            ToolOk::Value(v) => assert_eq!(v, 42),
            ToolOk::Text(_) => panic!("expected Value"),
        }
    }

    #[test]
    fn tool_ok_text_reachable_without_rmcp() {
        let ok: ToolOk<u32> = ToolOk::Text("advisory".to_string());
        match ok {
            ToolOk::Text(s) => assert_eq!(s, "advisory"),
            ToolOk::Value(_) => panic!("expected Text"),
        }
    }

    #[test]
    fn tool_error_reachable_without_rmcp() {
        let err = ToolError("boom".to_string());
        assert_eq!(err.0, "boom");
    }

    #[test]
    fn require_indexed_ok_when_indexed() {
        require_indexed(true).expect("indexed must pass");
    }

    #[test]
    fn require_indexed_message_matches_server_verbatim() {
        let err = require_indexed(false).expect_err("unindexed must fail");
        assert_eq!(err.0, "no codebase indexed — call analyze_codebase first");
    }
}

#[cfg(test)]
mod adapter_tests {
    //! Adapter round-trip: `to_call_tool_result` must produce output
    //! byte-identical to today's wire-layer helpers, for each of the
    //! three `ToolResult` arms.
    use super::*;

    fn body_text(result: &CallToolResult) -> String {
        result
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.clone())
            .unwrap_or_default()
    }

    #[derive(Serialize)]
    struct Sample {
        a: u32,
        b: String,
    }

    #[test]
    fn value_matches_tool_success_json() {
        let sample = Sample {
            a: 1,
            b: "x".to_string(),
        };
        let via_core = to_call_tool_result(Ok::<_, ToolError>(ToolOk::Value(Sample {
            a: 1,
            b: "x".to_string(),
        })));
        let via_helper = tool_success_json(&sample);
        assert_eq!(via_core.is_error, via_helper.is_error);
        assert_eq!(body_text(&via_core), body_text(&via_helper));
    }

    #[test]
    fn text_matches_plain_text_success() {
        let via_core: CallToolResult =
            to_call_tool_result::<()>(Ok(ToolOk::Text("hello".to_string())));
        let via_helper = CallToolResult::success(vec![Content::text("hello")]);
        assert_eq!(via_core.is_error, via_helper.is_error);
        assert_eq!(body_text(&via_core), body_text(&via_helper));
    }

    #[test]
    fn err_matches_tool_error() {
        let via_core: CallToolResult =
            to_call_tool_result::<()>(Err(ToolError("nope".to_string())));
        let via_helper = tool_error("nope");
        assert_eq!(via_core.is_error, via_helper.is_error);
        assert_eq!(body_text(&via_core), body_text(&via_helper));
    }
}
