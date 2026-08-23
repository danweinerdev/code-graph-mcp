//! End-to-end Go import and call-resolution regression coverage.
//!
//! Go package imports resolve to one deterministic representative source file:
//! the lexicographically first indexed `.go` file in the imported package. This
//! keeps `Includes` a file graph without multiplying a single import across every
//! source file in a package.

mod common;

use std::path::{Path, PathBuf};

use code_graph_lang::LanguageRegistry;
use code_graph_lang_go::GoParser;
use code_graph_tools::handlers::analyze::analyze_codebase;
use code_graph_tools::handlers::query::{callers_or_callees, get_dependencies, Direction};
use code_graph_tools::handlers::structure::get_coupling;
use code_graph_tools::handlers::NO_BYTE_BUDGET;
use code_graph_tools::CodeGraphServer;
use common::ok_json;
use tempfile::TempDir;

fn go_only_server() -> CodeGraphServer {
    let mut registry = LanguageRegistry::new();
    registry
        .register(Box::new(GoParser::new().expect("GoParser::new")))
        .expect("register GoParser");
    CodeGraphServer::new(registry)
}

async fn analyze(server: &CodeGraphServer, dir: &Path) {
    analyze_with_force(server, dir, true).await;
}

async fn analyze_with_force(server: &CodeGraphServer, dir: &Path, force: bool) {
    let result = analyze_codebase(
        server.inner.clone(),
        dir.to_string_lossy().into_owned(),
        force,
        None,
        None,
    )
    .await;
    assert!(
        result.is_error.is_none() || result.is_error == Some(false),
        "analyze_codebase must succeed: {result:?}"
    );
}

fn write_go(dir: &Path, relative: &str, source: &str) -> PathBuf {
    let path = dir.join(relative);
    std::fs::create_dir_all(path.parent().expect("Go file has parent"))
        .expect("create Go parent directory");
    std::fs::write(&path, source).expect("write Go source");
    code_graph_core::paths::canonicalize(&path).expect("canonicalize Go source")
}

fn strip_resolver_metadata_extension(dir: &Path) {
    const HEADER_SIZE: usize = 8;
    const FOOTER_SIZE: usize = 24;
    const MAGIC: &[u8; 8] = b"CGMETA01";

    let cache = dir.join(".code-graph-cache.db");
    let mut bytes = std::fs::read(&cache).expect("read extended cache");
    assert!(
        bytes.ends_with(MAGIC),
        "cache must contain metadata extension"
    );
    let footer_start = bytes.len() - FOOTER_SIZE;
    let main_len = u64::from_ne_bytes(
        bytes[footer_start..footer_start + 8]
            .try_into()
            .expect("main archive length footer"),
    ) as usize;
    bytes.truncate(HEADER_SIZE + main_len);
    std::fs::write(cache, bytes).expect("write footerless v13 cache");
}

/// Materialize one module that has a package with multiple files, a second
/// imported package, and unrelated collision symbols. The caller exercises all
/// resolver branches in one graph while the assertions below keep each contract
/// independent and readable.
fn fixture() -> (TempDir, PathBuf, PathBuf, PathBuf, PathBuf) {
    let dir = TempDir::new().expect("TempDir");
    std::fs::write(dir.path().join("go.mod"), "module example.test/project\n")
        .expect("write go.mod");

    let main = write_go(
        dir.path(),
        "cmd/main.go",
        r#"package main

import (
    "fmt"
    external "example.test/external"
    dep "example.test/project/pkg/dep"
    "example.test/project/pkg/other"
)

type Receiver struct{}

func (Receiver) Method() {}
func local() {}

func Main(parameter func(), callbacks []func(), receiver Receiver, errorReceiver interface{ Error() }) {
    cleanup := func() {}
    cleanup()
    parameter()
    fmt.Println("not a project call")
    external.Call()
    dep.Run()
    other.Use()
    Receiver{}.Method()
    receiver.Method()
    errorReceiver.Error()
    local()
    PackageLocal()
    Ambiguous()
    for callback := range callbacks {
        callback()
    }
}
"#,
    );
    write_go(
        dir.path(),
        "cmd/other.go",
        "package main\nfunc PackageLocal() {}\n",
    );
    let dep_a = write_go(dir.path(), "pkg/dep/a.go", "package dep\nfunc Run() {}\n");
    write_go(
        dir.path(),
        "pkg/dep/0_test.go",
        "package dep_test\nfunc Run() {}\n",
    );
    write_go(
        dir.path(),
        "pkg/dep/z.go",
        "package dep\ntype Receiver struct{}\nfunc (Receiver) Run() {}\nfunc Spare() {}\n",
    );
    let other = write_go(
        dir.path(),
        "pkg/other/other.go",
        "package other\nfunc Use() {}\n",
    );
    let noise = write_go(
        dir.path(),
        "pkg/noise/noise.go",
        r#"package noise

type Cache struct{}
func (Cache) cleanup() {}
func Println() {}
func (Cache) parameter() {}
func PackageLocal() {}
func callback() {}
func (Cache) Error() {}
type Receiver struct{}
func (Receiver) Method() {}
"#,
    );
    write_go(
        dir.path(),
        "pkg/ambiguous/one.go",
        "package ambiguous\nfunc Ambiguous() {}\n",
    );
    write_go(
        dir.path(),
        "pkg/other_ambiguous/two.go",
        "package other_ambiguous\nfunc Ambiguous() {}\n",
    );

    (dir, main, dep_a, other, noise)
}

