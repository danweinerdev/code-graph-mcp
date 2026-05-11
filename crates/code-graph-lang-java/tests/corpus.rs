//! Phase 3.6 corpus regression test for the Java parser.
//!
//! Walks every `.java` file under `testdata/java/`, parses each via
//! [`JavaParser`], aggregates symbol/edge totals, and asserts the
//! aggregates match what `testdata/java/MANIFEST.md` documents. The
//! MANIFEST is the regression contract — every count in this test must
//! line up with what the manifest claims.
//!
//! Per-file breakdowns are also asserted so a regression is localized to
//! a single fixture rather than reporting only the global total drift.
//!
//! Edge-case coverage (per Phase 3.6 verification):
//!   - empty class body → 1 Class symbol, 0 method symbols, no panic
//!     (Java requires at least a class declaration for the file to
//!     parse — there is no zero-symbol equivalent to C#'s 0-byte
//!     `empty.cs`)
//!   - comments-only-around-shell file → same (the class shell extracts
//!     as 1 Class symbol with 0 methods)
//!   - syntax-error file → parser skips ERROR nodes gracefully — the
//!     recovered-symbol count is pinned at the **run-and-record** value
//!     of `5` (NOT zero — tree-sitter-java 0.23.5 recovers `bar` as a
//!     method despite the malformed parameter list, plus the enclosing
//!     `Broken` class, the sibling `good` method, and the post-error
//!     `AlsoGood` class with its `run` method). Mirrors the Phase 7
//!     `broken.py` discovery
//!   - 2-level nested classes → immediate-parent contract
//!   - anonymous-class-inside-method → Decision 4 collision: two anon
//!     `run()` methods produce two `AnonymousInside::run` symbols with
//!     identical IDs, disambiguated only by `Symbol.line`
//!   - record + methods → Decision 6: record-as-Class, methods inside
//!     the record body parent to the record name (no orphan Function
//!     leak — the C# 2.2 records-leak bug analog)
//!   - enum with abstract + per-constant method bodies → Decision 12:
//!     all extracted methods parent to the enum type (NOT a synthetic
//!     `Planet$EARTH`); enum-level abstract method filtered as forward
//!     declaration
//!   - method references → Phase 3.3 documented limitation: `Type::new`
//!     constructor references produce zero call edges
//!
//! The dogfood-baseline regression test
//! (`commons_lang_dogfood_baseline_within_ten_percent`) lives at the
//! bottom and auto-skips when the `external/commons-lang` git submodule
//! is not initialized.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

// `BTreeMap` for deterministic per-file iteration; `HashMap` for the
// kind-keyed tallies because `SymbolKind`/`EdgeKind` are `Hash`-only
// (`non_exhaustive`, not `Ord`). Same rationale as the C# / Python
// corpus tests.

use code_graph_core::{Edge, EdgeKind, FileGraph, Symbol, SymbolKind};
use code_graph_lang::LanguagePlugin;
use code_graph_lang_java::JavaParser;
use pretty_assertions::assert_eq;

/// Resolve the absolute path of `testdata/java` from this crate's
/// manifest directory. Two `..` segments back up out of
/// `crates/code-graph-lang-java/` to the workspace root.
fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("testdata")
        .join("java")
}

/// Recursively discover every `.java` file under the corpus root, in
/// deterministic (sorted) order. Walks subdirectories so the
/// `edge_cases/` fixtures are picked up.
fn discover_corpus_files() -> Vec<PathBuf> {
    let root = corpus_root();
    let mut out = Vec::new();
    walk_collect_java(&root, &mut out);
    out.sort();
    out
}

