//! Process coverage for default-on daemon proxy attachment.

#![cfg(unix)]

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(debug_assertions)]
use code_graph_graph::Graph;
use serde_json::{json, Value};

const TIMEOUT: Duration = Duration::from_secs(10);
static ROOT_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static PROCESS_TEST_SERIALIZATION: OnceLock<Mutex<()>> = OnceLock::new();

fn process_test_guard() -> MutexGuard<'static, ()> {
    PROCESS_TEST_SERIALIZATION
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct TestRoot(PathBuf, Arc<Mutex<Option<u32>>>);

impl TestRoot {
    fn new(enabled: bool) -> Self {
        let sequence = ROOT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "code-graph-mcp-daemon-proxy-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create test root");
        if enabled {
            fs::write(root.join(".code-graph.toml"), "[daemon]\nenabled = true\n")
                .expect("write enabled daemon config");
        }
        Self(root, Arc::new(Mutex::new(None)))
    }

    fn track_daemon(&self, metadata: &Value) {
        *self.1.lock().expect("daemon guard lock") = metadata["pid"].as_u64().map(|pid| pid as u32);
    }

    fn disarm_daemon(&self) {
        *self.1.lock().expect("daemon guard lock") = None;
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let tracked = self.1.lock().ok().and_then(|mut guard| guard.take());
        let published = fs::read(self.0.join(".code-graph/daemon.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|metadata| metadata["pid"].as_u64().map(|pid| pid as u32));
        if let Some(pid) = tracked.or(published) {
            let _ = Command::new("kill")
                .args(["-INT", &pid.to_string()])
                .status();
            thread::sleep(Duration::from_millis(50));
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct LineReader {
    lines: Receiver<std::io::Result<String>>,
    join: Option<thread::JoinHandle<()>>,
}

impl LineReader {
    fn new(stdout: std::process::ChildStdout) -> Self {
        let (sender, lines) = mpsc::channel();
        let join = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) if sender.send(Ok(line)).is_err() => break,
                    Ok(_) => {}
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        break;
                    }
                }
            }
        });
        Self {
            lines,
            join: Some(join),
        }
    }

    fn json(&self) -> Value {
        match self.lines.recv_timeout(TIMEOUT) {
            Ok(Ok(line)) => serde_json::from_str(&line).expect("JSON-RPC response"),
            Ok(Err(error)) => panic!("read client stdout: {error}"),
            Err(RecvTimeoutError::Timeout) => panic!("timed out waiting for client response"),
            Err(RecvTimeoutError::Disconnected) => panic!("client stdout closed before response"),
        }
    }
}

impl Drop for LineReader {
    fn drop(&mut self) {
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// A real stdio client. Dropping it closes stdin, waits for the proxy to
/// drain its daemon output, then reaps it so failed assertions cannot leak a
/// process into the next test.
struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: LineReader,
    stderr: Option<ChildStderr>,
    next_id: u64,
}

impl Client {
    fn spawn(cwd: &Path, args: &[&str]) -> Self {
        Self::launch(cwd, args).initialize()
    }

    fn launch(cwd: &Path, args: &[&str]) -> Self {
        Self::launch_with_env(cwd, args, &[])
    }

