//! Performance measurement for the two expensive graph queries added by
//! `find_path` (`Graph::shortest_path`) and `get_file_communities`
//! (`Graph::file_communities`) — closes review finding F-02 / AC-43.
//!
//! `code-graph-bench` (`crates/code-graph-parse-test`) only measures
//! index/cache/verify; it has no coverage of either query, which is why
//! AC-43 was never runnable. This test fills that gap with a real-corpus,
//! `#[ignore]`-gated timing run.
//!
//! Follows the dogfood-baseline convention from `CLAUDE.md` / the
//! `external/` corpus tests in `crates/code-graph-lang-cpp/tests/corpus.rs`:
//! auto-skip with an `eprintln!` hint (no panic) when the corpus submodule
//! is not initialized.
//!
//! Run with:
//! ```text
//! cargo test -p code-graph-tools --release --test query_perf -- --ignored --nocapture
//! ```
//! `--release` matters — a debug-build number is meaningless for a
//! performance record.

use std::path::PathBuf;
use std::time::Instant;

use code_graph_core::symbol_id;
use code_graph_lang::LanguageRegistry;
use code_graph_lang_cpp::CppParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::server::CodeGraphServer;

/// Resolve `external/<name>` relative to this crate's manifest dir, two
/// `..` segments up to the workspace root (matches
/// `crates/code-graph-lang-cpp/tests/corpus.rs::external_repo`).
fn external_repo(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("external")
        .join(name)
}

/// `external/<name>` is a git submodule; an uninitialized submodule
/// directory exists (git worktree tracks it) but is empty. Treat both
/// "missing" and "empty" as "not present" — mirrors
/// `corpus.rs::dogfood_within_ten_percent`.
fn corpus_present(root: &std::path::Path) -> bool {
    if !root.is_dir() {
        return false;
    }
    !root
        .read_dir()
        .map(|mut it| it.next().is_none())
        .unwrap_or(true)
}

fn server_with_cpp_parser() -> CodeGraphServer {
    let mut reg = LanguageRegistry::new();
    reg.register(Box::new(CppParser::new().expect("CppParser::new")))
        .expect("register CppParser");
    CodeGraphServer::new(reg)
}

/// Indexes the given corpus root with `force=true`, returning the parsed
/// `{files, symbols, edges}` analyze response.
async fn index_corpus(server: &CodeGraphServer, root: &std::path::Path) -> serde_json::Value {
    let r = analyze_codebase(
        server.inner.clone(),
        root.to_string_lossy().into_owned(),
        true,
        None,
        None,
    )
    .await;
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "analyze_codebase must succeed, got: {r:?}",
    );
    let body = r
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.to_string())
        .unwrap_or_default();
    serde_json::from_str(&body).expect("analyze response must be valid JSON")
}