fn walk_collect_java(dir: &Path, out: &mut Vec<PathBuf>) {
    let rd = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}"));
    for entry in rd {
        let entry = entry.unwrap_or_else(|e| panic!("read_dir entry: {e}"));
        let p = entry.path();
        let ft = entry.file_type().expect("file_type");
        if ft.is_dir() {
            walk_collect_java(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("java") {
            out.push(p);
        }
    }
}

/// Parse a single file via [`JavaParser`]. Reads bytes from disk;
/// failures surface as panics with the path so the calling test
/// localizes them.
fn parse_file(parser: &JavaParser, path: &Path) -> FileGraph {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    parser
        .parse_file(path, &bytes)
        .unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

/// Build a per-file map keyed by basename for the per-file assertions.
/// Every file in the corpus has a unique basename (top-level `.java`
/// files plus the `edge_cases/*.java` fixtures).
fn parse_corpus(parser: &JavaParser) -> BTreeMap<String, FileGraph> {
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
/// `testdata/java/MANIFEST.md` in the same commit.
const TOTAL_SYMBOLS: usize = 57;
const TOTAL_EDGES: usize = 46;

#[test]
fn corpus_aggregate_counts_match_manifest() {
    let parser = JavaParser::new().expect("JavaParser::new");
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
        3,
        "MANIFEST claims 3 Functions (default interface methods — \
         Decision 11: IGreeter::greet, IGreeter::banner, IExtended::doBoth)"
    );
    assert_eq!(
        by_kind.get(&SymbolKind::Method).copied().unwrap_or(0),
        25,
        "MANIFEST claims 25 Methods"
    );
    assert_eq!(
        by_kind.get(&SymbolKind::Class).copied().unwrap_or(0),
        23,
        "MANIFEST claims 23 Classes (records count as Class per Decision 6)"
    );
    assert_eq!(
        by_kind.get(&SymbolKind::Interface).copied().unwrap_or(0),
        5,
        "MANIFEST claims 5 Interfaces (IMixin, IService, IExtended, \
         IGreeter, Shape — sealed counts as Interface)"
    );
    assert_eq!(
        by_kind.get(&SymbolKind::Enum).copied().unwrap_or(0),
        1,
        "MANIFEST claims 1 Enum (EnumWithMethods)"
    );

    let edge_by_kind = count_edges_by_kind(&all_edges);
    assert_eq!(
        edge_by_kind.get(&EdgeKind::Calls).copied().unwrap_or(0),
        29,
        "MANIFEST claims 29 Calls edges"
    );
    assert_eq!(
        edge_by_kind.get(&EdgeKind::Includes).copied().unwrap_or(0),
        8,
        "MANIFEST claims 8 Includes edges"
    );
    assert_eq!(
        edge_by_kind.get(&EdgeKind::Inherits).copied().unwrap_or(0),
        9,
        "MANIFEST claims 9 Inherits edges (IExtended->IMixin, \
         IExtended->IService, Beta->Alpha, Gamma->Alpha, Gamma->IMixin, \
         Service->IService, Box<T>->BoxBase<T>, Circle->Shape, \
         Square->Shape)"
    );
}

#[test]
fn corpus_per_file_counts_match_manifest() {
    let parser = JavaParser::new().expect("JavaParser::new");
    let corpus = parse_corpus(&parser);

    // Expected (symbols, edges) per fixture file basename. Mirrors the
    // per-file tables in the MANIFEST.
    let expected: &[(&str, usize, usize)] = &[
        ("Program.java", 3, 10),
        ("Models.java", 17, 14),
        ("Handlers.java", 12, 6),
        ("Empty.java", 1, 0),
        ("CommentsOnly.java", 1, 0),
        ("Broken.java", 5, 0),
        ("NestedClasses.java", 4, 0),
        ("AnonymousInside.java", 4, 6),
        ("Records.java", 3, 0),
        ("EnumWithMethods.java", 4, 2),
        ("MethodReferences.java", 3, 8),
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
fn empty_class_body_produces_one_class_symbol_zero_methods() {
    // Anti-regression: parser must not panic on a class with an empty
    // body. Unlike C#'s 0-byte empty.cs (which produces 0 symbols),
    // Java requires at least a top-level class for the file to parse
    // cleanly, so Empty.java yields exactly 1 Class symbol.
    let parser = JavaParser::new().expect("JavaParser::new");
    let corpus = parse_corpus(&parser);
    let empty = corpus.get("Empty.java").expect("Empty.java in corpus");
    assert_eq!(
        empty.symbols.len(),
        1,
        "Empty.java must have exactly 1 Class symbol; got: {:?}",
        empty
            .symbols
            .iter()
            .map(|s| (s.name.as_str(), s.kind))
            .collect::<Vec<_>>()
    );
    assert_eq!(empty.symbols[0].name, "Empty");
    assert_eq!(empty.symbols[0].kind, SymbolKind::Class);
    assert!(empty.edges.is_empty(), "Empty.java must have 0 edges");
}

#[test]
fn comments_only_file_produces_only_the_class_shell() {
    // The class shell extracts (1 Class symbol); the surrounding and
    // embedded comments produce no methods/fields.
    let parser = JavaParser::new().expect("JavaParser::new");
    let corpus = parse_corpus(&parser);
    let comments = corpus
        .get("CommentsOnly.java")
        .expect("CommentsOnly.java in corpus");
    assert_eq!(
        comments.symbols.len(),
        1,
        "CommentsOnly.java must have exactly 1 Class symbol; got: {:?}",
        comments
            .symbols
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(comments.symbols[0].name, "CommentsOnly");
    assert_eq!(comments.symbols[0].kind, SymbolKind::Class);
    assert!(comments.edges.is_empty());
}

#[test]
fn broken_file_recovers_around_error_nodes_without_panic() {
    // The malformed `public void bar(` (opening paren followed by `{`
    // rather than a parameter list) produces ERROR nodes in tree-
    // sitter's parse. Unlike the C# analog, tree-sitter-java 0.23.5
    // STILL extracts the malformed `bar` method (the recovery is
    // aggressive enough that the `method_declaration` node matches
    // the definition query). The other symbols all extract cleanly.
    // The recovered count is **5**, NOT zero — mirrors the Phase 7
    // `broken.py` discovery (tree-sitter recovers more than expected).
    let parser = JavaParser::new().expect("JavaParser::new");
    let corpus = parse_corpus(&parser);
    let broken = corpus.get("Broken.java").expect("Broken.java in corpus");
    let names: Vec<&str> = broken.symbols.iter().map(|s| s.name.as_str()).collect();
    for want in ["Broken", "good", "AlsoGood", "run"] {
        assert!(
            names.contains(&want),
            "{want} must still extract from Broken.java; got: {names:?}"
        );
    }
    assert_eq!(
        broken.symbols.len(),
        5,
        "Broken.java must extract exactly 5 symbols (Broken, bar, good, \
         AlsoGood, run) — tree-sitter-java 0.23.5 recovers the malformed \
         `bar` method despite the syntax error; got: {names:?}"
    );
}

#[test]
fn nested_classes_record_immediate_enclosing_class_as_parent() {
    // `NestedClasses > Outer > Inner > leaf` — Outer records the
    // top-level `NestedClasses` as parent; Inner records `Outer` (bare
    // name, NOT a dotted path); leaf records `Inner`. Mirrors the
    // C++/Rust/Python/C# nested-class convention.
    let parser = JavaParser::new().expect("JavaParser::new");
    let corpus = parse_corpus(&parser);
    let nested = corpus
        .get("NestedClasses.java")
        .expect("NestedClasses.java in corpus");

    let by_name: HashMap<&str, &Symbol> = nested
        .symbols
        .iter()
        .map(|s| (s.name.as_str(), s))
        .collect();

    let outer = by_name.get("Outer").expect("Outer must exist");
    assert_eq!(outer.kind, SymbolKind::Class);
    assert_eq!(
        outer.parent, "NestedClasses",
        "Outer's parent is the top-level class"
    );

    let inner = by_name.get("Inner").expect("Inner must exist");
    assert_eq!(inner.kind, SymbolKind::Class);
    assert_eq!(
        inner.parent, "Outer",
        "Inner's parent is the immediate enclosing class (bare name), \
         NOT a dotted path like `NestedClasses.Outer`"
    );

    let leaf = by_name.get("leaf").expect("leaf must exist");
    assert_eq!(leaf.kind, SymbolKind::Method);
    assert_eq!(leaf.parent, "Inner");
}

#[test]
fn anonymous_class_collision_yields_two_run_symbols_same_id() {
    // Decision 4: two anonymous classes inside the same enclosing
    // method that BOTH define `run()` produce two `AnonymousInside::run`
    // symbols with identical symbol IDs, disambiguated only by line.
    // This is the load-bearing Decision 4 anti-regression — anonymous
    // classes emit NO Class symbol, so the methods take the outer
    // named entity's parent.
    let parser = JavaParser::new().expect("JavaParser::new");
    let corpus = parse_corpus(&parser);
    let anon = corpus
        .get("AnonymousInside.java")
        .expect("AnonymousInside.java in corpus");

    // No Class symbol named `Runnable`, `Anonymous`, `Anonymous$1`, or
    // any synthetic name — anonymous classes are invisible to the
    // symbol graph.
    for forbidden in ["Runnable", "Anonymous", "Anonymous$1", "Anonymous$2"] {
        assert!(
            !anon
                .symbols
                .iter()
                .any(|s| s.name == forbidden && s.kind == SymbolKind::Class),
            "Decision 4: no synthetic Class symbol for the anonymous \
             body; forbidden name {forbidden:?} appeared"
        );
    }

    let runs: Vec<&Symbol> = anon
        .symbols
        .iter()
        .filter(|s| s.name == "run" && s.kind == SymbolKind::Method)
        .collect();
    assert_eq!(
        runs.len(),
        2,
        "two anonymous `run()` methods must both produce Method symbols; \
         got: {runs:?}"
    );

    for r in &runs {
        assert_eq!(
            r.parent, "AnonymousInside",
            "Decision 4: anonymous methods take the outer NAMED entity's \
             parent (`AnonymousInside`), NOT a synthetic anonymous parent"
        );
    }

    // The two `run` symbols' IDs collide; only `Symbol.line`
    // disambiguates them. This is the documented limitation per the
    // crate docstring.
    assert_ne!(
        runs[0].line, runs[1].line,
        "the two anonymous `run` methods must live on different lines; \
         otherwise the collision documentation is misleading"
    );
}

#[test]
fn record_extracts_as_class_with_methods_parented_to_record() {
    // Decision 6: `record Records(String name, int age)` extracts as
    // Class — NOT a new SymbolKind::Record. Methods inside the record
    // body extract as Method with parent = `Records` (NOT as orphan
    // Function symbols — the C# 2.2 records-leak bug analog).
    let parser = JavaParser::new().expect("JavaParser::new");
    let corpus = parse_corpus(&parser);
    let records = corpus.get("Records.java").expect("Records.java in corpus");

    let by_name: HashMap<&str, &Symbol> = records
        .symbols
        .iter()
        .map(|s| (s.name.as_str(), s))
        .collect();

    let rec = by_name.get("Records").expect("Records record must extract");
    assert_eq!(
        rec.kind,
        SymbolKind::Class,
        "record extracts as Class (Decision 6 — SymbolKind::Record is \
         intentionally not added)"
    );

    for method_name in ["greeting", "nextAge"] {
        let m = by_name
            .get(method_name)
            .unwrap_or_else(|| panic!("record body method {method_name} must extract"));
        assert_eq!(
            m.kind,
            SymbolKind::Method,
            "record body methods must extract as Method, NOT orphan \
             Function (records-leak anti-regression)"
        );
        assert_eq!(
            m.parent, "Records",
            "record body methods' parent is the record name"
        );
    }

    // Record components (name, age) MUST NOT produce method/function
    // symbols — they parse as `formal_parameter`, not
    // `method_declaration`, so the definition query correctly skips
    // them. Auto-generated accessor methods (`name()`, `age()`) are
    // ALSO not extracted because they don't appear in source.
    for forbidden in ["name", "age"] {
        assert!(
            !records.symbols.iter().any(|s| s.name == forbidden),
            "record component {forbidden:?} must not surface as a symbol; \
             record components are formal_parameter nodes, not \
             method_declaration"
        );
    }
}

#[test]
fn enum_with_methods_records_enum_type_as_parent_for_all_methods() {
    // Decision 12: enum-level methods AND per-constant method bodies
    // all extract as Method with parent = enum type. There is NO
    // synthetic `EnumWithMethods$EARTH` parent. Enum-level abstract
    // methods (no body) are filtered as forward declarations. Enum
    // constants themselves (EARTH, MARS) are NOT extracted as symbols.
    let parser = JavaParser::new().expect("JavaParser::new");
    let corpus = parse_corpus(&parser);
    let planet = corpus
        .get("EnumWithMethods.java")
        .expect("EnumWithMethods.java in corpus");

    // The enum type itself is the only Enum symbol.
    let enums: Vec<&Symbol> = planet
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Enum)
        .collect();
    assert_eq!(
        enums.len(),
        1,
        "exactly one Enum symbol expected; got: {enums:?}"
    );
    assert_eq!(enums[0].name, "EnumWithMethods");

    // Two `surfaceGravity` per-constant method bodies (EARTH + MARS)
    // both extract, with parent = `EnumWithMethods`. The enum-level
    // abstract `surfaceGravity()` declaration (no body) is filtered as
    // a forward declaration and does NOT produce a third symbol.
    let surfaces: Vec<&Symbol> = planet
        .symbols
        .iter()
        .filter(|s| s.name == "surfaceGravity")
        .collect();
    assert_eq!(
        surfaces.len(),
        2,
        "two per-constant `surfaceGravity` bodies expected (EARTH + MARS); \
         enum-level abstract `surfaceGravity()` must NOT add a third; \
         got: {surfaces:?}"
    );
    for s in &surfaces {
        assert_eq!(s.kind, SymbolKind::Method);
        assert_eq!(
            s.parent, "EnumWithMethods",
            "per-constant method parent must be the enum type, NOT a \
             synthetic `EnumWithMethods$EARTH` parent"
        );
    }

    // The enum-level `describe()` method (with body) extracts with
    // parent = enum type.
    let describe = planet
        .symbols
        .iter()
        .find(|s| s.name == "describe")
        .expect("enum-level concrete `describe` must extract");
    assert_eq!(describe.kind, SymbolKind::Method);
    assert_eq!(describe.parent, "EnumWithMethods");

    // Enum constants themselves are NOT symbols (Decision 12).
    for forbidden in ["EARTH", "MARS"] {
        assert!(
            !planet.symbols.iter().any(|s| s.name == forbidden),
            "enum constant {forbidden:?} must not surface as a symbol"
        );
    }
}

#[test]
fn method_references_record_identifier_rhs_only_constructor_ref_is_limitation() {
    // Phase 3.3 documented limitation: method references with an
    // identifier on the right-hand side (`String::length`, `this::len`)
    // produce Calls edges to the bare RHS name. Constructor references
    // (`Type::new`) are NOT matched by the query — they produce zero
    // Calls edges, regardless of source form.
    let parser = JavaParser::new().expect("JavaParser::new");
    let corpus = parse_corpus(&parser);
    let refs = corpus
        .get("MethodReferences.java")
        .expect("MethodReferences.java in corpus");

    let call_targets: Vec<&str> = refs
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls)
        .map(|e| e.to.as_str())
        .collect();

    // `String::length` and `this::len` both fire — RHS recorded as
    // callee. The plain `length` callee also includes the body call
    // `s.length()` inside `len()`, so `length` appears more than once.
    assert!(
        call_targets.contains(&"length"),
        "String::length method reference must produce Calls edge to \
         `length`; got: {call_targets:?}"
    );
    assert!(
        call_targets.contains(&"len"),
        "this::len method reference must produce Calls edge to `len`; \
         got: {call_targets:?}"
    );

    // `MethodReferences::new` is a constructor reference — the
    // documented limitation. NO Calls edge with `to = "new"` is
    // produced. The lambda's enclosing `run` method's call edges
    // (`apply`, `get`) come from the subsequent `a.apply(...)` and
    // `c.get()` lines, NOT from the `::new` reference itself.
    assert!(
        !call_targets.contains(&"new"),
        "constructor reference `MethodReferences::new` MUST NOT produce \
         a Calls edge (Phase 3.3 documented limitation); got: \
         {call_targets:?}"
    );
}

// ---- Definition / call / import / inheritance forms ------------------

#[test]
fn default_interface_method_extracts_as_function_not_method() {
    // Decision 11: `interface IGreeter { default void greet() { ... } }`
    // — a default interface method — extracts as `Function` (no
    // parent), NOT `Method`. Matches the Rust trait-default-method
    // contract. The same rule covers `static` interface methods.
    let parser = JavaParser::new().expect("JavaParser::new");
    let corpus = parse_corpus(&parser);
    let handlers = corpus
        .get("Handlers.java")
        .expect("Handlers.java in corpus");
    let greet = handlers
        .symbols
        .iter()
        .find(|s| s.name == "greet")
        .expect("greet (default interface method) must extract");
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

    let banner = handlers
        .symbols
        .iter()
        .find(|s| s.name == "banner")
        .expect("banner (static interface method) must extract");
    assert_eq!(
        banner.kind,
        SymbolKind::Function,
        "static interface method must extract as Function, not Method"
    );
    assert!(banner.parent.is_empty());
}

#[test]
fn abstract_interface_method_produces_no_symbol() {
    // `interface IGreeter { void required(); }` — abstract method
    // (no body) — produces zero symbol records (forward-declaration
    // rule, mirroring the four shipped plugins). Pinned against the
    // `Handlers.java` fixture's `IGreeter::required`.
    let parser = JavaParser::new().expect("JavaParser::new");
    let corpus = parse_corpus(&parser);
    let handlers = corpus
        .get("Handlers.java")
        .expect("Handlers.java in corpus");
    let required = handlers.symbols.iter().find(|s| s.name == "required");
    assert!(
        required.is_none(),
        "abstract interface method (no body) must NOT produce a Symbol; \
         got: {required:?}"
    );
}

#[test]
fn sealed_interface_extracts_as_interface_permits_ignored() {
    // Decision 6: `sealed interface Shape permits Circle, Square`
    // extracts as ordinary Interface; the `permits` clause is ignored
    // (no Inherits edges from Shape are produced via the permits path).
    let parser = JavaParser::new().expect("JavaParser::new");
    let corpus = parse_corpus(&parser);
    let handlers = corpus
        .get("Handlers.java")
        .expect("Handlers.java in corpus");
    let shape = handlers
        .symbols
        .iter()
        .find(|s| s.name == "Shape")
        .expect("Shape (sealed interface) must extract");
    assert_eq!(
        shape.kind,
        SymbolKind::Interface,
        "sealed interface extracts as ordinary Interface"
    );

    // The permits clause MUST NOT produce edges from `Shape` itself —
    // the only Inherits edges involving Shape are FROM Circle and Square,
    // not FROM Shape pointing at Circle/Square.
    let shape_outgoing: Vec<&Edge> = handlers
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Inherits && e.from == "Shape")
        .collect();
    assert!(
        shape_outgoing.is_empty(),
        "Decision 6: sealed permits clause must NOT produce Inherits \
         edges FROM Shape; got: {shape_outgoing:?}"
    );
}

#[test]
fn static_import_drops_static_modifier_from_path() {
    // Decision 7: `import static java.lang.Math.max;` produces an
    // Includes edge with `to = "java.lang.Math.max"` — the `static`
    // modifier is dropped from the recorded path.
    let parser = JavaParser::new().expect("JavaParser::new");
    let corpus = parse_corpus(&parser);
    let handlers = corpus
        .get("Handlers.java")
        .expect("Handlers.java in corpus");
    let includes_to: Vec<&str> = handlers
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Includes)
        .map(|e| e.to.as_str())
        .collect();
    assert!(
        includes_to.contains(&"java.lang.Math.max"),
        "import static java.lang.Math.max; must produce Includes \
         to=java.lang.Math.max; got: {includes_to:?}"
    );
}

#[test]
fn wildcard_import_preserves_asterisk_verbatim() {
    // Decision 7: `import java.util.*;` produces an Includes edge with
    // `to = "java.util.*"` — the wildcard is preserved verbatim,
    // matching the Rust plugin's `use foo::*` rule.
    let parser = JavaParser::new().expect("JavaParser::new");
    let corpus = parse_corpus(&parser);
    let program = corpus.get("Program.java").expect("Program.java in corpus");
    let includes_to: Vec<&str> = program
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Includes)
        .map(|e| e.to.as_str())
        .collect();
    assert!(
        includes_to.contains(&"java.util.*"),
        "import java.util.*; must produce Includes to=java.util.*; \
         got: {includes_to:?}"
    );
}

