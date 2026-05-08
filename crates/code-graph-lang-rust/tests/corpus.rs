//! Phase 5.5 corpus regression test.
//!
//! Walks every `.rs` file under `testdata/rust/src/`, parses each via
//! [`RustParser`], aggregates symbol/edge totals, and asserts the
//! aggregates match what `testdata/rust/MANIFEST.md` documents. The
//! MANIFEST is the regression contract — every count in this test must
//! line up with what the manifest claims.
//!
//! Per-file breakdowns are also asserted so a regression is localized to
//! a single fixture rather than reporting only the global total drift.
//!
//! Edge-case coverage (per Phase 5.5 verification):
//!   - empty file (`empty.rs`)
//!   - mod-only file (`mod_only.rs`)
//!   - `unsafe` block (`utils.rs::unsafe_op`, `traits.rs::Greeter::do_unsafe`)
//!   - `extern crate` (`main.rs`)
//!   - `cfg` attribute (`utils.rs::cfg_gated_fn`)
//!   - nested mods (`models::nested`)
//!   - inherent impl with no methods (`traits.rs::EmptyImpl`)
//!   - trait with default methods (`traits.rs::Greet::default_greet`)

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

// `BTreeMap` is used for the basename-keyed corpus map so its iteration
// order is deterministic; `HashMap` is used for the kind-keyed tallies
// because `SymbolKind`/`EdgeKind` are `Hash`-only (`non_exhaustive`, not
// `Ord`).

use code_graph_core::{Edge, EdgeKind, FileGraph, Symbol, SymbolKind};
use code_graph_lang::LanguagePlugin;
use code_graph_lang_rust::RustParser;
use pretty_assertions::assert_eq;

/// Resolve the absolute path of `testdata/rust` from this crate's manifest
/// directory. Two `..` segments back up out of `crates/code-graph-lang-rust/`
/// to the workspace root.
fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("testdata")
        .join("rust")
}

/// Discover every `.rs` file under `testdata/rust/src/`, in deterministic
/// (sorted) order so a missing or extra file shows up as a stable diff.
fn discover_corpus_files() -> Vec<PathBuf> {
    let src = corpus_root().join("src");
    let mut out: Vec<PathBuf> = std::fs::read_dir(&src)
        .unwrap_or_else(|e| panic!("read_dir {src:?}: {e}"))
        .filter_map(|entry| {
            let p = entry.ok()?.path();
            if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                Some(p)
            } else {
                None
            }
        })
        .collect();
    out.sort();
    out
}

