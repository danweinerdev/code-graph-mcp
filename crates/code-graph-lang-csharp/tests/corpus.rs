//! Phase 2.6 corpus regression test for the C# parser.
//!
//! Walks every `.cs` file under `testdata/csharp/`, parses each via
//! [`CSharpParser`], aggregates symbol/edge totals, and asserts the
//! aggregates match what `testdata/csharp/MANIFEST.md` documents. The
//! MANIFEST is the regression contract — every count in this test must
//! line up with what the manifest claims.
//!
//! Per-file breakdowns are also asserted so a regression is localized to
//! a single fixture rather than reporting only the global total drift.
//!
//! Edge-case coverage (per Phase 2.6 verification):
//!   - empty file (zero bytes) → 0 symbols, 0 edges, no panic
//!   - comments-only file → same
//!   - syntax-error file → parser skips ERROR nodes gracefully (4
//!     symbols recovered; the `broken.py` analog — run and record, not
//!     zero)
//!   - 2-level nested classes → immediate-parent contract
//!   - partial classes across files → two Class symbols, file-path
//!     disambiguates
//!   - method name collides with another class's method → distinct
//!     symbols (parent-disambiguated)
//!
//! The dogfood-baseline regression test
//! (`efcore_dogfood_baseline_within_ten_percent`) lives at the bottom
//! and auto-skips when the `external/efcore` git submodule is not
//! initialized.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

// `BTreeMap` for deterministic per-file iteration; `HashMap` for the
// kind-keyed tallies because `SymbolKind`/`EdgeKind` are `Hash`-only
// (`non_exhaustive`, not `Ord`). Same rationale as the Python corpus
// test.

use code_graph_core::{Edge, EdgeKind, FileGraph, Symbol, SymbolKind};
use code_graph_lang::LanguagePlugin;
use code_graph_lang_csharp::CSharpParser;
use pretty_assertions::assert_eq;

/// Resolve the absolute path of `testdata/csharp` from this crate's
/// manifest directory. Two `..` segments back up out of
/// `crates/code-graph-lang-csharp/` to the workspace root.
fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("testdata")
        .join("csharp")
}

/// Recursively discover every `.cs` file under the corpus root, in
/// deterministic (sorted) order. Walks subdirectories so the
/// `edge_cases/` fixtures are picked up.
fn discover_corpus_files() -> Vec<PathBuf> {
    let root = corpus_root();
    let mut out = Vec::new();
    walk_collect_csharp(&root, &mut out);
    out.sort();
    out
}