#[tokio::test]
async fn go_project_imports_resolve_to_deterministic_package_representatives() {
    let (dir, main, dep_a, other, _) = fixture();
    let root = code_graph_core::paths::canonicalize(dir.path()).expect("canonicalize root");
    let server = go_only_server();
    analyze(&server, &root).await;

    let main = main.to_string_lossy().into_owned();
    let dep_a = dep_a.to_string_lossy().into_owned();
    let other = other.to_string_lossy().into_owned();

    let first = ok_json(&get_dependencies(
        &server.inner.graph,
        &main,
        Some(1),
        Some(0),
        NO_BYTE_BUDGET,
    ));
    assert_eq!(first["total"], 2, "only indexed project imports survive");
    assert_eq!(first["offset"], 0);
    assert_eq!(first["limit"], 1);
    assert_eq!(first["truncated"], true);
    assert_eq!(first["next_offset"], 1);
    let first_rows = first["results"].as_array().expect("dependency page rows");
    assert_eq!(first_rows.len(), 1);
    assert_eq!(first_rows[0]["file"], dep_a);
    assert_eq!(first_rows[0]["kind"], "includes");

    let second = ok_json(&get_dependencies(
        &server.inner.graph,
        &main,
        Some(1),
        Some(1),
        NO_BYTE_BUDGET,
    ));
    assert_eq!(second["total"], 2);
    assert_eq!(second["offset"], 1);
    assert_eq!(second["limit"], 1);
    assert_eq!(second["truncated"], false);
    assert_eq!(second["next_offset"], serde_json::Value::Null);
    let second_rows = second["results"].as_array().expect("dependency page rows");
    assert_eq!(second_rows.len(), 1);
    assert_eq!(second_rows[0]["file"], other);
    assert_eq!(second_rows[0]["kind"], "includes");
}

#[tokio::test]
async fn go_resolution_rejects_bound_and_external_calls_without_losing_valid_calls() {
    let (dir, main, _dep_a, _other, noise) = fixture();
    let root = code_graph_core::paths::canonicalize(dir.path()).expect("canonicalize root");
    let server = go_only_server();
    analyze(&server, &root).await;

    let main = main.to_string_lossy().into_owned();
    let noise = noise.to_string_lossy().into_owned();
    let caller = format!("{main}:Main");

    let any = ok_json(&callers_or_callees(
        &server.inner.graph,
        &caller,
        Some(1),
        Direction::Callees,
        Some(50),
        Some(0),
        NO_BYTE_BUDGET,
        None,
    ));
    let any_ids: Vec<&str> = any["results"]
        .as_array()
        .expect("callee rows")
        .iter()
        .map(|row| row["symbol_id"].as_str().expect("callee symbol ID"))
        .collect();
    assert!(
        any_ids.iter().any(|id| id.ends_with("pkg/dep/a.go:Run")),
        "imported project package call must resolve: {any_ids:?}"
    );
    assert!(
        any_ids
            .iter()
            .any(|id| id.ends_with("cmd/main.go:Receiver::Method")),
        "resolvable local receiver method call must remain: {any_ids:?}"
    );
    assert!(
        any_ids.iter().any(|id| id.ends_with("cmd/main.go:local")),
        "same-package local function call must remain: {any_ids:?}"
    );
    assert!(
        any_ids
            .iter()
            .any(|id| id.ends_with("cmd/other.go:PackageLocal")),
        "same-package cross-file call must resolve: {any_ids:?}"
    );
    assert!(
        !any_ids.iter().any(|id| id.contains("pkg/noise/noise.go")),
        "bound local/parameter and qualified external calls must not resolve to noise: {any_ids:?}"
    );

    let resolved = ok_json(&callers_or_callees(
        &server.inner.graph,
        &caller,
        Some(1),
        Direction::Callees,
        Some(50),
        Some(0),
        NO_BYTE_BUDGET,
        Some("resolved"),
    ));
    let resolved_ids: Vec<&str> = resolved["results"]
        .as_array()
        .expect("resolved callee rows")
        .iter()
        .map(|row| row["symbol_id"].as_str().expect("callee symbol ID"))
        .collect();
    assert!(
        resolved_ids
            .iter()
            .any(|id| id.ends_with("pkg/dep/a.go:Run")),
        "resolved mode retains imported project call: {resolved_ids:?}"
    );
    assert!(
        resolved_ids
            .iter()
            .any(|id| id.ends_with("cmd/main.go:Receiver::Method")),
        "resolved mode retains receiver method: {resolved_ids:?}"
    );
    assert!(
        !resolved_ids.iter().any(|id| id.ends_with(":Ambiguous")),
        "resolved mode excludes genuinely ambiguous calls: {resolved_ids:?}"
    );
    assert!(
        !resolved_ids
            .iter()
            .any(|id| id.contains("pkg/noise/noise.go")),
        "resolved mode must not revive rejected false calls: {resolved_ids:?}"
    );

    let coupling = ok_json(&get_coupling(
        &server.inner.graph,
        &main,
        Some("outgoing"),
        Some(50),
        Some(0),
        NO_BYTE_BUDGET,
    ));
    let coupled: Vec<&str> = coupling["results"]
        .as_array()
        .expect("coupling rows")
        .iter()
        .map(|row| row["file"].as_str().expect("coupled file"))
        .collect();
    assert!(
        !coupled.contains(&noise.as_str()),
        "false calls must not create coupling to noise.go: {coupled:?}"
    );
}

