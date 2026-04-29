//! Concurrent reader/writer integration test for `codegraph-graph` (Phase 2.6).
//!
//! The Phase 2 design keeps `Graph` as a plain (non-locked) struct and applies
//! the lock at the `ServerInner` level in Phase 3. This test exercises the
//! locking pattern *now* — wrapping `Graph` in a [`parking_lot::RwLock`] and
//! pounding on it with several reader threads and a couple of writer threads
//! — so that any structural assumption that breaks under contention surfaces
//! before Phase 3 builds on top of it.
//!
//! What we verify:
//! - Multiple readers can hold the read lock concurrently and call the public
//!   query surface (`search`, `callers`, `symbol_summary`, `file_symbols`,
//!   `coupling`, `class_hierarchy`) without panicking.
//! - Writers calling [`Graph::merge_file_graph`] under the write lock cannot
//!   leave readers observing a partially-merged state — every observation is
//!   internally consistent with itself.
//! - The whole exercise completes well under the default `cargo test` timeout
//!   (1.5s wall-clock target; `cargo test` will hang the suite if a deadlock
//!   is introduced, which is an acceptable failure mode for this gate).
//!
//! What we explicitly do **not** do:
//! - No `loom` / no `miri` / no race-detection deps. Rust's borrow checker plus
//!   `parking_lot`'s well-tested `RwLock` semantics are sufficient for the
//!   correctness this phase needs to establish.
//! - No assertion on absolute counts at the end — writers may overwrite each
//!   other's files (both pick from the same pool of paths), and readers may
//!   observe any valid intermediate state, so a "final node count == X"
//!   assertion would either be racy or trivial. The completion of all threads
//!   without panic *is* the success signal.
//!
//! Test helpers (`sym`, edge constructors, `make_fg`) are duplicated from the
//! crate-internal `test_fixtures` module rather than imported. The shared
//! fixtures are `pub(crate)` and gated on `#[cfg(test)]`, which makes them
//! invisible to integration tests in `tests/` (those compile against the
//! public crate boundary). Duplicating ~30 lines is cheaper than introducing
//! a public-but-test-only helper module.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use codegraph_core::{Edge, EdgeKind, FileGraph, Language, Symbol, SymbolKind};
use codegraph_graph::{Graph, RwLock, SearchParams};

/// Build a [`Symbol`] with sensible defaults — local copy of the
/// `test_fixtures::sym` helper since `tests/` cannot reach `pub(crate)` items.
fn sym(name: &str, kind: SymbolKind, file: &str) -> Symbol {
    Symbol {
        name: name.to_string(),
        kind,
        file: file.to_string(),
        line: 1,
        column: 0,
        end_line: 1,
        signature: String::new(),
        namespace: String::new(),
        parent: String::new(),
        language: Language::Cpp,
    }
}

/// `Calls` edge from `from` to `to` attributed to `file`.
fn call_edge(from: &str, to: &str, file: &str) -> Edge {
    Edge {
        from: from.to_string(),
        to: to.to_string(),
        kind: EdgeKind::Calls,
        file: file.to_string(),
        line: 1,
    }
}

/// `Inherits` edge — used so `class_hierarchy` queries have something to walk.
fn inherit_edge(from: &str, to: &str, file: &str) -> Edge {
    Edge {
        from: from.to_string(),
        to: to.to_string(),
        kind: EdgeKind::Inherits,
        file: file.to_string(),
        line: 0,
    }
}

/// `Includes` edge so `coupling` and `file_dependencies` exercise the
/// includes map under contention.
fn include_edge(from: &str, to: &str) -> Edge {
    Edge {
        from: from.to_string(),
        to: to.to_string(),
        kind: EdgeKind::Includes,
        file: from.to_string(),
        line: 0,
    }
}

/// Construct a [`FileGraph`] keyed at `path` with a small but non-trivial
/// symbol/edge mix so query handlers have meaningful work to do per
/// iteration. The shape (4 symbols, 3 edges of mixed kind) is deliberately
/// minimal — the test is about lock contention, not graph cardinality.
fn make_fg(path: &str, iteration: u32) -> FileGraph {
    let caller = format!("caller_{}", iteration % 16);
    let callee = format!("callee_{}", iteration % 16);
    let base = format!("Base_{}", iteration % 8);
    let derived = format!("Derived_{}", iteration % 8);

    let symbols = vec![
        sym(&caller, SymbolKind::Function, path),
        sym(&callee, SymbolKind::Function, path),
        sym(&base, SymbolKind::Class, path),
        sym(&derived, SymbolKind::Class, path),
    ];

    // Edge sources/targets use the same `file:Name` shape `symbol_id` produces
    // for free symbols, so calls land in adj/radj keyed by valid symbol IDs.
    let caller_id = format!("{path}:{caller}");
    let callee_id = format!("{path}:{callee}");
    let edges = vec![
        call_edge(&caller_id, &callee_id, path),
        inherit_edge(&derived, &base, path),
        include_edge(path, "/header.h"),
    ];

    FileGraph {
        path: path.to_string(),
        language: Language::Cpp,
        symbols,
        edges,
    }
}

/// Seed the graph with one stable file so reader threads have something to
/// query even before any writer thread has had a chance to merge. Without
/// this, the first few reader iterations would race against an empty graph
/// and trivially short-circuit, weakening the contention signal.
fn seed_graph(graph: &RwLock<Graph>) {
    let mut writer = graph.write();
    writer.merge_file_graph(make_fg("/seed.cpp", 0));
}

