//! Process coverage for default-on daemon proxy attachment.

#![cfg(any(unix, windows))]

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(target_os = "linux")]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(debug_assertions)]
use code_graph_graph::Graph;
use serde_json::{json, Value};

const TIMEOUT: Duration = Duration::from_secs(10);
const DROP_REAP_TIMEOUT: Duration = Duration::from_secs(1);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);
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
        let root = (0..1_000)
            .find_map(|_| {
                let sequence = ROOT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
                let nonce = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system clock after Unix epoch")
                    .as_nanos();
                let root = std::env::temp_dir().join(format!(
                    "code-graph-mcp-daemon-proxy-{}-{nonce}-{sequence}",
                    std::process::id()
                ));
                match fs::create_dir(&root) {
                    // Canonicalize so every derived string — analyze paths,
                    // debug-hook roots, job-view comparisons — matches the
                    // daemon's own canonical form. Windows bash exports a
                    // short-form (8.3) TEMP; the daemon compares long forms.
                    Ok(()) => Some(
                        code_graph_core::paths::canonicalize(&root)
                            .expect("canonicalize daemon proxy test root"),
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                    Err(error) => panic!("create fresh test root: {error}"),
                }
            })
            .expect("could not allocate a fresh daemon proxy test root");
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
        let mut pids: Vec<_> = [tracked, published].into_iter().flatten().collect();
        pids.sort_unstable();
        pids.dedup();
        for pid in pids {
            reap_process_bounded(pid);
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Releases a debug-only held persist even when a test assertion panics.
/// Instances must be declared after their [`TestRoot`] so this drop runs
/// before `TestRoot` asks the daemon to drain during cleanup.
#[cfg(debug_assertions)]
struct ReleaseMarker(PathBuf);

#[cfg(debug_assertions)]
impl ReleaseMarker {
    fn new(path: PathBuf) -> Self {
        Self(path)
    }

    fn release(&self) {
        let _ = fs::write(&self.0, b"release held persist\n");
    }
}

#[cfg(debug_assertions)]
impl Drop for ReleaseMarker {
    fn drop(&mut self) {
        self.release();
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

/// Only the Linux-gated truncated-request replacement test waits for a new
/// owner to publish over an old one.
#[cfg(target_os = "linux")]
fn wait_replacement_metadata(root: &TestRoot, previous_owner: &Value) -> Value {
    let path = root.0.join(".code-graph/daemon.json");
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Ok(bytes) = fs::read(&path) {
            let metadata: Value = serde_json::from_slice(&bytes).expect("daemon metadata");
            if metadata["owner"] != *previous_owner {
                root.track_daemon(&metadata);
                return metadata;
            }
        }
        assert!(
            Instant::now() < deadline,
            "replacement daemon did not publish new metadata"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(unix)]
fn stop_daemon(root: &Path, metadata: &Value) {
    let _ = root;
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

/// Windows has no SIGINT equivalent for a detached console process. The
/// daemon's replacement protocol is the graceful-stop channel there: naming
/// the current owner in `shutdown.request` makes it drain, save the cache,
/// clean its runtime records, and exit — the same path a replacing client
/// takes in production.
#[cfg(windows)]
fn stop_daemon(root: &Path, metadata: &Value) {
    fs::write(
        root.join(".code-graph/shutdown.request"),
        serde_json::to_vec(&metadata["owner"]).expect("encode daemon owner identity"),
    )
    .expect("write daemon shutdown request");
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

fn wait_for_path(path: &Path, description: &str) {
    let deadline = Instant::now() + TIMEOUT;
    while !path.exists() {
        assert!(Instant::now() < deadline, "{description}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_marker_count(path: &Path, expected: usize, description: &str) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let count = fs::read_to_string(path)
            .map(|marker| marker.lines().count())
            .unwrap_or(0);
        if count >= expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{description}; observed {count} of {expected} pending admissions"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn assert_async_admitted(response: &Value, description: &str) {
    assert!(response["result"].is_object(), "{description}: {response}");
    assert!(
        response["result"].get("isError").is_none() || response["result"]["isError"] == false,
        "{description} must not return a tool error: {response}"
    );
}

fn async_job_id(response: &Value) -> String {
    Client::text(response)["job_id"]
        .as_str()
        .expect("async kickoff job ID")
        .to_owned()
}

fn wait_for_running_path(client: &mut Client, path: &Path) -> Value {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let status = Client::text(&client.tool("get_status", json!({})));
        let job = &status["analyze_job"];
        if job["status"] == "running" && job["path"] == path.to_string_lossy().as_ref() {
            return job.clone();
        }
        assert!(
            Instant::now() < deadline,
            "daemon did not promote expected running path {}: {status}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(debug_assertions)]
fn wait_for_process_exit(pid: u32) {
    let deadline = Instant::now() + TIMEOUT;
    while process_is_alive(pid) {
        assert!(
            Instant::now() < deadline,
            "daemon process {pid} did not exit"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn admitted_uds_connection(endpoint: &str) -> UnixStream {
    let mut stream = UnixStream::connect(endpoint).expect("connect UDS saturation holder");
    stream
        .set_read_timeout(Some(TIMEOUT))
        .expect("set UDS saturation holder timeout");
    let mut acknowledgement = [0_u8; 6];
    stream
        .read_exact(&mut acknowledgement)
        .expect("read UDS admission acknowledgement");
    assert_eq!(
        &acknowledgement, b"CG-OK\n",
        "holder is admitted before occupying a daemon connection permit"
    );
    stream
}

#[cfg(unix)]
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

#[cfg(windows)]
fn child_pids(parent: u32) -> Vec<u32> {
    let parent = sysinfo::Pid::from_u32(parent);
    let mut system = sysinfo::System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    system
        .processes()
        .values()
        .filter(|process| process.parent() == Some(parent))
        .map(|process| process.pid().as_u32())
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
    assert_eq!(names.len(), 25, "expected every advertised tool");
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
            "get_analyze_status" => json!({"job_id": "unknown"}),
            "find_overrides" => json!({"symbol": symbol}),
            "find_class_candidates" => json!({"name": "Missing"}),
            "get_symbol_at" => json!({"file": source, "line": 1}),
            "find_path" => json!({"from": symbol, "to": symbol}),
            "detect_communities" => json!({}),
            "blame_symbol" => json!({"symbol": symbol}),
            "symbol_history" => json!({"symbol": symbol}),
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

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    let target = sysinfo::Pid::from_u32(pid);
    let mut system = sysinfo::System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[target]), true);
    system.process(target).is_some()
}

/// Unclean termination for crash-recovery tests: SIGKILL on Unix, a forced
/// `taskkill` on Windows. Both leave runtime records behind by design.
fn kill_hard(pid: u32) {
    #[cfg(unix)]
    let status = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .expect("SIGKILL daemon");
    #[cfg(windows)]
    let status = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("taskkill daemon");
    assert!(status.success(), "hard-kill daemon {pid}");
}

/// Best-effort test-root cleanup. Do not propagate failures from `Drop`: a
/// held queued persist can take longer than a graceful signal, so boundedly
/// escalate to SIGKILL before removing the runtime directory.
#[cfg(unix)]
fn reap_process_bounded(pid: u32) {
    let _ = Command::new("kill")
        .args(["-INT", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if wait_for_process_exit_bounded(pid, DROP_REAP_TIMEOUT) {
        return;
    }
    let _ = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = wait_for_process_exit_bounded(pid, DROP_REAP_TIMEOUT);
}

/// Best-effort test-root cleanup. Windows offers no graceful console signal
/// for a detached process, and successful tests already stopped their daemon
/// through the shutdown-request protocol, so `Drop` escalates straight to a
/// forced termination of anything left.
#[cfg(windows)]
fn reap_process_bounded(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = wait_for_process_exit_bounded(pid, DROP_REAP_TIMEOUT);
}

fn wait_for_process_exit_bounded(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while process_is_alive(pid) {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(PROCESS_POLL_INTERVAL);
    }
    true
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
        25
    );
    client.close();
    let metadata = wait_metadata(&default_root);
    stop_daemon(&default_root.0, &metadata);
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
        25
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
        25
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
        25
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
    stop_daemon(&root.0, &metadata);
    wait_runtime_cleanup(&root.0);
    root.disarm_daemon();
}

#[cfg(debug_assertions)]
#[test]
fn live_proxy_queue_compacts_pending_analyzes_and_shares_sync_terminal_outcomes() {
    let _guard = process_test_guard();
    let root = TestRoot::new(true);
    let release_marker = root.0.join("persist-release.marker");
    let release = ReleaseMarker::new(release_marker.clone());
    let running = root.0.join("running");
    let alpha_child = root.0.join("alpha/child");
    let alpha_descendant = alpha_child.join("grandchild");
    let alpha = root.0.join("alpha");
    let alpha_sync = alpha.join("sync");
    let beta = root.0.join("beta");
    let malformed = root.0.join("malformed");
    let malformed_follower = malformed.join("follower");
    for directory in [
        &running,
        &alpha_descendant,
        &alpha_sync,
        &beta,
        &malformed_follower,
    ] {
        fs::create_dir_all(directory).expect("create queue fixture directory");
        fs::write(directory.join("fixture.rs"), "fn fixture() {}\n")
            .expect("write queue fixture source");
    }
    let root_string = root.0.to_string_lossy().into_owned();
    let admitted_marker = root.0.join("persist-admitted.marker");
    let admitted_marker_string = admitted_marker.to_string_lossy().into_owned();
    let release_marker_string = release_marker.to_string_lossy().into_owned();
    let pending_marker = root.0.join("pending-admissions.marker");
    let pending_marker_string = pending_marker.to_string_lossy().into_owned();
    let mut status_client = Client::launch_with_env(
        &root.0,
        &[],
        &[
            ("CODE_GRAPH_TEST_PERSIST_DELAY_ROOT", &root_string),
            ("CODE_GRAPH_TEST_PERSIST_DELAY_MILLIS", "2000"),
            (
                "CODE_GRAPH_TEST_PERSIST_ADMITTED_MARKER",
                &admitted_marker_string,
            ),
            (
                "CODE_GRAPH_TEST_PERSIST_RELEASE_MARKER",
                &release_marker_string,
            ),
            (
                "CODE_GRAPH_TEST_PENDING_ADMISSION_MARKER",
                &pending_marker_string,
            ),
        ],
    )
    .initialize();
    let metadata = wait_metadata(&root);
    let initial = status_client.tool(
        "analyze_codebase_async",
        json!({"path":running, "force":true}),
    );
    assert!(
        initial["result"].is_object(),
        "initial analyze starts: {initial}"
    );
    wait_for_path(
        &admitted_marker,
        "initial analyze did not reach the deterministic persist hold",
    );
    wait_for_running_path(&mut status_client, &running);

    let mut child_client = Client::spawn(&root.0, &[]);
    let child = child_client.tool(
        "analyze_codebase_async",
        json!({"path":alpha_child, "force":false}),
    );
    let child_id = async_job_id(&child);
    child_client.close();

    let mut beta_client = Client::spawn(&root.0, &[]);
    let beta_kickoff = beta_client.tool(
        "analyze_codebase_async",
        json!({"path":beta, "force":false}),
    );
    let beta_id = async_job_id(&beta_kickoff);
    beta_client.close();

    let mut descendant_client = Client::spawn(&root.0, &[]);
    let descendant = descendant_client.tool(
        "analyze_codebase_async",
        json!({"path":alpha_descendant, "force":true}),
    );
    let descendant_id = async_job_id(&descendant);
    assert_ne!(
        descendant_id, child_id,
        "an absorbed pending descendant retains a distinct asynchronous handle across clients"
    );
    descendant_client.close();

    let mut ancestor_client = Client::spawn(&root.0, &[]);
    let ancestor = ancestor_client.tool(
        "analyze_codebase_async",
        json!({"path":alpha, "force":false}),
    );
    let alpha_id = async_job_id(&ancestor);
    assert_ne!(
        alpha_id, child_id,
        "incoming ancestor replaces pending child"
    );
    for alias in [&child_id, &descendant_id] {
        let polled =
            Client::text(&ancestor_client.tool("get_analyze_status", json!({"job_id": alias})));
        assert_eq!(
            polled["job_id"], alpha_id,
            "an alias polls the satisfying canonical pending scan across clients"
        );
        assert_eq!(polled["status"], "running");
    }
    ancestor_client.close();

    let success_root = root.0.clone();
    let success_path = alpha_sync.clone();
    let sync_success = thread::spawn(move || {
        let mut client = Client::spawn(&success_root, &[]);
        let response = client.tool(
            "analyze_codebase",
            json!({"path":success_path, "force":false}),
        );
        let stderr = client.close();
        (response, stderr)
    });

    // Async admission captures config discovery before it joins the queue.
    // Make the canonical request invalid before that probe, then attach the
    // synchronous descendant while path-only compaction is still in effect.
    fs::write(malformed.join(".code-graph.toml"), "[daemon\n")
        .expect("make canonical parent config malformed before async admission");
    let mut malformed_client = Client::spawn(&root.0, &[]);
    let malformed_kickoff = malformed_client.tool(
        "analyze_codebase_async",
        json!({"path":malformed, "force":false}),
    );
    assert!(
        malformed_kickoff["result"].is_object(),
        "malformed request is admitted behind held work: {malformed_kickoff}"
    );
    malformed_client.close();

    let error_root = root.0.clone();
    let error_path = malformed_follower.clone();
    let sync_error = thread::spawn(move || {
        let mut client = Client::spawn(&error_root, &[]);
        let response = client.tool(
            "analyze_codebase",
            json!({"path":error_path, "force":false}),
        );
        let stderr = client.close();
        (response, stderr)
    });

    wait_for_marker_count(
        &pending_marker,
        7,
        "all cross-client pending requests did not reach serialized admission",
    );
    fs::write(
        malformed_follower.join(".code-graph.toml"),
        "[daemon]\nenabled = true\n",
    )
    .expect("make child config valid after follower admission");
    release.release();
    let alpha_view = wait_for_running_path(&mut status_client, &alpha);
    assert_eq!(alpha_view["job_id"], alpha_id);
    assert_eq!(
        alpha_view["force"], true,
        "replacement retains the force from an absorbed pending descendant"
    );
    let (success, success_stderr) = sync_success.join().expect("sync success client joins");
    assert!(
        success_stderr.is_empty(),
        "sync success proxy stderr: {success_stderr}"
    );
    let beta_view = wait_for_running_path(&mut status_client, &beta);
    assert_eq!(beta_view["job_id"], beta_id);
    assert_eq!(beta_view["force"], false);
    let post_success_status = Client::text(&status_client.tool("get_status", json!({})));
    assert_eq!(
        post_success_status["analyze_job_previous_terminal"]["path"],
        alpha.to_string_lossy().as_ref(),
        "the synchronous follower completed on alpha before the next FIFO scan"
    );
    assert_eq!(
        post_success_status["analyze_job_previous_terminal"]["status"],
        "completed"
    );
    for handle in [&alpha_id, &child_id, &descendant_id] {
        let polled =
            Client::text(&status_client.tool("get_analyze_status", json!({"job_id": handle})));
        assert_eq!(
            polled["job_id"], alpha_id,
            "canonical and alias polling return the satisfying canonical ID"
        );
        assert_eq!(polled["status"], "completed");
        assert!(
            polled["result"].is_object(),
            "terminal poll includes result"
        );
    }
    assert_eq!(
        Client::text(&success)["root_path"],
        root.0.to_string_lossy().as_ref(),
        "a synchronous pending follower receives the daemon's normal terminal success"
    );
    let (error, error_stderr) = sync_error.join().expect("sync error client joins");
    assert!(
        error_stderr.is_empty(),
        "sync error proxy stderr: {error_stderr}"
    );
    assert_eq!(
        error["result"]["isError"], true,
        "follower receives tool error"
    );
    assert!(
        error["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| {
                text.contains("failed to parse .code-graph.toml")
                    && !text.contains("daemon is bound to project root")
            }),
        "follower receives the canonical parent's parse failure, not the valid child's daemon-root error: {error}"
    );

    status_client.close();
    stop_daemon(&root.0, &metadata);
    wait_runtime_cleanup(&root.0);
    wait_for_process_exit(metadata["pid"].as_u64().expect("daemon pid") as u32);
    root.disarm_daemon();
}

#[cfg(debug_assertions)]
#[test]
fn live_proxy_queue_cap_rejects_covered_followers_across_clients() {
    let _guard = process_test_guard();
    let root = TestRoot::new(true);
    let release_marker = root.0.join("persist-release.marker");
    let _release = ReleaseMarker::new(release_marker.clone());
    let running = root.0.join("running");
    fs::create_dir_all(&running).unwrap();
    fs::write(running.join("fixture.rs"), "fn running() {}\n").unwrap();
    let pending = root.0.join("pending");
    for index in 0..33 {
        let directory = pending.join(index.to_string());
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("fixture.rs"),
            format!("fn pending_{index}() {{}}\n"),
        )
        .unwrap();
    }
    let covered = pending.join("0/covered");
    fs::create_dir_all(&covered).unwrap();
    fs::write(covered.join("fixture.rs"), "fn covered() {}\n").unwrap();
    let overflow = root.0.join("overflow");
    fs::create_dir_all(&overflow).unwrap();
    fs::write(overflow.join("fixture.rs"), "fn overflow() {}\n").unwrap();

    let root_string = root.0.to_string_lossy().into_owned();
    let admitted_marker = root.0.join("persist-admitted.marker");
    let admitted_marker_string = admitted_marker.to_string_lossy().into_owned();
    let release_marker_string = release_marker.to_string_lossy().into_owned();
    let pending_marker = root.0.join("pending-admissions.marker");
    let pending_marker_string = pending_marker.to_string_lossy().into_owned();
    let mut holder = Client::launch_with_env(
        &root.0,
        &[],
        &[
            ("CODE_GRAPH_TEST_PERSIST_DELAY_ROOT", &root_string),
            ("CODE_GRAPH_TEST_PERSIST_DELAY_MILLIS", "5000"),
            (
                "CODE_GRAPH_TEST_PERSIST_ADMITTED_MARKER",
                &admitted_marker_string,
            ),
            (
                "CODE_GRAPH_TEST_PERSIST_RELEASE_MARKER",
                &release_marker_string,
            ),
            (
                "CODE_GRAPH_TEST_PENDING_ADMISSION_MARKER",
                &pending_marker_string,
            ),
        ],
    )
    .initialize();
    let metadata = wait_metadata(&root);
    holder.tool(
        "analyze_codebase_async",
        json!({"path":running, "force":true}),
    );
    wait_for_path(
        &admitted_marker,
        "capacity holder did not reach the deterministic persist hold",
    );
    wait_for_running_path(&mut holder, &running);

    // Keep both proxy connections open while alternating admissions. Capacity
    // belongs to the daemon's shared queue, not to either client connection.
    let mut first_client = Client::spawn(&root.0, &[]);
    let mut second_client = Client::spawn(&root.0, &[]);
    let first = first_client.tool(
        "analyze_codebase_async",
        json!({"path":pending.join("0"), "force":false}),
    );
    assert_async_admitted(&first, "first of 32 distinct pending analyzes");
    for index in 1..32 {
        let client = if index % 2 == 0 {
            &mut first_client
        } else {
            &mut second_client
        };
        let admitted = client.tool(
            "analyze_codebase_async",
            json!({"path":pending.join(index.to_string()), "force":false}),
        );
        assert_async_admitted(&admitted, &format!("pending analyze {index}"));
    }
    let covered_response = second_client.tool(
        "analyze_codebase_async",
        json!({"path":covered, "force":true}),
    );
    wait_for_marker_count(
        &pending_marker,
        32,
        "32 distinct pending analyzes did not reach serialized admission",
    );
    assert_eq!(
        covered_response["result"]["isError"], true,
        "a covered follower is the 33rd pending request and must reject"
    );
    assert!(
        covered_response["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("queue is full") && text.contains("retry")),
        "covered overflow is retryable across daemon clients: {covered_response}"
    );
    let rejected = first_client.tool(
        "analyze_codebase_async",
        json!({"path":overflow, "force":false}),
    );
    assert_eq!(
        rejected["result"]["isError"], true,
        "33rd distinct request rejects"
    );
    assert!(
        rejected["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("queue is full") && text.contains("retry")),
        "overflow is retryable: {rejected}"
    );
    let running_view = wait_for_running_path(&mut holder, &running);
    assert_eq!(
        running_view["status"], "running",
        "overflow did not start another worker while held work remains active"
    );

    first_client.close();
    second_client.close();
    holder.close();
    let pid = metadata["pid"].as_u64().expect("daemon pid") as u32;
    kill_hard(pid);
    wait_for_process_exit(pid);
    root.disarm_daemon();
    let _ = fs::remove_file(root.0.join(".code-graph/daemon.json"));
}

#[cfg(debug_assertions)]
#[test]
fn live_proxy_shutdown_drains_queued_analyzes_before_cleaning_runtime() {
    let _guard = process_test_guard();
    let root = TestRoot::new(true);
    let release_marker = root.0.join("persist-release.marker");
    let release = ReleaseMarker::new(release_marker.clone());
    let running = root.0.join("running");
    let alpha = root.0.join("alpha");
    let beta = root.0.join("beta");
    for (directory, function) in [
        (&running, "running_before_shutdown"),
        (&alpha, "alpha_before_shutdown"),
        (&beta, "beta_before_shutdown"),
    ] {
        fs::create_dir_all(directory).unwrap();
        fs::write(
            directory.join("fixture.rs"),
            format!("fn {function}() {{}}\n"),
        )
        .unwrap();
    }

    let root_string = root.0.to_string_lossy().into_owned();
    let admitted_marker = root.0.join("persist-admitted.marker");
    let admitted_marker_string = admitted_marker.to_string_lossy().into_owned();
    let release_marker_string = release_marker.to_string_lossy().into_owned();
    let pending_marker = root.0.join("pending-admissions.marker");
    let pending_marker_string = pending_marker.to_string_lossy().into_owned();
    let mut holder = Client::launch_with_env(
        &root.0,
        &[],
        &[
            ("CODE_GRAPH_TEST_PERSIST_DELAY_ROOT", &root_string),
            ("CODE_GRAPH_TEST_PERSIST_DELAY_MILLIS", "500"),
            (
                "CODE_GRAPH_TEST_PERSIST_ADMITTED_MARKER",
                &admitted_marker_string,
            ),
            (
                "CODE_GRAPH_TEST_PERSIST_RELEASE_MARKER",
                &release_marker_string,
            ),
            (
                "CODE_GRAPH_TEST_PENDING_ADMISSION_MARKER",
                &pending_marker_string,
            ),
        ],
    )
    .initialize();
    let metadata = wait_metadata(&root);
    holder.tool(
        "analyze_codebase_async",
        json!({"path":running, "force":true}),
    );
    wait_for_path(
        &admitted_marker,
        "shutdown holder did not reach the deterministic persist hold",
    );
    wait_for_running_path(&mut holder, &running);

    let mut alpha_client = Client::spawn(&root.0, &[]);
    let alpha_kickoff = alpha_client.tool(
        "analyze_codebase_async",
        json!({"path":alpha, "force":false}),
    );
    assert_async_admitted(&alpha_kickoff, "first shutdown-drain pending analyze");
    alpha_client.close();
    let mut beta_client = Client::spawn(&root.0, &[]);
    let beta_kickoff = beta_client.tool(
        "analyze_codebase_async",
        json!({"path":beta, "force":false}),
    );
    assert_async_admitted(&beta_kickoff, "second shutdown-drain pending analyze");
    beta_client.close();
    wait_for_marker_count(
        &pending_marker,
        2,
        "queued shutdown-drain analyzes did not reach serialized admission",
    );
    holder.close();

    release.release();
    let pid = metadata["pid"].as_u64().expect("daemon pid") as u32;
    stop_daemon(&root.0, &metadata);
    wait_runtime_cleanup(&root.0);
    wait_for_process_exit(pid);
    let mut graph = Graph::new();
    assert!(
        graph.load(&root.0).unwrap(),
        "shutdown leaves a loadable cache"
    );
    assert_eq!(
        graph.stats().files,
        3,
        "shutdown drained every queued canonical scope before its final cache save"
    );
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
        25
    );
    let metadata = wait_metadata(&tcp);
    // A planted daemon.sock file occupies the UDS pathname and forces the
    // loopback-TCP fallback on Unix. Windows pipe names are per-PID and never
    // touch daemon.sock, so the same setup must instead prove a stray socket
    // file cannot disturb pipe attachment.
    #[cfg(unix)]
    {
        assert_eq!(
            metadata["transport"], "tcp",
            "proxy read metadata and authenticated TCP"
        );
        let stderr = client.close();
        assert_eq!(
            stderr
                .matches("local IPC was unavailable; loopback TCP fallback is active")
                .count(),
            1,
            "the attaching proxy reports its metadata-selected TCP fallback"
        );
    }
    #[cfg(windows)]
    {
        assert_eq!(
            metadata["transport"], "pipe",
            "a stray daemon.sock file does not disturb pipe transport"
        );
        let stderr = client.close();
        assert!(
            !stderr.contains("local IPC was unavailable"),
            "no fallback diagnostic on the pipe transport: {stderr}"
        );
    }
    stop_daemon(&tcp.0, &metadata);
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
        25
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
    stop_daemon(&a_root.0, &a_metadata);
    stop_daemon(&b_root.0, &b_metadata);
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
        25,
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
    let pid = metadata["pid"].as_u64().expect("daemon pid") as u32;
    kill_hard(pid);
    wait_or_kill(&mut client.child);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "daemon EOF must cancel the proxy's blocked stdin copy"
    );
    root.disarm_daemon();
    let _ = fs::remove_file(root.0.join(".code-graph/daemon.json"));
}

/// The UDS accept queue can accept one more transport connection than the
/// daemon's 128-service limit. The admission prelude must make that peer a
/// failed proxy attach rather than a byte pump connected to no MCP service.
#[cfg(target_os = "linux")]
#[test]
fn saturated_uds_attachment_falls_back_instead_of_reporting_a_dead_connection() {
    let _guard = process_test_guard();
    let root = TestRoot::new(true);

    // Start through the same public proxy route, then close that bootstrap
    // client before filling every daemon permit with acknowledged raw clients.
    let bootstrap = Client::spawn(&root.0, &[]);
    let metadata = wait_metadata(&root);
    bootstrap.close();
    let endpoint = metadata["endpoint"].as_str().expect("UDS endpoint");
    let holders: Vec<_> = (0..128)
        .map(|_| admitted_uds_connection(endpoint))
        .collect();

    // This is the 129th peer. A successful MCP initialize proves that the
    // normal proxy retried the failed admission and fell back in-process; it
    // cannot have treated the immediately dropped UDS connection as attached.
    let mut client = Client::spawn(&root.0, &[]);
    assert_eq!(
        client.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        25,
        "the saturated proxy still has MCP service through fallback"
    );
    let stderr = client.close();
    assert!(
        stderr.contains("daemon unavailable")
            && stderr.contains("falling back to in-process stdio"),
        "the 129th UDS client must fail attachment and use fallback: {stderr}"
    );

    drop(holders);
    stop_daemon(&root.0, &metadata);
    wait_runtime_cleanup(&root.0);
    root.disarm_daemon();
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
            25
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
    stop_daemon(&root.0, &metadata);
    wait_runtime_cleanup(&root.0);
    root.disarm_daemon();
}

#[cfg(target_os = "linux")]
#[test]
fn clean_binary_mismatch_recovers_a_truncated_request_and_replaces_the_old_owner() {
    let _guard = process_test_guard();
    let root = TestRoot::new(false);
    let client = Client::spawn(&root.0, &[]);
    let old = wait_metadata(&root);
    client.close();
    replace_metadata_sha(&root, &old, "different-clean-build".to_owned());
    fs::write(root.0.join(".code-graph/shutdown.request"), b"{\"pid\":")
        .expect("plant interrupted shutdown request");

    let started = Instant::now();
    let mut replacement = Client::spawn(&root.0, &[]);
    assert_eq!(
        replacement.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        25
    );
    let new = wait_replacement_metadata(&root, &old["owner"]);
    assert_ne!(
        new["owner"], old["owner"],
        "replacement publishes a new owner"
    );
    assert!(
        started.elapsed() < TIMEOUT,
        "replacement stays within client bound"
    );
    replacement.close();
    stop_daemon(&root.0, &new);
    wait_runtime_cleanup(&root.0);
    root.disarm_daemon();
}

#[test]
fn different_executable_content_replaces_even_when_build_sha_matches() {
    let _guard = process_test_guard();
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let root = TestRoot::new(false);
    // Windows resolves executables by extension; Unix by mode bits. The copy
    // must remain spawnable on both.
    #[cfg(unix)]
    let copied = root.0.join("altered-code-graph-mcp");
    #[cfg(windows)]
    let copied = root.0.join("altered-code-graph-mcp.exe");
    fs::copy(env!("CARGO_BIN_EXE_code-graph-mcp"), &copied).unwrap();
    OpenOptions::new()
        .append(true)
        .open(&copied)
        .unwrap()
        .write_all(b"code-graph-test-trailing-bytes")
        .unwrap();
    #[cfg(unix)]
    {
        let mode = fs::metadata(env!("CARGO_BIN_EXE_code-graph-mcp"))
            .unwrap()
            .permissions()
            .mode();
        fs::set_permissions(&copied, fs::Permissions::from_mode(mode)).unwrap();
    }
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
        25
    );
    let new = wait_metadata(&root);
    assert_ne!(new["owner"], old["owner"], "different executable replaced");
    client.close();
    let _ = altered.wait();
    stop_daemon(&root.0, &new);
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
        25
    );
    let current = wait_metadata(&root);
    assert_eq!(current["owner"], old["owner"]);
    first.close();
    second.close();
    stop_daemon(&root.0, &current);
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
        25
    );
    let new = wait_metadata(&root);
    assert_ne!(new["owner"], old["owner"], "dirty owner was replaced once");
    replacement.close();
    stop_daemon(&root.0, &new);
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
        25
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
    let admitted_marker = root.0.join("persist-admitted.marker");
    let admitted_marker_string = admitted_marker.to_string_lossy().into_owned();
    let completion_marker = root.0.join("persist-complete.marker");
    let completion_marker_string = completion_marker.to_string_lossy().into_owned();
    fs::write(&source, "fn persisted() {}\n").unwrap();
    let mut client = Client::launch_with_env(
        &root.0,
        &[],
        &[
            ("CODE_GRAPH_TEST_PERSIST_DELAY_ROOT", &root_string),
            ("CODE_GRAPH_TEST_PERSIST_DELAY_MILLIS", "3000"),
            (
                "CODE_GRAPH_TEST_PERSIST_ADMITTED_MARKER",
                &admitted_marker_string,
            ),
            ("CODE_GRAPH_TEST_PERSIST_MARKER", &completion_marker_string),
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
        if admitted_marker.exists() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "analyze did not enter an admitted delayed persist"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !completion_marker.exists(),
        "admission marker is emitted before delayed persistence completes"
    );
    replace_metadata_sha(&root, &old, "persist-replacement-dirty".to_owned());
    let started = Instant::now();
    let mut replacement = Client::spawn(&root.0, &[]);
    assert_eq!(
        replacement.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        25
    );
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "acknowledged replacement waited past request grace for the admitted cache save"
    );
    let mut graph = Graph::new();
    assert!(graph.load(&root.0).unwrap(), "drained cache is loadable");
    assert_eq!(graph.stats().files, 1, "drained cache is current");
    assert!(
        completion_marker.exists(),
        "completion marker remains a post-save signal"
    );
    let new = wait_metadata(&root);
    client.close();
    replacement.close();
    stop_daemon(&root.0, &new);
    wait_runtime_cleanup(&root.0);
    root.disarm_daemon();
}

#[cfg(all(debug_assertions, target_os = "linux"))]
#[test]
fn root_replacement_during_admitted_persist_uses_the_retained_cache_inode() {
    let _guard = process_test_guard();
    let root = TestRoot::new(false);
    let root_string = root.0.to_string_lossy().into_owned();
    let source = root.0.join("persist.rs");
    let admitted_marker = root.0.join("persist-admitted.marker");
    let admitted_marker_string = admitted_marker.to_string_lossy().into_owned();
    let completion_marker = root.0.join("persist-complete.marker");
    let completion_marker_string = completion_marker.to_string_lossy().into_owned();
    fs::write(&source, "fn persisted_after_root_replace() {}\n").unwrap();
    let mut client = Client::launch_with_env(
        &root.0,
        &[],
        &[
            ("CODE_GRAPH_TEST_PERSIST_DELAY_ROOT", &root_string),
            ("CODE_GRAPH_TEST_PERSIST_DELAY_MILLIS", "3000"),
            (
                "CODE_GRAPH_TEST_PERSIST_ADMITTED_MARKER",
                &admitted_marker_string,
            ),
            ("CODE_GRAPH_TEST_PERSIST_MARKER", &completion_marker_string),
        ],
    )
    .initialize();
    client.tool(
        "analyze_codebase_async",
        json!({"path":root.0, "force":true}),
    );
    let deadline = Instant::now() + TIMEOUT;
    while !admitted_marker.exists() {
        assert!(
            Instant::now() < deadline,
            "analyze did not enter delayed admitted persistence"
        );
        thread::sleep(Duration::from_millis(10));
    }

    let relocated = root.0.with_extension("relocated");
    fs::rename(&root.0, &relocated).unwrap();
    fs::create_dir(&root.0).unwrap();
    client.close();

    let deadline = Instant::now() + TIMEOUT;
    while !completion_marker.exists() {
        assert!(
            Instant::now() < deadline,
            "retained-root persistence did not complete"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !code_graph_graph::cache_path(&root.0).exists(),
        "the old daemon never writes its cache through the replacement root"
    );
    let mut graph = Graph::new();
    assert!(
        graph.load(&relocated).unwrap(),
        "the retained original root receives a loadable admitted cache"
    );

    let mut successor = Client::spawn(&root.0, &[]);
    assert_eq!(
        successor.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        25,
        "a new proxy client converges on the replacement-root daemon"
    );
    let new = wait_metadata(&root);
    successor.close();
    stop_daemon(&root.0, &new);
    wait_runtime_cleanup(&root.0);
    root.disarm_daemon();
    fs::remove_dir_all(relocated).unwrap();
}