#[tokio::test]
async fn go_dot_import_calls_do_not_fall_back_to_unrelated_project_symbols() {
    let dir = TempDir::new().expect("TempDir");
    std::fs::write(dir.path().join("go.mod"), "module example.test/project\n")
        .expect("write go.mod");
    let main = write_go(
        dir.path(),
        "cmd/main.go",
        "package main\nimport . \"example.test/project/pkg/dot\"\nfunc local() {}\nfunc Main() { Run(); local() }\n",
    );
    write_go(dir.path(), "pkg/dot/dot.go", "package dot\nfunc Run() {}\n");
    let noise = write_go(
        dir.path(),
        "pkg/noise/noise.go",
        "package noise\nfunc Run() {}\n",
    );
    let root = code_graph_core::paths::canonicalize(dir.path()).expect("canonicalize root");
    let server = go_only_server();
    analyze(&server, &root).await;

    let caller = format!("{}:Main", main.to_string_lossy());
    let result = ok_json(&callers_or_callees(
        &server.inner.graph,
        &caller,
        Some(1),
        Direction::Callees,
        Some(50),
        Some(0),
        NO_BYTE_BUDGET,
        None,
    ));
    let ids: Vec<&str> = result["results"]
        .as_array()
        .expect("callee rows")
        .iter()
        .map(|row| row["symbol_id"].as_str().expect("callee symbol ID"))
        .collect();
    assert!(
        !ids.iter()
            .any(|id| id.contains(noise.to_string_lossy().as_ref())),
        "dot-import call must not resolve to an unrelated project symbol: {ids:?}"
    );
    assert!(
        ids.iter().any(|id| id.ends_with("cmd/main.go:local")),
        "dot import must not hide a unique local package call: {ids:?}"
    );
}

#[tokio::test]
async fn go_package_variables_in_sibling_files_block_generic_call_resolution() {
    let dir = TempDir::new().expect("TempDir");
    std::fs::write(dir.path().join("go.mod"), "module example.test/project\n")
        .expect("write go.mod");
    let main = write_go(
        dir.path(),
        "pkg/current/main.go",
        "package current\nfunc Main() { callback() }\n",
    );
    write_go(
        dir.path(),
        "pkg/current/values.go",
        "package current\nvar callback = func() {}\n",
    );
    let noise = write_go(
        dir.path(),
        "pkg/noise/noise.go",
        "package noise\nfunc callback() {}\n",
    );
    let root = code_graph_core::paths::canonicalize(dir.path()).expect("canonicalize root");
    let server = go_only_server();
    analyze(&server, &root).await;

    let caller = format!("{}:Main", main.to_string_lossy());
    let result = ok_json(&callers_or_callees(
        &server.inner.graph,
        &caller,
        Some(1),
        Direction::Callees,
        Some(50),
        Some(0),
        NO_BYTE_BUDGET,
        None,
    ));
    let ids: Vec<&str> = result["results"]
        .as_array()
        .expect("callee rows")
        .iter()
        .map(|row| row["symbol_id"].as_str().expect("callee symbol ID"))
        .collect();
    assert!(
        !ids.iter()
            .any(|id| id.contains(noise.to_string_lossy().as_ref())),
        "package variable call must not resolve to another package's function: {ids:?}"
    );
}

