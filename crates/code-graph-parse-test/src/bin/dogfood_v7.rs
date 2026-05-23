//! Dogfood the v7 binary cache (Phase C) on a real directory.
//!
//! Indexes the target dir, builds a [`Graph`], times `save` → reports
//! on-disk cache size; times `load` against a fresh `Graph` to confirm
//! round-trip; times `stale_paths`. Validates the v7 design's central
//! claims (small cache, fast warm load) on whatever corpus the user
//! points at.
//!
//! Usage: `cargo run -p code-graph-parse-test --bin dogfood-v7 -- <dir>`
//! (defaults to the workspace's own `crates/` if no dir is given).

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use code_graph_core::RootConfig;
use code_graph_graph::{cache_path, stale_paths, Graph};
use code_graph_lang::LanguageRegistry;
use code_graph_lang_cpp::CppParser;
use code_graph_lang_csharp::CSharpParser;
use code_graph_lang_go::GoParser;
use code_graph_lang_java::JavaParser;
use code_graph_lang_python::PythonParser;
use code_graph_lang_rust::RustParser;
use code_graph_tools::discovery::discover;
use code_graph_tools::indexer::{index_directory, resolve_all_edges, NoopProgressSink};

fn main() -> ExitCode {
    let dir = match env::args().nth(1) {
        Some(d) => PathBuf::from(d),
        None => default_target_dir(),
    };
    if !dir.is_dir() {
        eprintln!("error: {} is not a directory", dir.display());
        return ExitCode::from(1);
    }

    println!("=== v7 cache dogfood ===");
    println!("target: {}", dir.display());

    let registry = match build_registry() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("registry setup failed: {e}");
            return ExitCode::from(1);
        }
    };

    let mut cfg = RootConfig::default();
    cfg.resolve_concurrency();

    // ----- Index -----
    let t = Instant::now();
    let discovered = discover(&dir, &registry, &cfg, &NoopProgressSink);
    let discover_dur = t.elapsed();
    println!(
        "\n[discover]   {:.2?}  ({} files)",
        discover_dur,
        discovered.files.len()
    );

    let t = Instant::now();
    let (mut graphs, warnings) = match index_directory(&dir, &registry, &cfg, &NoopProgressSink) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("index_directory failed: {e}");
            return ExitCode::from(1);
        }
    };
    let parse_dur = t.elapsed();
    println!(
        "[parse]      {:.2?}  ({} file graphs, {} warnings)",
        parse_dur,
        graphs.len(),
        warnings.len()
    );

    // Resolve edges + build live Graph.
    let t = Instant::now();
    resolve_all_edges(&mut graphs, &registry, &NoopProgressSink);
    let resolve_dur = t.elapsed();
    println!("[resolve]    {:.2?}", resolve_dur);

    let t = Instant::now();
    let mut graph = Graph::new();
    for fg in graphs {
        graph.merge_file_graph(fg);
    }
    let merge_dur = t.elapsed();
    let stats = graph.stats();
    println!(
        "[merge]      {:.2?}  ({} nodes, {} edges, {} files)",
        merge_dur, stats.nodes, stats.edges, stats.files
    );

    // ----- Save (v7) -----
    let tmpdir = match tempdir_inside(&dir) {
        Some(d) => d,
        None => {
            eprintln!("could not create tempdir for cache");
            return ExitCode::from(1);
        }
    };
    let cache_dir = tmpdir.as_path();

    let t = Instant::now();
    if let Err(e) = graph.save(cache_dir) {
        eprintln!("graph.save failed: {e}");
        cleanup(&tmpdir);
        return ExitCode::from(1);
    }
    let save_dur = t.elapsed();
    let cache_file = cache_path(cache_dir);
    let cache_size = std::fs::metadata(&cache_file).map(|m| m.len()).unwrap_or(0);
    let per_node = if stats.nodes > 0 {
        cache_size as f64 / stats.nodes as f64
    } else {
        0.0
    };
    println!(
        "\n[save]       {:.2?}  cache file: {} bytes ({:.1} bytes/node)",
        save_dur, cache_size, per_node
    );

    // ----- Load (v7 mmap + bytecheck + decode) -----
    let t = Instant::now();
    let mut reloaded = Graph::new();
    match reloaded.load(cache_dir) {
        Ok(true) => {}
        Ok(false) => {
            eprintln!("error: load returned Ok(false) on a freshly written cache");
            cleanup(&tmpdir);
            return ExitCode::from(1);
        }
        Err(e) => {
            eprintln!("graph.load failed: {e}");
            cleanup(&tmpdir);
            return ExitCode::from(1);
        }
    }
    let load_dur = t.elapsed();
    let reloaded_stats = reloaded.stats();
    println!(
        "[load]       {:.2?}  ({} nodes, {} edges, {} files)",
        load_dur, reloaded_stats.nodes, reloaded_stats.edges, reloaded_stats.files
    );

    // Round-trip check via the public `stats()` API. Field-level
    // equality on Graph requires crate-private access; checking that
    // the stats triple matches is enough for a smoke test, and any
    // semantic divergence would surface during normal query usage
    // (the load path's `decode` already runs SymbolId-derivation
    // consistency checks per-symbol — see DecodeError::InconsistentSymbolId).
    let saved_stats = graph.stats();
    let loaded_stats = reloaded.stats();
    let stats_match = saved_stats == loaded_stats;
    if stats_match {
        println!("[round-trip] ✓ loaded stats == saved stats");
    } else {
        println!(
            "[round-trip] ✗ divergence: saved={:?} loaded={:?}",
            saved_stats, loaded_stats
        );
    }
    let all_match = stats_match;

    // ----- stale_paths -----
    let t = Instant::now();
    let stale = match stale_paths(cache_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("stale_paths failed: {e}");
            cleanup(&tmpdir);
            return ExitCode::from(1);
        }
    };
    let stale_dur = t.elapsed();
    println!(
        "[stale_paths]{:.2?}  ({} reported stale — expected 0 right after save)",
        stale_dur,
        stale.len()
    );

    println!("\n=== summary ===");
    println!(
        "  parsed:      {} files → {} symbols, {} edges",
        stats.files, stats.nodes, stats.edges
    );
    println!(
        "  cache size:  {} bytes ({:.1} KiB)",
        cache_size,
        cache_size as f64 / 1024.0
    );
    println!("  save:        {:.2?}", save_dur);
    println!(
        "  load:        {:.2?}  (mmap + bytecheck + decode)",
        load_dur
    );
    println!("  stale_paths: {:.2?}", stale_dur);
    if !warnings.is_empty() {
        println!("  warnings:    {} (first 3 shown below)", warnings.len());
        for w in warnings.iter().take(3) {
            println!("    - {}", w);
        }
    }

    cleanup(&tmpdir);
    if all_match {
        ExitCode::from(0)
    } else {
        ExitCode::from(2)
    }
}

