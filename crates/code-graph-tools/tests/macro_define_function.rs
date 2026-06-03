//! End-to-end integration test for `[cpp].macro_define_function`.
//!
//! Drives a synthetic C++ file containing a token-pasting macro
//! invocation through the indexer pipeline and asserts the
//! synthesized `<Type><suffix>` function appears as a top-level
//! Symbol.
//!
//! Identifier hygiene: generic placeholders only
//! (`DECLARE_RELEASE_FN`, `MyType`, `_Release` suffix) — same
//! posture as the other anonymised integration suites.

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

/// `IMPLEMENT_RELEASE_FN(Bar)` configured with `arg=0, suffix="_Release"`
/// synthesizes `Bar_Release` as a top-level Function symbol.
#[tokio::test]
async fn macro_define_function_synthesizes_top_level_symbol() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(
        root.join(".code-graph.toml"),
        r#"
[cpp]
macro_define_function = [
    { name = "IMPLEMENT_RELEASE_FN", arg = 0, suffix = "_Release" },
]
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("subject.cpp"),
        r#"
// The token-pasting macro is opaque to tree-sitter; the synthesizer
// must produce the Bar_Release Symbol from the macro invocation.
#define IMPLEMENT_RELEASE_FN(name) void name##_Release(void* p) { (void)p; }

IMPLEMENT_RELEASE_FN(Bar)
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
    let bar_release = syms
        .iter()
        .find(|s| s.name == "Bar_Release")
        .expect("Bar_Release must be synthesized by macro_define_function");
    assert_eq!(bar_release.kind, SymbolKind::Function);
    // No parent — synthesized at top-level scope.
    assert!(bar_release.parent.is_empty());

    // #define-line false-positive guard (Deliverable 1): the macro's own
    // `#define IMPLEMENT_RELEASE_FN(name) ...` line must NOT be scanned as
    // an invocation — doing so would synthesize a junk `name_Release` symbol
    // named after the macro's formal parameter. Only the real `Bar`
    // invocation produces a symbol.
    assert!(
        !syms.iter().any(|s| s.name == "name_Release"),
        "the #define line must not synthesize a `name_Release` symbol from the macro parameter; \
         got: {:?}",
        syms.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
}

/// Multiple invocations in the same file produce multiple synthetic
/// symbols. Verifies the byte scanner walks the whole content.
#[tokio::test]
async fn macro_define_function_handles_multiple_invocations() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(
        root.join(".code-graph.toml"),
        r#"
[cpp]
macro_define_function = [
    { name = "MAKE_FN", arg = 0, suffix = "_impl" },
]
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("subject.cpp"),
        "MAKE_FN(Alpha)\nMAKE_FN(Beta)\nMAKE_FN(Gamma)\n",
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
    let synth: Vec<&str> = syms
        .iter()
        .filter(|s| s.name.ends_with("_impl"))
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(synth.len(), 3, "got: {synth:?}");
    assert!(synth.contains(&"Alpha_impl"));
    assert!(synth.contains(&"Beta_impl"));
    assert!(synth.contains(&"Gamma_impl"));
}

/// With no `macro_define_function` entries configured, no synthesis
/// happens — even if the source contains plausible macro
/// invocations.
#[tokio::test]
async fn macro_define_function_inactive_when_unconfigured() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(root.join(".code-graph.toml"), "[cpp]\n").unwrap();
    std::fs::write(root.join("subject.cpp"), "IMPLEMENT_RELEASE_FN(Bar)\n").unwrap();

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
        !syms.iter().any(|s| s.name.ends_with("_Release")),
        "no synthesis expected without config; got: {:?}",
        syms.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
}

/// Second-arg-position config: `DEFINE_FN(Module, FnName)` with
/// `arg=1, suffix=""` synthesizes `FnName`.
#[tokio::test]
async fn macro_define_function_picks_correct_arg_index() {
    let dir = TempDir::new().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(
        root.join(".code-graph.toml"),
        r#"
[cpp]
macro_define_function = [
    { name = "DEFINE_FN", arg = 1, suffix = "" },
]
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("subject.cpp"),
        "DEFINE_FN(MyModule, my_function)\n",
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
        syms.iter().any(|s| s.name == "my_function"),
        "arg=1 must capture the second macro argument; got: {:?}",
        syms.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
    // The Module name (arg 0) must NOT be synthesized.
    assert!(
        !syms.iter().any(|s| s.name == "MyModule"),
        "arg 0 (MyModule) must not be synthesized when arg=1 is configured"
    );
}
