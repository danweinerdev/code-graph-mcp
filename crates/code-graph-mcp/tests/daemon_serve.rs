//! Process coverage for the explicit `--serve` daemon mode.

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

const READY_TIMEOUT: Duration = Duration::from_secs(5);
static ROOT_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static PROCESS_TEST_SERIALIZATION: OnceLock<Mutex<()>> = OnceLock::new();

fn process_test_guard() -> MutexGuard<'static, ()> {
    PROCESS_TEST_SERIALIZATION
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Reaps a test daemon on every failure path. Successful tests explicitly
/// wait for children before this guard is dropped, so it cannot hide leaks.
struct DaemonChild {
    child: Child,
    stderr: Option<ChildStderr>,
}

impl DaemonChild {
    fn spawn(root: &std::path::Path, capture_stderr: bool) -> Self {
        Self::spawn_with_env(root, capture_stderr, &[])
    }

    fn spawn_with_env(
        root: &std::path::Path,
        capture_stderr: bool,
        environment: &[(String, String)],
    ) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_code-graph-mcp"));
        command
            .arg("--serve")
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(if capture_stderr {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        command.envs(environment.iter().map(|(key, value)| (key, value)));
        let mut child = command.spawn().expect("spawn daemon contender");
        let stderr = child.stderr.take();
        Self { child, stderr }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn wait(&mut self) {
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if self.child.try_wait().expect("poll daemon child").is_some() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "daemon child {} did not exit",
                self.pid()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn stderr_text(&mut self) -> String {
        let mut output = String::new();
        if let Some(mut stderr) = self.stderr.take() {
            stderr
                .read_to_string(&mut output)
                .expect("read daemon stderr");
        }
        output
    }
}

impl Drop for DaemonChild {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct TestRoot(PathBuf);

impl TestRoot {
    fn new(iteration: usize) -> Self {
        for _ in 0..1_000 {
            let sequence = ROOT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "code-graph-mcp-daemon-serve-{}-{iteration}-{nonce}-{sequence}",
                std::process::id(),
            ));
            match fs::create_dir(&root) {
                Ok(()) => return Self(root),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create fresh test project root: {error}"),
            }
        }
        panic!("could not allocate a fresh daemon serve test root")
    }

    fn with_idle_timeout(iteration: usize, seconds: u64) -> Self {
        let root = Self::new(iteration);
        fs::write(
            root.0.join(".code-graph.toml"),
            format!("[daemon]\nidle_timeout_secs = {seconds}\n"),
        )
        .expect("write daemon test config");
        root
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn wait_for_metadata(root: &std::path::Path) -> Value {
    let metadata = root.join(".code-graph/daemon.json");
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if let Ok(contents) = fs::read(&metadata) {
            return serde_json::from_slice(&contents).expect("daemon metadata JSON");
        }
        assert!(Instant::now() < deadline, "daemon did not publish metadata");
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_metadata_for_pid(root: &std::path::Path, pid: u32) -> Value {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        let metadata = wait_for_metadata(root);
        if metadata["pid"].as_u64() == Some(u64::from(pid)) {
            return metadata;
        }
        assert!(
            Instant::now() < deadline,
            "replacement daemon did not publish metadata"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn stop_owned_daemon(metadata: &Value) {
    let pid = metadata["pid"].as_u64().expect("metadata pid").to_string();
    let status = Command::new("kill")
        .args(["-INT", &pid])
        .status()
        .expect("send SIGINT to daemon");
    assert!(status.success(), "SIGINT daemon");
}

fn kill_unclean(child: &mut DaemonChild) {
    child.child.kill().expect("SIGKILL daemon");
    child.wait();
}

fn wait_for_cleanup(root: &std::path::Path) {
    let runtime = root.join(".code-graph");
    let deadline = Instant::now() + READY_TIMEOUT;
    while runtime.join("daemon.lock").exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!runtime.join("daemon.lock").exists(), "daemon lock cleanup");
    assert!(
        !runtime.join("daemon.json").exists(),
        "daemon metadata cleanup"
    );
}

fn assert_alive(daemon: &mut DaemonChild, detail: &str) {
    assert!(
        daemon
            .child
            .try_wait()
            .expect("poll daemon child")
            .is_none(),
        "{detail}"
    );
}

fn mcp_round_trip<W: Write, R: Read>(mut writer: W, reader: R) {
    writeln!(
        writer,
        "{}",
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "daemon-test", "version": "0.1.0" }
            }
        })
    )
    .expect("write initialize");
    writer.flush().expect("flush initialize");
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .expect("read initialize response");
    assert_eq!(
        serde_json::from_str::<Value>(&line).expect("initialize JSON")["id"],
        1
    );

    writeln!(
        writer,
        "{}",
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
    )
    .expect("write initialized notification");
    writeln!(
        writer,
        "{}",
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
    )
    .expect("write tools/list");
    writer.flush().expect("flush tools/list");
    line.clear();
    reader
        .read_line(&mut line)
        .expect("read tools/list response");
    let response: Value = serde_json::from_str(&line).expect("tools/list JSON");
    assert_eq!(response["id"], 2);
    assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 23);
}

fn uds_connect(endpoint: &str) -> UnixStream {
    let mut stream = UnixStream::connect(endpoint).expect("connect UDS from metadata");
    stream.set_read_timeout(Some(READY_TIMEOUT)).unwrap();
    let mut acknowledgement = [0_u8; 6];
    stream
        .read_exact(&mut acknowledgement)
        .expect("read UDS admission acknowledgement");
    assert_eq!(&acknowledgement, b"CG-OK\n");
    stream
}

fn uds_mcp(endpoint: &str) {
    let stream = uds_connect(endpoint);
    mcp_round_trip(stream.try_clone().unwrap(), stream);
}

fn uds_analyze(endpoint: &str, root: &std::path::Path, asynchronous: bool, force: bool) -> Value {
    let stream = uds_connect(endpoint);
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    writeln!(
        writer,
        "{}",
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                       "clientInfo": {"name": "daemon-idle-test", "version": "1"}}
        })
    )
    .unwrap();
    writer.flush().unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["id"], 1);
    writeln!(
        writer,
        "{}",
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
    )
    .unwrap();
    writeln!(
        writer,
        "{}",
        json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": if asynchronous { "analyze_codebase_async" } else { "analyze_codebase" },
                       "arguments": {"path": root, "force": force}}
        })
    )
    .unwrap();
    writer.flush().unwrap();
    line.clear();
    reader.read_line(&mut line).unwrap();
    let response: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["id"], 2);
    response
}

