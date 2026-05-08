//! Phase 7.6 corpus regression test for the Python parser.
//!
//! Walks every `.py` and `.pyi` file under `testdata/python/`, parses
//! each via [`PythonParser`], aggregates symbol/edge totals, and asserts
//! the aggregates match what `testdata/python/MANIFEST.md` documents.
//! The MANIFEST is the regression contract — every count in this test
//! must line up with what the manifest claims.
//!
//! Per-file breakdowns are also asserted so a regression is localized to
//! a single fixture rather than reporting only the global total drift.
//!
//! Edge-case coverage (per Phase 7.6 verification):
//!   - empty file (zero bytes) → 0 symbols, 0 edges, no panic
//!   - comments-only file → same
//!   - syntax-error file → parser skips ERROR nodes gracefully
//!   - deeply nested (4-level) classes → immediate-parent contract
//!   - method same name as free function in same module → no collision
//!   - `*args/**kwargs` signature preserved
//!   - generator function (no special kind, ordinary Function)
//!   - `@property` decorator (transparent — Method, no flag)
//!   - `.pyi` stub: function + class with method stubs extract identically
//!
//! The dogfood-baseline regression test (`requests_dogfood_baseline_within_ten_percent`)
//! lives at the bottom and auto-skips when the `external/requests` git
//! submodule is not initialized.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

// `BTreeMap` for deterministic per-file iteration; `HashMap` for the
// kind-keyed tallies because `SymbolKind`/`EdgeKind` are `Hash`-only
// (`non_exhaustive`, not `Ord`). Same rationale as the Go corpus test.

use code_graph_core::{Edge, EdgeKind, FileGraph, Symbol, SymbolKind};
use code_graph_lang::LanguagePlugin;
use code_graph_lang_python::PythonParser;
use pretty_assertions::assert_eq;

/// Resolve the absolute path of `testdata/python` from this crate's
/// manifest directory. Two `..` segments back up out of
/// `crates/code-graph-lang-python/` to the workspace root.
fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("testdata")
        .join("python")
}

/// Recursively discover every `.py` and `.pyi` file under the corpus
/// root, in deterministic (sorted) order. Walks subdirectories so the
/// `edge_cases/` fixtures are picked up.
fn discover_corpus_files() -> Vec<PathBuf> {
    let root = corpus_root();
    let mut out = Vec::new();
    walk_collect_python(&root, &mut out);
    out.sort();
    out
}

fn walk_collect_python(dir: &Path, out: &mut Vec<PathBuf>) {
    let rd = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}"));
    for entry in rd {
        let entry = entry.unwrap_or_else(|e| panic!("read_dir entry: {e}"));
        let p = entry.path();
        let ft = entry.file_type().expect("file_type");
        if ft.is_dir() {
            walk_collect_python(&p, out);
        } else {
            let ext = p.extension().and_then(|s| s.to_str());
            if matches!(ext, Some("py") | Some("pyi")) {
                out.push(p);
            }
        }
    }
}

