//! `code-graph-bench` — repo-agnostic benchmark + verification tool.
//!
//! Replaces the prior `dogfood_v7` (cache round-trip story) and
//! `dogfood_phase_e` (Phase E feature checks) one-off scripts with a
//! single CLI that:
//!
//! 1. Indexes any directory you point at (Rust / C++ / Go / Python /
//!    C# / Java — every plugin the workspace ships).
//! 2. Reports per-stage timings (discover, parse, resolve, merge).
//! 3. Saves + loads the v7 cache `--iterations` times, reports
//!    median / min / max latency.
//! 4. Reports graph shape: total counts + per-language breakdown +
//!    top-N files and dirs by symbol count.
//! 5. Optionally runs subtree benchmarks (`get_orphans_under`,
//!    `search` with subtree, `detect_cycles` subtree post-filter).
//! 6. Optionally verifies the Rust RCMM and Go GMM
//!    `longest_prefix`-driven namespace upgrades are firing on the
//!    target's source.
//!
//! ## Usage
//!
//! ```text
//! code-graph-bench <PATH> [OPTIONS]
//!
//! OPTIONS
//!   --iterations N          Save+load runs (default 1). Median/min/max reported.
//!   --subtree PREFIX        Run subtree benchmarks scoped to this dir.
//!                           If omitted, auto-picks the first subdir of PATH
//!                           that has at least one indexed file. Pass an empty
//!                           string to disable: --subtree=
//!   --no-subtree            Disable subtree benchmarks entirely.
//!   --json                  Emit one JSON object to stdout (no human report).
//!   --cache-dir DIR         Where to write the cache (default: tempdir).
//!   --skip-verify           Skip RCMM/GMM namespace verification.
//!   --top-n N               Top-N for dirs/files lists (default 10).
//!   -h, --help              Show this help and exit.
//! ```
//!
//! ## Exit codes
//!
//! - `0`: all steps completed; verification (if not skipped) passed.
//! - `1`: usage / IO / parse error.
//! - `2`: ran to completion but verification failed (RCMM/GMM not live
//!   on a target that should have triggered them).

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use code_graph_core::{paths, ConfigError, Language, RootConfig};
use code_graph_graph::{cache_path, stale_paths, Graph, SearchParams};
use code_graph_lang::LanguageRegistry;
use code_graph_lang_cpp::CppParser;
use code_graph_lang_csharp::CSharpParser;
use code_graph_lang_go::GoParser;
use code_graph_lang_java::JavaParser;
use code_graph_lang_python::PythonParser;
use code_graph_lang_rust::RustParser;
use code_graph_tools::discovery::discover;
use code_graph_tools::indexer::{index_directory, resolve_all_edges, NoopProgressSink};
use serde::Serialize;

// ============================================================================
// CLI
// ============================================================================

struct Args {
    target: PathBuf,
    iterations: u32,
    subtree: SubtreeArg,
    json: bool,
    cache_dir: Option<PathBuf>,
    skip_verify: bool,
    top_n: usize,
}

enum SubtreeArg {
    Auto, // pick first subdir with indexed files
    Explicit(PathBuf),
    Disabled,
}

fn parse_args() -> Result<Args, String> {
    let mut target: Option<PathBuf> = None;
    let mut iterations: u32 = 1;
    let mut subtree = SubtreeArg::Auto;
    let mut json = false;
    let mut cache_dir: Option<PathBuf> = None;
    let mut skip_verify = false;
    let mut top_n: usize = 10;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "--iterations" => {
                iterations = args
                    .next()
                    .ok_or("--iterations requires a value")?
                    .parse()
                    .map_err(|e| format!("--iterations: {e}"))?;
                if iterations == 0 {
                    return Err("--iterations must be >= 1".into());
                }
            }
            "--subtree" => {
                let v = args.next().ok_or("--subtree requires a value")?;
                subtree = if v.is_empty() {
                    SubtreeArg::Disabled
                } else {
                    SubtreeArg::Explicit(PathBuf::from(v))
                };
            }
            "--no-subtree" => subtree = SubtreeArg::Disabled,
            "--json" => json = true,
            "--cache-dir" => {
                cache_dir = Some(PathBuf::from(
                    args.next().ok_or("--cache-dir requires a value")?,
                ));
            }
            "--skip-verify" => skip_verify = true,
            "--top-n" => {
                top_n = args
                    .next()
                    .ok_or("--top-n requires a value")?
                    .parse()
                    .map_err(|e| format!("--top-n: {e}"))?;
            }
            s if s.starts_with("--") => return Err(format!("unknown flag: {s}")),
            _ => {
                if target.is_some() {
                    return Err(format!("unexpected positional arg: {arg}"));
                }
                target = Some(PathBuf::from(arg));
            }
        }
    }
    let target = target.ok_or_else(|| {
        "usage: code-graph-bench <PATH> [OPTIONS]; pass --help for details".to_string()
    })?;
    Ok(Args {
        target,
        iterations,
        subtree,
        json,
        cache_dir,
        skip_verify,
        top_n,
    })
}