/// Parse a single file via [`RustParser`]. Reads bytes from disk; failures
/// surface as panics with the path so the calling test localizes them.
fn parse_file(parser: &RustParser, path: &Path) -> FileGraph {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    parser
        .parse_file(path, &bytes)
        .unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

/// Build a per-file map keyed by the file's basename (e.g. `"errors.rs"`)
/// for use in the per-file assertions. Basename keying keeps the test
/// independent of the absolute corpus path.
fn parse_corpus(parser: &RustParser) -> BTreeMap<String, FileGraph> {
    let mut out = BTreeMap::new();
    for p in discover_corpus_files() {
        let name = p
            .file_name()
            .and_then(|os| os.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| panic!("file with no UTF-8 basename: {p:?}"));
        let fg = parse_file(parser, &p);
        out.insert(name, fg);
    }
    out
}

/// Tally symbols by kind across a slice of `Symbol`. `SymbolKind` is not
/// `Ord` (it's `non_exhaustive` and may grow), so a HashMap keyed on the
/// kind is the natural fit.
fn count_symbols_by_kind(symbols: &[Symbol]) -> HashMap<SymbolKind, usize> {
    let mut m = HashMap::new();
    for s in symbols {
        *m.entry(s.kind).or_insert(0) += 1;
    }
    m
}

/// Tally edges by kind across a slice of `Edge`. Same `non_exhaustive`
/// rationale as [`count_symbols_by_kind`].
fn count_edges_by_kind(edges: &[Edge]) -> HashMap<EdgeKind, usize> {
    let mut m = HashMap::new();
    for e in edges {
        *m.entry(e.kind).or_insert(0) += 1;
    }
    m
}

/// MANIFEST asserts these aggregates across the entire corpus. If a fixture
/// changes, update both this constant and `testdata/rust/MANIFEST.md` in
/// the same commit.
const TOTAL_SYMBOLS: usize = 39;
const TOTAL_EDGES: usize = 39;

#[test]
fn corpus_aggregate_counts_match_manifest() {
    let parser = RustParser::new().expect("RustParser::new");
    let corpus = parse_corpus(&parser);

    let all_symbols: Vec<Symbol> = corpus
        .values()
        .flat_map(|fg| fg.symbols.iter().cloned())
        .collect();
    let all_edges: Vec<Edge> = corpus
        .values()
        .flat_map(|fg| fg.edges.iter().cloned())
        .collect();

    assert_eq!(
        all_symbols.len(),
        TOTAL_SYMBOLS,
        "MANIFEST claims {TOTAL_SYMBOLS} symbols total; got {}",
        all_symbols.len()
    );
    assert_eq!(
        all_edges.len(),
        TOTAL_EDGES,
        "MANIFEST claims {TOTAL_EDGES} edges total; got {}",
        all_edges.len()
    );

    // By-kind totals — these are the line-items in the MANIFEST tables.
    let by_kind = count_symbols_by_kind(&all_symbols);
    assert_eq!(
        by_kind.get(&SymbolKind::Function).copied().unwrap_or(0),
        10,
        "MANIFEST claims 10 Functions"
    );
    assert_eq!(
        by_kind.get(&SymbolKind::Method).copied().unwrap_or(0),
        11,
        "MANIFEST claims 11 Methods"
    );
    assert_eq!(
        by_kind.get(&SymbolKind::Struct).copied().unwrap_or(0),
        9,
        "MANIFEST claims 9 Structs"
    );
    assert_eq!(
        by_kind.get(&SymbolKind::Enum).copied().unwrap_or(0),
        3,
        "MANIFEST claims 3 Enums"
    );
    assert_eq!(
        by_kind.get(&SymbolKind::Trait).copied().unwrap_or(0),
        3,
        "MANIFEST claims 3 Traits"
    );
    assert_eq!(
        by_kind.get(&SymbolKind::Typedef).copied().unwrap_or(0),
        3,
        "MANIFEST claims 3 Typedefs"
    );

    let edge_by_kind = count_edges_by_kind(&all_edges);
    assert_eq!(
        edge_by_kind.get(&EdgeKind::Calls).copied().unwrap_or(0),
        22,
        "MANIFEST claims 22 Calls edges"
    );
    assert_eq!(
        edge_by_kind.get(&EdgeKind::Includes).copied().unwrap_or(0),
        11,
        "MANIFEST claims 11 Includes edges"
    );
    assert_eq!(
        edge_by_kind.get(&EdgeKind::Inherits).copied().unwrap_or(0),
        6,
        "MANIFEST claims 6 Inherits edges"
    );
}

#[test]
fn corpus_per_file_counts_match_manifest() {
    let parser = RustParser::new().expect("RustParser::new");
    let corpus = parse_corpus(&parser);

    // Expected (symbols, edges) per fixture file basename. Mirrors the
    // per-file tables in the MANIFEST.
    let expected: &[(&str, usize, usize)] = &[
        ("empty.rs", 0, 0),
        ("mod_only.rs", 0, 0),
        ("lib.rs", 2, 0),
        ("main.rs", 1, 16),
        ("models.rs", 9, 0),
        ("traits.rs", 13, 7),
        ("utils.rs", 6, 2),
        ("errors.rs", 8, 14),
    ];

    let actual_files: Vec<&str> = corpus.keys().map(String::as_str).collect();
    let expected_names: Vec<&str> = expected.iter().map(|(n, _, _)| *n).collect();
    let mut missing: Vec<&str> = expected_names
        .iter()
        .filter(|n| !actual_files.contains(n))
        .copied()
        .collect();
    let mut extra: Vec<&str> = actual_files
        .iter()
        .filter(|n| !expected_names.contains(n))
        .copied()
        .collect();
    missing.sort();
    extra.sort();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "corpus file mismatch — missing: {missing:?}, extra: {extra:?}"
    );

    for (name, want_syms, want_edges) in expected {
        let fg = corpus
            .get(*name)
            .unwrap_or_else(|| panic!("missing fixture file: {name}"));
        assert_eq!(
            fg.symbols.len(),
            *want_syms,
            "{name}: MANIFEST claims {want_syms} symbols, got {}: {:?}",
            fg.symbols.len(),
            fg.symbols
                .iter()
                .map(|s| (s.name.as_str(), s.kind))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            fg.edges.len(),
            *want_edges,
            "{name}: MANIFEST claims {want_edges} edges, got {}: {:?}",
            fg.edges.len(),
            fg.edges
                .iter()
                .map(|e| (e.kind, e.to.as_str()))
                .collect::<Vec<_>>()
        );
    }
}