/// Times `Graph::file_communities` and `Graph::shortest_path` (worst case:
/// an unconnected pair, forcing the search to exhaust before returning
/// not-found) against a real, already-initialized `external/` C++ corpus.
///
/// Points directly at the submodule checkout under `external/` rather than
/// copying it into a `TempDir` — the corpus is large enough (abseil-cpp:
/// ~16k symbols) that copying would only add noise to the timing without
/// changing what's measured.
#[test]
#[ignore]
fn query_perf_against_real_cpp_corpus() {
    // abseil-cpp is the largest initialized C++ corpus in `external/`
    // (baseline ~16,294 symbols vs. curl/fmt in the low thousands), so it
    // gives the most meaningful stress numbers for both queries. Fall back
    // to curl, then ripgrep... no, ripgrep is Rust, not indexable by the
    // C++ plugin used here — fall back to curl if abseil-cpp is absent.
    let (corpus_name, root, fallback_note): (&str, PathBuf, &str) = {
        let abseil_root = external_repo("abseil-cpp").join("absl");
        if corpus_present(&abseil_root) {
            ("abseil-cpp", abseil_root, "")
        } else {
            let curl_root = external_repo("curl");
            if corpus_present(&curl_root) {
                (
                    "curl",
                    curl_root,
                    " (fell back from abseil-cpp: submodule not initialized)",
                )
            } else {
                eprintln!(
                    "skipping query_perf_against_real_cpp_corpus: neither \
                     external/abseil-cpp nor external/curl is present — run \
                     `git submodule update --init external/abseil-cpp` (or \
                     `make submodules`) to opt in"
                );
                return;
            }
        }
    };

    let server = server_with_cpp_parser();
    let rt = tokio::runtime::Runtime::new().expect("build tokio runtime");
    let analyze_result = rt.block_on(index_corpus(&server, &root));

    let files = analyze_result["files"].as_u64().unwrap_or(0);
    let symbols = analyze_result["symbols"].as_u64().unwrap_or(0);
    let edges = analyze_result["edges"].as_u64().unwrap_or(0);

    println!(
        "\n=== query_perf: corpus={corpus_name}{fallback_note} files={files} \
         symbols={symbols} edges={edges} ==="
    );

    let g = server.inner.graph.read();

    // --- file_communities ------------------------------------------------
    let t0 = Instant::now();
    let community_result = g.file_communities(50); // default max_iterations
    let community_elapsed = t0.elapsed();

    let termination = match community_result.termination {
        code_graph_graph::Termination::Converged { iterations } => {
            format!("converged after {iterations} iteration(s)")
        }
        code_graph_graph::Termination::IterationCeiling { iterations } => {
            format!("hit iteration ceiling at {iterations} iteration(s)")
        }
    };

    println!(
        "file_communities: node_count={} edge_count={} communities={} \
         termination={termination} elapsed_ms={:.3}",
        community_result.node_count,
        community_result.edge_count,
        community_result.communities.len(),
        community_elapsed.as_secs_f64() * 1000.0,
    );

    assert!(
        !community_result.communities.is_empty() || community_result.node_count == 0,
        "file_communities must return a partition when the aggregated file \
         graph has nodes"
    );

    // --- shortest_path (worst case: unconnected pair) ---------------------
    // `Graph::orphans(None)` returns every Function/Method with ZERO
    // incoming `Calls` edges. Picking the target from that set guarantees
    // no path can ever reach it via Calls edges (the final hop into `to`
    // would itself have to be an incoming Calls edge) — so any distinct
    // `from` is unconnected to it by construction, no probing required.
    // Because both endpoints are drawn from a Calls-reachable-adjacent
    // pool, the search still walks the full reachable component from
    // `from` before concluding not-found (the genuine worst case for this
    // query, short of an artificially small node_cap).
    let mut orphan_ids: Vec<String> = g.orphans(None).iter().map(symbol_id).collect();
    orphan_ids.sort();
    orphan_ids.dedup();

    assert!(
        orphan_ids.len() >= 2,
        "expected at least two orphan (zero-incoming-call) symbols in \
         {corpus_name} to construct a guaranteed-unconnected pair; got {}",
        orphan_ids.len()
    );

    // Among the orphan pool, pick `from` as the one with the largest
    // immediate (depth-1) fan-out, so the Dijkstra search below actually
    // walks a non-trivial reachable subgraph before concluding not-found,
    // rather than dead-ending after a single node. `to` is a distinct
    // orphan (zero incoming Calls edges by construction, see above), so
    // `from` can never reach it regardless of which `from` is chosen.
    let from = orphan_ids
        .iter()
        .max_by_key(|id| g.callees(id, 1, None).len())
        .expect("checked len >= 2")
        .clone();
    let to = orphan_ids
        .iter()
        .find(|id| **id != from)
        .expect("checked len >= 2")
        .clone();
    assert_ne!(from, to, "from/to must be distinct symbols");

    const NODE_CAP: u32 = 100_000; // matches find_path's default resolution

    let t1 = Instant::now();
    let (path, nodes_examined, cap_reached) = g.shortest_path(&from, &to, NODE_CAP, None);
    let path_elapsed = t1.elapsed();

    println!(
        "shortest_path: from={from:?} to={to:?} node_cap={NODE_CAP} \
         nodes_examined={nodes_examined} cap_reached={cap_reached} \
         found={} elapsed_ms={:.3}",
        path.is_some(),
        path_elapsed.as_secs_f64() * 1000.0,
    );

    assert!(
        path.is_none(),
        "shortest_path must report not-found for a target with zero \
         incoming Calls edges (from={from:?} to={to:?})"
    );

    drop(g);

    println!("=== query_perf: done ===\n");
}