#[tokio::test]
async fn go_bare_calls_do_not_resolve_builtins_or_unrelated_single_candidates() {
    let dir = TempDir::new().expect("TempDir");
    std::fs::write(dir.path().join("go.mod"), "module example.test/project\n")
        .expect("write go.mod");
    let main = write_go(
        dir.path(),
        "cmd/main.go",
        "package main\nfunc Main() { len(nil); External() }\n",
    );
    write_go(
        dir.path(),
        "pkg/noise/noise.go",
        "package noise\nfunc External() {}\n",
    );
    let root = code_graph_core::paths::canonicalize(dir.path()).expect("canonicalize root");
    let server = go_only_server();
    analyze(&server, &root).await;

    let caller = format!("{}:Main", main.to_string_lossy());
    let result = ok_json(&callers_or_callees(
        &server.inner.graph,
        &caller,
        Some(1),
        Direction::Callees,
        Some(50),
        Some(0),
        NO_BYTE_BUDGET,
        None,
    ));
    assert!(
        result["results"]
            .as_array()
            .expect("callee rows")
            .is_empty(),
        "builtins and unrelated sole candidates must remain unresolved: {result}"
    );
}

#[tokio::test]
async fn go_without_module_keeps_same_named_packages_directory_local() {
    let dir = TempDir::new().expect("TempDir");
    let caller = write_go(
        dir.path(),
        "first/caller.go",
        "package util\nfunc Caller() { Target() }\n",
    );
    let local_target = write_go(
        dir.path(),
        "first/target.go",
        "package util\nfunc Target() {}\n",
    );
    let other_target = write_go(
        dir.path(),
        "second/target.go",
        "package util\nfunc Target() {}\n",
    );
    let root = code_graph_core::paths::canonicalize(dir.path()).expect("canonicalize root");
    let server = go_only_server();
    analyze(&server, &root).await;

    let result = ok_json(&callers_or_callees(
        &server.inner.graph,
        &format!("{}:Caller", caller.to_string_lossy()),
        Some(1),
        Direction::Callees,
        Some(50),
        Some(0),
        NO_BYTE_BUDGET,
        None,
    ));
    let ids: Vec<&str> = result["results"]
        .as_array()
        .expect("callee rows")
        .iter()
        .map(|row| row["symbol_id"].as_str().expect("symbol ID"))
        .collect();
    assert!(
        ids.iter()
            .any(|id| id.starts_with(local_target.to_string_lossy().as_ref())),
        "same-directory package symbols must remain visible without go.mod: {ids:?}"
    );
    assert!(
        ids.iter()
            .all(|id| !id.starts_with(other_target.to_string_lossy().as_ref())),
        "same-named packages in other directories must stay isolated without go.mod: {ids:?}"
    );
}

#[tokio::test]
async fn go_control_initializer_shadowing_never_resolves_the_outer_receiver() {
    let dir = TempDir::new().expect("TempDir");
    std::fs::write(dir.path().join("go.mod"), "module example.test/project\n")
        .expect("write go.mod");
    let source = write_go(
        dir.path(),
        "control.go",
        r#"package control
import "strings"
type Outer struct{}
func (Outer) Check() bool { return true }
func (Outer) Stream() []Inner { return nil }
func (Outer) Channel() <-chan Inner { return nil }
func (Outer) OuterStream() []Outer { return nil }
func (Outer) OuterChannel() <-chan Outer { return nil }
func (Outer) AsAny() any { return nil }
type Inner struct{}
func (Inner) Check() bool { return true }
func makeInner() Inner { return Inner{} }
func IfString(s Outer) {
    if s := makeInner(); strings.Contains("a:b", ":") && s.Check() {}
}
func IfComment(s Outer) {
    if s := makeInner(); true /* a:b */ && s.Check() {}
}
func SwitchBody(s Outer) {
    switch s := makeInner(); true {
    case true:
        s.Check()
    }
}
func ForClause(s Outer) {
    for s := makeInner(); s.Check(); { s.Check() }
}
func ForRange(s Outer) {
    for _, s := range s.Stream() { s.Check() }
}
func ForRangeAssign(s Outer) {
    for s = range s.OuterStream() { s.Check() }
}
func TypeSwitch(s Outer) {
    switch s := s.AsAny().(type) {
    case Outer:
        s.Check()
    }
}
func SelectReceive(s Outer) {
    select {
    case s := <-s.Channel():
        s.Check()
    }
}
func SelectAssign(s Outer) {
    select {
    case s = <-s.OuterChannel():
        s.Check()
    }
}
"#,
    );
    let root = code_graph_core::paths::canonicalize(dir.path()).expect("canonicalize root");
    let server = go_only_server();
    analyze(&server, &root).await;

    for caller in [
        "IfString",
        "IfComment",
        "SwitchBody",
        "ForClause",
        "ForRange",
        "TypeSwitch",
        "SelectReceive",
    ] {
        let result = ok_json(&callers_or_callees(
            &server.inner.graph,
            &format!("{}:{caller}", source.to_string_lossy()),
            Some(1),
            Direction::Callees,
            Some(50),
            Some(0),
            NO_BYTE_BUDGET,
            None,
        ));
        assert!(
            result["results"]
                .as_array()
                .expect("callee rows")
                .iter()
                .all(|row| !row["symbol_id"]
                    .as_str()
                    .is_some_and(|id| id.ends_with("control.go:Outer::Check"))),
            "{caller} must not resolve its shadowed receiver to Outer::Check: {result}"
        );
    }

    for caller in ["ForRangeAssign", "SelectAssign"] {
        let result = ok_json(&callers_or_callees(
            &server.inner.graph,
            &format!("{}:{caller}", source.to_string_lossy()),
            Some(1),
            Direction::Callees,
            Some(50),
            Some(0),
            NO_BYTE_BUDGET,
            None,
        ));
        assert!(
            result["results"]
                .as_array()
                .expect("callee rows")
                .iter()
                .any(|row| row["symbol_id"]
                    .as_str()
                    .is_some_and(|id| id.ends_with("control.go:Outer::Check"))),
            "{caller} assigns rather than declares and must retain Outer::Check: {result}"
        );
    }
}