fn print_help() {
    println!("{}", env!("CARGO_PKG_NAME"));
    println!();
    println!("code-graph-bench — index, cache, and verify a code-graph-mcp target");
    println!();
    println!("USAGE");
    println!("    code-graph-bench <PATH> [OPTIONS]");
    println!();
    println!("OPTIONS");
    println!("    --iterations N         Save+load runs (default 1). Median/min/max reported.");
    println!("    --subtree PREFIX       Run subtree benchmarks scoped to this directory.");
    println!("                           Auto-picks the first subdir with indexed files when");
    println!("                           omitted. Pass --subtree= to disable.");
    println!("    --no-subtree           Disable subtree benchmarks entirely.");
    println!("    --json                 Emit JSON to stdout (no human report).");
    println!("    --cache-dir DIR        Where to write the cache (default: tempdir).");
    println!("    --skip-verify          Skip Phase E namespace verification.");
    println!("    --top-n N              Top-N entries in dir/file breakdowns (default 10).");
    println!("    -h, --help             Show this help and exit.");
}

// ============================================================================
// Report types (JSON-serializable)
// ============================================================================

#[derive(Serialize)]
struct Report {
    target: String,
    files_discovered: usize,
    parse_warnings: usize,

    stage_us: StageTimings,

    graph: GraphCounts,
    languages: BTreeMap<String, LangBreakdown>,
    top_dirs: Vec<DirCount>,
    top_files: Vec<FileCount>,

    cache_bytes: u64,
    bytes_per_node: f64,
    save_us: TimingStats,
    load_us: TimingStats,
    stale_paths_us: TimingStats,

    round_trip_ok: bool,

    subtree: Option<SubtreeReport>,

    verification: Option<Verification>,
}

#[derive(Serialize)]
struct StageTimings {
    discover: u64,
    parse: u64,
    resolve: u64,
    merge: u64,
}

#[derive(Serialize)]
struct GraphCounts {
    nodes: u32,
    edges: u32,
    files: u32,
}

#[derive(Serialize)]
struct LangBreakdown {
    files: usize,
    symbols: usize,
}

#[derive(Serialize)]
struct DirCount {
    dir: String,
    files: usize,
    symbols: usize,
}

#[derive(Serialize)]
struct FileCount {
    file: String,
    symbols: usize,
    language: String,
}

#[derive(Serialize)]
struct TimingStats {
    iterations: u32,
    median: u64,
    min: u64,
    max: u64,
    all: Vec<u64>,
}

impl TimingStats {
    fn from_durations(ds: &[Duration]) -> Self {
        let mut us: Vec<u64> = ds.iter().map(|d| d.as_micros() as u64).collect();
        us.sort_unstable();
        let median = us.get(us.len() / 2).copied().unwrap_or(0);
        let min = us.first().copied().unwrap_or(0);
        let max = us.last().copied().unwrap_or(0);
        Self {
            iterations: us.len() as u32,
            median,
            min,
            max,
            all: us,
        }
    }
}

#[derive(Serialize)]
struct SubtreeReport {
    prefix: String,
    orphans_whole_graph: usize,
    orphans_under_prefix: usize,
    search_whole_total: u32,
    search_subtree_total: u32,
    cycles_whole_count: usize,
    cycles_under_prefix: usize,
}

#[derive(Serialize)]
struct Verification {
    rust_namespace_upgrade: Option<bool>,
    rust_namespace_sample: Option<String>,
    go_namespace_upgrade: Option<bool>,
    go_namespace_sample: Option<String>,
    rust_symbols_indexed: usize,
    go_symbols_indexed: usize,
}

// ============================================================================
// Main
// ============================================================================

