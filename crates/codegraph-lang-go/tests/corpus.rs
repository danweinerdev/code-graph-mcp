//! Phase 6.5 corpus regression test.
//!
//! Walks every `.go` file under `testdata/go/`, parses each via
//! [`GoParser`], aggregates symbol/edge totals, and asserts the
//! aggregates match what `testdata/go/MANIFEST.md` documents. The
//! MANIFEST is the regression contract — every count in this test must
//! line up with what the manifest claims.
//!
//! Per-file breakdowns are also asserted so a regression is localized to
//! a single fixture rather than reporting only the global total drift.
//!
//! Edge-case coverage (per Phase 6.5 verification):
//!   - empty file with only `package` clause (`empty.go`)
//!   - interface embedding interface (`Repo embeds Closer`,
//!     `ReadWriter embeds Reader`)
//!   - anonymous struct field in struct definition (`Memo.Cache`,
//!     `Cluster.Endpoints`)
//!   - blank-identifier function (`func _() {}` in `models/deps.go`)
//!   - structural interface implementation produces NO `Inherits` edges
//!   - embedded struct field produces NO `Inherits` edge
//!   - all import forms (single, grouped, aliased, dot, blank)
//!   - pointer + value receivers on the same type
//!   - goroutines and `defer`
//!   - package-level closure fallback
//!   - generic functions

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

// `BTreeMap` for deterministic per-file iteration; `HashMap` for the
// kind-keyed tallies because `SymbolKind`/`EdgeKind` are `Hash`-only
// (`non_exhaustive`, not `Ord`). Same rationale as the Rust corpus test.

use codegraph_core::{Edge, EdgeKind, FileGraph, Symbol, SymbolKind};
use codegraph_lang::LanguagePlugin;
use codegraph_lang_go::GoParser;
use pretty_assertions::assert_eq;

/// Resolve the absolute path of `testdata/go` from this crate's manifest
/// directory. Two `..` segments back up out of `crates/codegraph-lang-go/`
/// to the workspace root.
fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("testdata")
        .join("go")
}

/// Recursively discover every `.go` file under the corpus root, in
/// deterministic (sorted) order. Walks subdirectories so multi-package
/// fixtures (`server/`, `models/`, `utils/`) are picked up.
fn discover_corpus_files() -> Vec<PathBuf> {
    let root = corpus_root();
    let mut out = Vec::new();
    walk_collect_go(&root, &mut out);
    out.sort();
    out
}