/// Parse a single file via [`PythonParser`]. Reads bytes from disk;
/// failures surface as panics with the path so the calling test
/// localizes them.
fn parse_file(parser: &PythonParser, path: &Path) -> FileGraph {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    parser
        .parse_file(path, &bytes)
        .unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

/// Build a per-file map keyed by basename for the per-file assertions.
/// Every file in the corpus has a unique basename (the `__init__.py`,
/// `app.py`, `models.py`, ... at the root level + `empty.py`,
/// `comments_only.py`, `broken.py`, `nested.py`, `collide.py` under
/// `edge_cases/`). Basename keying still uniquely identifies each file.
fn parse_corpus(parser: &PythonParser) -> BTreeMap<String, FileGraph> {
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
/// fixture changes, update both this constant and
/// `testdata/python/MANIFEST.md` in the same commit.
const TOTAL_SYMBOLS: usize = 45;
const TOTAL_EDGES: usize = 27;

#[test]
fn corpus_aggregate_counts_match_manifest() {
    let parser = PythonParser::new().expect("PythonParser::new");
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
        13,
        "MANIFEST claims 13 Functions"
    );
    assert_eq!(
        by_kind.get(&SymbolKind::Method).copied().unwrap_or(0),
        18,
        "MANIFEST claims 18 Methods"
    );
    assert_eq!(
        by_kind.get(&SymbolKind::Class).copied().unwrap_or(0),
        14,
        "MANIFEST claims 14 Classes"
    );

    let edge_by_kind = count_edges_by_kind(&all_edges);
    assert_eq!(
        edge_by_kind.get(&EdgeKind::Calls).copied().unwrap_or(0),
        14,
        "MANIFEST claims 14 Calls edges"
    );
    assert_eq!(
        edge_by_kind.get(&EdgeKind::Includes).copied().unwrap_or(0),
        9,
        "MANIFEST claims 9 Includes edges"
    );
    assert_eq!(
        edge_by_kind.get(&EdgeKind::Inherits).copied().unwrap_or(0),
        4,
        "MANIFEST claims 4 Inherits edges (Beta->Alpha, Gamma->Alpha, \
         Gamma->Mixin, Delta->abc.ABC)"
    );
}

#[test]
fn corpus_per_file_counts_match_manifest() {
    let parser = PythonParser::new().expect("PythonParser::new");
    let corpus = parse_corpus(&parser);

    // Expected (symbols, edges) per fixture file basename. Mirrors the
    // per-file tables in the MANIFEST.
    let expected: &[(&str, usize, usize)] = &[
        ("__init__.py", 0, 2),
        ("app.py", 2, 8),
        ("models.py", 14, 10),
        ("handlers.py", 10, 6),
        ("utils.py", 3, 1),
        ("stubs.pyi", 6, 0),
        ("empty.py", 0, 0),
        ("comments_only.py", 0, 0),
        ("broken.py", 2, 0),
        ("nested.py", 5, 0),
        ("collide.py", 3, 0),
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
    let parser = PythonParser::new().expect("PythonParser::new");
    let corpus = parse_corpus(&parser);
    let empty = corpus.get("empty.py").expect("empty.py in corpus");
    assert!(
        empty.symbols.is_empty(),
        "empty.py must have 0 symbols; got: {:?}",
        empty
            .symbols
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
    );
    assert!(empty.edges.is_empty(), "empty.py must have 0 edges");
}

#[test]
fn comments_only_file_produces_zero_symbols_and_zero_edges() {
    let parser = PythonParser::new().expect("PythonParser::new");
    let corpus = parse_corpus(&parser);
    let comments = corpus
        .get("comments_only.py")
        .expect("comments_only.py in corpus");
    assert!(comments.symbols.is_empty());
    assert!(comments.edges.is_empty());
}

#[test]
fn broken_file_recovers_around_error_nodes_without_panic() {
    // `def foo(:` produces ERROR nodes in the parameter list. Tree-
    // sitter's error recovery still emits a `function_definition` for
    // foo, and the subsequent `def good():` parses cleanly. The parser
    // must not panic and must not abort the file mid-stream.
    let parser = PythonParser::new().expect("PythonParser::new");
    let corpus = parse_corpus(&parser);
    let broken = corpus.get("broken.py").expect("broken.py in corpus");
    let names: Vec<&str> = broken.symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"good"),
        "post-error `good` must still extract; got: {names:?}"
    );
    // `foo` is the load-bearing function with ERROR children — pre-fix
    // a regression that lost `foo` would still pass via `len == 2 + good`
    // if a third spurious symbol slipped in. Pinning `foo` by name closes
    // that gap so both halves of the recovery contract stay enforced.
    assert!(
        names.contains(&"foo"),
        "pre-error `foo` (the function with ERROR children) must still \
         extract; got: {names:?}"
    );
    // Both `foo` (with ERROR children) and `good` (clean) extract.
    assert_eq!(
        broken.symbols.len(),
        2,
        "broken.py must extract exactly 2 functions despite the syntax \
         error; got: {:?}",
        names
    );
    for s in &broken.symbols {
        assert_eq!(s.kind, SymbolKind::Function);
    }
}