fn main() -> ExitCode {
    let mut args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };

    if !args.target.is_dir() {
        eprintln!("error: {} is not a directory", args.target.display());
        return ExitCode::from(1);
    }

    // Canonicalize before discovery so `Path::parent()` walks resolved-target
    // ancestry (relative paths like `.` under-walk otherwise) and so the
    // reported target matches what the MCP analyze handler would resolve.
    match paths::canonicalize(&args.target) {
        Ok(p) => args.target = p,
        Err(e) => {
            eprintln!(
                "error: failed to canonicalize {}: {e}",
                args.target.display()
            );
            return ExitCode::from(1);
        }
    }

    let registry = match build_registry() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("registry setup failed: {e}");
            return ExitCode::from(1);
        }
    };

    // Discover `.code-graph.toml` by walking upward from the target, same as
    // `analyze_codebase`. Surface the discovery outcome on stderr so JSON-mode
    // benchmark output stays clean while the user still sees which config
    // applied — relevant when comparing bench numbers to MCP server output
    // against the same tree (UE-style C++ classes need `[cpp].macro_strip`
    // to extract; an unconfigured tree under-reports symbol counts).
    let (mut cfg, project_root) = match RootConfig::load(&args.target) {
        Ok(pair) => pair,
        Err(ConfigError::Toml(e)) => {
            eprintln!("error: failed to parse .code-graph.toml: {e}");
            return ExitCode::from(1);
        }
        Err(ConfigError::Io(e)) => {
            eprintln!("error: failed to read .code-graph.toml: {e}");
            return ExitCode::from(1);
        }
        Err(e) => {
            eprintln!("error: invalid .code-graph.toml: {e}");
            return ExitCode::from(1);
        }
    };
    let toml_path = project_root.join(".code-graph.toml");
    if toml_path.exists() {
        if project_root == args.target {
            eprintln!("config: loaded {}", toml_path.display());
        } else {
            eprintln!(
                "config: loaded {} (parent of target {})",
                toml_path.display(),
                args.target.display()
            );
        }
    } else {
        eprintln!(
            "config: no .code-graph.toml found between {} and filesystem root; \
             using built-in defaults (engine-style C++ classes like `class CORE_API Foo` \
             will NOT be indexed without [cpp].macro_strip)",
            args.target.display()
        );
    }
    cfg.resolve_concurrency();

    // ----- Index pipeline -----
    let t = Instant::now();
    let discovered = discover(&args.target, &registry, &cfg, &NoopProgressSink);
    let discover_d = t.elapsed();
    let files_discovered = discovered.files.len();

    let t = Instant::now();
    let (mut graphs, warnings) =
        match index_directory(&args.target, &registry, &cfg, &NoopProgressSink) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("index_directory: {e}");
                return ExitCode::from(1);
            }
        };
    let parse_d = t.elapsed();

    let t = Instant::now();
    resolve_all_edges(&mut graphs, &registry, &NoopProgressSink);
    let resolve_d = t.elapsed();

    let t = Instant::now();
    let mut graph = Graph::new();
    for fg in &graphs {
        graph.merge_file_graph(fg.clone());
    }
    let merge_d = t.elapsed();

    let counts = graph.stats();

    // Per-language + top-N stats (from `graphs` — pre-merge — because
    // it carries language alongside symbols).
    let (languages, top_dirs, top_files) = aggregate_breakdowns(&graphs, args.top_n);

    // ----- Cache: save + load loop -----
    let cache_dir = match prepare_cache_dir(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cache dir setup: {e}");
            return ExitCode::from(1);
        }
    };

    let mut save_durs = Vec::with_capacity(args.iterations as usize);
    let mut load_durs = Vec::with_capacity(args.iterations as usize);
    let mut stale_durs = Vec::with_capacity(args.iterations as usize);
    let mut round_trip_ok = true;
    let mut cache_bytes: u64 = 0;

    for _ in 0..args.iterations {
        let t = Instant::now();
        if let Err(e) = graph.save(cache_dir.path()) {
            eprintln!("graph.save: {e}");
            cleanup(&cache_dir);
            return ExitCode::from(1);
        }
        save_durs.push(t.elapsed());

        let file = cache_path(cache_dir.path());
        cache_bytes = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);

        let t = Instant::now();
        let mut reloaded = Graph::new();
        match reloaded.load(cache_dir.path()) {
            Ok(true) => {}
            Ok(false) => {
                eprintln!("load returned Ok(false) on fresh cache");
                cleanup(&cache_dir);
                return ExitCode::from(1);
            }
            Err(e) => {
                eprintln!("graph.load: {e}");
                cleanup(&cache_dir);
                return ExitCode::from(1);
            }
        }
        load_durs.push(t.elapsed());

        if reloaded.stats() != counts {
            round_trip_ok = false;
        }

        let t = Instant::now();
        if stale_paths(cache_dir.path()).is_err() {
            eprintln!("stale_paths returned Err");
        }
        stale_durs.push(t.elapsed());
    }

    let bytes_per_node = if counts.nodes > 0 {
        cache_bytes as f64 / counts.nodes as f64
    } else {
        0.0
    };

    // ----- Subtree benchmarks -----
    let subtree_report = resolve_subtree(&args, &graph).map(|prefix| run_subtree(&graph, &prefix));

    // ----- Phase E verification -----
    let verification = if args.skip_verify {
        None
    } else {
        Some(run_verification(&graph))
    };

    let verification_ok = verification
        .as_ref()
        .map(verification_passed)
        .unwrap_or(true);

    let report = Report {
        target: args.target.display().to_string(),
        files_discovered,
        parse_warnings: warnings.len(),
        stage_us: StageTimings {
            discover: discover_d.as_micros() as u64,
            parse: parse_d.as_micros() as u64,
            resolve: resolve_d.as_micros() as u64,
            merge: merge_d.as_micros() as u64,
        },
        graph: GraphCounts {
            nodes: counts.nodes,
            edges: counts.edges,
            files: counts.files,
        },
        languages,
        top_dirs,
        top_files,
        cache_bytes,
        bytes_per_node,
        save_us: TimingStats::from_durations(&save_durs),
        load_us: TimingStats::from_durations(&load_durs),
        stale_paths_us: TimingStats::from_durations(&stale_durs),
        round_trip_ok,
        subtree: subtree_report,
        verification,
    };

    if args.json {
        match serde_json::to_string_pretty(&report) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("serializing report: {e}");
                cleanup(&cache_dir);
                return ExitCode::from(1);
            }
        }
    } else {
        print_human_report(&report);
    }

    cleanup(&cache_dir);

    if round_trip_ok && verification_ok {
        ExitCode::from(0)
    } else {
        ExitCode::from(2)
    }
}