#[test]
fn rwlock_concurrent_readers_and_writers() {
    // Per the Phase 2.6 verification criterion: run for ≥ 1s. We pick 1.5s as
    // a balance between fishing out timing-dependent issues and keeping CI
    // fast. `Instant::now() < deadline` bounds each thread independently.
    let test_duration = Duration::from_millis(1_500);
    let graph = RwLock::new(Graph::new());
    seed_graph(&graph);

    // Tracks whether any thread observed an obviously broken result (e.g. a
    // search returning more symbols than `total`). Threads set this to `true`
    // on failure rather than panicking so the assertion message points at the
    // test, not the worker thread's stack frame.
    let consistency_violation = AtomicBool::new(false);

    let started = Instant::now();
    thread::scope(|s| {
        // 10 readers — each picks a different mix of queries so we exercise
        // the read-side surface broadly under contention.
        for thread_idx in 0..10u32 {
            let graph = &graph;
            let consistency_violation = &consistency_violation;
            s.spawn(move || {
                let deadline = Instant::now() + test_duration;
                let mut iteration: u32 = 0;
                while Instant::now() < deadline {
                    let snapshot = graph.read();

                    // search: keep the read lock busy traversing every node.
                    // (`symbols.len() <= total` is a structural invariant of
                    // SearchResult — slicing a Vec of length `total` cannot
                    // produce a longer slice — so it cannot detect a half-merged
                    // snapshot. The seed-symbol lookup below is the actual
                    // consistency probe.)
                    let _ = snapshot.search(SearchParams {
                        pattern: format!("caller_{}", iteration % 16),
                        limit: 10,
                        ..Default::default()
                    });

                    // The seed file's `callee_0` symbol must always be locatable
                    // by ID — it is never removed by any writer. If a half-merge
                    // were observable through the read lock, this lookup could
                    // miss it transiently.
                    if snapshot.symbol_detail("/seed.cpp:callee_0").is_none() {
                        consistency_violation.store(true, Ordering::Relaxed);
                    }

                    // callers: every returned hop's depth must respect the
                    // requested bound.
                    let depth_limit = 3;
                    let chains = snapshot.callers("/seed.cpp:callee_0", depth_limit);
                    for chain in &chains {
                        if chain.depth > depth_limit {
                            consistency_violation.store(true, Ordering::Relaxed);
                        }
                    }

                    // symbol_summary: empty graph case is fine, non-empty is
                    // also fine — we just want to keep the read lock busy
                    // while traversing every node.
                    let _ = snapshot.symbol_summary(None);

                    // file_symbols / coupling / class_hierarchy round out the
                    // read surface; rotate which one runs by thread_idx so we
                    // don't always slam the same code path first.
                    match thread_idx % 3 {
                        0 => {
                            let _ = snapshot.file_symbols(Path::new("/seed.cpp"));
                        }
                        1 => {
                            let _ = snapshot.coupling(Path::new("/seed.cpp"));
                        }
                        _ => {
                            let _ = snapshot.class_hierarchy("Base_0", 2);
                        }
                    }

                    drop(snapshot);
                    iteration = iteration.wrapping_add(1);
                    // Yield so writers actually get a chance to acquire the
                    // write lock — without this, on a busy core readers can
                    // starve the writer for the whole duration.
                    thread::yield_now();
                }
            });
        }

        // 2 writers — each cycles through writer-specific paths (w0 vs w1
        // prefix) so they don't overwrite each other's files (keeping the
        // final graph contents predictable for the seed-file assertion) and
        // re-merge their own files on roughly every 4th iteration, exercising
        // the remove_file_unsafe path inside merge_file_graph. The RwLock
        // itself prevents lock-level deadlock regardless of which paths are
        // chosen.
        for writer_idx in 0..2u32 {
            let graph = &graph;
            s.spawn(move || {
                let deadline = Instant::now() + test_duration;
                let mut iteration: u32 = 0;
                while Instant::now() < deadline {
                    // Each writer cycles through 4 paths so they re-merge the
                    // same file ~25% of the time, exercising the
                    // remove-then-insert idempotency path under contention.
                    let path = format!("/writer_{writer_idx}_{}.cpp", iteration % 4);
                    let mut writer = graph.write();
                    writer.merge_file_graph(make_fg(&path, iteration));
                    drop(writer);
                    iteration = iteration.wrapping_add(1);
                    thread::yield_now();
                }
            });
        }
    });
    let elapsed = started.elapsed();

    // The duration assertions are inclusive: scoped threads auto-join, so by
    // here every reader and writer has completed without panicking.
    assert!(
        elapsed >= test_duration,
        "test ended before deadline ({elapsed:?} < {test_duration:?}) — \
         did all threads exit early?"
    );
    assert!(
        !consistency_violation.load(Ordering::Relaxed),
        "a reader observed an internally inconsistent graph snapshot under \
         concurrent writes — the RwLock guarantees should make this impossible",
    );

    // Final sanity check: the seed file plus whatever remains from writers'
    // last merges should leave the graph in a queryable state. We don't pin
    // exact counts (writers race), but stats() must be callable and the seed
    // file's contents survive (no writer touches /seed.cpp).
    let final_snapshot = graph.read();
    let stats = final_snapshot.stats();
    assert!(
        stats.files >= 1,
        "expected at least the seed file in the final graph, got {} files",
        stats.files,
    );
    let seed_symbols = final_snapshot.file_symbols(Path::new("/seed.cpp"));
    assert_eq!(
        seed_symbols.len(),
        4,
        "seed file's symbols should be untouched after concurrent test",
    );
}