fn build_registry() -> Result<LanguageRegistry, String> {
    let mut r = LanguageRegistry::new();
    r.register(Box::new(CppParser::new().map_err(|e| format!("cpp: {e}"))?))
        .map_err(|e| format!("register cpp: {e}"))?;
    r.register(Box::new(
        RustParser::new().map_err(|e| format!("rust: {e}"))?,
    ))
    .map_err(|e| format!("register rust: {e}"))?;
    r.register(Box::new(GoParser::new().map_err(|e| format!("go: {e}"))?))
        .map_err(|e| format!("register go: {e}"))?;
    r.register(Box::new(
        PythonParser::new().map_err(|e| format!("python: {e}"))?,
    ))
    .map_err(|e| format!("register python: {e}"))?;
    r.register(Box::new(
        CSharpParser::new().map_err(|e| format!("csharp: {e}"))?,
    ))
    .map_err(|e| format!("register csharp: {e}"))?;
    r.register(Box::new(
        JavaParser::new().map_err(|e| format!("java: {e}"))?,
    ))
    .map_err(|e| format!("register java: {e}"))?;
    Ok(r)
}

/// Workspace's own `crates/` dir, resolved relative to this binary's
/// `CARGO_MANIFEST_DIR`. Lets `cargo run --bin dogfood-v7` work with
/// no args.
fn default_target_dir() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir).join("..")
}

fn tempdir_inside(near: &Path) -> Option<TempDirHandle> {
    let parent = near.parent().unwrap_or(Path::new("/tmp"));
    let candidate = parent.join(format!(".dogfood-v7-{}", std::process::id()));
    std::fs::create_dir_all(&candidate).ok()?;
    Some(TempDirHandle(candidate))
}

struct TempDirHandle(PathBuf);
impl TempDirHandle {
    fn as_path(&self) -> &Path {
        &self.0
    }
}

fn cleanup(t: &TempDirHandle) {
    let _ = std::fs::remove_dir_all(&t.0);
}