fn walk_collect_csharp(dir: &Path, out: &mut Vec<PathBuf>) {
    let rd = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}"));
    for entry in rd {
        let entry = entry.unwrap_or_else(|e| panic!("read_dir entry: {e}"));
        let p = entry.path();
        let ft = entry.file_type().expect("file_type");
        if ft.is_dir() {
            walk_collect_csharp(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("cs") {
            out.push(p);
        }
    }
}

/// Parse a single file via [`CSharpParser`]. Reads bytes from disk;
/// failures surface as panics with the path so the calling test
/// localizes them.
fn parse_file(parser: &CSharpParser, path: &Path) -> FileGraph {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    parser
        .parse_file(path, &bytes)
        .unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

/// Build a per-file map keyed by basename for the per-file assertions.
/// Every file in the corpus has a unique basename (top-level `.cs`
/// files plus the `edge_cases/*.cs` fixtures).
fn parse_corpus(parser: &CSharpParser) -> BTreeMap<String, FileGraph> {
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

/// Tally symbols by kind across a slice of `Symbol`.
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
/// fixture changes, update both this constant and
/// `testdata/csharp/MANIFEST.md` in the same commit.
const TOTAL_SYMBOLS: usize = 41;
const TOTAL_EDGES: usize = 22;

#[test]
fn corpus_aggregate_counts_match_manifest() {
    let parser = CSharpParser::new().expect("CSharpParser::new");
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
        1,
        "MANIFEST claims 1 Function (the default interface method \
         IGreeter::Greet — Decision 11)"
    );
    assert_eq!(
        by_kind.get(&SymbolKind::Method).copied().unwrap_or(0),
        19,
        "MANIFEST claims 19 Methods"
    );
    assert_eq!(
        by_kind.get(&SymbolKind::Class).copied().unwrap_or(0),
        18,
        "MANIFEST claims 18 Classes"
    );
    assert_eq!(
        by_kind.get(&SymbolKind::Interface).copied().unwrap_or(0),
        3,
        "MANIFEST claims 3 Interfaces"
    );

    let edge_by_kind = count_edges_by_kind(&all_edges);
    assert_eq!(
        edge_by_kind.get(&EdgeKind::Calls).copied().unwrap_or(0),
        10,
        "MANIFEST claims 10 Calls edges"
    );
    assert_eq!(
        edge_by_kind.get(&EdgeKind::Includes).copied().unwrap_or(0),
        7,
        "MANIFEST claims 7 Includes edges"
    );
    assert_eq!(
        edge_by_kind.get(&EdgeKind::Inherits).copied().unwrap_or(0),
        5,
        "MANIFEST claims 5 Inherits edges (Beta->Alpha, Gamma->Alpha, \
         Gamma->IMixin, Service->IService, Box<T>->BoxBase<T>)"
    );
}

#[test]
fn corpus_per_file_counts_match_manifest() {
    let parser = CSharpParser::new().expect("CSharpParser::new");
    let corpus = parse_corpus(&parser);

    // Expected (symbols, edges) per fixture file basename. Mirrors the
    // per-file tables in the MANIFEST.
    let expected: &[(&str, usize, usize)] = &[
        ("Program.cs", 3, 7),
        ("Models.cs", 14, 9),
        ("Handlers.cs", 9, 6),
        ("empty.cs", 0, 0),
        ("comments_only.cs", 0, 0),
        ("broken.cs", 4, 0),
        ("nested_classes.cs", 3, 0),
        ("partial_class_a.cs", 2, 0),
        ("partial_class_b.cs", 2, 0),
        ("method_name_collides_with_free_function.cs", 4, 0),
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

// ---- Edge-case fixtures ----------------------------------------------

#[test]
fn empty_file_zero_bytes_produces_zero_symbols_and_zero_edges() {
    // Anti-regression: parser must not panic on a 0-byte input.
    let parser = CSharpParser::new().expect("CSharpParser::new");
    let corpus = parse_corpus(&parser);
    let empty = corpus.get("empty.cs").expect("empty.cs in corpus");
    assert!(
        empty.symbols.is_empty(),
        "empty.cs must have 0 symbols; got: {:?}",
        empty
            .symbols
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
    );
    assert!(empty.edges.is_empty(), "empty.cs must have 0 edges");
}

#[test]
fn comments_only_file_produces_zero_symbols_and_zero_edges() {
    let parser = CSharpParser::new().expect("CSharpParser::new");
    let corpus = parse_corpus(&parser);
    let comments = corpus
        .get("comments_only.cs")
        .expect("comments_only.cs in corpus");
    assert!(comments.symbols.is_empty());
    assert!(comments.edges.is_empty());
}

#[test]
fn broken_file_recovers_around_error_nodes_without_panic() {
    // The malformed `public void Bar(` (opening paren followed by `{`
    // rather than a parameter list) produces ERROR nodes in tree-
    // sitter's parse. The `Bar` method itself is dropped from the
    // recovered tree, but `Foo` (the enclosing class), `Good` (the
    // sibling method after Bar), and the subsequent `AlsoGood` class
    // with its `Run` method all extract. The recovered count is
    // **4**, NOT zero — mirrors the Phase 7 `broken.py` discovery
    // (tree-sitter recovers more than expected).
    let parser = CSharpParser::new().expect("CSharpParser::new");
    let corpus = parse_corpus(&parser);
    let broken = corpus.get("broken.cs").expect("broken.cs in corpus");
    let names: Vec<&str> = broken.symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"Foo"),
        "Foo class (enclosing the malformed method) must still \
         extract; got: {names:?}"
    );
    assert!(
        names.contains(&"Good"),
        "sibling `Good` method must still extract; got: {names:?}"
    );
    assert!(
        names.contains(&"AlsoGood"),
        "post-error `AlsoGood` class must still extract; got: {names:?}"
    );
    assert!(
        names.contains(&"Run"),
        "post-error `Run` method must still extract; got: {names:?}"
    );
    assert_eq!(
        broken.symbols.len(),
        4,
        "broken.cs must extract exactly 4 symbols (Foo, Good, AlsoGood, \
         Run) despite the malformed Bar method; got: {:?}",
        names
    );
}

#[test]
fn nested_classes_record_immediate_enclosing_class_as_parent() {
    // `Outer > Inner > Leaf` — Inner records `Outer` as parent (bare
    // name, NOT `Nested.Outer`); Leaf records `Inner` for the same
    // reason. This matches the C++/Rust/Python nested-class
    // convention.
    let parser = CSharpParser::new().expect("CSharpParser::new");
    let corpus = parse_corpus(&parser);
    let nested = corpus
        .get("nested_classes.cs")
        .expect("nested_classes.cs in corpus");

    let by_name: HashMap<&str, &Symbol> = nested
        .symbols
        .iter()
        .map(|s| (s.name.as_str(), s))
        .collect();

    let outer = by_name.get("Outer").expect("Outer must exist");
    assert_eq!(outer.kind, SymbolKind::Class);
    assert!(outer.parent.is_empty(), "top-level class has no parent");

    let inner = by_name.get("Inner").expect("Inner must exist");
    assert_eq!(inner.kind, SymbolKind::Class);
    assert_eq!(
        inner.parent, "Outer",
        "inner class's parent is the immediate enclosing class (bare \
         name), not a dotted path like `Nested.Outer`"
    );

    let leaf = by_name.get("Leaf").expect("Leaf must exist");
    assert_eq!(leaf.kind, SymbolKind::Method);
    assert_eq!(
        leaf.parent, "Inner",
        "leaf method's parent is the immediate enclosing class"
    );
}

#[test]
fn partial_classes_across_files_yield_two_class_symbols() {
    // Decision 3: two `partial class Foo` declarations across two
    // files produce TWO Class symbols both named `Foo`. The file
    // paths disambiguate them at query time. Methods inside each
    // partial carry the bare-name parent `Foo`; the merge-by-bare-
    // name contract in `Graph::class_hierarchy` handles the cross-
    // file lookup.
    let parser = CSharpParser::new().expect("CSharpParser::new");
    let corpus = parse_corpus(&parser);
    let a = corpus
        .get("partial_class_a.cs")
        .expect("partial_class_a.cs in corpus");
    let b = corpus
        .get("partial_class_b.cs")
        .expect("partial_class_b.cs in corpus");

    let a_foo = a
        .symbols
        .iter()
        .find(|s| s.name == "Foo" && s.kind == SymbolKind::Class)
        .expect("partial_class_a.cs must declare a Foo class");
    let b_foo = b
        .symbols
        .iter()
        .find(|s| s.name == "Foo" && s.kind == SymbolKind::Class)
        .expect("partial_class_b.cs must declare a Foo class");

    assert_ne!(
        a_foo.file, b_foo.file,
        "the two partial Foo declarations must carry distinct file paths"
    );

    // Methods inside each partial use the bare-name parent.
    let a_method = a
        .symbols
        .iter()
        .find(|s| s.name == "A" && s.kind == SymbolKind::Method)
        .expect("partial A method must extract");
    assert_eq!(
        a_method.parent, "Foo",
        "method inside partial class carries bare-name parent Foo"
    );

    let b_method = b
        .symbols
        .iter()
        .find(|s| s.name == "B" && s.kind == SymbolKind::Method)
        .expect("partial B method must extract");
    assert_eq!(b_method.parent, "Foo");
}

#[test]
fn method_name_collision_across_classes_produces_distinct_symbols() {
    // Two methods named `Foo` on different parent classes
    // (`Container::Foo` and `FreeFunctions::Foo`) must extract as
    // distinct symbols. The parent string disambiguates them; the
    // SymbolIndex's parent-aware keying prevents cross-talk.
    let parser = CSharpParser::new().expect("CSharpParser::new");
    let corpus = parse_corpus(&parser);
    let collide = corpus
        .get("method_name_collides_with_free_function.cs")
        .expect("method_name_collides_with_free_function.cs in corpus");

    let foos: Vec<&Symbol> = collide
        .symbols
        .iter()
        .filter(|s| s.name == "Foo" && s.kind == SymbolKind::Method)
        .collect();
    assert_eq!(
        foos.len(),
        2,
        "must have exactly 2 methods named Foo; got: {foos:?}"
    );
    let parents: Vec<&str> = foos.iter().map(|s| s.parent.as_str()).collect();
    assert!(
        parents.contains(&"Container"),
        "one Foo must have parent Container; got parents: {parents:?}"
    );
    assert!(
        parents.contains(&"FreeFunctions"),
        "the other Foo must have parent FreeFunctions; got parents: {parents:?}"
    );
}

// ---- Definition / call / import / inheritance forms ------------------

#[test]
fn default_interface_method_extracts_as_function_not_method() {
    // Decision 11 (C# follow-up): `interface I { void Foo() { ... } }`
    // — a default interface method (C# 8+) — extracts as `Function`
    // (no parent), NOT `Method`. The `Greet` symbol in
    // `Handlers.cs::IGreeter` is the corpus instance.
    let parser = CSharpParser::new().expect("CSharpParser::new");
    let corpus = parse_corpus(&parser);
    let handlers = corpus.get("Handlers.cs").expect("Handlers.cs in corpus");
    let greet = handlers
        .symbols
        .iter()
        .find(|s| s.name == "Greet")
        .expect("Greet (default interface method) must extract");
    assert_eq!(
        greet.kind,
        SymbolKind::Function,
        "default interface method must extract as Function, not Method"
    );
    assert!(
        greet.parent.is_empty(),
        "default interface method's parent is empty (matches Rust's \
         trait-default-method rule)"
    );
}

#[test]
fn abstract_interface_method_produces_no_symbol() {
    // `interface I { void Required(); }` — abstract method (no body)
    // — produces zero symbol records (forward-declaration rule,
    // mirroring the four shipped plugins). `IGreeter::Required` in
    // Handlers.cs is the corpus instance.
    let parser = CSharpParser::new().expect("CSharpParser::new");
    let corpus = parse_corpus(&parser);
    let handlers = corpus.get("Handlers.cs").expect("Handlers.cs in corpus");
    let required = handlers.symbols.iter().find(|s| s.name == "Required");
    assert!(
        required.is_none(),
        "abstract interface method (no body) must NOT produce a Symbol; \
         got: {required:?}"
    );
}

#[test]
fn record_extracts_as_class_with_methods_parented_to_record() {
    // Decision 6 analog for C#: `record User(string Name)` extracts
    // as Class; methods inside the record extract as Method with
    // parent = record name.
    let parser = CSharpParser::new().expect("CSharpParser::new");
    let corpus = parse_corpus(&parser);
    let handlers = corpus.get("Handlers.cs").expect("Handlers.cs in corpus");
    let user = handlers
        .symbols
        .iter()
        .find(|s| s.name == "User")
        .expect("User record must extract");
    assert_eq!(
        user.kind,
        SymbolKind::Class,
        "record extracts as Class (Decision 6 analog)"
    );
    let display = handlers
        .symbols
        .iter()
        .find(|s| s.name == "Display")
        .expect("User::Display method must extract");
    assert_eq!(display.kind, SymbolKind::Method);
    assert_eq!(
        display.parent, "User",
        "method inside record carries record-name parent"
    );
}

#[test]
fn extension_method_parent_is_syntactic_enclosing_class() {
    // Decision 5: `static class StringExt { static int CountWords(
    // this string s) {...} }` — `CountWords` extracts with parent
    // `StringExt` (syntactic enclosing class), NOT `string` (the
    // semantic extended type).
    let parser = CSharpParser::new().expect("CSharpParser::new");
    let corpus = parse_corpus(&parser);
    let handlers = corpus.get("Handlers.cs").expect("Handlers.cs in corpus");
    let count_words = handlers
        .symbols
        .iter()
        .find(|s| s.name == "CountWords")
        .expect("CountWords extension method must extract");
    assert_eq!(count_words.kind, SymbolKind::Method);
    assert_eq!(
        count_words.parent, "StringExt",
        "extension method's parent is the syntactic enclosing static \
         class, NOT the extended type (Decision 5)"
    );
}

#[test]
fn using_static_drops_static_modifier_from_path() {
    // Decision 7: `using static System.Math;` produces an Includes
    // edge with `to = "System.Math"` — the `static` modifier is
    // dropped from the recorded path.
    let parser = CSharpParser::new().expect("CSharpParser::new");
    let corpus = parse_corpus(&parser);
    let handlers = corpus.get("Handlers.cs").expect("Handlers.cs in corpus");
    let includes_to: Vec<&str> = handlers
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Includes)
        .map(|e| e.to.as_str())
        .collect();
    assert!(
        includes_to.contains(&"System.Math"),
        "using static System.Math; must produce Includes to=System.Math; \
         got: {includes_to:?}"
    );
}

#[test]
fn using_alias_drops_alias_name_keeps_target_path() {
    // Decision 7: `using StrList = System.Collections.Generic.List<string>;`
    // produces an Includes edge with `to =
    // "System.Collections.Generic.List<string>"` — the alias name
    // (`StrList`) is dropped; the target path is preserved verbatim
    // (including generic parameters).
    let parser = CSharpParser::new().expect("CSharpParser::new");
    let corpus = parse_corpus(&parser);
    let handlers = corpus.get("Handlers.cs").expect("Handlers.cs in corpus");
    let includes_to: Vec<&str> = handlers
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Includes)
        .map(|e| e.to.as_str())
        .collect();
    assert!(
        includes_to.contains(&"System.Collections.Generic.List<string>"),
        "using alias must record the target path verbatim; got: \
         {includes_to:?}"
    );
    assert!(
        !includes_to.contains(&"StrList"),
        "alias name must NOT appear as an Includes target; got: \
         {includes_to:?}"
    );
}