// ============================================================================
// Helpers
// ============================================================================

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

struct CacheDirGuard {
    path: PathBuf,
    cleanup: bool,
}
impl CacheDirGuard {
    fn path(&self) -> &Path {
        &self.path
    }
}

fn prepare_cache_dir(args: &Args) -> Result<CacheDirGuard, String> {
    let (path, cleanup) = match &args.cache_dir {
        Some(p) => {
            std::fs::create_dir_all(p).map_err(|e| format!("mkdir {}: {e}", p.display()))?;
            (p.clone(), false)
        }
        None => {
            let parent = args.target.parent().unwrap_or(Path::new("/tmp"));
            let path = parent.join(format!(".cg-bench-{}", std::process::id()));
            std::fs::create_dir_all(&path).map_err(|e| format!("mkdir {}: {e}", path.display()))?;
            (path, true)
        }
    };
    Ok(CacheDirGuard { path, cleanup })
}

fn cleanup(guard: &CacheDirGuard) {
    if guard.cleanup {
        let _ = std::fs::remove_dir_all(&guard.path);
    }
}

fn aggregate_breakdowns(
    graphs: &[code_graph_core::FileGraph],
    top_n: usize,
) -> (
    BTreeMap<String, LangBreakdown>,
    Vec<DirCount>,
    Vec<FileCount>,
) {
    let mut langs: BTreeMap<String, LangBreakdown> = BTreeMap::new();
    let mut dirs: BTreeMap<String, DirCount> = BTreeMap::new();
    let mut files: Vec<FileCount> = Vec::with_capacity(graphs.len());

    for fg in graphs {
        let lang = format!("{:?}", fg.language);
        let entry = langs.entry(lang.clone()).or_insert(LangBreakdown {
            files: 0,
            symbols: 0,
        });
        entry.files += 1;
        entry.symbols += fg.symbols.len();

        let parent = Path::new(&fg.path)
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let d = dirs.entry(parent.clone()).or_insert(DirCount {
            dir: parent,
            files: 0,
            symbols: 0,
        });
        d.files += 1;
        d.symbols += fg.symbols.len();

        files.push(FileCount {
            file: fg.path.clone(),
            symbols: fg.symbols.len(),
            language: lang,
        });
    }

    let mut top_dirs: Vec<DirCount> = dirs.into_values().collect();
    top_dirs.sort_by(|a, b| b.symbols.cmp(&a.symbols).then(a.dir.cmp(&b.dir)));
    top_dirs.truncate(top_n);

    files.sort_by(|a, b| b.symbols.cmp(&a.symbols).then(a.file.cmp(&b.file)));
    files.truncate(top_n);

    (langs, top_dirs, files)
}