#[tokio::test]
async fn go_typed_local_and_composite_receivers_resolve_but_unknown_and_chained_do_not() {
    let dir = TempDir::new().expect("TempDir");
    std::fs::write(dir.path().join("go.mod"), "module example.test/project\n")
        .expect("write go.mod");
    let main = write_go(
        dir.path(),
        "cmd/main.go",
        r#"package main
import dep "example.test/project/pkg/dep"
type Client struct{}
func (Client) Ping() {}
type First struct{}
func (First) Ping() {}
type Second struct{}
func (Second) Ping() {}
func Main(client dep.Client) {
    var r Client; r.Ping()
    short := Client{}; short.Ping()
    pointer := &Client{}; pointer.Ping()
    var assigned = Client{}; assigned.Ping()
    client.Ping()
    unknown.Ping()
    unknown.Next().Ping()
}
func PointerLocal() { var pointer *Client; pointer.Ping() }
func MultiLocal() {
    first, second := First{}, Second{}
    second.Ping()
    var third, fourth = First{}, Second{}
    fourth.Ping()
}
"#,
    );
    write_go(
        dir.path(),
        "pkg/dep/dep.go",
        "package dep\ntype Client struct{}\nfunc (Client) Ping() {}\n",
    );
    let noise = write_go(
        dir.path(),
        "pkg/noise/noise.go",
        "package noise\ntype Client struct{}\nfunc (Client) Ping() {}\n",
    );
    let root = code_graph_core::paths::canonicalize(dir.path()).expect("canonicalize root");
    let server = go_only_server();
    analyze(&server, &root).await;

    let caller = format!("{}:Main", main.to_string_lossy());
    let result = ok_json(&callers_or_callees(
        &server.inner.graph,
        &caller,
        Some(1),
        Direction::Callees,
        Some(50),
        Some(0),
        NO_BYTE_BUDGET,
        None,
    ));
    let ids: Vec<&str> = result["results"]
        .as_array()
        .expect("callee rows")
        .iter()
        .map(|row| row["symbol_id"].as_str().expect("callee symbol ID"))
        .collect();
    assert!(
        ids.iter()
            .any(|id| id.ends_with("cmd/main.go:Client::Ping")),
        "typed local declarations must retain a concrete receiver call: {ids:?}"
    );
    assert!(
        ids.iter()
            .any(|id| id.ends_with("pkg/dep/dep.go:Client::Ping")),
        "qualified imported parameter type must resolve to its imported package: {ids:?}"
    );
    assert!(
        !ids.iter()
            .any(|id| id.contains(noise.to_string_lossy().as_ref())),
        "unknown/chained receivers must not hit unrelated methods: {ids:?}"
    );

    let pointer_result = ok_json(&callers_or_callees(
        &server.inner.graph,
        &format!("{}:PointerLocal", main.to_string_lossy()),
        Some(1),
        Direction::Callees,
        Some(50),
        Some(0),
        NO_BYTE_BUDGET,
        None,
    ));
    assert!(
        pointer_result["results"]
            .as_array()
            .expect("pointer-local callee rows")
            .iter()
            .any(|row| row["symbol_id"]
                .as_str()
                .is_some_and(|id| id.ends_with("cmd/main.go:Client::Ping"))),
        "explicit pointer locals must normalize to their bare receiver type: {pointer_result}"
    );

    let multi_result = ok_json(&callers_or_callees(
        &server.inner.graph,
        &format!("{}:MultiLocal", main.to_string_lossy()),
        Some(1),
        Direction::Callees,
        Some(50),
        Some(0),
        NO_BYTE_BUDGET,
        None,
    ));
    assert!(
        multi_result["results"]
            .as_array()
            .expect("multi-local callee rows")
            .iter()
            .all(|row| !row["symbol_id"]
                .as_str()
                .is_some_and(|id| id.ends_with("cmd/main.go:First::Ping"))),
        "multi-name inferred declarations must not reuse the first initializer type: {multi_result}"
    );
}