#[test]
fn global_using_drops_global_modifier_from_path() {
    // Decision 7 (C# 10+): `global using System.Linq;` produces an
    // Includes edge with `to = "System.Linq"` — the `global`
    // modifier is dropped.
    let parser = CSharpParser::new().expect("CSharpParser::new");
    let corpus = parse_corpus(&parser);
    let handlers = corpus.get("Handlers.cs").expect("Handlers.cs in corpus");
    let includes_to: Vec<&str> = handlers
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Includes)
        .map(|e| e.to.as_str())
        .collect();
    assert!(
        includes_to.contains(&"System.Linq"),
        "global using System.Linq; must produce Includes to=System.Linq; \
         got: {includes_to:?}"
    );
}

#[test]
fn multiple_base_classes_produce_one_inherits_edge_per_base() {
    // `class Gamma : Alpha, IMixin` → 2 Inherits edges with
    // `from=Gamma`, `to=Alpha` and `to=IMixin`. Per Decision 2, both
    // class extension and interface implementation produce the same
    // `Inherits` edge kind.
    let parser = CSharpParser::new().expect("CSharpParser::new");
    let corpus = parse_corpus(&parser);
    let models = corpus.get("Models.cs").expect("Models.cs in corpus");
    let gamma_edges: Vec<&Edge> = models
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Inherits && e.from == "Gamma")
        .collect();
    assert_eq!(
        gamma_edges.len(),
        2,
        "Gamma must produce 2 Inherits edges (one per base); got: {:?}",
        gamma_edges
    );
    let tos: Vec<&str> = gamma_edges.iter().map(|e| e.to.as_str()).collect();
    assert!(tos.contains(&"Alpha"));
    assert!(tos.contains(&"IMixin"));
}