fn walk_collect_go(dir: &Path, out: &mut Vec<PathBuf>) {
    let rd = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}"));
    for entry in rd {
        let entry = entry.unwrap_or_else(|e| panic!("read_dir entry: {e}"));
        let p = entry.path();
        let ft = entry.file_type().expect("file_type");
        if ft.is_dir() {
            walk_collect_go(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("go") {
            out.push(p);
        }
    }
}

/// Parse a single file via [`GoParser`]. Reads bytes from disk; failures
/// surface as panics with the path so the calling test localizes them.
fn parse_file(parser: &GoParser, path: &Path) -> FileGraph {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    parser
        .parse_file(path, &bytes)
        .unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

/// Build a per-file map keyed by basename for the per-file assertions.
/// Since the corpus has files at multiple depths but unique basenames
/// (`server/server.go` vs `models/repo.go`), basename keying still
/// uniquely identifies each file.
fn parse_corpus(parser: &GoParser) -> BTreeMap<String, FileGraph> {
    let mut out = BTreeMap::new();
    for p in discover_corpus_files() {
        let name = p
            .file_name()
            .and_then(|os| os.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| panic!("file with no UTF-8 basename: {p:?}"));
        let fg = parse_file(parser, &p);
        let prev = out.insert(name.clone(), fg);
        assert!(
            prev.is_none(),
            "corpus has duplicate basename {name:?}; the per-file assertions \
             below assume unique basenames"
        );
    }
    out
}

/// Tally symbols by kind across a slice of `Symbol`. `SymbolKind` is not
/// `Ord` (it's `non_exhaustive`), so a HashMap is the natural fit.
fn count_symbols_by_kind(symbols: &[Symbol]) -> HashMap<SymbolKind, usize> {
    let mut m = HashMap::new();
    for s in symbols {
        *m.entry(s.kind).or_insert(0) += 1;
    }
    m
}

/// Tally edges by kind across a slice of `Edge`.
fn count_edges_by_kind(edges: &[Edge]) -> HashMap<EdgeKind, usize> {
    let mut m = HashMap::new();
    for e in edges {
        *m.entry(e.kind).or_insert(0) += 1;
    }
    m
}

/// MANIFEST asserts these aggregates across the entire corpus. If a
/// fixture changes, update both this constant and `testdata/go/MANIFEST.md`
/// in the same commit.
const TOTAL_SYMBOLS: usize = 42;
const TOTAL_EDGES: usize = 41;

#[test]
fn corpus_aggregate_counts_match_manifest() {
    let parser = GoParser::new().expect("GoParser::new");
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
        14,
        "MANIFEST claims 14 Functions"
    );
    assert_eq!(
        by_kind.get(&SymbolKind::Method).copied().unwrap_or(0),
        12,
        "MANIFEST claims 12 Methods"
    );
    assert_eq!(
        by_kind.get(&SymbolKind::Struct).copied().unwrap_or(0),
        7,
        "MANIFEST claims 7 Structs"
    );
    assert_eq!(
        by_kind.get(&SymbolKind::Interface).copied().unwrap_or(0),
        6,
        "MANIFEST claims 6 Interfaces"
    );
    assert_eq!(
        by_kind.get(&SymbolKind::Typedef).copied().unwrap_or(0),
        3,
        "MANIFEST claims 3 Typedefs"
    );

    let edge_by_kind = count_edges_by_kind(&all_edges);
    assert_eq!(
        edge_by_kind.get(&EdgeKind::Calls).copied().unwrap_or(0),
        30,
        "MANIFEST claims 30 Calls edges"
    );
    assert_eq!(
        edge_by_kind.get(&EdgeKind::Includes).copied().unwrap_or(0),
        11,
        "MANIFEST claims 11 Includes edges"
    );
    // CRITICAL invariant: zero `Inherits` edges across the entire corpus.
    // Go interfaces are structurally typed (`*Server` satisfies `Runner`,
    // `*User` satisfies `Named` — neither produces an edge). Embedded
    // struct fields are structural composition, not inheritance.
    assert_eq!(
        edge_by_kind.get(&EdgeKind::Inherits).copied().unwrap_or(0),
        0,
        "Go is structurally typed — zero Inherits edges expected"
    );
}

#[test]
fn corpus_per_file_counts_match_manifest() {
    let parser = GoParser::new().expect("GoParser::new");
    let corpus = parse_corpus(&parser);

    // Expected (symbols, edges) per fixture file basename. Mirrors the
    // per-file tables in the MANIFEST.
    let expected: &[(&str, usize, usize)] = &[
        ("empty.go", 0, 0),
        ("main.go", 2, 9),
        ("deps.go", 7, 1),
        ("repo.go", 10, 11),
        ("user.go", 5, 2),
        ("handler.go", 2, 5),
        ("server.go", 7, 8),
        ("helpers.go", 9, 5),
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

#[test]
fn empty_file_produces_zero_symbols_and_zero_edges() {
    // `empty.go` declares only `package main`. The package_clause is
    // consumed without emitting a Symbol; no decls means no edges.
    let parser = GoParser::new().expect("GoParser::new");
    let corpus = parse_corpus(&parser);
    let empty = corpus.get("empty.go").expect("empty.go in corpus");
    assert!(
        empty.symbols.is_empty(),
        "empty.go must have 0 symbols; got: {:?}",
        empty
            .symbols
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
    );
    assert!(empty.edges.is_empty(), "empty.go must have 0 edges");
}

#[test]
fn no_inherits_edges_anywhere_in_corpus() {
    // The CRITICAL anti-regression: Go is structurally typed. *Server
    // satisfies Runner, *User satisfies Named, etc., but the parser
    // emits ZERO `Inherits` edges across every fixture file. Embedded
    // struct fields and embedded interfaces also produce no edges.
    //
    // This is asserted in aggregate above too, but pinned per-file here
    // so a regression localises to the offending fixture.
    let parser = GoParser::new().expect("GoParser::new");
    let corpus = parse_corpus(&parser);
    for (name, fg) in &corpus {
        let inherits: Vec<&Edge> = fg
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Inherits)
            .collect();
        assert!(
            inherits.is_empty(),
            "{name}: Go parser must emit zero Inherits edges; got: {inherits:?}"
        );
    }
}

#[test]
fn interface_embedding_interface_produces_no_extra_symbols() {
    // `Repo` embeds `Closer` in repo.go; `ReadWriter` embeds `Reader`
    // in deps.go. The embedded interface line is a `type_elem`, not a
    // `method_elem`, so it produces no Symbol. The four type-level
    // symbols (`Closer`, `Repo`, `Reader`, `ReadWriter`) account for
    // every interface in those two files — no nested ones.
    let parser = GoParser::new().expect("GoParser::new");
    let corpus = parse_corpus(&parser);

    let repo = corpus.get("repo.go").expect("repo.go in corpus");
    let interfaces_in_repo: Vec<&str> = repo
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Interface)
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(
        interfaces_in_repo.len(),
        2,
        "repo.go must have exactly 2 Interface symbols (Closer, Repo); got: {interfaces_in_repo:?}"
    );
    assert!(interfaces_in_repo.contains(&"Closer"));
    assert!(interfaces_in_repo.contains(&"Repo"));

    let deps = corpus.get("deps.go").expect("deps.go in corpus");
    let interfaces_in_deps: Vec<&str> = deps
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Interface)
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(
        interfaces_in_deps.len(),
        2,
        "deps.go must have exactly 2 Interface symbols (Reader, ReadWriter); got: {interfaces_in_deps:?}"
    );
    assert!(interfaces_in_deps.contains(&"Reader"));
    assert!(interfaces_in_deps.contains(&"ReadWriter"));
}