#[test]
fn deeply_nested_classes_record_immediate_enclosing_class_as_parent() {
    // `Outer > Mid > Inner > Deepest > leaf` — each child records the
    // *immediate* enclosing class (bare name), NOT a dotted path. This
    // matches the C++/Rust nested-class convention.
    let parser = PythonParser::new().expect("PythonParser::new");
    let corpus = parse_corpus(&parser);
    let nested = corpus.get("nested.py").expect("nested.py in corpus");

    let by_name: HashMap<&str, &Symbol> = nested
        .symbols
        .iter()
        .map(|s| (s.name.as_str(), s))
        .collect();

    let outer = by_name.get("Outer").expect("Outer must exist");
    assert_eq!(outer.kind, SymbolKind::Class);
    assert!(outer.parent.is_empty(), "top-level class has no parent");

    let mid = by_name.get("Mid").expect("Mid must exist");
    assert_eq!(mid.kind, SymbolKind::Class);
    assert_eq!(mid.parent, "Outer");

    let inner = by_name.get("Inner").expect("Inner must exist");
    assert_eq!(inner.kind, SymbolKind::Class);
    assert_eq!(inner.parent, "Mid");

    let deepest = by_name.get("Deepest").expect("Deepest must exist");
    assert_eq!(deepest.kind, SymbolKind::Class);
    assert_eq!(deepest.parent, "Inner");

    let leaf = by_name.get("leaf").expect("leaf must exist");
    assert_eq!(leaf.kind, SymbolKind::Method);
    assert_eq!(
        leaf.parent, "Deepest",
        "leaf's parent is the immediate enclosing class, not a dotted path"
    );
}

#[test]
fn method_and_free_function_with_same_name_coexist_without_collision() {
    // `Adder::add` (method) and module-level `add` (function) must both
    // extract as distinct symbols. The parent disambiguates the symbol
    // ID; the SymbolIndex's parent-aware keying prevents any cross-talk.
    let parser = PythonParser::new().expect("PythonParser::new");
    let corpus = parse_corpus(&parser);
    let collide = corpus.get("collide.py").expect("collide.py in corpus");

    let method = collide
        .symbols
        .iter()
        .find(|s| s.name == "add" && s.kind == SymbolKind::Method)
        .expect("Adder::add must exist as Method");
    assert_eq!(method.parent, "Adder");

    let free = collide
        .symbols
        .iter()
        .find(|s| s.name == "add" && s.kind == SymbolKind::Function)
        .expect("free function add must exist as Function");
    assert!(free.parent.is_empty(), "free function has no class parent");
}

// ---- Definition / call / import / inheritance forms ------------------

#[test]
fn property_staticmethod_classmethod_are_methods_with_no_special_flag() {
    // Decorators are transparent for definition extraction. Service has
    // a @property (`value`), @staticmethod (`factory`), @classmethod
    // (`from_name`), and an async def method (`handle`). All four must
    // extract as Kind=Method with parent=Service. No flag distinguishes
    // them — the decorator is metadata, not a different kind.
    let parser = PythonParser::new().expect("PythonParser::new");
    let corpus = parse_corpus(&parser);
    let handlers = corpus.get("handlers.py").expect("handlers.py in corpus");

    for name in ["value", "factory", "from_name", "handle"] {
        let m = handlers
            .symbols
            .iter()
            .find(|s| s.name == name && s.kind == SymbolKind::Method)
            .unwrap_or_else(|| panic!("Service::{name} method must exist"));
        assert_eq!(
            m.parent, "Service",
            "Service::{name}: parent must be Service regardless of decorator"
        );
    }
}

#[test]
fn async_free_function_extracts_as_function_kind() {
    // `async def fetch(): ...` at module scope is Kind=Function with no
    // parent. Async-ness is not a separate kind.
    let parser = PythonParser::new().expect("PythonParser::new");
    let corpus = parse_corpus(&parser);
    let handlers = corpus.get("handlers.py").expect("handlers.py in corpus");
    let fetch = handlers
        .symbols
        .iter()
        .find(|s| s.name == "fetch")
        .expect("fetch must exist");
    assert_eq!(fetch.kind, SymbolKind::Function);
    assert!(fetch.parent.is_empty());
}

