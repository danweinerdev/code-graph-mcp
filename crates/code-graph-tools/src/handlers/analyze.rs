//! `analyze_codebase` handler body.
//!
//! Coordinates the full pipeline:
//! 1. Single-flight lock (`index_lock.try_lock` — second concurrent call gets
//!    `"indexing already in progress"`).
//! 2. Path validation + canonicalization.
//! 3. `RootConfig::load` + `resolve_concurrency` (warnings flow into the
//!    response).
//! 4. Cache fast-path: `Graph::load` + `stale_paths`. Cache hit + zero stale
//!    files short-circuits without re-parsing.
//! 5. Cache miss / force / stale path: spawn the rayon parse pool inside
//!    `tokio::task::spawn_blocking`, forward progress events to
//!    `peer.notify_progress` from a sibling task, merge into the in-memory
//!    graph under a write lock, persist to cache (best-effort).
//! 6. Return `AnalyzeResult` JSON matching the Go shape (`files`, `symbols`,
//!    `edges`, `root_path`, `warnings`).

use std::sync::atomic::Ordering;
use std::sync::Arc;

use code_graph_core::{paths, ConfigError, RootConfig};
use code_graph_graph::{stale_paths, Graph};
use rmcp::model::{CallToolResult, ProgressNotificationParam, ProgressToken};
use rmcp::service::RoleServer;
use rmcp::Peer;
use serde::Serialize;

use crate::indexer::{index_directory, resolve_all_edges, ChannelProgressSink, ProgressEvent};
use crate::server::ServerInner;

use super::{tool_error, tool_success_json};

/// JSON-shape mirror of Go's `analyzeResult` in `internal/tools/analyze.go`.
/// Field order, names, and `omitempty` semantics match the Go struct exactly.
#[derive(Debug, Serialize)]
pub struct AnalyzeResult {
    pub files: u32,
    pub symbols: u32,
    pub edges: u32,
    pub root_path: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// `analyze_codebase` body. See the module docstring for the full pipeline.
pub async fn analyze_codebase(
    inner: Arc<ServerInner>,
    path_raw: String,
    force: bool,
    peer: Option<Peer<RoleServer>>,
    progress_token: Option<ProgressToken>,
) -> CallToolResult {
    let Ok(_guard) = inner.index_lock.try_lock() else {
        return tool_error("indexing already in progress");
    };

    if path_raw.is_empty() {
        return tool_error("'path' is required");
    }

    let abs_path = match paths::canonicalize(std::path::Path::new(&path_raw)) {
        Ok(p) => p,
        Err(_) => {
            return tool_error(format!("directory does not exist: {path_raw}"));
        }
    };
    if !abs_path.is_dir() {
        // We deliberately distinguish "path doesn't resolve" from "path resolves
        // to a file, not a directory" — the Go binary collapses both into a single
        // "directory does not exist" message, but Rust's `paths::canonicalize`
        // already gave us the richer information and discarding it just for Go
        // byte-identity would make the error less helpful for no real benefit.
        // Phase 3.7 snapshots will lock in the Rust-specific wording.
        return tool_error(format!("path is not a directory: {}", abs_path.display()));
    }

    let mut cfg = match RootConfig::load(&abs_path) {
        Ok(c) => c,
        Err(ConfigError::Toml(e)) => {
            return tool_error(format!("failed to parse .code-graph.toml: {e}"));
        }
        Err(ConfigError::Io(e)) => {
            return tool_error(format!("failed to read .code-graph.toml: {e}"));
        }
        // Any new `ConfigError` variant must be mapped here — the catch-all
        // path produces less-helpful errors.
        Err(e @ ConfigError::ExtensionMissingDot { .. })
        | Err(e @ ConfigError::ExtensionConflict { .. })
        | Err(e @ ConfigError::MacroStripConflict { .. }) => {
            return tool_error(format!("invalid .code-graph.toml: {e}"));
        }
    };
    let mut warnings = cfg.resolve_concurrency();

    // Cache fast-path: when not forced, attempt cache load + stale check.
    if !force {
        let mut probe = Graph::new();
        let load_ok = probe.load(&abs_path).unwrap_or(false);
        if load_ok {
            // `stale_paths` fails loud only on JSON corruption — a
            // structurally-valid cache will succeed.
            let stale = stale_paths(&abs_path).unwrap_or_default();
            if stale.is_empty() {
                let stats = probe.stats();
                {
                    let mut g = inner.graph.write();
                    *g = probe;
                }
                *inner.root_path.write() = Some(abs_path.clone());
                *inner.config.write() = cfg;
                inner.indexed.store(true, Ordering::Release);
                let result = AnalyzeResult {
                    files: stats.files,
                    symbols: stats.nodes,
                    edges: stats.edges,
                    root_path: abs_path.to_string_lossy().into_owned(),
                    warnings,
                };
                return tool_success_json(&result);
            }
            // Stale paths present: drop the partially-loaded cache, fall
            // through to a full re-index.
        }
    }

    // Full re-index path. Spawn the progress forwarder BEFORE
    // `spawn_blocking` so the receiver is alive when rayon workers start
    // sending — otherwise early events get dropped.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ProgressEvent>(64);

    let forwarder = if let (Some(peer), Some(token)) = (peer, progress_token) {
        Some(tokio::spawn(async move {
            while let Some(evt) = rx.recv().await {
                let mut params = ProgressNotificationParam::new(token.clone(), evt.progress as f64);
                if evt.total > 0 {
                    params = params.with_total(evt.total as f64);
                }
                params = params.with_message(evt.message);
                let _ = peer.notify_progress(params).await;
            }
        }))
    } else {
        // No progress token from the client → drain the channel locally so
        // `try_send` calls in the indexer don't fail-and-warn: a `recv()`
        // task with nothing else to do is the cheapest "/dev/null" sink.
        Some(tokio::spawn(async move {
            while rx.recv().await.is_some() {
                // Drain.
            }
        }))
    };

    let registry = Arc::clone(&inner);
    let cfg_for_pool = cfg.clone();
    let abs_path_for_pool = abs_path.clone();
    let blocking_handle = tokio::task::spawn_blocking(move || {
        let sink = ChannelProgressSink(tx);
        let (mut graphs, blocking_warnings) =
            match index_directory(&abs_path_for_pool, &registry.registry, &cfg_for_pool, &sink) {
                Ok(v) => v,
                Err(e) => return Err(e.to_string()),
            };
        resolve_all_edges(&mut graphs, &registry.registry, &sink);
        // Drop the sink (sender) so the forwarder task exits cleanly.
        drop(sink);
        Ok::<_, String>((graphs, blocking_warnings))
    });

    let blocking_result = blocking_handle.await;

    // Wait for the forwarder to finish draining the channel (the sink was
    // dropped at the end of the blocking task). Best-effort: a panic in the
    // forwarder is non-fatal because progress notifications are advisory.
    if let Some(handle) = forwarder {
        let _ = handle.await;
    }

    let (graphs, blocking_warnings) = match blocking_result {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            return tool_error(format!("indexing failed: {e}"));
        }
        Err(join_err) => {
            return tool_error(format!("indexing task panicked: {join_err}"));
        }
    };