#[test]
fn anonymous_struct_field_does_not_produce_nested_symbols() {
    // `Memo.Cache` (in repo.go) and `Cluster.Endpoints` (in deps.go) are
    // anonymous-struct fields. The anonymous struct's inner fields
    // (`hits`, `host`, `port`) MUST NOT appear as Symbol records.
    let parser = GoParser::new().expect("GoParser::new");
    let corpus = parse_corpus(&parser);

    let repo = corpus.get("repo.go").expect("repo.go in corpus");
    for nested in ["hits", "Cache"] {
        assert!(
            !repo.symbols.iter().any(|s| s.name == nested),
            "{nested:?} (anonymous-struct inner) must not be a Symbol; \
             got: {:?}",
            repo.symbols
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
        );
    }

    let deps = corpus.get("deps.go").expect("deps.go in corpus");
    for nested in ["host", "port", "Endpoints"] {
        assert!(
            !deps.symbols.iter().any(|s| s.name == nested),
            "{nested:?} (anonymous-struct inner) must not be a Symbol; \
             got: {:?}",
            deps.symbols
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn blank_identifier_function_extracts_with_underscore_name() {
    // `func _() {}` in `models/deps.go` parses as a `function_declaration`
    // with name=identifier("_"). Anti-regression for the brief's
    // "blank-identifier function name" edge case.
    let parser = GoParser::new().expect("GoParser::new");
    let corpus = parse_corpus(&parser);
    let deps = corpus.get("deps.go").expect("deps.go in corpus");
    let blank: Vec<&Symbol> = deps
        .symbols
        .iter()
        .filter(|s| s.name == "_" && s.kind == SymbolKind::Function)
        .collect();
    assert_eq!(
        blank.len(),
        1,
        "deps.go must contain exactly one Function with name '_'; \
         got: {:?}",
        deps.symbols
            .iter()
            .map(|s| (s.name.as_str(), s.kind))
            .collect::<Vec<_>>()
    );
    assert_eq!(blank[0].namespace, "models");
    assert!(blank[0].parent.is_empty());
}

#[test]
fn embedded_struct_fields_in_user_produce_no_extra_symbols() {
    // `User` embeds `Profile` and `*sync.Mutex` (both anonymous fields).
    // Neither embed contributes a Symbol record beyond the base type
    // declarations themselves. The MANIFEST already pins user.go's
    // symbol count at 5; this test pins which 5 they are.
    let parser = GoParser::new().expect("GoParser::new");
    let corpus = parse_corpus(&parser);
    let user = corpus.get("user.go").expect("user.go in corpus");
    let names: Vec<&str> = user.symbols.iter().map(|s| s.name.as_str()).collect();
    let expected = ["Named", "Profile", "User", "Name", "Close"];
    for want in &expected {
        assert!(
            names.contains(want),
            "user.go must contain {want:?}; got: {names:?}"
        );
    }
    assert!(
        !names.contains(&"Mutex"),
        "embedded *sync.Mutex must NOT produce a Symbol; got: {names:?}"
    );
}

#[test]
fn package_level_closure_call_attributes_to_file_path_in_handler_go() {
    // CRITICAL anti-regression at the corpus level for the
    // package-level-closure fallback. `var Logger = func(...) { ... }` in
    // server/handler.go has a `fmt.Println` call inside; the call's
    // `from` MUST be the bare file path (no enclosing function/method).
    let parser = GoParser::new().expect("GoParser::new");
    let corpus = parse_corpus(&parser);
    let handler = corpus.get("handler.go").expect("handler.go in corpus");
    // The Println edges in handler.go are: file-path -> Println (Logger),
    // handle -> Println, withLog -> Println. We're looking for the one
    // whose `from` is the bare path (no `:`).
    let logger_edge = handler
        .edges
        .iter()
        .find(|e| e.kind == EdgeKind::Calls && e.to == "Println" && !e.from.contains(':'))
        .unwrap_or_else(|| {
            panic!(
                "expected one Calls edge with To=Println and bare-path \
                 from; edges in handler.go: {:?}",
                handler.edges
            )
        });
    // The bare path must be the absolute file path of handler.go.
    assert!(
        logger_edge.from.ends_with("handler.go"),
        "package-level closure's from must end with handler.go; got: {:?}",
        logger_edge.from
    );
}

#[test]
fn pointer_and_value_receivers_on_same_type_disambiguate() {
    // `Memo` has both a value-receiver method (`Get`) and a pointer-
    // receiver method (`Hits`); both must record parent="Memo". `Server`
    // has Run/Status/cleanup (pointer) and Stop (value); all parent="Server".
    let parser = GoParser::new().expect("GoParser::new");
    let corpus = parse_corpus(&parser);

    let repo = corpus.get("repo.go").expect("repo.go in corpus");
    for name in ["Get", "Hits"] {
        let m = repo
            .symbols
            .iter()
            .find(|s| s.name == name && s.kind == SymbolKind::Method)
            .unwrap_or_else(|| panic!("Memo::{name} method must exist"));
        assert_eq!(
            m.parent, "Memo",
            "Memo::{name}: parent must be Memo regardless of receiver \
             kind (value vs pointer)"
        );
    }

    let server = corpus.get("server.go").expect("server.go in corpus");
    for name in ["Run", "Stop", "Status", "cleanup"] {
        let m = server
            .symbols
            .iter()
            .find(|s| s.name == name && s.kind == SymbolKind::Method)
            .unwrap_or_else(|| panic!("Server::{name} method must exist"));
        assert_eq!(
            m.parent, "Server",
            "Server::{name}: parent must be Server regardless of receiver \
             kind (value vs pointer)"
        );
    }
}

#[test]
fn package_namespace_is_set_on_every_symbol() {
    // Every emitted Symbol carries its package's name in `namespace`.
    // Go packages are flat, so it's a single-level string.
    let parser = GoParser::new().expect("GoParser::new");
    let corpus = parse_corpus(&parser);

    let expected_ns: HashMap<&str, &str> = [
        ("main.go", "main"),
        ("deps.go", "models"),
        ("repo.go", "models"),
        ("user.go", "models"),
        ("handler.go", "server"),
        ("server.go", "server"),
        ("helpers.go", "utils"),
    ]
    .into_iter()
    .collect();

    for (name, fg) in &corpus {
        let Some(want_ns) = expected_ns.get(name.as_str()) else {
            // empty.go has no symbols, so no namespace assertion needed.
            continue;
        };
        for s in &fg.symbols {
            assert_eq!(
                s.namespace, *want_ns,
                "{name}: symbol {:?} must carry namespace={want_ns:?}, got {:?}",
                s.name, s.namespace
            );
        }
    }
}

#[test]
fn aliased_dot_blank_imports_record_path_not_alias() {
    // main.go's grouped block has every form: plain "fmt", aliased
    // umath="utils", dot="models", blank=_"image/png". Every Includes
    // edge's `to` must be the path; aliases / `.` / `_` must NOT appear.
    let parser = GoParser::new().expect("GoParser::new");
    let corpus = parse_corpus(&parser);
    let main = corpus.get("main.go").expect("main.go in corpus");
    let includes_to: Vec<&str> = main
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Includes)
        .map(|e| e.to.as_str())
        .collect();
    assert_eq!(
        includes_to.len(),
        5,
        "main.go must have 5 Includes edges; got: {includes_to:?}"
    );
    for want in [
        "fmt",
        "code-graph-go-corpus/server",
        "code-graph-go-corpus/utils",
        "code-graph-go-corpus/models",
        "image/png",
    ] {
        assert!(
            includes_to.contains(&want),
            "main.go imports must include {want:?}; got: {includes_to:?}"
        );
    }
    // Aliases and `.` / `_` must never leak into `to`.
    for forbidden in ["umath", ".", "_"] {
        assert!(
            !includes_to.contains(&forbidden),
            "alias / dot / blank ({forbidden:?}) must not appear as \
             import To; got: {includes_to:?}"
        );
    }
}

#[test]
fn generic_functions_signatures_carry_type_parameter_lists() {
    // `Map[T any, U any]` and `Filter[T any]` in repo.go must extract
    // with their type-parameter lists intact (truncate_signature drops
    // only from the body opener `{` onwards).
    let parser = GoParser::new().expect("GoParser::new");
    let corpus = parse_corpus(&parser);
    let repo = corpus.get("repo.go").expect("repo.go in corpus");
    let map_sym = repo
        .symbols
        .iter()
        .find(|s| s.name == "Map")
        .expect("Map function must exist");
    assert!(
        map_sym.signature.contains("Map[T any, U any]"),
        "Map's signature must carry the type-parameter list; got: {:?}",
        map_sym.signature
    );
    assert!(
        !map_sym.signature.contains('{'),
        "Map's signature must drop the body opener; got: {:?}",
        map_sym.signature
    );

    let filter_sym = repo
        .symbols
        .iter()
        .find(|s| s.name == "Filter")
        .expect("Filter function must exist");
    assert!(
        filter_sym.signature.contains("Filter[T any]"),
        "Filter's signature must carry the type-parameter list; got: {:?}",
        filter_sym.signature
    );
}

#[test]
fn two_init_functions_coexist_in_distinct_packages() {
    // `init()` in `main.go` (namespace=main) and `init()` in
    // `utils/helpers.go` (namespace=utils) are both extracted as
    // distinct Symbol records. The package namespace disambiguates.
    let parser = GoParser::new().expect("GoParser::new");
    let corpus = parse_corpus(&parser);

    let main = corpus.get("main.go").expect("main.go in corpus");
    let helpers = corpus.get("helpers.go").expect("helpers.go in corpus");
    let main_init = main
        .symbols
        .iter()
        .find(|s| s.name == "init" && s.kind == SymbolKind::Function)
        .expect("main.go init() must exist");
    let helpers_init = helpers
        .symbols
        .iter()
        .find(|s| s.name == "init" && s.kind == SymbolKind::Function)
        .expect("helpers.go init() must exist");
    assert_eq!(main_init.namespace, "main");
    assert_eq!(helpers_init.namespace, "utils");
}

/// Phase 6.5 dogfood-baseline regression test — gated on
/// `/tmp/logrus` being present. Runs `parse_file` over every `.go`
/// file in `/tmp/logrus` and asserts the symbol count stays within
/// ±10% of the baseline recorded in `testdata/go/logrus-baseline.txt`.
///
/// Marked `#[ignore]` so it does not run unless explicitly opted into
/// (e.g. `cargo test -p codegraph-lang-go -- --ignored
/// logrus_dogfood_baseline_within_ten_percent`). The baseline file is
/// committed; the test reads it at runtime.
///
/// **Setup is environment-only, not a real failure.** When `/tmp/logrus`
/// is missing this test silently skips with an `eprintln!` setup hint
/// rather than panicking. Without this skip, `cargo test --
/// --include-ignored` (which runs every `#[ignore]`-gated test in one
/// pass) would fail loudly on machines that haven't cloned the dogfood
/// fixture — the panic message reads as a setup instruction, not a code
/// bug, and shouldn't be a hard failure. The `eprintln! + return` form
/// matches how other dogfood-style tests in the workspace handle their
/// optional dependencies. Reading the committed baseline file IS still a
/// hard failure (the file is in-tree; if it's gone, that's a real bug),
/// hence the panic on `read_to_string` below.
#[test]
#[ignore]
fn logrus_dogfood_baseline_within_ten_percent() {
    let logrus_root = Path::new("/tmp/logrus");
    if !logrus_root.is_dir() {
        eprintln!(
            "skipping logrus dogfood baseline test: /tmp/logrus not present \
             — clone https://github.com/sirupsen/logrus.git at v1.9.3 \
             (commit d40e25cd45ed9c6b2b66e6b97573a0413e4c23bd) into \
             /tmp/logrus before running this test"
        );
        return;
    }

    let baseline_path = corpus_root().join("logrus-baseline.txt");
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

    let parser = GoParser::new().expect("GoParser::new");
    let mut files = Vec::new();
    walk_collect_go(logrus_root, &mut files);
    files.sort();

    let mut total_symbols: usize = 0;
    for f in &files {
        let bytes = std::fs::read(f).unwrap_or_else(|e| panic!("read {f:?}: {e}"));
        let fg = parser
            .parse_file(f, &bytes)
            .unwrap_or_else(|e| panic!("parse {f:?}: {e}"));
        total_symbols += fg.symbols.len();
    }

    // ±10% tolerance — round to integer counts.
    let lower = (baseline_count as f64 * 0.9).floor() as usize;
    let upper = (baseline_count as f64 * 1.1).ceil() as usize;
    assert!(
        total_symbols >= lower && total_symbols <= upper,
        "logrus parse produced {total_symbols} symbols; expected within \
         ±10% of baseline {baseline_count} (range [{lower}, {upper}])"
    );
}
