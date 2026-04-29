//! Developer CLI harness mirroring `cmd/parse-test/main.go`.
//!
//! Walks a directory, dispatches each file to the matching `LanguagePlugin`
//! via [`LanguageRegistry`], and prints a structured report of files,
//! symbols, edges, and warnings. The output format is byte-equivalent to the
//! Go binary's so a `diff` between them validates Rust/Go output parity.
//!
//! Phase 1 uses a synchronous `walkdir`-based scan; Phase 3 swaps in
//! `codegraph-tools::discovery::discover` and re-runs validation.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use codegraph_core::{Edge, EdgeKind, Symbol, SymbolKind};
use codegraph_lang::LanguageRegistry;
use codegraph_lang_cpp::CppParser;
use walkdir::WalkDir;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: parse-test <directory>");
        return ExitCode::from(1);
    }

    let dir = PathBuf::from(&args[1]);
    match fs::metadata(&dir) {
        Ok(m) if m.is_dir() => {}
        _ => {
            eprintln!("Error: {} is not a valid directory", dir.display());
            return ExitCode::from(1);
        }
    }

    let mut registry = LanguageRegistry::new();
    let parser = match CppParser::new() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error initializing parser: {e}");
            return ExitCode::from(1);
        }
    };
    if let Err(e) = registry.register(Box::new(parser)) {
        eprintln!("Error registering parser: {e}");
        return ExitCode::from(1);
    }

    let mut files: Vec<String> = Vec::new();
    let mut all_symbols: Vec<Symbol> = Vec::new();
    let mut all_edges: Vec<Edge> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // First pass: collect (absolute_path, plugin) tuples sorted by path so
    // symbol/edge accumulation matches the Go binary's `sort.Strings(files)`
    // ordering.
    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in WalkDir::new(&dir).follow_links(false).sort_by_file_name() {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                let path = err
                    .path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| dir.display().to_string());
                warnings.push(format!("{path}: {err}"));
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if registry.for_path(path).is_none() {
            continue;
        }
        let abs = match fs::canonicalize(path) {
            Ok(a) => a,
            // `filepath.Abs` in Go does not require the path to exist; if
            // canonicalize fails for a reason other than "not found", fall
            // back to the path as-is rather than dropping the file silently.
            Err(_) => path.to_path_buf(),
        };
        candidates.push(abs);
    }
    // `sort_by_file_name` already gives a deterministic order, but it's a
    // per-directory order, not a flat lexicographic one. Match Go's
    // `sort.Strings(files)` by sorting the absolute-path strings globally.
    candidates.sort();

    for abs in candidates {
        let plugin = match registry.for_path(&abs) {
            Some(p) => p,
            None => continue,
        };
        let content = match fs::read(&abs) {
            Ok(c) => c,
            Err(err) => {
                warnings.push(format!("{}: read error: {err}", abs.display()));
                continue;
            }
        };
        match plugin.parse_file(&abs, &content) {
            Ok(fg) => {
                files.push(abs.display().to_string());
                all_symbols.extend(fg.symbols);
                all_edges.extend(fg.edges);
            }
            Err(err) => {
                warnings.push(format!("{}: parse error: {err}", abs.display()));
            }
        }
    }

    print_report(&files, &all_symbols, &all_edges, &warnings);

    ExitCode::SUCCESS
}

/// Mirrors Go `fmt.Printf("  [%-8s] %-30s (%s:%d)%s\n", kind, name, base, line, ns)`
/// and `fmt.Printf("  [%-8s] %s -> %s%s\n", kind, from, to, loc)`.
fn print_report(files: &[String], symbols: &[Symbol], edges: &[Edge], warnings: &[String]) {
    println!("=== Files ({}) ===", files.len());
    for f in files {
        println!("  {f}");
    }

    println!();
    println!("=== Symbols ({}) ===", symbols.len());
    for s in symbols {
        let base = Path::new(&s.file)
            .file_name()
            .map(|os| os.to_string_lossy().into_owned())
            .unwrap_or_else(|| s.file.clone());
        let name = if s.parent.is_empty() {
            s.name.clone()
        } else {
            format!("{}::{}", s.parent, s.name)
        };
        let ns = if s.namespace.is_empty() {
            String::new()
        } else {
            format!(" ns={}", s.namespace)
        };
        println!(
            "  [{:<8}] {:<30} ({}:{}){}",
            symbol_kind_str(s.kind),
            name,
            base,
            s.line,
            ns
        );
    }

    println!();
    println!("=== Edges ({}) ===", edges.len());
    for e in edges {
        let from = shorten(&e.from);
        let loc = if e.line > 0 {
            format!(" (line {})", e.line)
        } else {
            String::new()
        };
        println!(
            "  [{:<8}] {} -> {}{}",
            edge_kind_str(e.kind),
            from,
            e.to,
            loc
        );
    }

    if !warnings.is_empty() {
        println!();
        println!("=== Warnings ({}) ===", warnings.len());
        for w in warnings {
            println!("  {w}");
        }
    }

    println!();
    println!(
        "Done: {} files, {} symbols, {} edges, {} warnings",
        files.len(),
        symbols.len(),
        edges.len(),
        warnings.len()
    );
}

/// Shorten a `from` identifier for display. If the `from` string contains a
/// `:` (e.g. `path:funcName`), the path component is replaced by its
/// basename. Otherwise the whole string is treated as a path and basename'd.
/// Mirrors `shorten` in `cmd/parse-test/main.go`.
fn shorten(from: &str) -> String {
    if let Some(idx) = from.rfind(':') {
        if idx > 0 {
            let path_part = &from[..idx];
            let suffix = &from[idx..];
            let base = Path::new(path_part)
                .file_name()
                .map(|os| os.to_string_lossy().into_owned())
                .unwrap_or_else(|| path_part.to_owned());
            return format!("{base}{suffix}");
        }
    }
    Path::new(from)
        .file_name()
        .map(|os| os.to_string_lossy().into_owned())
        .unwrap_or_else(|| from.to_owned())
}

/// Render a [`SymbolKind`] as the same lowercase string the Go binary uses.
///
/// `SymbolKind` is `#[non_exhaustive]` — it gains variants when new languages
/// land. Rather than enumerate variants here (and force every binary to
/// update on every kind addition), we lowercase the `Debug` name. The
/// invariant that `format!("{:?}", SymbolKind::Function) == "Function"`
/// holds for every derived `Debug` impl on a unit variant.
fn symbol_kind_str(k: SymbolKind) -> String {
    format!("{k:?}").to_ascii_lowercase()
}

/// Render an [`EdgeKind`] as the same lowercase string the Go binary uses.
/// See [`symbol_kind_str`] for the rationale on using `Debug` over a
/// hand-written match.
fn edge_kind_str(k: EdgeKind) -> String {
    format!("{k:?}").to_ascii_lowercase()
}
