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

/// Seed a multi-file fixture: a `name -> contents` map written into
/// a single TempDir, then analyzed. Returns the canonical project
/// root so callers can construct file paths with `root.join(name)`.
async fn seed_and_analyze_multi(
    server: &CodeGraphServer,
    files: &[(&str, &str)],
) -> std::path::PathBuf {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(root.join(".code-graph.toml"), "[cpp]\n").unwrap();
    for (name, contents) in files {
        std::fs::write(root.join(name), contents).unwrap();
    }
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
    std::mem::forget(dir);
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

/// Non-virtual same-named methods in an inheritance chain DO emit
/// Override edges under the post-cross-file global pass. Strictly
/// these are C++ "shadowing" not "overriding" — under per-spec C++
/// semantics no override exists because the base method isn't
/// virtual. The post_index pass accepts this false positive as the
/// trade-off for unblocking the dominant cross-file UE pattern,
/// where the only place the `virtual` keyword appears is the in-class
/// declaration in a header which is intentionally not extracted as
/// a Symbol (see `extract_overrides_global` doc-comment for the
/// rationale). Production C++ rarely uses non-virtual shadowing, so
/// the cost is small.
#[tokio::test]
async fn non_virtual_shadowing_emits_override_edge_by_design() {
    let src = "\
class Base { public: void Foo() {} };
class Derived : public Base
{
public:
    void Foo() {}  // shadows; emits an Override edge under the loose rule
};
";
    let server = fresh_server();
    let root = seed_and_analyze(&server, src).await;
    let subject = root.join("subject.cpp");
    let base_foo = format!("{}:Base::Foo", subject.to_string_lossy());
    let derived_foo = format!("{}:Derived::Foo", subject.to_string_lossy());

    let g = server.inner.graph.read();
    let overrides = g.find_overrides(&base_foo);
    let ids: Vec<&str> = overrides.iter().map(|c| c.symbol_id.as_str()).collect();
    assert!(
        ids.contains(&derived_foo.as_str()),
        "Even non-virtual shadowing must emit an Override edge under the loose \
         cross-file rule — the per-file `virtual`-gated pass was deleted; got: {ids:?}"
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

/// Cross-file overrides — the UE-dominant pattern. `Base` and the
/// `class Derived : public Base {}` declaration live in `base.h`; the
/// override body `void Derived::Foo() override {}` lives in
/// `derived.cpp`. The per-file extract_overrides pass couldn't see
/// `derived.cpp`'s parent's inheritance edge because that edge was
/// emitted from `base.h`. The post_index pass aggregates Inherits
/// edges globally, so the Override edge survives.
#[tokio::test]
async fn cross_file_out_of_line_override_emits_edge() {
    let header = "\
class Base
{
public:
    virtual void Foo();
};

class Derived : public Base
{
public:
    virtual void Foo() override;
};
";
    let impl_src = "\
#include \"base.h\"

void Derived::Foo() {}
void Base::Foo() {}
";
    let server = fresh_server();
    let root =
        seed_and_analyze_multi(&server, &[("base.h", header), ("derived.cpp", impl_src)]).await;
    let header_path = root.join("base.h");
    let impl_path = root.join("derived.cpp");

    let g = server.inner.graph.read();
    // The base method is declared in base.h, so the symbol_id is keyed
    // to base.h. The out-of-line definition's symbol carries
    // `parent = "Base"` per the C++ method extraction rule.
    let base_foo_id = format!("{}:Base::Foo", impl_path.to_string_lossy());
    // The override method lives out-of-line in derived.cpp.
    let derived_foo_id = format!("{}:Derived::Foo", impl_path.to_string_lossy());

    let overrides = g.find_overrides(&base_foo_id);
    let ids: Vec<&str> = overrides.iter().map(|c| c.symbol_id.as_str()).collect();
    assert!(
        ids.contains(&derived_foo_id.as_str()),
        "Derived::Foo defined in derived.cpp must show up as overriding Base::Foo \
         even though `class Derived : public Base` is declared in base.h; \
         got: {ids:?}"
    );
    // Anti-regression: keep the header_path used so the binding isn't
    // optimised away during refactors.
    let _ = header_path;
}

/// Multi-level cross-file inheritance. `Base` in `base.h`, `Mid` in
/// `mid.h` inheriting from `Base` (with its own virtual `Foo`), and
/// `Derived` in `derived.cpp` inheriting from `Mid` and overriding
/// `Foo`. The override edge must fan out to BOTH `Mid::Foo` and
/// `Base::Foo` so `find_overrides` returns the override at every
/// ancestor's level.
#[tokio::test]
async fn cross_file_multi_level_inheritance_walks_ancestry() {
    let base_h = "\
class Base
{
public:
    virtual void Foo();
};
";
    let mid_h = "\
#include \"base.h\"

class Mid : public Base
{
public:
    virtual void Foo() override;
};
";
    let derived_cpp = "\
#include \"mid.h\"

void Base::Foo() {}
void Mid::Foo() {}

class Derived : public Mid
{
public:
    void Foo() override {}
};
";
    let server = fresh_server();
    let root = seed_and_analyze_multi(
        &server,
        &[
            ("base.h", base_h),
            ("mid.h", mid_h),
            ("derived.cpp", derived_cpp),
        ],
    )
    .await;
    let derived_cpp_path = root.join("derived.cpp");
    let g = server.inner.graph.read();
    let derived_foo_id = format!("{}:Derived::Foo", derived_cpp_path.to_string_lossy());
    // Find the Mid::Foo and Base::Foo symbols. Mid::Foo's out-of-line
    // definition lives in derived.cpp; Base::Foo's lives there too.
    let mid_foo_id = format!("{}:Mid::Foo", derived_cpp_path.to_string_lossy());
    let base_foo_id = format!("{}:Base::Foo", derived_cpp_path.to_string_lossy());

    // find_overrides(Mid::Foo) must include Derived::Foo (direct override).
    let mid_overrides = g.find_overrides(&mid_foo_id);
    let mid_ids: Vec<&str> = mid_overrides.iter().map(|c| c.symbol_id.as_str()).collect();
    assert!(
        mid_ids.contains(&derived_foo_id.as_str()),
        "Derived::Foo must override Mid::Foo directly; got: {mid_ids:?}"
    );

    // find_overrides(Base::Foo) must ALSO include Derived::Foo because
    // Derived transitively overrides Foo from Base via Mid. The global
    // ancestry-walk emits an Override edge to every ancestor whose
    // class defines the same-named method.
    let base_overrides = g.find_overrides(&base_foo_id);
    let base_ids: Vec<&str> = base_overrides
        .iter()
        .map(|c| c.symbol_id.as_str())
        .collect();
    assert!(
        base_ids.contains(&derived_foo_id.as_str()),
        "Derived::Foo must transitively override Base::Foo through Mid; got: {base_ids:?}"
    );
}

/// Cross-file override where the ancestor has NO matching method
/// name. `class Derived : public Base` with `Derived::Foo` declared,
/// but Base has only `Bar` — no `Foo`. The override candidate's
/// ancestry walk must NOT emit an edge pointing at `Base::Foo`
/// because Base doesn't define `Foo`. (Otherwise the graph carries
/// unresolvable Override edges, bloating `find_overrides` output.)
#[tokio::test]
async fn cross_file_ancestor_without_matching_method_emits_no_edge() {
    let base_h = "\
class Base
{
public:
    virtual void Bar();
};
";
    let derived_cpp = "\
#include \"base.h\"

void Base::Bar() {}

class Derived : public Base
{
public:
    virtual void Foo() {}
};
";
    let server = fresh_server();
    let root =
        seed_and_analyze_multi(&server, &[("base.h", base_h), ("derived.cpp", derived_cpp)]).await;
    let derived_cpp_path = root.join("derived.cpp");
    let g = server.inner.graph.read();
    // Build the Base::Foo id even though Base::Foo doesn't exist —
    // querying it returns empty because no symbol with that id.
    let phantom_base_foo = format!("{}:Base::Foo", derived_cpp_path.to_string_lossy());
    let overrides = g.find_overrides(&phantom_base_foo);
    assert!(
        overrides.is_empty(),
        "Base doesn't define Foo so Derived::Foo can't override it; \
         got phantom overrides: {:?}",
        overrides.iter().map(|c| &c.symbol_id).collect::<Vec<_>>()
    );
}

/// Inheritance edge in one file, override declaration in the same
/// header — the standard all-inline case. The new global pass must
/// continue to handle this (it's a subset of the cross-file case),
/// otherwise we'd regress the existing per-file coverage.
#[tokio::test]
async fn inline_same_file_override_still_works_under_global_pass() {
    let src = "\
class Base
{
public:
    virtual void Foo();
};
void Base::Foo() {}

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
    let base_foo_id = format!("{}:Base::Foo", subject.to_string_lossy());
    let derived_foo_id = format!("{}:Derived::Foo", subject.to_string_lossy());
    let overrides = g.find_overrides(&base_foo_id);
    let ids: Vec<&str> = overrides.iter().map(|c| c.symbol_id.as_str()).collect();
    assert!(
        ids.contains(&derived_foo_id.as_str()),
        "Same-file all-inline override must still work; got: {ids:?}"
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
