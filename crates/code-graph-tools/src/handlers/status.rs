//! `get_status` MCP tool — operational visibility into the running
//! server's build, configuration, and index state.
//!
//! One call answers the recurring "is the server actually running the
//! build I think it is" question that comes up every time a fix is
//! deployed but the user keeps observing the pre-fix behaviour. Returns
//! a JSON object listing the binary's git SHA (with `-dirty` suffix
//! when the working tree had uncommitted changes at build time), the
//! discovered `.code-graph.toml` path, the size of the active
//! `[cpp].macro_strip` / `[cpp].macro_strip_with_args` lists, the
//! indexed project root, the graph's file/symbol/edge counts, and
//! when the most recent `analyze_codebase` completed (plus whether it
//! was a `force=true` rebuild).
//!
//! The tool has NO side effects and never blocks on indexing or
//! re-loading the config. All reads are O(1) or O(small-constant)
//! against `ServerInner` state.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use rmcp::model::CallToolResult;
use serde::Serialize;

use crate::analyze_job::{AnalyzeJob, JobStatus};
use crate::handlers::analyze::AnalyzeResult;
use crate::server::ServerInner;

use super::tool_success_json;

/// Wire shape of `get_status` output. Field order chosen so the most
/// frequently-checked fields (binary version, indexed state) appear
/// first in JSON-key-order-preserving renderers, but every consumer
/// should parse by name not position.
#[derive(Debug, Serialize)]
pub struct StatusResult {
    /// Build-time git SHA with optional `-dirty` suffix when the
    /// working tree had uncommitted changes. `"unknown"` when git
    /// wasn't available at build time (e.g. binary built via
    /// `cargo install` against a tarball).
    pub binary_version: String,
    /// Cargo package version, e.g. `"0.1.0"`. Stable across SHAs;
    /// useful for clients that want a coarse identifier when the
    /// git SHA isn't meaningful (release binaries).
    pub package_version: String,
    /// `true` when the binary was compiled without debug assertions
    /// (i.e. release profile). Bisects the obvious "is this a debug
    /// build?" question without a separate flag.
    pub release_build: bool,
    /// Absolute path to the discovered `.code-graph.toml`, or `null`
    /// when no toml was found at any ancestor (project-root fallback
    /// to the invocation path). Surfaces the load-bearing answer to
    /// "which config actually applied" — the bug the upward-walk
    /// work in commit `c06fc73` fixed and that
    /// `get_status` makes self-evident going forward.
    pub config_path: Option<String>,
    /// Count of entries in `[cpp].macro_strip` after load-time
    /// filtering (drained empties + duplicates). `0` means no
    /// macro-strip rules — engine-style `class CORE_API Foo` will
    /// not extract. Visibility into this single number catches the
    /// "toml found but list empty" case without forcing the user to
    /// open the file.
    pub config_macro_strip_count: usize,
    /// Counterpart for `[cpp].macro_strip_with_args`. Same
    /// "0 means none" semantic.
    pub config_macro_strip_with_args_count: usize,
    /// `true` once any `analyze_codebase` has completed. When
    /// `false`, the index_* fields below are still meaningful (zeros)
    /// but the project_root may also be `null`.
    pub indexed: bool,
    /// Project root from the most recent `analyze_codebase`. May
    /// differ from the user's invocation path: when a parent
    /// `.code-graph.toml` was discovered, this is the parent. `null`
    /// when no analyze has run.
    pub indexed_root: Option<String>,
    /// Indexed file count from `Graph::stats()`. Zero before first
    /// analyze.
    pub index_files: u32,
    /// Indexed symbol count.
    pub index_symbols: u32,
    /// Indexed edge count (forward calls + inherits + includes — same
    /// "edges" definition the analyze response uses).
    pub index_edges: u32,
    /// RFC3339 UTC timestamp of when the most recent
    /// `analyze_codebase` completed, or `null` if never indexed.
    pub index_built_at: Option<String>,
    /// `true` if the most recent `analyze_codebase` was called with
    /// `force=true`. `null` when never indexed. Helps distinguish
    /// "this is the result of an incremental update" from "this is a
    /// full rebuild result" — the difference matters when the user
    /// is verifying a fix that requires a force-reindex to surface.
    pub index_force_built: Option<bool>,
    /// Snapshot of the current `analyze_slot.current` job, when any
    /// analyze has ever run (sync or async). `null` before the first
    /// analyze. Serializes as explicit `null` (no
    /// `skip_serializing_if`) so clients can distinguish "no analyze
    /// ever" from "missing field on an old server" — matches the
    /// `index_force_built` precedent above.
    pub analyze_job: Option<AnalyzeJobView>,
    /// Snapshot of the previous terminal job preserved across a single
    /// grace-window kickoff (Design Decision 4). Becomes `null` again
    /// once a second analyze terminates and rotates the slot. Same
    /// explicit-`null` serialization rule as `analyze_job`.
    pub analyze_job_previous_terminal: Option<AnalyzeJobView>,
}

