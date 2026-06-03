//! End-to-end integration test for `[cpp].macro_define_type`.
//!
//! Drives a synthetic C++ file whose struct/class definition is hidden inside
//! a macro invocation through the indexer pipeline and asserts that the
//! byte-preserving expansion in `CppParser::preprocess` recovers the type
//! symbol AND its members (so tree-sitter parses the real type natively).
//!
//! Identifier hygiene: generic placeholders only (`EXPORT_STRUCT`,
//! `EXPORT_CLASS`, `Foo`, `Base`) — same posture as the other anonymised
//! integration suites.

use code_graph_core::SymbolKind;
use code_graph_lang::LanguageRegistry;
use code_graph_lang_cpp::CppParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::CodeGraphServer;
use tempfile::TempDir;

fn fresh_server() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(CppParser::new().expect("CppParser::new")))
        .unwrap();
    CodeGraphServer::new(registry)
}

/// A struct-wrapping macro with a paren-wrapped body containing a field and a
/// method expands so both the `Foo` Struct and its `method` Method (parent
/// `Foo`) are recovered.
#[tokio::test]
async fn macro_define_type_recovers_struct_and_members() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(
        root.join(".code-graph.toml"),
        r#"
[cpp]
macro_define_type = [
    { name = "EXPORT_STRUCT", name_arg = 0, keyword = "struct" },
]
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("subject.cpp"),
        r#"
// The struct definition is hidden inside the macro invocation; tree-sitter
// cannot expand it, so the expansion pass rewrites it in place into a real
// `struct Foo { ... };` that the grammar parses natively.
#define EXPORT_STRUCT(name, ...) struct name { __VA_ARGS__ }

EXPORT_STRUCT(Foo, (
    int bar;
    void method() {}
));
"#,
    )
    .unwrap();

    let server = fresh_server();
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

    let g = server.inner.graph.read();
    let subject = root.join("subject.cpp");
    let syms = g.file_symbols(&subject);

    // Diagnostic sentinel: the type symbol must be present first. If this
    // fails, the expansion pass did not run / did not produce a parseable
    // struct (likely an indexing or preprocess wiring problem) — fail before
    // the members discriminator so the message names the likely root cause.
    let foo = syms.iter().find(|s| s.name == "Foo").unwrap_or_else(|| {
        panic!(
            "Foo struct must be recovered by macro_define_type expansion; \
                 got: {:?}",
            syms.iter().map(|s| &s.name).collect::<Vec<_>>()
        )
    });
    assert_eq!(foo.kind, SymbolKind::Struct);

    // Discriminator: the member method must be present with parent Foo —
    // proving the FULL type body (not just a synthetic name) was parsed.
    let method = syms.iter().find(|s| s.name == "method").unwrap_or_else(|| {
        panic!(
            "method member must be recovered (full body must parse); got: {:?}",
            syms.iter().map(|s| &s.name).collect::<Vec<_>>()
        )
    });
    assert_eq!(method.kind, SymbolKind::Method);
    assert_eq!(method.parent, "Foo", "method parent must be Foo");

    // The macro's own #define line must NOT produce a junk `name` type.
    assert!(
        !syms.iter().any(|s| s.name == "name"),
        "the #define line must not be expanded into a `name`-typed symbol"
    );
}

/// A class-wrapping macro whose name argument carries a base-class clause
/// recovers the inheritance edge.
#[tokio::test]
async fn macro_define_type_recovers_class_inheritance() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(
        root.join(".code-graph.toml"),
        r#"
[cpp]
macro_define_type = [
    { name = "EXPORT_CLASS", name_arg = 0, keyword = "class" },
]
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("subject.cpp"),
        r#"
#define EXPORT_CLASS(decl, ...) class decl { __VA_ARGS__ }

EXPORT_CLASS(Derived : public Base, (
    void m();
));
"#,
    )
    .unwrap();

    let server = fresh_server();
    let _ = analyze_codebase(
        server.inner.clone(),
        root.to_string_lossy().into_owned(),
        true,
        None,
        None,
    )
    .await;

    let g = server.inner.graph.read();
    let subject = root.join("subject.cpp");
    let syms = g.file_symbols(&subject);

    // Sentinel: the class symbol extracts.
    let derived = syms
        .iter()
        .find(|s| s.name == "Derived")
        .unwrap_or_else(|| {
            panic!(
                "Derived class must be recovered; got: {:?}",
                syms.iter().map(|s| &s.name).collect::<Vec<_>>()
            )
        });
    assert_eq!(derived.kind, SymbolKind::Class);

    // Discriminator: the inheritance edge survives the expansion. Walk the
    // class hierarchy directly via `Graph::class_hierarchy` and assert `Base`
    // appears among `Derived`'s bases — proving the rewritten
    // `class Derived : public Base { ... };` emitted the Inherits edge.
    let (root_node, _seen, _truncated) = g
        .class_hierarchy("Derived", 2, 250)
        .expect("class_hierarchy(\"Derived\") must resolve the expanded class");
    assert!(
        root_node.bases.iter().any(|b| b.name == "Base"),
        "expected Base among Derived's bases; got: {:?}",
        root_node.bases.iter().map(|b| &b.name).collect::<Vec<_>>()
    );
}