    fn launch_with_env(cwd: &Path, args: &[&str], environment: &[(&str, &str)]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_code-graph-mcp"));
        command
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in environment {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("spawn proxy client");
        let stdin = child.stdin.take();
        let stdout = LineReader::new(child.stdout.take().expect("client stdout"));
        let stderr = child.stderr.take();
        Self {
            child,
            stdin,
            stdout,
            stderr,
            next_id: 1,
        }
    }

    fn initialize(mut self) -> Self {
        let response = self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "daemon-proxy-test", "version": "0.1.0"}
            }),
        );
        assert_eq!(response["id"], 1);
        self.notify("notifications/initialized", json!({}));
        self
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        writeln!(
            self.stdin.as_mut().expect("open client stdin"),
            "{}",
            json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params})
        )
        .expect("write client request");
        self.stdin
            .as_mut()
            .unwrap()
            .flush()
            .expect("flush client request");
        loop {
            let response = self.stdout.json();
            if response["id"] == id {
                return response;
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        writeln!(
            self.stdin.as_mut().expect("open client stdin"),
            "{}",
            json!({"jsonrpc":"2.0", "method":method, "params":params})
        )
        .expect("write client notification");
        self.stdin
            .as_mut()
            .unwrap()
            .flush()
            .expect("flush notification");
    }

    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name":name, "arguments":arguments}))
    }

    fn text(response: &Value) -> Value {
        serde_json::from_str(
            response["result"]["content"][0]["text"]
                .as_str()
                .expect("tool text body"),
        )
        .expect("tool text JSON")
    }

    fn close(mut self) -> String {
        self.stdin.take();
        wait_or_kill(&mut self.child);
        let mut stderr = String::new();
        if let Some(mut handle) = self.stderr.take() {
            handle
                .read_to_string(&mut stderr)
                .expect("read client stderr");
        }
        stderr
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stdin.take();
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn wait_or_kill(child: &mut Child) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if child.try_wait().expect("poll client").is_some() {
            return;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("proxy client did not exit after stdin EOF");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_metadata(root: &TestRoot) -> Value {
    let path = root.0.join(".code-graph/daemon.json");
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Ok(bytes) = fs::read(&path) {
            let metadata = serde_json::from_slice(&bytes).expect("daemon metadata");
            root.track_daemon(&metadata);
            return metadata;
        }
        assert!(Instant::now() < deadline, "daemon did not publish metadata");
        thread::sleep(Duration::from_millis(20));
    }
}

fn stop_daemon(metadata: &Value) {
    let status = Command::new("kill")
        .args([
            "-INT",
            metadata["pid"]
                .as_u64()
                .expect("daemon pid")
                .to_string()
                .as_str(),
        ])
        .status()
        .expect("signal daemon");
    assert!(status.success(), "SIGINT daemon");
}

fn replace_metadata_sha(root: &TestRoot, metadata: &Value, binary_sha: String) {
    let mut replacement = metadata.clone();
    replacement["binary_sha"] = Value::String(binary_sha);
    fs::write(
        root.0.join(".code-graph/daemon.json"),
        serde_json::to_vec(&replacement).unwrap(),
    )
    .unwrap();
}

fn wait_runtime_cleanup(root: &Path) {
    let lock = root.join(".code-graph/daemon.lock");
    let deadline = Instant::now() + TIMEOUT;
    while lock.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!lock.exists(), "daemon lock was cleaned up");
}

fn child_pids(parent: u32) -> Vec<u32> {
    let output = Command::new("pgrep")
        .args(["-P", &parent.to_string()])
        .output()
        .expect("list proxy child processes");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|pid| pid.parse().expect("child pid"))
        .collect()
}

/// Every advertised tool must have an explicit route-valid fixture here. The
/// exhaustive match deliberately fails to compile/test loudly when a new tool
/// is added to `tools/list` without adding its process-level fallback route.
fn assert_all_advertised_tools_route(client: &mut Client, root: &Path, source: &Path) {
    let advertised = client.request("tools/list", json!({}));
    let names: Vec<String> = advertised["result"]["tools"]
        .as_array()
        .expect("tools/list result array")
        .iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .expect("advertised tool name")
                .to_owned()
        })
        .collect();
    assert_eq!(names.len(), 22, "expected every advertised tool");
    let symbol = format!("{}:fallback_query", source.display());

    for name in names {
        let arguments = match name.as_str() {
            "analyze_codebase" => json!({"path": root, "force": false}),
            "analyze_codebase_async" => json!({"path": root, "force": false}),
            "get_file_symbols" => json!({"file": source}),
            "search_symbols" => json!({"query": "fallback_query"}),
            "get_symbol_detail" => json!({"symbol": symbol}),
            "get_symbol_summary" => json!({}),
            "get_callers" => json!({"symbol": symbol}),
            "get_callees" => json!({"symbol": symbol}),
            "get_dependencies" => json!({"file": source}),
            "detect_cycles" => json!({}),
            "get_orphans" => json!({}),
            "get_class_hierarchy" => json!({"class": "Missing"}),
            "get_coupling" => json!({"file": source}),
            "generate_diagram" => json!({"symbol": symbol}),
            "watch_start" => json!({}),
            "watch_stop" => json!({}),
            "get_status" => json!({}),
            "find_overrides" => json!({"symbol": symbol}),
            "find_class_candidates" => json!({"name": "Missing"}),
            "get_symbol_at" => json!({"file": source, "line": 1}),
            "find_path" => json!({"from": symbol, "to": symbol}),
            "detect_communities" => json!({}),
            unexpected => panic!("new advertised tool {unexpected} needs fallback test arguments"),
        };
        let response = client.tool(&name, arguments);
        assert!(
            response["result"].is_object()
                && response["result"]["content"].is_array()
                && response.get("error").is_none(),
            "{name} must route through tools/call rather than return method-not-found/unknown-tool: {response}"
        );
    }
}