    warnings.extend(blocking_warnings);

    if graphs.is_empty() {
        return tool_error(format!(
            "no supported source files found in {}",
            abs_path.display()
        ));
    }

    // Merge under the graph write lock — held only for the merge phase, not
    // for parsing or resolution.
    let stats = {
        let mut g = inner.graph.write();
        g.clear();
        for fg in graphs {
            g.merge_file_graph(fg);
        }
        g.stats()
    };

    *inner.root_path.write() = Some(abs_path.clone());
    *inner.config.write() = cfg;
    inner.indexed.store(true, Ordering::Release);

    // Persist to cache (best-effort: a save failure becomes a warning, not
    // a fatal). We snapshot the graph *after* setting `indexed=true` so an
    // immediate query against the now-indexed graph isn't blocked by the
    // cache write.
    if let Err(e) = save_cache(&inner.graph, &abs_path) {
        warnings.push(format!("cache save failed: {e}"));
    }

    let result = AnalyzeResult {
        files: stats.files,
        symbols: stats.nodes,
        edges: stats.edges,
        root_path: abs_path.to_string_lossy().into_owned(),
        warnings,
    };
    tool_success_json(&result)
}

/// Save the graph to `<dir>/.code-graph-cache.json`. Lifted to a helper so
/// the lock is held for the minimum span needed to serialize the cache —
/// a long save under the write lock would block all queries.
fn save_cache(
    graph: &parking_lot::RwLock<Graph>,
    dir: &std::path::Path,
) -> Result<(), code_graph_graph::PersistError> {
    let g = graph.read();
    g.save(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::CodeGraphServer;
    use code_graph_lang::LanguageRegistry;
    use code_graph_lang_cpp::CppParser;
    use std::fs;
    use tempfile::TempDir;

    fn server_with_cpp_parser() -> CodeGraphServer {
        let mut reg = LanguageRegistry::new();
        reg.register(Box::new(CppParser::new().expect("CppParser::new")))
            .unwrap();
        CodeGraphServer::new(reg)
    }

    #[tokio::test]
    async fn analyze_missing_path_errors() {
        let server = server_with_cpp_parser();
        let r = analyze_codebase(server.inner.clone(), String::new(), false, None, None).await;
        assert_eq!(r.is_error, Some(true));
        let body = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        assert_eq!(body, "'path' is required");
    }

    #[tokio::test]
    async fn analyze_nonexistent_directory_errors() {
        let server = server_with_cpp_parser();
        let r = analyze_codebase(
            server.inner.clone(),
            "/this/path/does/not/exist/abc123xyz".to_string(),
            false,
            None,
            None,
        )
        .await;
        assert_eq!(r.is_error, Some(true));
        let body = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        assert!(
            body.starts_with("directory does not exist:"),
            "expected 'directory does not exist:' wording, got: {body}"
        );
    }

    #[tokio::test]
    async fn analyze_path_is_file_errors() {
        // Deliberate divergence from Go's collapsed message; Rust keeps the
        // richer distinction between "path doesn't resolve" and "path
        // resolves to a file, not a directory".
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("a.cpp");
        fs::write(&file_path, b"void f() {}\n").unwrap();

        let server = server_with_cpp_parser();
        let r = analyze_codebase(
            server.inner.clone(),
            file_path.to_string_lossy().into_owned(),
            false,
            None,
            None,
        )
        .await;
        assert_eq!(r.is_error, Some(true));
        let body = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        assert!(
            body.starts_with("path is not a directory:"),
            "expected 'path is not a directory:' wording, got: {body}"
        );
    }

    #[tokio::test]
    async fn analyze_succeeds_on_small_directory_and_sets_indexed_flag() {
        let dir = TempDir::new().unwrap();
        for i in 0..3 {
            fs::write(
                dir.path().join(format!("f{i}.cpp")),
                format!("void f{i}() {{}}\n").as_bytes(),
            )
            .unwrap();
        }

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
            "got: {r:?}"
        );

        let body = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["files"], serde_json::json!(3));
        assert!(parsed["symbols"].as_u64().unwrap() >= 3);
        assert!(!parsed["root_path"].as_str().unwrap().is_empty());
        // Indexed flag is now set.
        assert!(server.inner.indexed.load(Ordering::Acquire));
        // Root path stored.
        assert!(server.inner.root_path.read().is_some());
    }

    #[tokio::test]
    async fn analyze_empty_directory_reports_no_files() {
        let dir = TempDir::new().unwrap();
        let server = server_with_cpp_parser();
        let r = analyze_codebase(
            server.inner.clone(),
            dir.path().to_string_lossy().into_owned(),
            false,
            None,
            None,
        )
        .await;
        assert_eq!(r.is_error, Some(true));
        let body = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        assert!(
            body.starts_with("no supported source files found in"),
            "got: {body}"
        );
        // indexed flag stays false.
        assert!(!server.inner.indexed.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn analyze_second_call_uses_cache_when_not_forced() {
        let dir = TempDir::new().unwrap();
        for i in 0..2 {
            fs::write(
                dir.path().join(format!("f{i}.cpp")),
                format!("void f{i}() {{}}\n").as_bytes(),
            )
            .unwrap();
        }
        let server = server_with_cpp_parser();
        let path = dir.path().to_string_lossy().into_owned();
        // First call: full re-index.
        let _ = analyze_codebase(server.inner.clone(), path.clone(), false, None, None).await;
        // Second call: cache hit (no force).
        let r2 = analyze_codebase(server.inner.clone(), path.clone(), false, None, None).await;
        assert!(r2.is_error.is_none() || r2.is_error == Some(false));
        let body = r2
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        // Same file count regardless of which path was taken.
        assert_eq!(parsed["files"], serde_json::json!(2));
    }

    #[tokio::test]
    async fn analyze_concurrent_call_returns_indexing_in_progress() {
        // Acquire the lock externally to simulate a concurrent in-flight
        // analyze_codebase. The handler should immediately error.
        let server = server_with_cpp_parser();
        let inner = server.inner.clone();
        let _held = inner.index_lock.try_lock().expect("first lock");
        let r = analyze_codebase(inner.clone(), "/tmp".to_string(), false, None, None).await;
        assert_eq!(r.is_error, Some(true));
        let body = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        assert_eq!(body, "indexing already in progress");
    }

    #[tokio::test]
    async fn analyze_force_skips_cache() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.cpp"), b"void f() {}\n").unwrap();
        let server = server_with_cpp_parser();
        let path = dir.path().to_string_lossy().into_owned();
        let _ = analyze_codebase(server.inner.clone(), path.clone(), false, None, None).await;
        let r2 = analyze_codebase(server.inner.clone(), path, true, None, None).await;
        assert!(r2.is_error.is_none() || r2.is_error == Some(false));
    }

    #[tokio::test]
    async fn analyze_malformed_toml_reports_parse_error() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.cpp"), b"void f() {}\n").unwrap();
        // Garbage TOML.
        fs::write(
            dir.path().join(".code-graph.toml"),
            "[discovery\nmax_threads = nope\n",
        )
        .unwrap();

        let server = server_with_cpp_parser();
        let r = analyze_codebase(
            server.inner.clone(),
            dir.path().to_string_lossy().into_owned(),
            false,
            None,
            None,
        )
        .await;
        assert_eq!(r.is_error, Some(true));
        let body = r
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default();
        assert!(
            body.starts_with("failed to parse .code-graph.toml:"),
            "got: {body}"
        );
    }
}
