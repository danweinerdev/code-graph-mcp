//! Integration tests for Rust `mod`-declaration `Includes` edge
//! resolution.
//!
//! These tests drive the full `analyze_codebase` pipeline against a
//! `TempDir`-materialized real Rust crate fixture and assert against
//! the client-visible JSON of `get_dependencies`,
//! `generate_diagram(file=…)`, and `detect_cycles`. They pin the
//! end-to-end mod-edge resolution contract: every external
//! `mod foo;` in a Rust crate produces a surviving file-level
//! `Includes` edge from the declaring file to the resolved child
//! (sibling `dir/foo.rs`, `dir/foo/mod.rs`, or a `#[path = "x.rs"]`
//! override target), provided the child is in the discovered set.
//! `use` / `extern crate` edges still drop — they remain dotted
//! module-path tokens, not absolute paths, and the Rust
//! `resolve_include` override returns `None` for any non-absolute
//! string. This is the documented intentional scope boundary: the
//! Rust plugin models the intra-crate module tree as file-level
//! Includes; cross-crate `use`-path resolution is deliberately out
//! of scope (a heuristic would risk emitting false dependency edges,
//! corrupting `detect_cycles`/`get_coupling` worse than missing
//! edges).

mod common;

use code_graph_lang::LanguageRegistry;
use code_graph_lang_rust::RustParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::handlers::query::get_dependencies;
use code_graph_tools::handlers::structure::{
    detect_cycles, generate_diagram, GenerateDiagramInput,
};
use code_graph_tools::handlers::NO_BYTE_BUDGET;
use code_graph_tools::CodeGraphServer;
use common::{first_text, ok_json};
use tempfile::TempDir;

/// Fresh server with only the Rust parser registered. The Rust-only
/// fixture keeps these tests focused on the mod-resolution behavior;
/// adding other parsers would conflate the Rust mod-edge surface with
/// cross-language indexing.
fn rust_only_server() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(RustParser::new().expect("RustParser::new")))
        .expect("register RustParser");
    CodeGraphServer::new(registry)
}

/// Run `analyze_codebase` against `dir` with `force=true` (deterministic
/// — never takes the on-disk cache fast path). Panics with the
/// response body on failure so a regression names the offending stage.
async fn analyze(server: &CodeGraphServer, dir: &std::path::Path) {
    let r = analyze_codebase(
        server.inner.clone(),
        dir.to_string_lossy().into_owned(),
        true,
        None,
        None,
    )
    .await;
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "analyze_codebase must succeed for the fixture: {r:?}",
    );
}

/// Write `Cargo.toml` with the minimum fields RCMM consumes
/// (`[package].name`). Returns the canonical manifest path.
fn write_cargo_toml(dir: &std::path::Path, crate_name: &str) -> std::path::PathBuf {
    let manifest = dir.join("Cargo.toml");
    let body =
        format!("[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
    std::fs::write(&manifest, body).expect("write Cargo.toml");
    std::fs::canonicalize(&manifest).expect("canonicalize Cargo.toml")
}

/// Materialize a `.rs` file at `dir/rel_path`, creating parent dirs as
/// needed. Returns the canonicalized absolute path.
fn write_rs(dir: &std::path::Path, rel_path: &str, contents: &str) -> std::path::PathBuf {
    let abs = dir.join(rel_path);
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent).expect("create_dir_all parent");
    }
    std::fs::write(&abs, contents).expect("write .rs file");
    std::fs::canonicalize(&abs).expect("canonicalize written file")
}

// ---------------------------------------------------------------------
// get_dependencies + generate_diagram(file=…) non-empty for Rust
// ---------------------------------------------------------------------