#[test]
fn multiple_base_classes_produce_one_inherits_edge_per_base() {
    // `class Gamma extends Alpha implements IMixin` → 2 Inherits edges
    // with `from=Gamma`, `to=Alpha` and `to=IMixin`. Per Decision 2,
    // both `extends` (class extension) and `implements` (interface
    // implementation) produce the same `Inherits` edge kind.
    let parser = JavaParser::new().expect("JavaParser::new");
    let corpus = parse_corpus(&parser);
    let models = corpus.get("Models.java").expect("Models.java in corpus");
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
fn interface_extends_multiple_interfaces_produces_one_edge_per_base() {
    // `interface IExtended extends IMixin, IService` → 2 Inherits
    // edges, each `Inherits` kind. Interface-extends-interface flows
    // through `extends_interfaces` in the grammar (NOT `superclass`)
    // but produces the same edge kind per Decision 2.
    let parser = JavaParser::new().expect("JavaParser::new");
    let corpus = parse_corpus(&parser);
    let models = corpus.get("Models.java").expect("Models.java in corpus");
    let iext_edges: Vec<&Edge> = models
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Inherits && e.from == "IExtended")
        .collect();
    assert_eq!(
        iext_edges.len(),
        2,
        "IExtended must produce 2 Inherits edges; got: {:?}",
        iext_edges
    );
    let tos: Vec<&str> = iext_edges.iter().map(|e| e.to.as_str()).collect();
    assert!(tos.contains(&"IMixin"));
    assert!(tos.contains(&"IService"));
}

