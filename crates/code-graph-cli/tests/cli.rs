//! End-to-end tests for the `code-graph` binary (phase 7, task 7.2).
//!
//! Pins the task's verification surface: the three phase 1 queries are
//! invocable from the CLI (AC-33's CLI half), a query against an unindexed
//! repository reports the byte-exact domain error the MCP surface does,
//! exit statuses separate the three outcome classes (FR-20), and identical
//! invocations produce identical machine-readable output with and without
//! a running daemon (AC-40, FR-18) — including through the Decision 7
//! unindexed-daemon window.
//!
//! The daemon-backed tests need the sibling `code-graph-mcp` binary. When
//! it has not been built (a `cargo test -p code-graph-cli` invocation on a
//! cold target dir), they auto-skip with a setup hint — the dogfood
//! submodule precedent — and run under `cargo test --workspace`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

const READY_TIMEOUT: Duration = Duration::from_secs(10);

fn cli() -> &'static str {
    env!("CARGO_BIN_EXE_code-graph")
}

/// The sibling proxy/daemon binary, when it has been built.
fn mcp_binary() -> Option<PathBuf> {
    let sibling = Path::new(cli())
        .parent()?
        .join(format!("code-graph-mcp{}", std::env::consts::EXE_SUFFIX));
    sibling.exists().then_some(sibling)
}

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(cli())
        .arg("--root")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run code-graph")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr is UTF-8")
}

fn exit_code(output: &Output) -> i32 {
    output.status.code().expect("exit code")
}

/// Owns a daemon process for a CLI process test. Drop uses the platform's
/// normal daemon stop route first, then boundedly kills a wedged child so a
/// failed assertion cannot retain the TempDir through a live daemon.
struct DaemonChild {
    child: std::process::Child,
    root: PathBuf,
    metadata_path: PathBuf,
}

impl DaemonChild {
    fn spawn(binary: &Path, root: &Path) -> Self {
        let child = Command::new(binary)
            .arg("--serve")
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn daemon");
        let metadata_path = root.join(".code-graph/daemon.json");
        let mut daemon = Self {
            child,
            root: root.to_path_buf(),
            metadata_path,
        };
        let deadline = Instant::now() + READY_TIMEOUT;
        while !daemon.metadata_path.exists() {
            assert!(
                daemon.child.try_wait().ok().flatten().is_none(),
                "daemon exited before publishing metadata"
            );
            assert!(Instant::now() < deadline, "daemon did not publish metadata");
            std::thread::sleep(Duration::from_millis(20));
        }
        daemon
    }

