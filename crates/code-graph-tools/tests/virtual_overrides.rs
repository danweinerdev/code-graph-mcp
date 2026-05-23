//! End-to-end integration tests for `EdgeKind::Overrides` extraction
//! and the `find_overrides` MCP tool.
//!
//! Drives the C++ parser through a synthesised
//! `class Base { virtual void Foo(); }; class Derived : public Base
//! { void Foo() override; };` fixture and asserts:
//! - the override method's edge resolves through
//!   `resolve_edges_with_indexes` to the base method's symbol_id, and
//! - `find_overrides(<base_symbol_id>)` returns the derived method via
//!   the new MCP tool path.
//!
//! Identifier hygiene: generic placeholders only (`Base` / `Derived` /
//! `Foo`), no third-party identifiers — same posture as
//! `parser_bug_regressions.rs`.

use code_graph_core::SymbolKind;
use code_graph_lang::LanguageRegistry;
use code_graph_lang_cpp::CppParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::handlers::query::find_overrides;
use code_graph_tools::CodeGraphServer;
use std::path::Path;
use tempfile::TempDir;

fn fresh_server() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().expect("CppParser::new")))
        .unwrap();
    CodeGraphServer::new(registry)
}

async fn seed_and_analyze(server: &CodeGraphServer, src: &str) -> std::path::PathBuf {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(root.join(".code-graph.toml"), "[cpp]\n").unwrap();
    std::fs::write(root.join("subject.cpp"), src).unwrap();
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
        "analyze_codebase failed: {r:?}"
    );
    std::mem::forget(dir); // keep alive past the function — tests aren't long-running
    root
}

/// Single-base case: Derived::Foo overrides Base::Foo. The override
/// edge survives `resolve_edges_with_indexes` and points at the
/// base method's symbol_id.
#[tokio::test]
async fn override_edge_emits_and_resolves_single_base() {
    let src = "\
class Base
{
public:
    virtual void Foo();
    virtual void Foo() {}
};

class Derived : public Base
{
public:
    void Foo() override {}
};
";
    let server = fresh_server();
    let root = seed_and_analyze(&server, src).await;
    let subject = root.join("subject.cpp");

    let g = server.inner.graph.read();
    let derived_foo_id = format!("{}:Derived::Foo", subject.to_string_lossy());
    let base_foo_id = format!("{}:Base::Foo", subject.to_string_lossy());

    // The base method's reverse adjacency must contain an Overrides
    // edge from the derived method. find_overrides reads from radj
    // and filters to kind=Overrides.
    let overrides = g.find_overrides(&base_foo_id);
    assert!(
        overrides.iter().any(|c| c.symbol_id == derived_foo_id),
        "Derived::Foo must appear in find_overrides(Base::Foo); \
         got: {:?}",
        overrides.iter().map(|c| &c.symbol_id).collect::<Vec<_>>()
    );
}

/// The `find_overrides` MCP handler returns the standard
/// `Page<CallChain>` envelope wrapping the override list. Hits the
/// public surface a client would.
#[tokio::test]
async fn find_overrides_handler_returns_page_envelope() {
    let src = "\
class Base
{
public:
    virtual void Tick() {}
};

class Mid : public Base
{
public:
    void Tick() override {}
};

class Other : public Base
{
public:
    void Tick() override {}
};
";
    let server = fresh_server();
    let root = seed_and_analyze(&server, src).await;
    let subject = root.join("subject.cpp");
    let base_tick = format!("{}:Base::Tick", subject.to_string_lossy());

    let r = find_overrides(
        &server.inner.graph,
        &base_tick,
        None,
        None,
        100_000, // generous byte budget
    );
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "find_overrides handler failed: {r:?}"
    );
    let body = r
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.to_string())
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["total"], serde_json::json!(2));
    let names: Vec<String> = parsed["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["symbol_id"].as_str().unwrap().to_string())
        .collect();
    assert!(names.iter().any(|n| n.ends_with(":Mid::Tick")));
    assert!(names.iter().any(|n| n.ends_with(":Other::Tick")));
}

/// Non-virtual methods produce no Overrides edges — only `virtual` /
/// `override` declarators trigger the extraction.
#[tokio::test]
async fn non_virtual_method_emits_no_override_edge() {
    let src = "\
class Base { public: void Foo() {} };
class Derived : public Base
{
public:
    void Foo() {}  // shadows but does not override (no virtual)
};
";
    let server = fresh_server();
    let root = seed_and_analyze(&server, src).await;
    let subject = root.join("subject.cpp");
    let base_foo = format!("{}:Base::Foo", subject.to_string_lossy());

    let g = server.inner.graph.read();
    let overrides = g.find_overrides(&base_foo);
    assert!(
        overrides.is_empty(),
        "non-virtual shadowing must NOT produce Override edges; got: {:?}",
        overrides.iter().map(|c| &c.symbol_id).collect::<Vec<_>>()
    );
}

/// The `find_overrides` handler emits the standard "symbol not found"
/// error (with did-you-mean suggestions) for an unknown symbol.
#[tokio::test]
async fn find_overrides_unknown_symbol_returns_error() {
    let server = fresh_server();
    // Seed a graph with SOMETHING so the symbol-suggester has fodder.
    let _root = seed_and_analyze(
        &server,
        "class Base { public: virtual void Tick(); }; class Derived : public Base { void Tick() override {} };",
    )
    .await;
    let r = find_overrides(
        &server.inner.graph,
        "/does/not/exist.cpp:NotAMethod",
        None,
        None,
        100_000,
    );
    assert_eq!(r.is_error, Some(true));
    let body = r
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.to_string())
        .unwrap();
    assert!(
        body.starts_with("symbol not found:"),
        "expected 'symbol not found:' prefix; got: {body}"
    );
}

/// A method that's marked `virtual` but whose parent class has NO
/// base classes can't override anything — emits zero override
/// edges. Anti-regression for the early-return in `extract_overrides`.
#[tokio::test]
async fn virtual_method_without_bases_emits_no_override_edge() {
    let src = "\
class Standalone
{
public:
    virtual void Foo() {}
};
";
    let server = fresh_server();
    let root = seed_and_analyze(&server, src).await;
    let subject = root.join("subject.cpp");
    let standalone_foo = format!("{}:Standalone::Foo", subject.to_string_lossy());

    let g = server.inner.graph.read();
    // Standalone itself is the would-be base; find_overrides on it
    // returns nothing because nobody overrides it.
    let overrides = g.find_overrides(&standalone_foo);
    assert!(overrides.is_empty());

    // And the symbol_id confirms the method itself indexes (the test
    // is meaningfully scoped to "method exists, just has no
    // overrides").
    assert!(
        g.symbol_detail(&standalone_foo)
            .is_some_and(|s| s.kind == SymbolKind::Method),
        "Standalone::Foo must index as a Method"
    );
    // Keep `_subject` referenced for the IDE.
    let _ = Path::new(&subject);
}