#[test]
fn generic_inheritance_preserves_generic_parameter_text_verbatim() {
    // Decision 9 (Rust precedent): `class Box<T> : BoxBase<T>`
    // produces one Inherits edge with `from = "Box<T>"`, `to =
    // "BoxBase<T>"` — both endpoints preserve the generic parameter
    // text verbatim.
    let parser = CSharpParser::new().expect("CSharpParser::new");
    let corpus = parse_corpus(&parser);
    let models = corpus.get("Models.cs").expect("Models.cs in corpus");
    let box_edge = models
        .edges
        .iter()
        .find(|e| e.kind == EdgeKind::Inherits && e.from == "Box<T>")
        .expect("Box<T> Inherits edge must exist with generic in `from`");
    assert_eq!(
        box_edge.to, "BoxBase<T>",
        "generic parameter must survive verbatim in the `to` field"
    );
}

#[test]
fn interface_implementation_produces_inherits_edge_same_kind_as_class_extension() {
    // Decision 2: `class Service : IService` produces an Inherits
    // edge with EdgeKind::Inherits — the SAME kind as class
    // extension. No separate `Implements` edge.
    let parser = CSharpParser::new().expect("CSharpParser::new");
    let corpus = parse_corpus(&parser);
    let models = corpus.get("Models.cs").expect("Models.cs in corpus");
    let service_edge = models
        .edges
        .iter()
        .find(|e| e.kind == EdgeKind::Inherits && e.from == "Service")
        .expect("Service interface-impl Inherits edge must exist");
    assert_eq!(service_edge.to, "IService");
}