#[cfg(debug_assertions)]
fn process_is_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[test]
fn default_clients_are_daemon_backed_but_opt_outs_create_no_runtime_state() {
    let _guard = process_test_guard();
    let default_root = TestRoot::new(false);
    let mut client = Client::spawn(&default_root.0, &[]);
    assert_eq!(
        client.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        22
    );
    client.close();
    let metadata = wait_metadata(&default_root);
    stop_daemon(&metadata);
    wait_runtime_cleanup(&default_root.0);
    default_root.disarm_daemon();

    let disabled = TestRoot::new(false);
    fs::write(
        disabled.0.join(".code-graph.toml"),
        "[daemon]\nenabled = false\n",
    )
    .unwrap();
    let mut client = Client::spawn(&disabled.0, &[]);
    assert_eq!(
        client.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        22
    );
    client.close();
    assert!(!disabled.0.join(".code-graph").exists());

    let malformed = TestRoot::new(false);
    fs::write(malformed.0.join(".code-graph.toml"), "[daemon\n").unwrap();
    let mut client = Client::spawn(&malformed.0, &[]);
    assert_eq!(
        client.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        22
    );
    let stderr = client.close();
    assert!(!stderr.contains("daemon unavailable"));
    assert!(!malformed.0.join(".code-graph").exists());

    let forced = TestRoot::new(true);
    let mut client = Client::spawn(&forced.0, &["--no-daemon"]);
    assert_eq!(
        client.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        22
    );
    client.close();
    assert!(!forced.0.join(".code-graph").exists());
}