#[tokio::test]
async fn go_internal_test_package_can_call_production_without_leaking_back() {
    let dir = TempDir::new().expect("TempDir");
    std::fs::write(dir.path().join("go.mod"), "module example.test/project\n")
        .expect("write go.mod");
    let production = write_go(
        dir.path(),
        "pkg/prod/main.go",
        "package prod\nfunc Production() {}\nfunc Main() { TestOnly() }\n",
    );
    let internal_test = write_go(
        dir.path(),
        "pkg/prod/main_test.go",
        "package prod\nfunc TestOnly() {}\nfunc TestMain() { Production() }\n",
    );
    let root = code_graph_core::paths::canonicalize(dir.path()).expect("canonicalize root");
    let server = go_only_server();
    analyze(&server, &root).await;

    let production_result = ok_json(&callers_or_callees(
        &server.inner.graph,
        &format!("{}:Main", production.to_string_lossy()),
        Some(1),
        Direction::Callees,
        Some(50),
        Some(0),
        NO_BYTE_BUDGET,
        None,
    ));
    assert!(
        production_result["results"]
            .as_array()
            .expect("production callee rows")
            .is_empty(),
        "production files must not see test-only declarations: {production_result}"
    );

    let test_result = ok_json(&callers_or_callees(
        &server.inner.graph,
        &format!("{}:TestMain", internal_test.to_string_lossy()),
        Some(1),
        Direction::Callees,
        Some(50),
        Some(0),
        NO_BYTE_BUDGET,
        None,
    ));
    assert!(
        test_result["results"]
            .as_array()
            .expect("test callee rows")
            .iter()
            .any(|row| row["symbol_id"]
                .as_str()
                .is_some_and(|id| id.ends_with("pkg/prod/main.go:Production"))),
        "internal tests must retain production-package visibility: {test_result}"
    );
}

#[tokio::test]
async fn go_external_test_package_duplicate_does_not_ambiguate_production_call() {
    let dir = TempDir::new().expect("TempDir");
    std::fs::write(dir.path().join("go.mod"), "module example.test/project\n")
        .expect("write go.mod");
    let main = write_go(
        dir.path(),
        "pkg/prod/main.go",
        "package prod\nfunc Func() {}\nfunc Main() { Func() }\n",
    );
    let external_test = write_go(
        dir.path(),
        "pkg/prod/main_test.go",
        "package prod_test\nfunc Func() {}\n",
    );
    let root = code_graph_core::paths::canonicalize(dir.path()).expect("canonicalize root");
    let server = go_only_server();
    analyze(&server, &root).await;

    let caller = format!("{}:Main", main.to_string_lossy());
    let result = ok_json(&callers_or_callees(
        &server.inner.graph,
        &caller,
        Some(1),
        Direction::Callees,
        Some(50),
        Some(0),
        NO_BYTE_BUDGET,
        None,
    ));
    let ids: Vec<&str> = result["results"]
        .as_array()
        .expect("callee rows")
        .iter()
        .map(|row| row["symbol_id"].as_str().expect("callee symbol ID"))
        .collect();
    assert!(ids.iter().any(|id| id.ends_with("pkg/prod/main.go:Func")));
    assert!(
        !ids.iter()
            .any(|id| id.contains(external_test.to_string_lossy().as_ref())),
        "external _test package must not collide with production package: {ids:?}"
    );
}