#[test]
fn generic_inheritance_preserves_generic_parameter_text_verbatim() {
    // Decision 9 (Rust precedent): `class Box<T> extends BoxBase<T>`
    // produces one Inherits edge with `from = "Box<T>"`,
    // `to = "BoxBase<T>"` — both endpoints preserve the generic
    // parameter text verbatim.
    let parser = JavaParser::new().expect("JavaParser::new");
    let corpus = parse_corpus(&parser);
    let models = corpus.get("Models.java").expect("Models.java in corpus");
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
    // Decision 2: `class Service implements IService` produces an
    // Inherits edge with EdgeKind::Inherits — the SAME kind as class
    // extension. No separate `Implements` edge.
    let parser = JavaParser::new().expect("JavaParser::new");
    let corpus = parse_corpus(&parser);
    let models = corpus.get("Models.java").expect("Models.java in corpus");
    let service_edge = models
        .edges
        .iter()
        .find(|e| e.kind == EdgeKind::Inherits && e.from == "Service")
        .expect("Service interface-impl Inherits edge must exist");
    assert_eq!(service_edge.to, "IService");
}

#[test]
fn constructor_call_records_class_name_as_to_field() {
    // `new ArrayList<>()` in Program::main → records the bare type
    // name as the call target (same rule the C# 2.6 / Rust / Python
    // plugins use).
    let parser = JavaParser::new().expect("JavaParser::new");
    let corpus = parse_corpus(&parser);
    let program = corpus.get("Program.java").expect("Program.java in corpus");
    let main_callees: Vec<&str> = program
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls && e.from.ends_with(":Program::main"))
        .map(|e| e.to.as_str())
        .collect();
    assert!(
        main_callees.contains(&"ArrayList"),
        "Program::main must record the constructor call as to=ArrayList; \
         got: {main_callees:?}"
    );
}