#[cfg(debug_assertions)]
#[test]
fn root_and_nested_clients_share_index_watch_and_async_slot() {
    let _guard = process_test_guard();
    let root = TestRoot::new(true);
    let nested = root.0.join("nested");
    fs::create_dir_all(&nested).unwrap();
    let source = root.0.join("sample.rs");
    fs::write(&source, "fn before() {}\n").unwrap();

    // During a dirty development build each contender must observe the
    // genuinely absent runtime before the winner publishes, so both clients
    // trust that newly published owner instead of replacing it mid-session.
    let root_string = root.0.to_string_lossy().into_owned();
    let a = Client::launch_with_env(
        &root.0,
        &[],
        &[
            ("CODE_GRAPH_TEST_DAEMON_DELAY_ROOT", &root_string),
            ("CODE_GRAPH_TEST_DAEMON_DELAY_MILLIS", "300"),
        ],
    );
    let b = Client::launch_with_env(
        &nested,
        &[],
        &[
            ("CODE_GRAPH_TEST_DAEMON_DELAY_ROOT", &root_string),
            ("CODE_GRAPH_TEST_DAEMON_DELAY_MILLIS", "300"),
        ],
    );
    let mut a = a.initialize();
    let mut b = b.initialize();
    let metadata = wait_metadata(&root);
    assert!(
        !nested.join(".code-graph").exists(),
        "nested client attaches at canonical root"
    );

    let analyzed = a.tool("analyze_codebase", json!({"path":root.0, "force":true}));
    assert!(
        analyzed["result"].is_object(),
        "A indexed through shared daemon"
    );
    let symbols = b.tool("get_file_symbols", json!({"file":source}));
    assert_eq!(Client::text(&symbols)["total"], 1, "B queries A's graph");

    assert!(a.tool("watch_start", json!({}))["result"].is_object());
    let shared_watch = b.tool("watch_start", json!({}));
    assert_eq!(
        shared_watch["result"]["content"][0]["text"], "watch mode is already active",
        "B observes A's shared watcher rather than starting another"
    );
    fs::write(&source, "fn before() {}\nfn after_watch() {}\n").unwrap();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let from_a = a.tool("get_file_symbols", json!({"file":source}));
        let from_b = b.tool("get_file_symbols", json!({"file":source}));
        if Client::text(&from_a)["total"] == 2 && Client::text(&from_b)["total"] == 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "shared watcher did not update both client graphs"
        );
        thread::sleep(Duration::from_millis(50));
    }

    for number in 0..3_000 {
        fs::write(
            root.0.join(format!("work-{number}.rs")),
            format!("fn work_{number}() {{}}\n"),
        )
        .unwrap();
    }
    let kickoff = a.tool(
        "analyze_codebase_async",
        json!({"path":root.0, "force":true}),
    );
    let job_id = Client::text(&kickoff)["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let deadline = Instant::now() + TIMEOUT;
    let mut saw_running = false;
    let mut saw_progress = false;
    loop {
        let status = Client::text(&b.tool("get_status", json!({})));
        let job = &status["analyze_job"];
        assert_eq!(job["job_id"], job_id, "B sees A's shared analyze slot");
        assert!(job["progress"].is_u64(), "B sees shared progress");
        if job["status"] == "running" {
            saw_running = true;
            saw_progress |= job["progress"]
                .as_u64()
                .is_some_and(|progress| progress > 0);
        } else {
            assert_eq!(job["status"], "completed");
            assert!(job["result"].is_object(), "B sees terminal result");
            assert!(saw_running, "B observed the async job running");
            assert!(saw_progress, "B observed non-zero shared job progress");
            break;
        }
        assert!(Instant::now() < deadline, "shared async job did not finish");
        thread::yield_now();
    }

    a.close();
    b.close();
    stop_daemon(&metadata);
    wait_runtime_cleanup(&root.0);
    root.disarm_daemon();
}

#[test]
fn tcp_metadata_attachment_and_start_failure_fallback_are_safe() {
    let _guard = process_test_guard();
    let tcp = TestRoot::new(true);
    fs::create_dir_all(tcp.0.join(".code-graph")).unwrap();
    fs::write(tcp.0.join(".code-graph/daemon.sock"), "force TCP").unwrap();
    let mut client = Client::spawn(&tcp.0, &[]);
    assert_eq!(
        client.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        22
    );
    let metadata = wait_metadata(&tcp);
    assert_eq!(
        metadata["transport"], "tcp",
        "proxy read metadata and authenticated TCP"
    );
    client.close();
    stop_daemon(&metadata);
    wait_runtime_cleanup(&tcp.0);
    tcp.disarm_daemon();

    let failed = TestRoot::new(true);
    fs::write(failed.0.join(".code-graph"), "cannot create daemon runtime").unwrap();
    let fallback_source = failed.0.join("fallback.rs");
    fs::write(&fallback_source, "fn fallback_query() {}\n").unwrap();
    let mut client = Client::spawn(&failed.0, &[]);
    assert_eq!(
        client.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        22
    );
    assert!(
        client.tool("analyze_codebase", json!({"path":failed.0, "force":true}))["result"]
            .is_object(),
        "fallback serves analyze_codebase in process"
    );
    let symbols = client.tool("get_file_symbols", json!({"file":fallback_source}));
    assert_eq!(Client::text(&symbols)["total"], 1, "fallback query works");
    assert_all_advertised_tools_route(&mut client, &failed.0, &fallback_source);
    let stderr = client.close();
    assert_eq!(
        stderr.matches("daemon unavailable").count(),
        1,
        "one fallback diagnostic"
    );
    assert!(stderr.contains("falling back to in-process stdio"));
}