#[tokio::test]
async fn go_scoped_analyze_uses_cached_sibling_package_bindings() {
    let dir = TempDir::new().expect("TempDir");
    std::fs::write(dir.path().join(".code-graph.toml"), "").expect("write project config");
    std::fs::write(dir.path().join("go.mod"), "module example.test/project\n")
        .expect("write go.mod");
    let values = write_go(
        dir.path(),
        "pkg/current/values.go",
        "package current\nvar callback = func() {}\n",
    );
    let caller_path = dir.path().join("pkg/current/main.go");
    let candidate = write_go(
        dir.path(),
        "pkg/current/candidate.go",
        "package current\nfunc callback() {}\n",
    );
    let root = code_graph_core::paths::canonicalize(dir.path()).expect("canonicalize root");
    let server = go_only_server();
    // Force only the first root scope to establish the cache. Before the
    // second non-force pass, ignore the sibling without deleting it: its
    // cached graph survives while the newly-created caller is fresh.
    analyze_with_force(&server, &root, true).await;
    drop(server);
    // Diverge the ignored sibling on disk after the cache was written. The
    // cached graph still says `callback` is a package value, so resolver
    // metadata must come from that same cache snapshot rather than these newer
    // bytes (which would incorrectly enable the candidate function below).
    std::fs::write(&values, "package current\nvar renamed = func() {}\n")
        .expect("mutate ignored sibling after cache write");
    std::fs::write(
        &caller_path,
        "package current\nfunc Main() { callback() }\n",
    )
    .expect("write fresh caller");
    std::fs::write(
        dir.path().join(".code-graph.toml"),
        "[discovery]\nextra_ignore = [\"pkg/current/values.go\"]\n",
    )
    .expect("ignore cached sibling on second pass");
    let main = code_graph_core::paths::canonicalize(&caller_path).expect("canonicalize caller");
    // A new parser instance has no process-local metadata. The second analyze
    // must restore resolver context from the cache-loaded file universe, not
    // parse the newer ignored sibling from disk.
    let server = go_only_server();
    analyze_with_force(&server, &root, false).await;

    let caller = format!("{}:Main", main.to_string_lossy());
    let result = ok_json(&callers_or_callees(
        &server.inner.graph,
        &caller,
        Some(1),
        Direction::Callees,
        Some(50),
        Some(0),
        NO_BYTE_BUDGET,
        None,
    ));
    assert!(
        result["results"]
            .as_array()
            .expect("callee rows")
            .iter()
            .all(|row| !row["symbol_id"]
                .as_str()
                .expect("symbol ID")
                .contains(candidate.to_string_lossy().as_ref())),
        "cached sibling package value must block fresh caller fallback: {result}"
    );
}

#[tokio::test]
async fn go_scoped_analyze_keeps_cached_function_when_ignored_disk_file_becomes_value() {
    let dir = TempDir::new().expect("TempDir");
    std::fs::write(dir.path().join(".code-graph.toml"), "").expect("write project config");
    std::fs::write(dir.path().join("go.mod"), "module example.test/project\n")
        .expect("write go.mod");
    let target = write_go(
        dir.path(),
        "pkg/current/target.go",
        "package current\nfunc callback() {}\n",
    );
    let caller_path = dir.path().join("pkg/current/main.go");
    let root = code_graph_core::paths::canonicalize(dir.path()).expect("canonicalize root");
    let server = go_only_server();
    analyze_with_force(&server, &root, true).await;
    drop(server);

    // The disk file now declares a value, but it is excluded from the scoped
    // refresh. Cached symbols and cached resolver metadata must remain one
    // coherent old snapshot, so the fresh caller still resolves to the cached
    // function instead of being suppressed by newer on-disk metadata.
    std::fs::write(&target, "package current\nvar callback = func() {}\n")
        .expect("mutate ignored target after cache write");
    std::fs::write(
        &caller_path,
        "package current\nfunc Main() { callback() }\n",
    )
    .expect("write fresh caller");
    std::fs::write(
        dir.path().join(".code-graph.toml"),
        "[discovery]\nextra_ignore = [\"pkg/current/target.go\"]\n",
    )
    .expect("ignore cached target on second pass");
    let main = code_graph_core::paths::canonicalize(&caller_path).expect("canonicalize caller");

    let server = go_only_server();
    analyze_with_force(&server, &root, false).await;
    let caller = format!("{}:Main", main.to_string_lossy());
    let result = ok_json(&callers_or_callees(
        &server.inner.graph,
        &caller,
        Some(1),
        Direction::Callees,
        Some(50),
        Some(0),
        NO_BYTE_BUDGET,
        None,
    ));
    assert!(
        result["results"]
            .as_array()
            .expect("callee rows")
            .iter()
            .any(|row| row["symbol_id"]
                .as_str()
                .is_some_and(|id| id == format!("{}:callback", target.to_string_lossy()))),
        "cached function and metadata must remain coherent despite newer ignored bytes: {result}"
    );
}