/// With no `macro_define_type` entries configured, no expansion happens — the
/// macro-hidden struct stays invisible to tree-sitter (the base limitation).
#[tokio::test]
async fn macro_define_type_inactive_when_unconfigured() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(root.join(".code-graph.toml"), "[cpp]\n").unwrap();
    std::fs::write(
        root.join("subject.cpp"),
        "EXPORT_STRUCT(Foo, (int bar; void method();));\n",
    )
    .unwrap();

    let server = fresh_server();
    let _ = analyze_codebase(
        server.inner.clone(),
        root.to_string_lossy().into_owned(),
        true,
        None,
        None,
    )
    .await;

    let g = server.inner.graph.read();
    let subject = root.join("subject.cpp");
    let syms = g.file_symbols(&subject);
    assert!(
        !syms.iter().any(|s| s.name == "Foo"),
        "without config the macro-hidden struct must stay invisible; got: {:?}",
        syms.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
}

/// `macro_define_type` expansion runs BEFORE the `macro_strip` pass, so an
/// API-macro-decorated member revealed inside the expanded body is then blanked
/// by `macro_strip` and extracts normally. Drives BOTH passes through the real
/// `preprocess` pipeline.
#[tokio::test]
async fn macro_define_type_chains_with_macro_strip() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(
        root.join(".code-graph.toml"),
        r#"
[cpp]
macro_strip = ["CORE_API"]
macro_define_type = [
    { name = "EXPORT_STRUCT", name_arg = 0, keyword = "struct" },
]
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("subject.cpp"),
        // `plain` carries no API macro (sentinel); `decorated` carries CORE_API.
        // Both inside the macro-hidden struct body.
        "EXPORT_STRUCT(Foo, ( void plain() {} CORE_API void decorated() {} ));\n",
    )
    .unwrap();

    let server = fresh_server();
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

    let g = server.inner.graph.read();
    let subject = root.join("subject.cpp");
    let syms = g.file_symbols(&subject);

    // Sentinel: expansion alone reveals the body, so an undecorated member
    // extracts even if the strip pass were a no-op. Fail here first to
    // distinguish an expansion/wiring break from a strip-chaining break.
    let plain = syms.iter().find(|s| s.name == "plain").unwrap_or_else(|| {
        panic!(
            "sentinel: undecorated member `plain` must extract from the \
                 expanded body; got: {:?}",
            syms.iter().map(|s| &s.name).collect::<Vec<_>>()
        )
    });
    assert_eq!(plain.parent, "Foo");

    // Discriminator: the CORE_API-decorated member extracts ONLY because the
    // strip pass blanked `CORE_API` inside the revealed body. If expansion did
    // not run first, `CORE_API` would never have been exposed to strip.
    let decorated = syms
        .iter()
        .find(|s| s.name == "decorated")
        .unwrap_or_else(|| {
            panic!(
                "CORE_API-decorated member must extract after the strip pass \
                 blanks it inside the expanded body; got: {:?}",
                syms.iter().map(|s| &s.name).collect::<Vec<_>>()
            )
        });
    assert_eq!(decorated.parent, "Foo");
}

/// Same chaining contract with `macro_strip_with_args`: a parameterized
/// reflection macro line (`GENERATED_BODY()`) inside the expanded body is
/// blanked so the surrounding members still extract.
#[tokio::test]
async fn macro_define_type_chains_with_macro_strip_with_args() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(
        root.join(".code-graph.toml"),
        r#"
[cpp]
macro_strip_with_args = ["GENERATED_BODY"]
macro_define_type = [
    { name = "EXPORT_STRUCT", name_arg = 0, keyword = "struct" },
]
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("subject.cpp"),
        "EXPORT_STRUCT(Foo, ( void before() {}\n GENERATED_BODY()\n void after() {} ));\n",
    )
    .unwrap();

    let server = fresh_server();
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

    let g = server.inner.graph.read();
    let subject = root.join("subject.cpp");
    let syms = g.file_symbols(&subject);

    // Sentinel: a member preceding the reflection-macro line extracts.
    let before = syms.iter().find(|s| s.name == "before").unwrap_or_else(|| {
        panic!(
            "sentinel: member before GENERATED_BODY() must extract; got: {:?}",
            syms.iter().map(|s| &s.name).collect::<Vec<_>>()
        )
    });
    assert_eq!(before.parent, "Foo");

    // Discriminator: the member AFTER the `GENERATED_BODY()` line still
    // extracts — only possible if the args-macro was blanked cleanly inside the
    // expanded body (an un-stripped `GENERATED_BODY()` would derail the parse).
    let after = syms.iter().find(|s| s.name == "after").unwrap_or_else(|| {
        panic!(
            "member after GENERATED_BODY() must extract once it is stripped; \
                 got: {:?}",
            syms.iter().map(|s| &s.name).collect::<Vec<_>>()
        )
    });
    assert_eq!(after.parent, "Foo");
}