#[test]
fn constructor_call_records_class_name_as_to_field() {
    // `new Service()` in Program::Main → records the type name as
    // the call target (same rule Python uses for `MyClass()`).
    let parser = CSharpParser::new().expect("CSharpParser::new");
    let corpus = parse_corpus(&parser);
    let program = corpus.get("Program.cs").expect("Program.cs in corpus");
    let main_calls: Vec<&str> = program
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls && e.from.ends_with(":Program::Main"))
        .map(|e| e.to.as_str())
        .collect();
    assert!(
        main_calls.contains(&"Service"),
        "Program::Main must record the constructor call as to=Service; \
         got: {main_calls:?}"
    );
}

#[test]
fn no_inherits_edges_for_classes_without_bases() {
    // `Alpha` and `BoxBase` declare no bases. Zero Inherits edges
    // from either of them.
    let parser = CSharpParser::new().expect("CSharpParser::new");
    let corpus = parse_corpus(&parser);
    let models = corpus.get("Models.cs").expect("Models.cs in corpus");
    for from in ["Alpha", "BoxBase"] {
        let edges: Vec<&Edge> = models
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Inherits && e.from == from)
            .collect();
        assert!(
            edges.is_empty(),
            "{from} has no bases; expected zero Inherits edges, got: \
             {edges:?}"
        );
    }
}