#[tokio::test]
async fn footerless_v13_go_cache_upgrades_during_scoped_resolution() {
    let dir = TempDir::new().expect("TempDir");
    std::fs::write(dir.path().join(".code-graph.toml"), "").expect("write project config");
    std::fs::write(dir.path().join("go.mod"), "module example.test/project\n")
        .expect("write go.mod");
    let target = write_go(
        dir.path(),
        "pkg/current/target.go",
        "package current\nvar callback = func() {}\n",
    );
    let root = code_graph_core::paths::canonicalize(dir.path()).expect("canonicalize root");
    let server = go_only_server();
    analyze_with_force(&server, &root, true).await;
    drop(server);
    strip_resolver_metadata_extension(dir.path());

    // Diverge the ignored file after the footerless cache was written. The
    // migration must refresh its complete graph and metadata together rather
    // than pair this new function metadata with the old value-only graph.
    std::fs::write(&target, "package current\nfunc callback() {}\n")
        .expect("mutate legacy cached target");

    let main = write_go(
        dir.path(),
        "pkg/current/main.go",
        "package current\nfunc Main() { callback() }\n",
    );
    std::fs::write(
        dir.path().join(".code-graph.toml"),
        "[discovery]\nextra_ignore = [\"pkg/current/target.go\"]\n",
    )
    .expect("ignore cached target during upgrade pass");
    let server = go_only_server();
    analyze_with_force(&server, &root, false).await;

    let result = ok_json(&callers_or_callees(
        &server.inner.graph,
        &format!("{}:Main", main.to_string_lossy()),
        Some(1),
        Direction::Callees,
        Some(50),
        Some(0),
        NO_BYTE_BUDGET,
        None,
    ));
    let target_id = format!("{}:callback", target.display());
    assert!(
        result["results"]
            .as_array()
            .expect("callee rows")
            .iter()
            .any(|row| row["symbol_id"].as_str() == Some(target_id.as_str())),
        "fresh caller must resolve against a footerless-v13 cached Go target: {result}"
    );
    assert!(
        std::fs::read(dir.path().join(".code-graph-cache.db"))
            .expect("read upgraded cache")
            .ends_with(b"CGMETA01"),
        "the compatibility metadata must be persisted immediately"
    );
}

#[tokio::test]
async fn footerless_v13_go_cache_bypasses_unchanged_fast_path_for_upgrade() {
    let dir = TempDir::new().expect("TempDir");
    std::fs::write(dir.path().join(".code-graph.toml"), "").expect("write project config");
    std::fs::write(dir.path().join("go.mod"), "module example.test/project\n")
        .expect("write go.mod");
    write_go(
        dir.path(),
        "pkg/current/target.go",
        "package current\nfunc callback() {}\n",
    );
    let root = code_graph_core::paths::canonicalize(dir.path()).expect("canonicalize root");
    let server = go_only_server();
    analyze_with_force(&server, &root, true).await;
    drop(server);
    strip_resolver_metadata_extension(dir.path());

    let server = go_only_server();
    analyze_with_force(&server, &root, false).await;
    assert!(
        std::fs::read(dir.path().join(".code-graph-cache.db"))
            .expect("read upgraded cache")
            .ends_with(b"CGMETA01"),
        "an unchanged footerless Go cache must still run the metadata upgrade"
    );
}

#[tokio::test]
async fn go_non_force_analyze_replaces_cached_symbols_before_resolution() {
    let dir = TempDir::new().expect("TempDir");
    std::fs::write(dir.path().join(".code-graph.toml"), "").expect("write project config");
    std::fs::write(dir.path().join("go.mod"), "module example.test/project\n")
        .expect("write go.mod");
    let target = write_go(
        dir.path(),
        "pkg/current/target.go",
        "package current\nfunc Target() {}\n",
    );
    let root = code_graph_core::paths::canonicalize(dir.path()).expect("canonicalize root");
    let server = go_only_server();
    analyze_with_force(&server, &root, true).await;
    drop(server);

    let caller = write_go(
        dir.path(),
        "pkg/current/caller.go",
        "package current\nfunc Caller() { Target() }\n",
    );
    let server = go_only_server();
    analyze_with_force(&server, &root, false).await;

    let result = ok_json(&callers_or_callees(
        &server.inner.graph,
        &format!("{}:Caller", caller.to_string_lossy()),
        Some(1),
        Direction::Callees,
        Some(50),
        Some(0),
        NO_BYTE_BUDGET,
        None,
    ));
    let expected = format!("{}:Target", target.to_string_lossy());
    assert!(
        result["results"]
            .as_array()
            .expect("callee rows")
            .iter()
            .any(|row| row["symbol_id"].as_str() == Some(expected.as_str())),
        "fresh paths must replace same-path cached symbols before uniqueness checks: {result}"
    );
}