fn resolve_subtree(args: &Args, graph: &Graph) -> Option<PathBuf> {
    match &args.subtree {
        SubtreeArg::Disabled => None,
        SubtreeArg::Explicit(p) => Some(p.clone()),
        SubtreeArg::Auto => {
            // Pick the FIRST subdirectory of target that contains at
            // least one indexed file. Walk the snapshot in sorted
            // order so the choice is deterministic across runs.
            let mut snap = graph.file_graphs_snapshot();
            snap.sort_by(|a, b| a.path.cmp(&b.path));
            for fg in &snap {
                let p = Path::new(&fg.path);
                if let Ok(rel) = p.strip_prefix(&args.target) {
                    if let Some(first) = rel.components().next() {
                        let candidate = args.target.join(first.as_os_str());
                        if candidate.is_dir() {
                            return Some(candidate);
                        }
                    }
                }
            }
            None
        }
    }
}

fn run_subtree(graph: &Graph, prefix: &Path) -> SubtreeReport {
    let orphans_whole = graph.orphans(None).len();
    let orphans_under = graph.orphans_under(prefix, None).len();

    // Single search to avoid double-scanning the whole graph.
    let make_params = |subtree: Option<PathBuf>| SearchParams {
        pattern: String::new(),
        kind: None,
        namespace: String::new(),
        language: None,
        limit: 1, // we only read `total`; the result list cost is irrelevant
        offset: 0,
        count_only: true,
        subtree,
    };
    let search_whole = graph.search(make_params(None)).total;
    let search_subtree = graph.search(make_params(Some(prefix.to_path_buf()))).total;

    let cycles_all = graph.detect_cycles();
    let cycles_under = cycles_all
        .iter()
        .filter(|c| c.iter().all(|f| f.starts_with(prefix)))
        .count();

    SubtreeReport {
        prefix: prefix.display().to_string(),
        orphans_whole_graph: orphans_whole,
        orphans_under_prefix: orphans_under,
        search_whole_total: search_whole,
        search_subtree_total: search_subtree,
        cycles_whole_count: cycles_all.len(),
        cycles_under_prefix: cycles_under,
    }
}

fn run_verification(graph: &Graph) -> Verification {
    let rust = graph.search(SearchParams {
        pattern: String::new(),
        kind: None,
        namespace: String::new(),
        language: Some(Language::Rust),
        limit: 5000,
        offset: 0,
        count_only: false,
        subtree: None,
    });
    let rust_symbols = rust.symbols.len();
    let (rust_ok, rust_sample) = if rust_symbols == 0 {
        (None, None)
    } else {
        let sample = rust.symbols.iter().find(|s| !s.namespace.is_empty());
        match sample {
            Some(s) => (
                Some(true),
                Some(format!("{}::{} (at {})", s.namespace, s.name, s.file)),
            ),
            None => (Some(false), None),
        }
    };

    let go = graph.search(SearchParams {
        pattern: String::new(),
        kind: None,
        namespace: String::new(),
        language: Some(Language::Go),
        limit: 5000,
        offset: 0,
        count_only: false,
        subtree: None,
    });
    let go_symbols = go.symbols.len();
    let (go_ok, go_sample) = if go_symbols == 0 {
        (None, None)
    } else {
        // Module-qualified Go namespaces contain at least one `/`
        // (the import-path separator). Bare package names are
        // single tokens.
        let sample = go.symbols.iter().find(|s| s.namespace.contains('/'));
        match sample {
            Some(s) => (
                Some(true),
                Some(format!("{}::{} (at {})", s.namespace, s.name, s.file)),
            ),
            None => (Some(false), None),
        }
    };

    Verification {
        rust_namespace_upgrade: rust_ok,
        rust_namespace_sample: rust_sample,
        go_namespace_upgrade: go_ok,
        go_namespace_sample: go_sample,
        rust_symbols_indexed: rust_symbols,
        go_symbols_indexed: go_symbols,
    }
}