#[test]
fn closure_inside_factory_extracts_as_function_with_no_class_parent() {
    // `def make_handler(): def inner(): pass; return inner` — `inner` is
    // a nested function. It extracts as Function (no class parent — the
    // parser records only enclosing-class parents, not enclosing-fn).
    let parser = PythonParser::new().expect("PythonParser::new");
    let corpus = parse_corpus(&parser);
    let handlers = corpus.get("handlers.py").expect("handlers.py in corpus");
    let inner = handlers
        .symbols
        .iter()
        .find(|s| s.name == "inner")
        .expect("closure `inner` must extract as a symbol");
    assert_eq!(inner.kind, SymbolKind::Function);
    assert!(
        inner.parent.is_empty(),
        "inner has no class parent (its enclosing scope is a function)"
    );
}

#[test]
fn generator_function_extracts_as_function_no_special_kind() {
    // `def gen(): yield 1` — `yield` doesn't change the AST node kind;
    // it's still a `function_definition`. Extracted as Function.
    let parser = PythonParser::new().expect("PythonParser::new");
    let corpus = parse_corpus(&parser);
    let utils = corpus.get("utils.py").expect("utils.py in corpus");
    let gen = utils
        .symbols
        .iter()
        .find(|s| s.name == "gen")
        .expect("gen must exist");
    assert_eq!(gen.kind, SymbolKind::Function);
}

#[test]
fn variadic_signature_preserved_in_captured_signature_text() {
    // `def kw(*args, **kwargs): ...` — the captured signature must
    // include the `*args, **kwargs` markers (truncate_signature only
    // drops content from the body opener `:` onwards).
    let parser = PythonParser::new().expect("PythonParser::new");
    let corpus = parse_corpus(&parser);
    let utils = corpus.get("utils.py").expect("utils.py in corpus");
    let kw = utils
        .symbols
        .iter()
        .find(|s| s.name == "kw")
        .expect("kw must exist");
    assert!(
        kw.signature.contains("*args"),
        "signature must preserve *args; got: {:?}",
        kw.signature
    );
    assert!(
        kw.signature.contains("**kwargs"),
        "signature must preserve **kwargs; got: {:?}",
        kw.signature
    );
}

#[test]
fn pyi_stub_function_and_class_extract_identically_to_py() {
    // Load-bearing contract: `.pyi` dispatches to the same parser as
    // `.py`. `def foo(x: int) -> str: ...` is still a function_definition
    // with `...` as its body; `class Stub: def m(self) -> None: ...` is
    // still a class_definition + method_definition.
    let parser = PythonParser::new().expect("PythonParser::new");
    let corpus = parse_corpus(&parser);
    let stubs = corpus.get("stubs.pyi").expect("stubs.pyi in corpus");

    let foo = stubs
        .symbols
        .iter()
        .find(|s| s.name == "foo" && s.kind == SymbolKind::Function)
        .expect("`def foo(x: int) -> str: ...` must extract as Function");
    assert!(foo.parent.is_empty(), "free stub function has no parent");

    let stub_cls = stubs
        .symbols
        .iter()
        .find(|s| s.name == "Stub" && s.kind == SymbolKind::Class)
        .expect("class Stub must extract as Class");
    assert!(stub_cls.parent.is_empty());

    for name in ["m", "n"] {
        let m = stubs
            .symbols
            .iter()
            .find(|s| s.name == name && s.kind == SymbolKind::Method)
            .unwrap_or_else(|| panic!("Stub::{name} method must extract"));
        assert_eq!(m.parent, "Stub");
    }

    let proto = stubs
        .symbols
        .iter()
        .find(|s| s.name == "Protocol" && s.kind == SymbolKind::Class)
        .expect("class Protocol must extract as Class");
    assert!(proto.parent.is_empty());

    let required = stubs
        .symbols
        .iter()
        .find(|s| s.name == "required" && s.kind == SymbolKind::Method)
        .expect("Protocol::required method must extract");
    assert_eq!(required.parent, "Protocol");
}