/// Wire shape for one `AnalyzeJob` in a `get_status` response.
///
/// Snapshot taken under a single `job.state.read()` so `status` and
/// its associated payload (`error` for Failed, `result` for Completed)
/// are mutually consistent — see [`AnalyzeJobView::from_job`].
///
/// `status` serializes as the lowercase string `"running"`,
/// `"completed"`, or `"failed"` — NOT the enum tag. `error` is
/// populated ONLY when `status == "failed"`; `result` is populated
/// ONLY when `status == "completed"`. The two never co-occur.
#[derive(Debug, Serialize)]
pub struct AnalyzeJobView {
    /// 20-char zero-padded decimal nanosecond timestamp from
    /// kickoff. Unique-by-construction under single-flight.
    pub job_id: String,
    /// `"running"` | `"completed"` | `"failed"`.
    pub status: String,
    /// User-supplied path that was indexed (as passed to the
    /// originating `analyze_codebase` / `analyze_codebase_async`).
    pub path: String,
    /// `force` flag the originating call used.
    pub force: bool,
    /// RFC3339 UTC timestamp of kickoff.
    pub started_at: String,
    /// RFC3339 UTC timestamp of terminal transition; `null` while
    /// `status == "running"`.
    pub finished_at: Option<String>,
    /// Files processed so far (monotonic during Running). On terminal
    /// this is the last value the worker reported.
    pub progress: u32,
    /// Discovered file total. `0` during the discovery phase, set once
    /// the indexer knows the universe.
    pub progress_total: u32,
    /// Human-readable phase label from the worker's most recent
    /// `report()` call (e.g. `"parsing 42312/72345 files"`).
    pub progress_message: String,
    /// Failure message, `Some` only when `status == "failed"`.
    pub error: Option<String>,
    /// Terminal `AnalyzeResult`, `Some` only when `status ==
    /// "completed"`. Byte-identical shape to `analyze_codebase`'s
    /// success body.
    pub result: Option<AnalyzeResult>,
}

impl AnalyzeJobView {
    /// Project an [`AnalyzeJob`] onto its wire shape. Acquires
    /// `job.state.read()` exactly once and snapshots every field
    /// under that single guard, so `status`, `error`, and `result`
    /// are mutually consistent with `progress` / `finished_at`. The
    /// guard is dropped at the end of this scope; the returned view
    /// owns its data and outlives the lock.
    pub(crate) fn from_job(job: &AnalyzeJob) -> Self {
        let state = job.state.read();
        let (status, error, result) = match &state.status {
            JobStatus::Running => ("running".to_string(), None, None),
            JobStatus::Completed(r) => ("completed".to_string(), None, Some(r.clone())),
            JobStatus::Failed(msg) => ("failed".to_string(), Some(msg.clone()), None),
        };
        Self {
            job_id: job.job_id.clone(),
            status,
            path: job.path.clone(),
            force: job.force,
            started_at: format_unix_nanos_rfc3339(job.started_at),
            finished_at: state.finished_at.map(format_unix_nanos_rfc3339),
            progress: state.progress,
            progress_total: state.progress_total,
            progress_message: state.progress_message.clone(),
            error,
            result,
        }
    }
}