/// **Critical 2.2 deliverable.** Before 2.2: `get_dependencies(lib.rs)`
/// and `generate_diagram(file=lib.rs)` always returned empty for a
/// real Rust crate because every Rust `Includes` edge dropped at
/// `resolve_all_edges` time. After 2.2: both tools surface the real
/// `mod`-declared file dependencies.
///
/// Fixture: a 3-file crate
/// (`src/lib.rs` declares `mod a;` and `mod b;` → `src/a.rs`, `src/b.rs`).
#[tokio::test]
async fn get_dependencies_and_diagram_non_empty_for_rust() {
    let dir = TempDir::new().expect("TempDir");
    write_cargo_toml(dir.path(), "demo_crate");
    let lib = write_rs(
        dir.path(),
        "src/lib.rs",
        "pub mod a;\npub mod b;\n\nfn root() {}\n",
    );
    let a = write_rs(dir.path(), "src/a.rs", "fn af() {}\n");
    let b = write_rs(dir.path(), "src/b.rs", "fn bf() {}\n");

    let root = std::fs::canonicalize(dir.path()).expect("canonicalize root");
    let server = rust_only_server();
    analyze(&server, &root).await;

    let lib_str = lib.to_string_lossy().into_owned();
    let a_str = a.to_string_lossy().into_owned();
    let b_str = b.to_string_lossy().into_owned();

    // get_dependencies(lib.rs) must surface a.rs and b.rs (and nothing
    // else — there are no use/extern_crate edges in this fixture).
    let deps = get_dependencies(&server.inner.graph, &lib_str, None, None, NO_BYTE_BUDGET);
    let body = ok_json(&deps);
    let rows = body["results"]
        .as_array()
        .expect("Page<DependencyEntry> results array");
    let dep_files: Vec<&str> = rows
        .iter()
        .map(|r| r["file"].as_str().expect("DependencyEntry.file"))
        .collect();
    assert_eq!(
        rows.len(),
        2,
        "lib.rs must have exactly 2 mod-dependency entries (a, b); got {rows:?}",
    );
    assert!(
        dep_files.contains(&a_str.as_str()),
        "lib.rs must depend on a.rs; got {dep_files:?}",
    );
    assert!(
        dep_files.contains(&b_str.as_str()),
        "lib.rs must depend on b.rs; got {dep_files:?}",
    );
    // Every dep row must carry `kind = "includes"` — that's the only
    // EdgeKind a Rust mod-decl edge can be.
    assert!(
        rows.iter().all(|r| r["kind"].as_str() == Some("includes")),
        "every Rust mod-dep row must carry kind=\"includes\"; got {rows:?}",
    );

    // generate_diagram(file=lib.rs) must produce non-empty edges.
    // The Mermaid file-mode renders `from -> to` for every Includes
    // edge originating at the requested file.
    let r = generate_diagram(
        &server.inner.graph,
        GenerateDiagramInput {
            file: Some(&lib_str),
            format: Some("edges"),
            ..Default::default()
        },
    );
    assert!(
        r.is_error.is_none() || r.is_error == Some(false),
        "generate_diagram(file=lib.rs) must succeed: {r:?}",
    );
    let body = first_text(&r);
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("diagram edges JSON");
    let edges = parsed.as_array().expect("edges format is an array");
    assert!(
        !edges.is_empty(),
        "generate_diagram(file=lib.rs) must surface the mod-decl edges; got empty",
    );
    let edge_pairs: Vec<(String, String)> = edges
        .iter()
        .map(|e| {
            (
                e["from"].as_str().unwrap_or("").to_owned(),
                e["to"].as_str().unwrap_or("").to_owned(),
            )
        })
        .collect();
    // The diagram displays paths as-is. `from` is lib.rs's basename or
    // full path depending on the renderer's display rule; both should
    // contain the substring `lib.rs`/`a.rs`/`b.rs`. We assert on
    // substring presence to stay robust against the precise display
    // collapse.
    assert!(
        edge_pairs
            .iter()
            .any(|(_, t)| t.ends_with("a.rs") || t == "a"),
        "diagram must include an edge to a.rs (or its basename `a`); got {edge_pairs:?}",
    );
    assert!(
        edge_pairs
            .iter()
            .any(|(_, t)| t.ends_with("b.rs") || t == "b"),
        "diagram must include an edge to b.rs; got {edge_pairs:?}",
    );

    drop(dir);
}

// ---------------------------------------------------------------------
// detect_cycles finds a real `mod a → mod b → mod a` cycle
// ---------------------------------------------------------------------