fn verification_passed(v: &Verification) -> bool {
    // A None means the language wasn't present in the target — that's
    // fine. A Some(false) means symbols WERE indexed but the upgrade
    // didn't fire — that's a failure.
    !matches!(v.rust_namespace_upgrade, Some(false))
        && !matches!(v.go_namespace_upgrade, Some(false))
}

// ============================================================================
// Human report
// ============================================================================

fn print_human_report(r: &Report) {
    println!("=== code-graph-bench ===");
    println!("target:            {}", r.target);
    println!(
        "files discovered:  {}   parse warnings: {}",
        r.files_discovered, r.parse_warnings
    );
    println!();

    println!("--- stage timings (µs) ---");
    println!("  discover: {:>10}", r.stage_us.discover);
    println!("  parse:    {:>10}", r.stage_us.parse);
    println!("  resolve:  {:>10}", r.stage_us.resolve);
    println!("  merge:    {:>10}", r.stage_us.merge);
    println!();

    println!(
        "--- graph counts ---  nodes: {}   edges: {}   files: {}",
        r.graph.nodes, r.graph.edges, r.graph.files
    );
    println!();

    println!("--- per-language ---");
    for (lang, b) in &r.languages {
        println!(
            "  {:<10} files: {:>5}   symbols: {:>7}",
            lang, b.files, b.symbols
        );
    }
    println!();

    if !r.top_dirs.is_empty() {
        println!("--- top dirs by symbol count ---");
        for d in &r.top_dirs {
            println!(
                "  {:>7} symbols   {:>4} files   {}",
                d.symbols, d.files, d.dir
            );
        }
        println!();
    }

    if !r.top_files.is_empty() {
        println!("--- top files by symbol count ---");
        for f in &r.top_files {
            println!(
                "  {:>5} symbols   [{:<10}]   {}",
                f.symbols, f.language, f.file
            );
        }
        println!();
    }

    println!("--- cache ---");
    println!(
        "  size:       {:>10} bytes ({:.1} KiB, {:.1} bytes/node)",
        r.cache_bytes,
        r.cache_bytes as f64 / 1024.0,
        r.bytes_per_node
    );
    print_timing("  save:      ", &r.save_us);
    print_timing("  load:      ", &r.load_us);
    print_timing("  stale:     ", &r.stale_paths_us);
    println!(
        "  round-trip: {}",
        if r.round_trip_ok {
            "✓ stats match"
        } else {
            "✗ stats diverge — investigate"
        }
    );
    println!();

    if let Some(s) = &r.subtree {
        println!("--- subtree benchmarks ---");
        println!("  prefix: {}", s.prefix);
        println!(
            "  orphans:       whole {:>5}   under prefix {:>5}",
            s.orphans_whole_graph, s.orphans_under_prefix
        );
        println!(
            "  search total:  whole {:>5}   under prefix {:>5}",
            s.search_whole_total, s.search_subtree_total
        );
        println!(
            "  cycles:        whole {:>5}   under prefix {:>5}",
            s.cycles_whole_count, s.cycles_under_prefix
        );
        println!();
    }

    if let Some(v) = &r.verification {
        println!("--- Phase E verification ---");
        match v.rust_namespace_upgrade {
            None => println!("  Rust RCMM:  skipped (no Rust symbols indexed)"),
            Some(true) => println!(
                "  Rust RCMM:  ✓ live   sample: {}",
                v.rust_namespace_sample.as_deref().unwrap_or("?")
            ),
            Some(false) => println!(
                "  Rust RCMM:  ✗ FAILED — {} Rust symbols indexed, none with a \
                 crate-qualified namespace",
                v.rust_symbols_indexed
            ),
        }
        match v.go_namespace_upgrade {
            None => println!("  Go GMM:     skipped (no Go symbols indexed)"),
            Some(true) => println!(
                "  Go GMM:     ✓ live   sample: {}",
                v.go_namespace_sample.as_deref().unwrap_or("?")
            ),
            Some(false) => println!(
                "  Go GMM:     ✗ FAILED — {} Go symbols indexed, none with a \
                 module-qualified namespace",
                v.go_symbols_indexed
            ),
        }
    }
}

fn print_timing(label: &str, t: &TimingStats) {
    if t.iterations == 1 {
        println!("{label}{:>10} µs", t.median);
    } else {
        println!(
            "{label}median {:>8} µs   min {:>8} µs   max {:>8} µs   (n={})",
            t.median, t.min, t.max, t.iterations
        );
    }
}