/// CRITICAL anti-regression: `macro_rules! my_macro { ... }` in `utils.rs`
/// must NOT produce any Symbol whose name matches `my_macro`. Macro
/// definitions are intentionally excluded; only invocations produce Calls
/// edges. This is the corpus-level check; the lib's unit test
/// `macro_rules_definition_produces_zero_symbols` covers the inline form.
#[test]
fn macro_rules_definition_in_utils_produces_zero_symbols() {
    let parser = RustParser::new().expect("RustParser::new");
    let corpus = parse_corpus(&parser);
    let utils = corpus.get("utils.rs").expect("utils.rs in corpus");
    assert!(
        !utils.symbols.iter().any(|s| s.name == "my_macro"),
        "macro_rules! definition in utils.rs must not produce a Symbol; \
         got: {:?}",
        utils
            .symbols
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn empty_file_produces_zero_symbols_and_zero_edges() {
    let parser = RustParser::new().expect("RustParser::new");
    let corpus = parse_corpus(&parser);
    let empty = corpus.get("empty.rs").expect("empty.rs in corpus");
    assert!(empty.symbols.is_empty(), "empty.rs must have 0 symbols");
    assert!(empty.edges.is_empty(), "empty.rs must have 0 edges");
}

#[test]
fn mod_only_file_produces_zero_symbols_and_zero_edges() {
    // `pub mod a; pub mod b;` declares external modules but defines
    // nothing — no Symbols (mod_items are not symbols) and no Edges
    // (no `use`, no `extern crate`, no calls).
    let parser = RustParser::new().expect("RustParser::new");
    let corpus = parse_corpus(&parser);
    let mo = corpus.get("mod_only.rs").expect("mod_only.rs in corpus");
    assert!(mo.symbols.is_empty(), "mod_only.rs must have 0 symbols");
    assert!(mo.edges.is_empty(), "mod_only.rs must have 0 edges");
}

#[test]
fn extern_crate_in_main_produces_one_includes_edge() {
    let parser = RustParser::new().expect("RustParser::new");
    let corpus = parse_corpus(&parser);
    let main = corpus.get("main.rs").expect("main.rs in corpus");
    let alloc_edges: Vec<&Edge> = main
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Includes && e.to == "alloc")
        .collect();
    assert_eq!(
        alloc_edges.len(),
        1,
        "extern crate alloc; must produce exactly 1 Includes edge to 'alloc'; \
         got {alloc_edges:?}"
    );
}

#[test]
fn nested_mod_populates_namespace_on_inner_symbols() {
    let parser = RustParser::new().expect("RustParser::new");
    let corpus = parse_corpus(&parser);
    let models = corpus.get("models.rs").expect("models.rs in corpus");

    let inner = models
        .symbols
        .iter()
        .find(|s| s.name == "Inner")
        .expect("Inner struct must exist in models.rs");
    assert_eq!(inner.namespace, "nested");

    let helper = models
        .symbols
        .iter()
        .find(|s| s.name == "nested_helper")
        .expect("nested_helper fn must exist in models.rs");
    assert_eq!(helper.namespace, "nested");

    // The mod_item itself must NOT be emitted as a Symbol.
    assert!(
        !models.symbols.iter().any(|s| s.name == "nested"),
        "mod_item 'nested' must not appear as a Symbol record"
    );
}

#[test]
fn empty_inherent_impl_produces_no_methods_no_inheritance() {
    let parser = RustParser::new().expect("RustParser::new");
    let corpus = parse_corpus(&parser);
    let traits = corpus.get("traits.rs").expect("traits.rs in corpus");

    // EmptyImpl is the type backing `impl EmptyImpl {}`. It exists as a
    // Struct symbol; the impl block has no methods and no trait field, so
    // no method symbol and no inherits edge mention it.
    assert!(
        traits.symbols.iter().any(|s| s.name == "EmptyImpl"),
        "EmptyImpl struct must exist"
    );
    assert!(
        !traits.symbols.iter().any(|s| s.parent == "EmptyImpl"),
        "EmptyImpl must have no method symbols"
    );
    assert!(
        !traits
            .edges
            .iter()
            .any(|e| e.kind == EdgeKind::Inherits && e.from == "EmptyImpl"),
        "EmptyImpl must have no Inherits edges (inherent impl, no trait field)"
    );
}

#[test]
fn trait_default_method_extracts_as_function_no_parent() {
    // `Greet::default_greet`'s body is inside a `trait_item`, not an
    // `impl_item`, so the parent-resolution walk returns None; the
    // extractor emits Kind=Function with empty parent. (Methods only
    // exist inside impl blocks, by design.)
    let parser = RustParser::new().expect("RustParser::new");
    let corpus = parse_corpus(&parser);
    let traits = corpus.get("traits.rs").expect("traits.rs in corpus");
    let dg = traits
        .symbols
        .iter()
        .find(|s| s.name == "default_greet")
        .expect("default_greet must exist");
    assert_eq!(
        dg.kind,
        SymbolKind::Function,
        "default trait method must extract as Function (not Method) — \
         it's inside a trait_item, not an impl_item"
    );
    assert!(dg.parent.is_empty(), "default_greet has no parent");
}

#[test]
fn trait_impl_method_parent_is_implementing_type_not_trait() {
    // Anti-regression at the corpus level: `errors.rs::AppError::from`
    // (in `impl From<…> for AppError`) and `traits.rs::Greeter::greet`
    // (in `impl Greet for Greeter`) — both must have parent = the
    // implementing type, never the trait being implemented.
    let parser = RustParser::new().expect("RustParser::new");
    let corpus = parse_corpus(&parser);

    let errors = corpus.get("errors.rs").expect("errors.rs in corpus");
    let from_methods: Vec<&Symbol> = errors.symbols.iter().filter(|s| s.name == "from").collect();
    assert_eq!(
        from_methods.len(),
        2,
        "two `From` impls produce two `from` method symbols"
    );
    for m in &from_methods {
        assert_eq!(
            m.parent, "AppError",
            "From-impl method must have parent = AppError, never `From<…>`"
        );
    }

    let traits = corpus.get("traits.rs").expect("traits.rs in corpus");
    let greet = traits
        .symbols
        .iter()
        .find(|s| s.name == "greet" && s.kind == SymbolKind::Method)
        .expect("greet method must exist");
    assert_eq!(
        greet.parent, "Greeter",
        "trait-impl method parent must be the implementing type"
    );
    assert_ne!(greet.parent, "Greet", "must NOT use trait name as parent");
}

#[test]
fn inheritance_edges_capture_generic_type_text_verbatim() {
    // `impl<T: Display> Compute for Foo<T>` and `impl<T> Compute for Bar<T>
    // where T: Send` both record the type-field text verbatim — `Foo<T>`
    // and `Bar<T>` — including the generic parameter as written.
    let parser = RustParser::new().expect("RustParser::new");
    let corpus = parse_corpus(&parser);
    let traits = corpus.get("traits.rs").expect("traits.rs in corpus");

    let inherits: Vec<&Edge> = traits
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Inherits)
        .collect();
    let pairs: Vec<(&str, &str)> = inherits
        .iter()
        .map(|e| (e.from.as_str(), e.to.as_str()))
        .collect();
    assert!(
        pairs.contains(&("Foo<T>", "Compute")),
        "expected Foo<T> -> Compute, got {pairs:?}"
    );
    assert!(
        pairs.contains(&("Bar<T>", "Compute")),
        "expected Bar<T> -> Compute, got {pairs:?}"
    );
    assert!(
        pairs.contains(&("Greeter", "Greet")),
        "expected Greeter -> Greet, got {pairs:?}"
    );
}