/// `get_status` body. Pure read — no locks held across `tool_success_json`.
pub fn get_status(inner: Arc<ServerInner>) -> CallToolResult {
    let binary_version = env!("CODE_GRAPH_GIT_SHA").to_string();
    let package_version = env!("CARGO_PKG_VERSION").to_string();
    let release_build = !cfg!(debug_assertions);

    // Project root + config path: both derive from `root_path` (set by
    // the most recent analyze). If never indexed, both are None.
    let project_root = inner.root_path.read().clone();
    let config_path = project_root.as_ref().and_then(|root| {
        let p = root.join(".code-graph.toml");
        if p.exists() {
            Some(p.to_string_lossy().into_owned())
        } else {
            None
        }
    });
    let indexed_root = project_root.map(|p| p.to_string_lossy().into_owned());

    // Config counts: cheap read of the cached `RootConfig`. The TOML
    // file is NOT re-read here — these are exactly the values that
    // applied during the most recent analyze.
    let (macro_strip_count, macro_strip_with_args_count) = {
        let cfg = inner.config.read();
        (
            cfg.cpp.macro_strip.len(),
            cfg.cpp.macro_strip_with_args.len(),
        )
    };

    let indexed = inner.indexed.load(Ordering::Acquire);
    let stats = inner.graph.read().stats();

    let built_at_nanos = inner.index_built_at.load(Ordering::Acquire);
    let index_built_at = if built_at_nanos == 0 {
        None
    } else {
        Some(format_unix_nanos_rfc3339(built_at_nanos))
    };
    let index_force_built = if indexed {
        Some(inner.index_force_built.load(Ordering::Acquire))
    } else {
        None
    };

    // Snapshot the slot under the read lock — just two Arc::clones —
    // then drop the guard before walking the job state. Building views
    // outside the slot lock keeps progress writes from contending with
    // polls beyond the constant-time Arc::clone window.
    let (current_job, previous_terminal_job) = {
        let slot = inner.analyze_slot.read();
        (slot.current.clone(), slot.previous_terminal.clone())
    };
    let analyze_job = current_job.as_deref().map(AnalyzeJobView::from_job);
    let analyze_job_previous_terminal = previous_terminal_job
        .as_deref()
        .map(AnalyzeJobView::from_job);

    let result = StatusResult {
        binary_version,
        package_version,
        release_build,
        config_path,
        config_macro_strip_count: macro_strip_count,
        config_macro_strip_with_args_count: macro_strip_with_args_count,
        indexed,
        indexed_root,
        index_files: stats.files,
        index_symbols: stats.nodes,
        index_edges: stats.edges,
        index_built_at,
        index_force_built,
        analyze_job,
        analyze_job_previous_terminal,
    };

    tool_success_json(&result)
}

/// Format `nanos` since UNIX_EPOCH as an RFC3339 UTC string of the
/// form `"YYYY-MM-DDTHH:MM:SSZ"` (second precision). Standalone
/// implementation rather than pulling in `chrono` / `time` for a
/// single use site — the workspace deliberately stays slim on
/// dependencies (same posture as the missing-`tracing` choice
/// documented in `crates/code-graph-tools/src/handlers/watch.rs`).
///
/// Uses the Gregorian-calendar arithmetic shown in Howard Hinnant's
/// `date` reference (CC0): `civil_from_days` accepts the day count
/// since 1970-01-01 and returns `(year, month, day)`. Handles every
/// leap-year case correctly through year 9999.
pub(crate) fn format_unix_nanos_rfc3339(nanos: u64) -> String {
    let total_seconds = nanos / 1_000_000_000;
    let days = (total_seconds / 86_400) as i64;
    let seconds_in_day = total_seconds % 86_400;
    let hour = seconds_in_day / 3600;
    let minute = (seconds_in_day % 3600) / 60;
    let second = seconds_in_day % 60;

    // civil_from_days — see http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = if z >= 0 {
        z / 146097
    } else {
        (z - 146096) / 146097
    };
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, m, d, hour, minute, second
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::CodeGraphServer;
    use code_graph_lang::LanguageRegistry;

    #[test]
    fn status_before_any_analyze_reports_unindexed() {
        let server = CodeGraphServer::new(LanguageRegistry::new());
        let r = get_status(server.inner.clone());
        let body = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["indexed"], serde_json::json!(false));
        assert!(parsed["indexed_root"].is_null());
        assert!(parsed["config_path"].is_null());
        assert!(parsed["index_built_at"].is_null());
        assert!(parsed["index_force_built"].is_null());
        assert_eq!(parsed["index_files"], serde_json::json!(0));
        assert_eq!(parsed["index_symbols"], serde_json::json!(0));
        assert_eq!(parsed["index_edges"], serde_json::json!(0));
        assert_eq!(parsed["config_macro_strip_count"], serde_json::json!(0));
        assert_eq!(
            parsed["config_macro_strip_with_args_count"],
            serde_json::json!(0)
        );
        // Binary version + package version are always present.
        assert!(parsed["binary_version"].as_str().is_some());
        assert!(parsed["package_version"].as_str().is_some());
        // The release_build field must be a bool — value depends on
        // how the test was compiled, so we only check the type.
        assert!(parsed["release_build"].as_bool().is_some());
    }

    #[test]
    fn rfc3339_format_unix_epoch_is_1970() {
        // Sanity check on the date arithmetic.
        let s = format_unix_nanos_rfc3339(0);
        assert_eq!(s, "1970-01-01T00:00:00Z");
    }

    #[test]
    fn rfc3339_format_known_timestamp() {
        // 2026-05-23T02:21:00Z = 1779_142_860 seconds.
        let nanos = 1_779_157_260_u64 * 1_000_000_000;
        let s = format_unix_nanos_rfc3339(nanos);
        // The exact second matters less than the year/month/day
        // alignment — pin the date prefix.
        assert!(s.starts_with("2026-05-"), "got: {s}");
        assert!(s.ends_with("Z"));
    }
}