#[test]
fn future_import_records_dunder_module_path() {
    // `from __future__ import annotations` — the dunder module name is
    // captured verbatim; one Includes edge with `to="__future__"`.
    let parser = PythonParser::new().expect("PythonParser::new");
    let corpus = parse_corpus(&parser);
    let app = corpus.get("app.py").expect("app.py in corpus");
    let future_edges: Vec<&Edge> = app
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Includes && e.to == "__future__")
        .collect();
    assert_eq!(
        future_edges.len(),
        1,
        "app.py must have exactly one Includes edge to __future__; got: {:?}",
        app.edges
    );
}

#[test]
fn relative_imports_preserve_leading_dot_in_to_field() {
    // `from . import utils` and `from .models import Alpha` — both must
    // record `to=".utils"` / `to=".models"` (the leading dot preserved).
    let parser = PythonParser::new().expect("PythonParser::new");
    let corpus = parse_corpus(&parser);
    let app = corpus.get("app.py").expect("app.py in corpus");
    let includes_to: Vec<&str> = app
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Includes)
        .map(|e| e.to.as_str())
        .collect();
    for want in [".utils", ".handlers", ".models"] {
        assert!(
            includes_to.contains(&want),
            "app.py imports must include {want:?}; got: {includes_to:?}"
        );
    }
}

#[test]
fn from_import_records_module_path_not_imported_name() {
    // `from .models import Alpha` produces `to=".models"`, NOT
    // `to="Alpha"`. The agent reads "this file depends on the .models
    // module" — the imported name is in scope after the import, not the
    // dependency.
    let parser = PythonParser::new().expect("PythonParser::new");
    let corpus = parse_corpus(&parser);
    let app = corpus.get("app.py").expect("app.py in corpus");
    let includes_to: Vec<&str> = app
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Includes)
        .map(|e| e.to.as_str())
        .collect();
    assert!(
        !includes_to.contains(&"Alpha"),
        "imported name `Alpha` must not appear as an Includes target; \
         got: {includes_to:?}"
    );
    assert!(
        !includes_to.contains(&"handle"),
        "imported name `handle` must not appear as an Includes target; \
         got: {includes_to:?}"
    );
}

#[test]
fn multiple_inheritance_produces_one_edge_per_base() {
    // `class Gamma(Alpha, Mixin)` → 2 Inherits edges with `from=Gamma`,
    // `to=Alpha` and `to=Mixin`.
    let parser = PythonParser::new().expect("PythonParser::new");
    let corpus = parse_corpus(&parser);
    let models = corpus.get("models.py").expect("models.py in corpus");
    let gamma_edges: Vec<&Edge> = models
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Inherits && e.from == "Gamma")
        .collect();
    assert_eq!(
        gamma_edges.len(),
        2,
        "Gamma must produce 2 Inherits edges; got: {:?}",
        gamma_edges
    );
    let tos: Vec<&str> = gamma_edges.iter().map(|e| e.to.as_str()).collect();
    assert!(tos.contains(&"Alpha"));
    assert!(tos.contains(&"Mixin"));
}

#[test]
fn qualified_inheritance_preserves_dotted_text_verbatim() {
    // `class Delta(abc.ABC)` → `to="abc.ABC"` (the `attribute` node's
    // verbatim text — not the resolved class).
    let parser = PythonParser::new().expect("PythonParser::new");
    let corpus = parse_corpus(&parser);
    let models = corpus.get("models.py").expect("models.py in corpus");
    let delta = models
        .edges
        .iter()
        .find(|e| e.kind == EdgeKind::Inherits && e.from == "Delta")
        .expect("Delta inherits edge must exist");
    assert_eq!(delta.to, "abc.ABC");
}