#[test]
fn cfg_attributed_function_still_extracted() {
    // The parser does NOT evaluate `#[cfg(...)]`; the function is
    // unconditionally extracted as a Symbol regardless of host platform.
    let parser = RustParser::new().expect("RustParser::new");
    let corpus = parse_corpus(&parser);
    let utils = corpus.get("utils.rs").expect("utils.rs in corpus");
    assert!(
        utils.symbols.iter().any(|s| s.name == "cfg_gated_fn"),
        "cfg_gated_fn must extract regardless of platform; got: {:?}",
        utils.symbols.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
}

#[test]
fn unsafe_block_does_not_break_extraction() {
    let parser = RustParser::new().expect("RustParser::new");
    let corpus = parse_corpus(&parser);
    let utils = corpus.get("utils.rs").expect("utils.rs in corpus");
    assert!(
        utils.symbols.iter().any(|s| s.name == "unsafe_op"),
        "unsafe_op must extract despite unsafe block in body"
    );
    let traits = corpus.get("traits.rs").expect("traits.rs in corpus");
    assert!(
        traits
            .symbols
            .iter()
            .any(|s| s.name == "do_unsafe" && s.parent == "Greeter"),
        "Greeter::do_unsafe must extract despite unsafe block in body"
    );
}

/// Recursively collect every `.rs` file under `dir`. Used by the
/// ripgrep dogfood test below; not used by the per-file corpus assertions
/// above (those use a flat `read_dir` over `testdata/rust/src/`).
fn walk_collect_rust(dir: &Path, out: &mut Vec<PathBuf>) {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let p = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            walk_collect_rust(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}

/// Dogfood-baseline regression test against the `external/ripgrep`
/// submodule (pinned to 15.1.0). Walks every `.rs` file under
/// `external/ripgrep/crates/` and asserts the symbol count stays within
/// ±10% of the baseline recorded in `testdata/rust/ripgrep-baseline.txt`.
///
/// **Auto-skips when the submodule is not initialized.** Run
/// `git submodule update --init external/ripgrep` (or `make submodules`)
/// to opt in. The baseline file IS a hard read (it's in-tree); a missing
/// baseline panics. When the pinned submodule SHA is bumped, the symbol
/// count will usually drift — re-measure and update the baseline in the
/// same commit as the SHA bump.
#[test]
fn ripgrep_dogfood_baseline_within_ten_percent() {
    let ripgrep_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("external")
        .join("ripgrep")
        .join("crates");
    if !ripgrep_root.is_dir() {
        eprintln!(
            "skipping ripgrep dogfood baseline test: \
             external/ripgrep/crates not present — run `git submodule \
             update --init external/ripgrep` (or `make submodules`) to \
             opt in"
        );
        return;
    }

    let baseline_path = corpus_root().join("ripgrep-baseline.txt");
    let baseline_text = std::fs::read_to_string(&baseline_path)
        .unwrap_or_else(|e| panic!("read baseline {baseline_path:?}: {e}"));
    let baseline_count = baseline_text
        .lines()
        .find_map(|line| line.strip_prefix("symbols: "))
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or_else(|| {
            panic!(
                "baseline file {baseline_path:?} must contain a \
                 'symbols: N' line; got: {baseline_text:?}"
            )
        });

    let parser = RustParser::new().expect("RustParser::new");
    let mut files = Vec::new();
    walk_collect_rust(&ripgrep_root, &mut files);
    files.sort();

    let mut total_symbols: usize = 0;
    for f in &files {
        let bytes = std::fs::read(f).unwrap_or_else(|e| panic!("read {f:?}: {e}"));
        let fg = parser
            .parse_file(f, &bytes)
            .unwrap_or_else(|e| panic!("parse {f:?}: {e}"));
        total_symbols += fg.symbols.len();
    }

    let lower = (baseline_count as f64 * 0.9).floor() as usize;
    let upper = (baseline_count as f64 * 1.1).ceil() as usize;
    assert!(
        total_symbols >= lower && total_symbols <= upper,
        "ripgrep parse produced {total_symbols} symbols; expected within \
         ±10% of baseline {baseline_count} (range [{lower}, {upper}])"
    );
}