#[test]
fn namespace_is_populated_for_declarations_inside_namespace_block() {
    // Every declaration inside `namespace Models { ... }` carries
    // `Symbol.namespace = "Models"`. Top-level (no namespace block)
    // declarations have empty namespace.
    let parser = CSharpParser::new().expect("CSharpParser::new");
    let corpus = parse_corpus(&parser);
    let models = corpus.get("Models.cs").expect("Models.cs in corpus");
    for s in &models.symbols {
        assert_eq!(
            s.namespace, "Models",
            "every Models.cs declaration must carry namespace=Models; \
             {s:?}"
        );
    }
}

/// Dogfood-baseline regression test against the `external/efcore`
/// submodule (pinned to v8.0.25). Runs `parse_file` over every `.cs`
/// file under `external/efcore/src/EFCore` and asserts the symbol
/// count stays within ±10% of the baseline recorded in
/// `testdata/csharp/efcore-baseline.txt`.
///
/// **Auto-skips when the submodule is not initialized.** When
/// `external/efcore` is empty (the submodule has not been cloned)
/// this test prints a setup hint via `eprintln!` and returns — it
/// does NOT panic. Run `git submodule update --init external/efcore`
/// (or `make submodules`) to opt in.
///
/// When the pinned submodule SHA is bumped, the symbol count will
/// usually drift. Re-measure and update `efcore-baseline.txt` in the
/// same commit as the SHA bump.
#[test]
fn efcore_dogfood_baseline_within_ten_percent() {
    let efcore_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("external")
        .join("efcore")
        .join("src")
        .join("EFCore");
    if !efcore_root.is_dir() {
        eprintln!(
            "skipping efcore dogfood baseline test: \
             external/efcore/src/EFCore not present — run `git \
             submodule update --init external/efcore` (or `make \
             submodules`) to opt in"
        );
        return;
    }

    let baseline_path = corpus_root().join("efcore-baseline.txt");
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

    let parser = CSharpParser::new().expect("CSharpParser::new");
    let mut files = Vec::new();
    walk_collect_csharp(&efcore_root, &mut files);
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
        "efcore parse produced {total_symbols} symbols; expected within \
         ±10% of baseline {baseline_count} (range [{lower}, {upper}])"
    );
}