/// `detect_cycles` returns a real cycle for a fixture where two
/// top-level mod decls point at each other (`a.rs` declares `mod b;`,
/// `b.rs` declares `mod a;`). This was impossible before 2.2 — the
/// provisional edges dropped before reaching `Graph::detect_cycles`.
#[tokio::test]
async fn detect_cycles_finds_mod_a_mod_b_mod_a_cycle() {
    let dir = TempDir::new().expect("TempDir");
    write_cargo_toml(dir.path(), "cyclic");
    // lib.rs gates a and b into the index — `a.rs` and `b.rs` then
    // form the mutually-recursive mod pair. (Without lib.rs declaring
    // them, the discovery walk still finds them as ordinary .rs files,
    // but lib.rs makes the fixture mirror how a real crate would be
    // organized.)
    let _lib = write_rs(dir.path(), "src/lib.rs", "pub mod a;\npub mod b;\n");
    let a = write_rs(dir.path(), "src/a.rs", "mod b;\nfn af() {}\n");
    let b = write_rs(dir.path(), "src/b.rs", "mod a;\nfn bf() {}\n");

    let root = std::fs::canonicalize(dir.path()).expect("canonicalize root");
    let server = rust_only_server();
    analyze(&server, &root).await;

    let cycles = detect_cycles(&server.inner.graph, None, None, None, None);
    let parsed: serde_json::Value =
        serde_json::from_str(&first_text(&cycles)).expect("detect_cycles JSON");
    let results = parsed["results"]
        .as_array()
        .expect("Page<Cycle> results array");

    // Each Cycle's `files` array carries the cycle's file paths. We
    // search for the specific (a, b) pair — there should be exactly
    // one cycle here regardless of the cycle's reported direction.
    let a_str = a.to_string_lossy().into_owned();
    let b_str = b.to_string_lossy().into_owned();

    let has_ab_cycle = results.iter().any(|c| {
        let files = c["files"].as_array().map(|a| a.as_slice()).unwrap_or(&[]);
        let names: Vec<&str> = files.iter().filter_map(|v| v.as_str()).collect();
        names.contains(&a_str.as_str()) && names.contains(&b_str.as_str())
    });
    assert!(
        has_ab_cycle,
        "detect_cycles must surface the a.rs <-> b.rs cycle; got: {}",
        serde_json::to_string_pretty(&parsed).unwrap_or_default(),
    );

    drop(dir);
}

// ---------------------------------------------------------------------
// `use` / `extern crate` scope boundary — still drops post-resolve
// ---------------------------------------------------------------------

/// Scope boundary: `use std::io;` and `extern crate foo;` MUST NOT
/// produce surviving `Includes` edges even after 2.2's mod-resolution
/// pass + `resolve_include` override. The override returns `None` for
/// any non-absolute string, which is exactly the shape of dotted
/// use-paths and bare extern-crate names.
///
/// Fixture: one file with one `use` decl, one `extern crate` decl, and
/// no mod decls at all → after analyze, `get_dependencies` returns an
/// empty page.
#[tokio::test]
async fn use_and_extern_crate_still_drop_after_2_2() {
    let dir = TempDir::new().expect("TempDir");
    write_cargo_toml(dir.path(), "only_uses");
    let lib = write_rs(
        dir.path(),
        "src/lib.rs",
        "use std::io;\nextern crate alloc;\n\nfn f() {}\n",
    );

    let root = std::fs::canonicalize(dir.path()).expect("canonicalize root");
    let server = rust_only_server();
    analyze(&server, &root).await;

    let lib_str = lib.to_string_lossy().into_owned();
    let deps = get_dependencies(&server.inner.graph, &lib_str, None, None, NO_BYTE_BUDGET);
    let body = ok_json(&deps);
    let rows = body["results"]
        .as_array()
        .expect("Page<DependencyEntry> results array");
    assert!(
        rows.is_empty(),
        "use/extern_crate must NOT produce surviving Includes edges (scope boundary held); \
         got: {rows:?}",
    );

    drop(dir);
}