#[test]
fn super_dot_init_pattern_produces_two_call_edges() {
    // `super().__init__(label)` → `super` (direct) + `__init__`
    // (attribute on the chain). The parser produces both edges from
    // Beta::__init__.
    let parser = PythonParser::new().expect("PythonParser::new");
    let corpus = parse_corpus(&parser);
    let models = corpus.get("models.py").expect("models.py in corpus");
    let beta_init_calls: Vec<&str> = models
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls && e.from.ends_with(":Beta::__init__"))
        .map(|e| e.to.as_str())
        .collect();
    assert!(
        beta_init_calls.contains(&"super"),
        "Beta::__init__ must call super(); got: {beta_init_calls:?}"
    );
    assert!(
        beta_init_calls.contains(&"__init__"),
        "Beta::__init__ must call __init__ (the chained .__init__()); \
         got: {beta_init_calls:?}"
    );
}

#[test]
fn constructor_call_records_class_name_as_to_field() {
    // `Service.factory()` → `Service::factory -> Service` (constructor
    // call to the enclosing class — `Service("default")` is a direct
    // call to the identifier `Service`).
    let parser = PythonParser::new().expect("PythonParser::new");
    let corpus = parse_corpus(&parser);
    let handlers = corpus.get("handlers.py").expect("handlers.py in corpus");
    let factory_calls: Vec<&str> = handlers
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls && e.from.ends_with(":Service::factory"))
        .map(|e| e.to.as_str())
        .collect();
    assert!(
        factory_calls.contains(&"Service"),
        "Service::factory must record the constructor call as `to=Service`; \
         got: {factory_calls:?}"
    );
}

#[test]
fn attribute_call_records_attribute_name_as_to_field() {
    // `utils.kw(1, 2, key="value")` in app.py → `to="kw"` (the attribute
    // name, not the receiver `utils`).
    let parser = PythonParser::new().expect("PythonParser::new");
    let corpus = parse_corpus(&parser);
    let app = corpus.get("app.py").expect("app.py in corpus");
    let kw_call = app
        .edges
        .iter()
        .find(|e| e.kind == EdgeKind::Calls && e.to == "kw")
        .expect("attribute call utils.kw(...) must produce an edge with to=kw");
    assert!(
        kw_call.from.ends_with(":run"),
        "the attribute call originates from `run`; got: {:?}",
        kw_call.from
    );
}

#[test]
fn no_inherits_edges_for_classes_without_bases() {
    // `Mixin`, `Alpha`, `WithSlots` are all top-level classes with no
    // bases (no `superclasses` field). Zero `Inherits` edges from any
    // of them.
    let parser = PythonParser::new().expect("PythonParser::new");
    let corpus = parse_corpus(&parser);
    let models = corpus.get("models.py").expect("models.py in corpus");
    for from in ["Mixin", "Alpha", "WithSlots"] {
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

/// Dogfood-baseline regression test against the `external/requests`
/// submodule (pinned to v2.33.1). Runs `parse_file` over every `.py`
/// file under `external/requests/src/requests` and asserts the symbol
/// count stays within ±10% of the baseline recorded in
/// `testdata/python/requests-baseline.txt`.
///
/// **Auto-skips when the submodule is not initialized.** When
/// `external/requests` is empty (the submodule has not been cloned)
/// this test prints a setup hint via `eprintln!` and returns — it does
/// NOT panic. Run `git submodule update --init external/requests` (or
/// `make submodules`) to opt in.
///
/// When the pinned submodule SHA is bumped, the symbol count will
/// usually drift. Re-measure and update `requests-baseline.txt` in the
/// same commit as the SHA bump.
#[test]
fn requests_dogfood_baseline_within_ten_percent() {
    let requests_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("external")
        .join("requests")
        .join("src")
        .join("requests");
    if !requests_root.is_dir() {
        eprintln!(
            "skipping requests dogfood baseline test: \
             external/requests/src/requests not present — run `git \
             submodule update --init external/requests` (or `make \
             submodules`) to opt in"
        );
        return;
    }

    let baseline_path = corpus_root().join("requests-baseline.txt");
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

    let parser = PythonParser::new().expect("PythonParser::new");
    let mut files = Vec::new();
    walk_collect_python(&requests_root, &mut files);
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
        "requests parse produced {total_symbols} symbols; expected within \
         ±10% of baseline {baseline_count} (range [{lower}, {upper}])"
    );
}