    fn stop(&mut self) {
        if self.child.try_wait().ok().flatten().is_some() {
            return;
        }
        if self.metadata_path.exists() {
            stop_daemon_best_effort(&self.root, &self.metadata_path);
        }
        let deadline = Instant::now() + READY_TIMEOUT;
        while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if self.child.try_wait().ok().flatten().is_none() {
            // Bounded-shutdown diagnostic (task 12.8): name the last
            // observed process/control-file state before the hard kill so
            // a timed-out graceful stop is attributable.
            let runtime = self.root.join(".code-graph");
            eprintln!(
                "daemon child {} did not exit within {READY_TIMEOUT:?} after a graceful \
                 stop request; killing. Last observed control-file state: \
                 daemon.json={}, daemon.lock={}, shutdown.request={}, shutdown.ack={}",
                self.child.id(),
                self.metadata_path.exists(),
                runtime.join("daemon.lock").exists(),
                runtime.join("shutdown.request").exists(),
                runtime.join("shutdown.ack").exists(),
            );
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

impl Drop for DaemonChild {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Two Rust files with a cross-file call chain, so call-graph queries,
/// find-path, and community detection all have something to answer.
fn fixture() -> TempDir {
    let dir = TempDir::new().expect("fixture tempdir");
    std::fs::write(
        dir.path().join("alpha.rs"),
        "pub fn entry_point() -> u32 {\n    helper_one() + helper_two()\n}\n\npub fn helper_one() -> u32 {\n    1\n}\n",
    )
    .expect("write alpha.rs");
    std::fs::write(
        dir.path().join("beta.rs"),
        "pub fn helper_two() -> u32 {\n    2\n}\n\npub fn leaf() -> u32 {\n    helper_two()\n}\n",
    )
    .expect("write beta.rs");
    dir
}

fn canonical(root: &Path) -> PathBuf {
    code_graph_core::paths::canonicalize(root).expect("canonicalize fixture root")
}

fn symbol_id(root: &Path, file: &str, name: &str) -> String {
    format!("{}:{name}", canonical(root).join(file).to_string_lossy())
}

fn analyze(root: &Path) {
    let output = run(root, &["analyze-codebase"]);
    assert_eq!(
        exit_code(&output),
        0,
        "analyze-codebase failed: {}",
        stderr(&output)
    );
}

/// AC-33 (CLI half): position lookup, shortest path, and community
/// detection are invocable from the CLI, with machine output parsing as
/// the documented shapes.
#[test]
fn phase_one_queries_are_invocable_from_the_cli() {
    let dir = fixture();
    analyze(dir.path());

    let at = run(
        dir.path(),
        &[
            "get-symbol-at",
            &canonical(dir.path()).join("alpha.rs").to_string_lossy(),
            "2",
            "--json",
        ],
    );
    assert_eq!(exit_code(&at), 0, "get-symbol-at: {}", stderr(&at));
    let at_body: serde_json::Value =
        serde_json::from_str(&stdout(&at)).expect("get-symbol-at payload parses");
    assert_eq!(
        at_body["results"][0]["name"],
        serde_json::json!("entry_point"),
        "line 2 is inside entry_point: {at_body}"
    );

    let from = symbol_id(dir.path(), "alpha.rs", "entry_point");
    let to = symbol_id(dir.path(), "beta.rs", "helper_two");
    let path = run(dir.path(), &["find-path", &from, &to, "--json"]);
    assert_eq!(exit_code(&path), 0, "find-path: {}", stderr(&path));
    let path_body: serde_json::Value =
        serde_json::from_str(&stdout(&path)).expect("find-path payload parses");
    assert_eq!(path_body["found"], serde_json::json!(true));
    assert_eq!(path_body["hop_count"], serde_json::json!(1));

    let communities = run(
        dir.path(),
        &["detect-communities", "--json", "--limit", "5"],
    );
    assert_eq!(
        exit_code(&communities),
        0,
        "detect-communities: {}",
        stderr(&communities)
    );
    let communities_body: serde_json::Value =
        serde_json::from_str(&stdout(&communities)).expect("detect-communities payload parses");
    assert!(
        communities_body["results"].is_array(),
        "flattened Page envelope: {communities_body}"
    );
    assert_eq!(communities_body["granularity"], serde_json::json!("file"));
}

/// The unindexed domain error is byte-identical to the MCP surface's
/// (computed from the core guard, not duplicated), and exit statuses
/// separate the three outcome classes.
#[test]
fn exit_statuses_separate_the_three_outcome_classes() {
    // Tool error, unindexed: fresh directory, no cache.
    let dir = TempDir::new().expect("empty tempdir");
    std::fs::write(dir.path().join("lone.rs"), "pub fn lone() {}\n").expect("write source");
    let unindexed = run(dir.path(), &["get-callers", "whatever:symbol", "--json"]);
    assert_eq!(exit_code(&unindexed), 1, "unindexed is a tool error");
    let expected = code_graph_tools::core::require_indexed(false)
        .expect_err("guard message")
        .0;
    assert!(
        stderr(&unindexed).trim_end().ends_with(&expected),
        "stderr carries the MCP surface's domain error verbatim; got: {}",
        stderr(&unindexed)
    );
    assert!(
        stdout(&unindexed).is_empty(),
        "errors print nothing to stdout"
    );

    // Success (exit 0) including a success-shaped negative: found: false.
    let dir = fixture();
    analyze(dir.path());
    let no_path = run(
        dir.path(),
        &[
            "find-path",
            &symbol_id(dir.path(), "beta.rs", "helper_two"),
            &symbol_id(dir.path(), "alpha.rs", "entry_point"),
            "--json",
        ],
    );
    assert_eq!(exit_code(&no_path), 0, "found:false is SUCCESS");
    let body: serde_json::Value = serde_json::from_str(&stdout(&no_path)).expect("payload parses");
    assert_eq!(body["found"], serde_json::json!(false));

    // Tool error: unknown symbol (did-you-mean channel).
    let unknown = run(dir.path(), &["get-callers", "nope:nothing", "--json"]);
    assert_eq!(exit_code(&unknown), 1);
    assert!(
        stderr(&unknown).contains("symbol not found"),
        "tool error text on stderr: {}",
        stderr(&unknown)
    );

    // Operational failure: a DIRECTORY named like the cache file is a
    // genuine I/O error on the cache read path (not corrupt bytes, which
    // are honest-unindexed exit 1 by the Graph::load contract).
    let broken = TempDir::new().expect("broken tempdir");
    std::fs::create_dir(broken.path().join(".code-graph-cache.db")).expect("cache-shaped dir");
    std::fs::write(broken.path().join("lone.rs"), "pub fn lone() {}\n").expect("write source");
    let operational = run(broken.path(), &["get-status", "--json"]);
    assert_eq!(
        exit_code(&operational),
        2,
        "unreadable cache is operational: {}",
        stderr(&operational)
    );
}

/// FR-17's structural guarantee: the CLI crate depends on the typed core
/// only — no rmcp anywhere in its dependency declaration, no handlers
/// import anywhere in its sources (cargo-tree-shaped precedent:
/// `git_backend_dependency_is_confined_to_this_provider_crate`).
#[test]
fn cli_depends_on_the_typed_core_only() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest =
        std::fs::read_to_string(manifest_dir.join("Cargo.toml")).expect("read Cargo.toml");
    let declares_rmcp = manifest
        .lines()
        .map(|line| line.split('#').next().unwrap_or(""))
        .any(|code| code.trim_start().starts_with("rmcp"));
    assert!(
        !declares_rmcp,
        "code-graph-cli must not depend on rmcp (FR-17): the CLI would be \
         parsing wire envelopes instead of calling the typed core"
    );
    fn rust_sources(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read CLI source directory") {
            let entry = entry.expect("read CLI source entry");
            let path = entry.path();
            if path.is_dir() {
                rust_sources(&path, files);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }

    let mut sources = Vec::new();
    rust_sources(&manifest_dir.join("src"), &mut sources);
    assert!(
        !sources.is_empty(),
        "CLI source tree must contain Rust files"
    );
    for source in sources {
        let text = std::fs::read_to_string(&source).expect("read CLI source");
        assert!(
            !text.contains("rmcp"),
            "{} references rmcp — the CLI must stay on the typed core side of the wire boundary",
            source.display()
        );
        assert!(
            !text.contains("handlers::") && !text.contains("::handlers"),
            "{} imports the handlers layer — the unguarded \
              hardcoded-indexed surface the design forbids (Decision 1)",
            source.display()
        );
    }
}

/// AC-40 / FR-18: identical machine-readable output with and without a
/// running daemon — including through the Decision 7 window (daemon
/// attached but never analyzed answers from the cache, byte-identical to
/// the no-daemon invocation).
#[test]
fn daemon_and_standalone_output_are_byte_identical() {
    let Some(mcp) = mcp_binary() else {
        eprintln!(
            "skipping daemon parity test: code-graph-mcp binary not built \
             (run `cargo test --workspace` or `cargo build -p code-graph-mcp`)"
        );
        return;
    };

    let dir = fixture();
    let root = canonical(dir.path());
    analyze(dir.path());

    let symbol = symbol_id(dir.path(), "beta.rs", "helper_two");
    let query = |label: &str| {
        let output = run(dir.path(), &["get-callers", &symbol, "--json"]);
        assert_eq!(exit_code(&output), 0, "{label}: {}", stderr(&output));
        stdout(&output)
    };

    let standalone = query("standalone query");

    // Spawn a daemon and wait for it to publish metadata.
    let mut daemon = DaemonChild::spawn(&mcp, &root);

    // Decision 7 window: the daemon holds no graph yet; the CLI answers
    // from the on-disk cache, byte-identical to the no-daemon invocation.
    let through_window = query("Decision 7 window query");
    assert_eq!(
        through_window, standalone,
        "unindexed-daemon fallback must match the no-daemon output"
    );

    // Steady state: analyze THROUGH the daemon (analyze-codebase never
    // falls back), then the same query answers from the daemon's graph.
    analyze(dir.path());
    let attached = query("daemon-backed query");
    assert_eq!(
        attached, standalone,
        "daemon-backed output must be byte-identical to standalone (AC-40)"
    );

    daemon.stop();
}

/// Design Decision 3's attach-only contract: with a STALE `daemon.json`
/// (dead owner), the invocation still answers — via the child's in-process
/// fallback plus the Decision 7 standalone retry — and NO daemon is left
/// behind: the unmodified proxy would spawn a `--serve` contender (which
/// acquires `daemon.lock`); `--attach-only` must not.
#[test]
fn stale_daemon_metadata_answers_without_spawning_a_daemon() {
    if mcp_binary().is_none() {
        eprintln!(
            "skipping attach-only test: code-graph-mcp binary not built \
             (run `cargo test --workspace` or `cargo build -p code-graph-mcp`)"
        );
        return;
    }

    let dir = fixture();
    let root = canonical(dir.path());
    analyze(dir.path());
    let symbol = symbol_id(dir.path(), "beta.rs", "helper_two");

    let standalone = run(dir.path(), &["get-callers", &symbol, "--json"]);
    assert_eq!(exit_code(&standalone), 0);

    // Leave a real, compatible daemon publication behind after an unclean
    // stop. Attach-only can safely reclaim only this compatible dead owner.
    let runtime = root.join(".code-graph");
    let mut dead_daemon = DaemonChild::spawn(&mcp_binary().expect("MCP binary checked"), &root);
    dead_daemon.child.kill().expect("hard-stop daemon");
    dead_daemon.child.wait().expect("reap hard-stopped daemon");
    assert!(
        runtime.join("daemon.json").exists(),
        "crash leaves metadata"
    );

    let through_stale = run(dir.path(), &["get-callers", &symbol, "--json"]);
    assert_eq!(
        exit_code(&through_stale),
        0,
        "stale metadata still answers: {}",
        stderr(&through_stale)
    );
    assert_eq!(
        stdout(&through_stale),
        stdout(&standalone),
        "the answer is byte-identical to the no-daemon invocation"
    );
    assert!(
        !runtime.join("daemon.lock").exists(),
        "attach-only must not spawn a contender (a contender would acquire daemon.lock)"
    );
}

/// Required positionals stay unambiguous when their literal values are the
/// strings clap otherwise accepts as optional-bool values. The documented
/// `--flag=<true|false>` spelling leaves those positionals already consumed.
#[test]
fn optional_bool_flags_do_not_consume_true_or_false_positionals() {
    let dir = TempDir::new().expect("optional bool fixture");
    for file in ["true", "false"] {
        std::fs::write(dir.path().join(file), "fn fixture() {}\n").expect("write fixture");
        let output = run(dir.path(), &["get-file-symbols", file, "--brief=false"]);
        assert_eq!(
            exit_code(&output),
            1,
            "{file} positional must reach the unindexed tool guard: {}",
            stderr(&output)
        );
        assert!(
            stderr(&output).contains("no codebase indexed"),
            "{file} parsed as a file positional rather than an optional-bool value"
        );
    }
}

#[cfg(unix)]
fn stop_daemon_best_effort(_root: &Path, metadata_path: &Path) {
    let Some(pid) = std::fs::read(metadata_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|metadata| metadata["pid"].as_u64())
    else {
        return;
    };
    let _ = Command::new("kill")
        .args(["-INT", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Windows has no SIGINT for detached console processes; the owner-bound
/// `shutdown.request` file is the graceful-stop channel (daemon_serve
/// precedent).
#[cfg(windows)]
fn stop_daemon_best_effort(root: &Path, metadata_path: &Path) {
    let Some(owner) = std::fs::read(metadata_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|metadata| metadata.get("owner").cloned())
        .and_then(|owner| serde_json::to_vec(&owner).ok())
    else {
        return;
    };
    let _ = std::fs::write(root.join(".code-graph/shutdown.request"), owner);
}