fn wait_for_path(path: &std::path::Path) {
    let deadline = Instant::now() + READY_TIMEOUT;
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} was not created",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn tcp_mcp(endpoint: &str, token: &str) {
    let mut stream = TcpStream::connect(endpoint).expect("connect TCP from metadata");
    stream.set_read_timeout(Some(READY_TIMEOUT)).unwrap();
    writeln!(stream, "CG-AUTH {token}").expect("write TCP authentication");
    stream.flush().expect("flush TCP authentication");
    let mut acknowledgement = [0_u8; 6];
    stream
        .read_exact(&mut acknowledgement)
        .expect("read TCP authentication acknowledgement");
    assert_eq!(&acknowledgement, b"CG-OK\n");
    mcp_round_trip(stream.try_clone().unwrap(), stream);
}

fn tcp_rejected(endpoint: &str, prelude: &[u8]) {
    let mut stream = TcpStream::connect(endpoint).expect("connect rejected TCP client");
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream.write_all(prelude).expect("write rejected prelude");
    stream.flush().expect("flush rejected prelude");
    let mut byte = [0_u8; 1];
    assert!(
        stream.read(&mut byte).map_or(true, |read| read == 0),
        "unauthenticated TCP client must not receive MCP data"
    );
}

#[test]
fn serve_publishes_owner_only_uds_and_serves_mcp() {
    let _guard = process_test_guard();
    let root = TestRoot::new(0);
    let mut daemon = DaemonChild::spawn(&root.0, false);
    let metadata = wait_for_metadata(&root.0);
    let runtime = root.0.join(".code-graph");

    assert_eq!(metadata["pid"].as_u64(), Some(u64::from(daemon.pid())));
    assert_eq!(metadata["transport"], "uds");
    assert_eq!(
        fs::metadata(&runtime).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let endpoint = metadata["endpoint"].as_str().expect("UDS endpoint");
    assert!(fs::symlink_metadata(endpoint)
        .unwrap()
        .file_type()
        .is_socket());
    assert_eq!(
        fs::metadata(endpoint).unwrap().permissions().mode() & 0o777,
        0o600
    );
    uds_mcp(endpoint);

    stop_owned_daemon(&metadata);
    daemon.wait();
    wait_for_cleanup(&root.0);
    assert!(!runtime.join("daemon.sock").exists(), "UDS cleanup");
}

#[test]
fn simultaneous_serve_contenders_converge_twenty_times_without_leaks() {
    let _guard = process_test_guard();
    for iteration in 0..20 {
        let root = TestRoot::new(iteration);
        let mut children: Vec<DaemonChild> =
            (0..6).map(|_| DaemonChild::spawn(&root.0, false)).collect();
        let metadata = wait_for_metadata(&root.0);
        let winner = metadata["pid"].as_u64().expect("metadata pid") as u32;
        let winner_index = children
            .iter()
            .position(|child| child.pid() == winner)
            .expect("metadata PID identifies one spawned contender");
        for (index, child) in children.iter_mut().enumerate() {
            if index != winner_index {
                child.wait();
            }
        }
        uds_mcp(metadata["endpoint"].as_str().unwrap());
        stop_owned_daemon(&metadata);
        children[winner_index].wait();
        wait_for_cleanup(&root.0);
        assert!(
            !root.0.join(".code-graph/daemon.sock").exists(),
            "iteration {iteration}: no daemon socket leaks"
        );
    }
}

#[test]
fn simultaneous_contenders_recover_one_stale_lock() {
    let _guard = process_test_guard();
    let root = TestRoot::new(2);
    let runtime = root.0.join(".code-graph");
    fs::create_dir_all(&runtime).unwrap();
    fs::write(
        runtime.join("daemon.lock"),
        json!({"pid": 99999999_u32, "start_time": 0_u64, "nonce": "stale"}).to_string(),
    )
    .unwrap();
    let mut children: Vec<DaemonChild> =
        (0..6).map(|_| DaemonChild::spawn(&root.0, false)).collect();
    let metadata = wait_for_metadata(&root.0);
    let winner = metadata["pid"].as_u64().expect("metadata pid") as u32;
    let winner_index = children
        .iter()
        .position(|child| child.pid() == winner)
        .expect("one contender owns stale-lock replacement");
    for (index, child) in children.iter_mut().enumerate() {
        if index != winner_index {
            child.wait();
        }
    }
    uds_mcp(metadata["endpoint"].as_str().unwrap());
    stop_owned_daemon(&metadata);
    children[winner_index].wait();
    wait_for_cleanup(&root.0);
    assert!(!runtime.join("daemon.lock").exists(), "lock cleanup");
}

#[test]
fn tcp_fallback_authenticates_and_rotates_after_crash_recovery() {
    let _guard = process_test_guard();
    let root = TestRoot::new(1);
    let runtime = root.0.join(".code-graph");
    fs::create_dir_all(&runtime).unwrap();
    let socket_path = runtime.join("daemon.sock");
    fs::write(&socket_path, b"force TCP fallback").unwrap();

    let mut first = DaemonChild::spawn(&root.0, true);
    let first_metadata = wait_for_metadata_for_pid(&root.0, first.pid());
    assert_eq!(first_metadata["transport"], "tcp");
    assert!(fs::symlink_metadata(&socket_path)
        .unwrap()
        .file_type()
        .is_file());
    let first_token = fs::read_to_string(runtime.join("secret"))
        .unwrap()
        .trim()
        .to_owned();
    assert_eq!(first_token.len(), 64);
    assert_eq!(
        fs::metadata(runtime.join("secret"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    tcp_rejected(first_metadata["endpoint"].as_str().unwrap(), b"");
    tcp_rejected(
        first_metadata["endpoint"].as_str().unwrap(),
        b"CG-AUTH 0000000000000000000000000000000000000000000000000000000000000000\n",
    );
    tcp_mcp(first_metadata["endpoint"].as_str().unwrap(), &first_token);

    kill_unclean(&mut first);
    let mut second = DaemonChild::spawn(&root.0, true);
    let second_metadata = wait_for_metadata_for_pid(&root.0, second.pid());
    let second_token = fs::read_to_string(runtime.join("secret"))
        .unwrap()
        .trim()
        .to_owned();
    assert_eq!(second_metadata["transport"], "tcp");
    assert_ne!(
        first_token, second_token,
        "token rotates per daemon instance"
    );
    tcp_rejected(
        second_metadata["endpoint"].as_str().unwrap(),
        format!("CG-AUTH {first_token}\n").as_bytes(),
    );
    tcp_mcp(second_metadata["endpoint"].as_str().unwrap(), &second_token);

    stop_owned_daemon(&second_metadata);
    second.wait();
    wait_for_cleanup(&root.0);
    assert!(fs::symlink_metadata(&socket_path)
        .unwrap()
        .file_type()
        .is_file());
    let first_stderr = first.stderr_text();
    let second_stderr = second.stderr_text();
    assert!(
        first_stderr.contains("UDS unavailable") && second_stderr.contains("UDS unavailable"),
        "TCP fallback is reported"
    );
}

#[test]
fn idle_daemon_exits_at_zero_clients_and_zero_never_exits() {
    let _guard = process_test_guard();
    let one_second = TestRoot::with_idle_timeout(3, 1);
    let mut daemon = DaemonChild::spawn(&one_second.0, false);
    wait_for_metadata(&one_second.0);
    daemon.wait();
    wait_for_cleanup(&one_second.0);

    let never = TestRoot::with_idle_timeout(4, 0);
    let mut daemon = DaemonChild::spawn(&never.0, false);
    let metadata = wait_for_metadata(&never.0);
    thread::sleep(Duration::from_millis(1_300));
    assert_alive(
        &mut daemon,
        "zero idle timeout must not exit during observation",
    );
    stop_owned_daemon(&metadata);
    daemon.wait();
    wait_for_cleanup(&never.0);
}

#[test]
fn attached_client_and_disconnect_restart_the_full_idle_timeout() {
    let _guard = process_test_guard();
    let root = TestRoot::with_idle_timeout(5, 2);
    let mut daemon = DaemonChild::spawn(&root.0, false);
    let metadata = wait_for_metadata(&root.0);
    // Attach promptly, hold beyond the timeout, then verify disconnect starts
    // a fresh interval rather than resuming an elapsed zero-client countdown.
    let stream = uds_connect(metadata["endpoint"].as_str().unwrap());
    thread::sleep(Duration::from_millis(2_300));
    assert_alive(&mut daemon, "attached client must prevent idle exit");
    drop(stream);
    thread::sleep(Duration::from_millis(500));
    assert_alive(&mut daemon, "disconnect must restart a full idle timeout");
    daemon.wait();
    wait_for_cleanup(&root.0);
}

#[test]
fn idle_waits_for_delayed_async_analyze_then_persists_a_warm_cache() {
    let _guard = process_test_guard();
    let root = TestRoot::with_idle_timeout(6, 1);
    fs::write(root.0.join("main.rs"), "fn benchmark_idle() {}\n").unwrap();
    let marker = root.0.join("persist.marker");
    let environment = [
        (
            "CODE_GRAPH_TEST_PERSIST_DELAY_ROOT".to_owned(),
            root.0.to_string_lossy().into_owned(),
        ),
        (
            "CODE_GRAPH_TEST_PERSIST_DELAY_MILLIS".to_owned(),
            "1600".to_owned(),
        ),
        (
            "CODE_GRAPH_TEST_PERSIST_MARKER".to_owned(),
            marker.to_string_lossy().into_owned(),
        ),
    ];
    let mut daemon = DaemonChild::spawn_with_env(&root.0, true, &environment);
    let metadata = wait_for_metadata(&root.0);
    let response = uds_analyze(metadata["endpoint"].as_str().unwrap(), &root.0, true, true);
    assert_eq!(
        response["result"]["content"][0]["text"]
            .as_str()
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
            .unwrap()["status"],
        "running"
    );
    wait_for_path(&marker);
    // The debug marker is written only after g.save returns. The daemon must
    // remain alive for a fresh sub-timeout interval from this terminal point.
    thread::sleep(Duration::from_millis(400));
    assert_alive(
        &mut daemon,
        "idle timer must start after terminal analyze persistence completes",
    );
    daemon.wait();
    wait_for_cleanup(&root.0);
    let cache = root.0.join(".code-graph-cache.db");
    let first_cache = fs::read(&cache).expect("idle exit persisted cache");

    let mut warm = DaemonChild::spawn(&root.0, true);
    let warm_metadata = wait_for_metadata(&root.0);
    let response = uds_analyze(
        warm_metadata["endpoint"].as_str().unwrap(),
        &root.0,
        false,
        false,
    );
    assert!(response["result"].is_object(), "warm analyze succeeds");
    warm.wait();
    wait_for_cleanup(&root.0);
    assert_eq!(
        fs::read(&cache).unwrap(),
        first_cache,
        "a loadable unchanged cache takes the no-save fast path"
    );
    let stderr = warm.stderr_text();
    assert!(
        !stderr.contains("phase: discovering + parsing under"),
        "warm --serve analyze must not parse unchanged source: {stderr}"
    );
}

#[test]
fn unauthenticated_tcp_socket_does_not_hold_idle_daemon_alive() {
    let _guard = process_test_guard();
    let root = TestRoot::with_idle_timeout(7, 1);
    let runtime = root.0.join(".code-graph");
    fs::create_dir_all(&runtime).unwrap();
    fs::write(runtime.join("daemon.sock"), b"force TCP fallback").unwrap();
    let mut daemon = DaemonChild::spawn(&root.0, false);
    let metadata = wait_for_metadata(&root.0);
    assert_eq!(metadata["transport"], "tcp");
    let _unauthenticated = TcpStream::connect(metadata["endpoint"].as_str().unwrap()).unwrap();
    thread::sleep(Duration::from_millis(1_300));
    daemon.wait();
    wait_for_cleanup(&root.0);
}