#[test]
fn no_inherits_edges_for_classes_without_bases() {
    // `Alpha` and `BoxBase` declare no bases. Zero Inherits edges
    // from either of them.
    let parser = JavaParser::new().expect("JavaParser::new");
    let corpus = parse_corpus(&parser);
    let models = corpus.get("Models.java").expect("Models.java in corpus");
    for from in ["Alpha", "BoxBase"] {
        let edges: Vec<&Edge> = models
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Inherits && e.from == from)
            .collect();
        assert!(
            edges.is_empty(),
            "{from} has no bases; expected zero Inherits edges, got: {edges:?}"
        );
    }
}

/// Dogfood-baseline regression test against the `external/commons-lang`
/// submodule. Walks every `.java` file under
/// `external/commons-lang/src/main/java` and asserts the symbol count
/// stays within ±10% of the baseline recorded in
/// `testdata/java/commons-lang-baseline.txt`.
///
/// **Auto-skips when the submodule is not initialized.** When
/// `external/commons-lang` is empty (the submodule has not been cloned)
/// this test prints a setup hint via `eprintln!` and returns — it does
/// NOT panic. Run `git submodule update --init external/commons-lang`
/// (or `make submodules`) to opt in.
///
/// When the pinned submodule SHA is bumped, the symbol count will
/// usually drift. Re-measure and update `commons-lang-baseline.txt` in
/// the same commit as the SHA bump.
#[test]
fn commons_lang_dogfood_baseline_within_ten_percent() {
    let commons_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("external")
        .join("commons-lang")
        .join("src")
        .join("main")
        .join("java");
    if !commons_root.is_dir() {
        eprintln!(
            "skipping commons-lang dogfood baseline test: \
             external/commons-lang/src/main/java not present — run \
             `git submodule update --init external/commons-lang` (or \
             `make submodules`) to opt in"
        );
        return;
    }

    let baseline_path = corpus_root().join("commons-lang-baseline.txt");
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

    let parser = JavaParser::new().expect("JavaParser::new");
    let mut files = Vec::new();
    walk_collect_java(&commons_root, &mut files);
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
        "commons-lang parse produced {total_symbols} symbols; expected \
         within ±10% of baseline {baseline_count} (range [{lower}, {upper}])"
    );
}