#[test]
fn daemon_project_roots_are_isolated_for_sync_and_async_analyze() {
    let _guard = process_test_guard();
    let a_root = TestRoot::new(true);
    let b_root = TestRoot::new(true);
    let b_source = b_root.0.join("independent.rs");
    fs::write(&b_source, "fn independently_owned() {}\n").unwrap();

    let mut a = Client::spawn(&a_root.0, &[]);
    let a_metadata = wait_metadata(&a_root);
    let mut b = Client::spawn(&b_root.0, &[]);
    let b_metadata = wait_metadata(&b_root);
    assert_ne!(
        a_metadata["pid"], b_metadata["pid"],
        "separate configured roots have independent daemon owners"
    );
    let b_cache = b_root.0.join(".code-graph-cache.db");

    let sync = a.tool("analyze_codebase", json!({"path": b_root.0, "force": true}));
    assert_eq!(sync["result"]["isError"], true);
    assert!(
        sync["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("daemon is bound to project root")),
        "cross-root sync analyze uses the normal user-visible failure shape: {sync}"
    );
    assert!(!b_cache.exists(), "daemon A must not create B's cache");

    let async_kickoff = a.tool(
        "analyze_codebase_async",
        json!({"path": b_root.0, "force": true}),
    );
    assert!(async_kickoff["result"].is_object());
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let status = Client::text(&a.tool("get_status", json!({})));
        let job = &status["analyze_job"];
        if job["status"] == "failed" {
            assert!(
                job["error"]
                    .as_str()
                    .is_some_and(|text| text.contains("daemon is bound to project root")),
                "cross-root async analyze exposes the same user-visible failure: {job}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "cross-root async analyze did not reach failed terminal state"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!b_cache.exists(), "daemon A must not mutate B's cache");

    let owned = b.tool("analyze_codebase", json!({"path": b_root.0, "force": true}));
    assert!(owned["result"].is_object());
    assert_eq!(Client::text(&owned)["files"], 1);
    assert!(b_cache.exists(), "daemon B independently owns its cache");

    a.close();
    b.close();
    stop_daemon(&a_metadata);
    stop_daemon(&b_metadata);
    wait_runtime_cleanup(&a_root.0);
    wait_runtime_cleanup(&b_root.0);
    a_root.disarm_daemon();
    b_root.disarm_daemon();
}

#[cfg(debug_assertions)]
#[test]
fn forced_fallback_terminates_a_slow_contender_before_it_can_publish() {
    let _guard = process_test_guard();
    let root = TestRoot::new(true);
    let root_string = root.0.to_string_lossy().into_owned();
    let mut client = Client::launch_with_env(
        &root.0,
        &[],
        &[
            ("CODE_GRAPH_TEST_DAEMON_DELAY_ROOT", &root_string),
            ("CODE_GRAPH_TEST_DAEMON_DELAY_MILLIS", "5000"),
        ],
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    let contender_pid = loop {
        if let Some(pid) = child_pids(client.child.id()).into_iter().next() {
            break pid;
        }
        assert!(Instant::now() < deadline, "proxy did not spawn a contender");
        thread::sleep(Duration::from_millis(10));
    };

    client = client.initialize();
    assert_eq!(
        client.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        22,
        "forced fallback serves in process"
    );
    thread::sleep(Duration::from_secs(1));
    assert!(
        !process_is_alive(contender_pid),
        "fallback reaped the slow daemon contender"
    );
    assert!(
        !root.0.join(".code-graph/daemon.json").exists(),
        "the terminated contender never published daemon metadata"
    );
    assert!(
        child_pids(client.child.id()).is_empty(),
        "fallback retains no daemon child process"
    );
    let stderr = client.close();
    assert_eq!(stderr.matches("daemon unavailable").count(), 1);
}

#[test]
fn established_daemon_death_ends_proxy_while_stdin_remains_open() {
    let _guard = process_test_guard();
    let root = TestRoot::new(true);
    let mut client = Client::spawn(&root.0, &[]);
    let metadata = wait_metadata(&root);

    let started = Instant::now();
    let pid = metadata["pid"].as_u64().expect("daemon pid").to_string();
    let status = Command::new("kill")
        .args(["-KILL", &pid])
        .status()
        .expect("kill established daemon");
    assert!(status.success(), "SIGKILL daemon");
    wait_or_kill(&mut client.child);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "daemon EOF must cancel the proxy's blocked stdin copy"
    );
    root.disarm_daemon();
    let _ = fs::remove_file(root.0.join(".code-graph/daemon.json"));
}

#[test]
fn simultaneous_real_proxy_clients_converge_without_contender_leaks() {
    let _guard = process_test_guard();
    let root = TestRoot::new(false);
    let launched: Vec<_> = (0..6).map(|_| Client::launch(&root.0, &[])).collect();
    let mut clients: Vec<_> = launched.into_iter().map(Client::initialize).collect();
    let metadata = wait_metadata(&root);
    for client in &mut clients {
        assert_eq!(
            client.request("tools/list", json!({}))["result"]["tools"]
                .as_array()
                .unwrap()
                .len(),
            22
        );
    }
    let owner_pid = metadata["pid"].as_u64().expect("daemon pid") as u32;
    let contender_pids: Vec<_> = clients
        .iter()
        .flat_map(|client| child_pids(client.child.id()))
        .collect();
    assert_eq!(
        contender_pids,
        vec![owner_pid],
        "exactly one open proxy retains the metadata-owning daemon child"
    );
    for client in clients {
        client.close();
    }
    stop_daemon(&metadata);
    wait_runtime_cleanup(&root.0);
    root.disarm_daemon();
}

#[test]
fn clean_binary_metadata_mismatch_gracefully_replaces_the_old_owner() {
    let _guard = process_test_guard();
    let root = TestRoot::new(false);
    let client = Client::spawn(&root.0, &[]);
    let old = wait_metadata(&root);
    client.close();
    replace_metadata_sha(&root, &old, "different-clean-build".to_owned());

    let started = Instant::now();
    let mut replacement = Client::spawn(&root.0, &[]);
    assert_eq!(
        replacement.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        22
    );
    let new = wait_metadata(&root);
    assert_ne!(
        new["owner"], old["owner"],
        "replacement publishes a new owner"
    );
    assert!(
        started.elapsed() < TIMEOUT,
        "replacement stays within client bound"
    );
    replacement.close();
    stop_daemon(&new);
    wait_runtime_cleanup(&root.0);
    root.disarm_daemon();
}

#[test]
fn different_executable_content_replaces_even_when_build_sha_matches() {
    let _guard = process_test_guard();
    use std::os::unix::fs::PermissionsExt;

    let root = TestRoot::new(false);
    let copied = root.0.join("altered-code-graph-mcp");
    fs::copy(env!("CARGO_BIN_EXE_code-graph-mcp"), &copied).unwrap();
    OpenOptions::new()
        .append(true)
        .open(&copied)
        .unwrap()
        .write_all(b"code-graph-test-trailing-bytes")
        .unwrap();
    let mode = fs::metadata(env!("CARGO_BIN_EXE_code-graph-mcp"))
        .unwrap()
        .permissions()
        .mode();
    fs::set_permissions(&copied, fs::Permissions::from_mode(mode)).unwrap();
    let mut altered = Command::new(&copied)
        .arg("--serve")
        .current_dir(&root.0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let old = wait_metadata(&root);

    let mut client = Client::spawn(&root.0, &[]);
    assert_eq!(
        client.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        22
    );
    let new = wait_metadata(&root);
    assert_ne!(new["owner"], old["owner"], "different executable replaced");
    client.close();
    let _ = altered.wait();
    stop_daemon(&new);
    wait_runtime_cleanup(&root.0);
    root.disarm_daemon();
}

#[test]
fn sequential_clients_keep_the_same_matching_executable_owner() {
    let _guard = process_test_guard();
    let root = TestRoot::new(false);
    let first = Client::spawn(&root.0, &[]);
    let old = wait_metadata(&root);
    let mut second = Client::spawn(&root.0, &[]);
    assert_eq!(
        second.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        22
    );
    let current = wait_metadata(&root);
    assert_eq!(current["owner"], old["owner"]);
    first.close();
    second.close();
    stop_daemon(&current);
    wait_runtime_cleanup(&root.0);
    root.disarm_daemon();
}

#[test]
fn equal_dirty_metadata_is_replaced_once_then_converges_on_new_owner() {
    let _guard = process_test_guard();
    let root = TestRoot::new(false);
    let client = Client::spawn(&root.0, &[]);
    let old = wait_metadata(&root);
    client.close();
    replace_metadata_sha(&root, &old, "same-development-build-dirty".to_owned());

    let mut replacement = Client::spawn(&root.0, &[]);
    assert_eq!(
        replacement.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        22
    );
    let new = wait_metadata(&root);
    assert_ne!(new["owner"], old["owner"], "dirty owner was replaced once");
    replacement.close();
    stop_daemon(&new);
    wait_runtime_cleanup(&root.0);
    root.disarm_daemon();
}

#[cfg(debug_assertions)]
#[test]
fn ignored_replacement_request_is_hard_killed_and_client_falls_back() {
    let _guard = process_test_guard();
    let root = TestRoot::new(false);
    let root_string = root.0.to_string_lossy().into_owned();
    let client = Client::launch_with_env(
        &root.0,
        &[],
        &[("CODE_GRAPH_TEST_DAEMON_IGNORE_REQUEST_ROOT", &root_string)],
    )
    .initialize();
    let old = wait_metadata(&root);
    client.close();
    replace_metadata_sha(&root, &old, "force-replacement-dirty".to_owned());

    let started = Instant::now();
    let mut fallback = Client::spawn(&root.0, &[]);
    assert_eq!(
        fallback.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        22
    );
    fallback.close();
    assert!(
        started.elapsed() < TIMEOUT,
        "hard-kill fallback remains bounded"
    );
    assert!(
        !process_is_alive(old["pid"].as_u64().unwrap() as u32),
        "ignored daemon was hard-killed"
    );
    root.disarm_daemon();
    let _ = fs::remove_file(root.0.join(".code-graph/daemon.json"));
}

#[cfg(debug_assertions)]
#[test]
fn replacement_waits_for_delayed_persist_before_runtime_cleanup() {
    let _guard = process_test_guard();
    let root = TestRoot::new(false);
    let root_string = root.0.to_string_lossy().into_owned();
    let source = root.0.join("persist.rs");
    let marker = root.0.join("persist-admitted.marker");
    let marker_string = marker.to_string_lossy().into_owned();
    fs::write(&source, "fn persisted() {}\n").unwrap();
    let mut client = Client::launch_with_env(
        &root.0,
        &[],
        &[
            ("CODE_GRAPH_TEST_PERSIST_DELAY_ROOT", &root_string),
            ("CODE_GRAPH_TEST_PERSIST_DELAY_MILLIS", "3000"),
            ("CODE_GRAPH_TEST_PERSIST_MARKER", &marker_string),
        ],
    )
    .initialize();
    let old = wait_metadata(&root);
    client.tool(
        "analyze_codebase_async",
        json!({"path":root.0, "force":true}),
    );
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if marker.exists() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "analyze did not enter an admitted delayed persist"
        );
        thread::sleep(Duration::from_millis(10));
    }
    replace_metadata_sha(&root, &old, "persist-replacement-dirty".to_owned());
    let started = Instant::now();
    let mut replacement = Client::spawn(&root.0, &[]);
    assert_eq!(
        replacement.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        22
    );
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "acknowledged replacement waited past request grace for the admitted cache save"
    );
    let mut graph = Graph::new();
    assert!(graph.load(&root.0).unwrap(), "drained cache is loadable");
    assert_eq!(graph.stats().files, 1, "drained cache is current");
    let new = wait_metadata(&root);
    client.close();
    replacement.close();
    stop_daemon(&new);
    wait_runtime_cleanup(&root.0);
    root.disarm_daemon();
}
