//! Repository-local daemon transport, ownership, and lifecycle.

use std::fs::{self, OpenOptions};
use std::future::Future;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use code_graph_core::RootConfig;
use code_graph_tools::CodeGraphServer;
use fs2::FileExt;
use rmcp::ServiceExt;
use serde::{Deserialize, Serialize};
use sysinfo::System;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::timeout;

#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, PermissionsExt};

const LOCK_FILE: &str = "daemon.lock";
const METADATA_FILE: &str = "daemon.json";
const SECRET_FILE: &str = "secret";
const SOCKET_FILE: &str = "daemon.sock";
const AUTH_PREFIX: &[u8] = b"CG-AUTH ";
const AUTH_ACK: &[u8] = b"CG-OK\n";
const AUTH_LINE_LEN: usize = 73;
const AUTH_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_PENDING_CONNECTIONS: usize = 128;
const PROXY_ATTACH_DEADLINE: Duration = Duration::from_secs(4);
const PROXY_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const PROXY_RETRY_INTERVAL: Duration = Duration::from_millis(50);
const PROXY_MAX_SPAWN_BACKOFF: Duration = Duration::from_secs(1);
const PROXY_LOSER_REAP_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Clone, Debug)]
struct DaemonPaths {
    root: PathBuf,
    runtime: PathBuf,
    lock: PathBuf,
    metadata: PathBuf,
    secret: PathBuf,
    socket: PathBuf,
}

impl DaemonPaths {
    fn for_root(root: &Path) -> Self {
        let runtime = root.join(".code-graph");
        Self {
            root: root.to_path_buf(),
            lock: runtime.join(LOCK_FILE),
            metadata: runtime.join(METADATA_FILE),
            secret: runtime.join(SECRET_FILE),
            socket: runtime.join(SOCKET_FILE),
            runtime,
        }
    }

    fn ensure_runtime_dir(&self) -> anyhow::Result<()> {
        let canonical_root = fs::canonicalize(&self.root)
            .with_context(|| format!("canonicalize daemon project root {}", self.root.display()))?;
        match fs::symlink_metadata(&self.runtime) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                bail!(
                    "daemon runtime path {} must be a real directory",
                    self.runtime.display()
                )
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&self.runtime).with_context(|| {
                    format!("create daemon runtime directory {}", self.runtime.display())
                })?;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "inspect daemon runtime directory {}",
                        self.runtime.display()
                    )
                })
            }
        }
        let canonical_runtime = fs::canonicalize(&self.runtime).with_context(|| {
            format!(
                "canonicalize daemon runtime directory {}",
                self.runtime.display()
            )
        })?;
        if canonical_runtime != canonical_root.join(".code-graph")
            || canonical_runtime.parent() != Some(canonical_root.as_path())
        {
            bail!(
                "daemon runtime directory {} is not a direct child of project root {}",
                canonical_runtime.display(),
                canonical_root.display()
            )
        }
        #[cfg(unix)]
        fs::set_permissions(&self.runtime, std::fs::Permissions::from_mode(0o700)).with_context(
            || {
                format!(
                    "restrict daemon runtime directory {}",
                    self.runtime.display()
                )
            },
        )?;
        #[cfg(windows)]
        restrict_windows_runtime_dir(&self.runtime)?;
        Ok(())
    }
}

/// Attaches the invoking stdio process to the repository-local daemon.
///
/// The caller has already discovered `root` with [`RootConfig::load`]. This
/// function intentionally does not create `.code-graph`: disabled/direct
/// invocations must remain indistinguishable from the pre-daemon binary.
/// Every retry reads metadata and probes its recorded endpoint anew, because a
/// competing daemon can publish between any two attempts.
pub async fn proxy(root: PathBuf) -> anyhow::Result<()> {
    let paths = DaemonPaths::for_root(&root);
    let deadline = std::time::Instant::now() + PROXY_ATTACH_DEADLINE;
    let mut last_error = None;
    let mut contender = None;
    let mut next_spawn_at = std::time::Instant::now();
    let mut spawn_backoff = PROXY_RETRY_INTERVAL;

    while std::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match connect_from_metadata(&paths, remaining.min(PROXY_CONNECT_TIMEOUT)).await {
            Ok(connection) => {
                settle_contender(contender, connection.pid).await;
                return finish_proxy(connection.stream).await;
            }
            Err(error) => last_error = Some(error),
        }

        if contender.as_mut().is_some_and(contender_exited) {
            // `try_wait` reaped the naturally exiting loser. Only now may a
            // new contender be started; a slow starter is never killed just
            // because its first metadata probe missed publication.
            contender = None;
            next_spawn_at = std::time::Instant::now() + spawn_backoff;
            spawn_backoff = spawn_backoff.saturating_mul(2).min(PROXY_MAX_SPAWN_BACKOFF);
        }
        if contender.is_none() && std::time::Instant::now() >= next_spawn_at {
            match spawn_contender(&root).await {
                Ok(child) => {
                    contender = Some(child);
                    spawn_backoff = PROXY_RETRY_INTERVAL;
                }
                Err(error) => {
                    last_error = Some(error);
                    next_spawn_at = std::time::Instant::now() + spawn_backoff;
                    spawn_backoff = spawn_backoff.saturating_mul(2).min(PROXY_MAX_SPAWN_BACKOFF);
                }
            }
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let until_spawn = next_spawn_at.saturating_duration_since(std::time::Instant::now());
        tokio::time::sleep(remaining.min(PROXY_RETRY_INTERVAL.max(until_spawn))).await;
    }

    // One final short probe closes the publication race at the deadline while
    // keeping the total attach budget comfortably under five seconds.
    if let Ok(connection) = connect_from_metadata(&paths, Duration::from_millis(250)).await {
        settle_contender(contender, connection.pid).await;
        return finish_proxy(connection.stream).await;
    }
    terminate_contender(contender).await;
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("daemon did not publish metadata")))
}

async fn finish_proxy(stream: ClientStream) -> anyhow::Result<()> {
    // Once connected, MCP framing may already be in flight. A second server
    // cannot safely reconstruct that session, so an established-stream error
    // ends this process rather than falling back to a new in-process server.
    let _ = pump_connection(stream).await;
    Ok(())
}

async fn spawn_contender(root: &Path) -> anyhow::Result<tokio::process::Child> {
    let executable = std::env::current_exe().context("locate code-graph-mcp executable")?;
    tokio::process::Command::new(executable)
        .arg("--serve")
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn repository-local daemon contender")
}

fn contender_exited(child: &mut tokio::process::Child) -> bool {
    child.try_wait().is_ok_and(|status| status.is_some())
}

async fn settle_contender(contender: Option<tokio::process::Child>, owner_pid: u32) {
    if let Some(mut child) = contender {
        if child.id() != Some(owner_pid) {
            // A successfully attached, different owner proves this child is
            // a loser. Give it time to exit from lock loss; only this proven
            // loser is terminated if it ignores that clean path. Never kill a
            // still-starting child merely because one metadata probe missed.
            if timeout(PROXY_LOSER_REAP_TIMEOUT, child.wait())
                .await
                .is_err()
            {
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
            return;
        }
        // The child is the long-lived owner. Keep a waiter alive after this
        // proxy exits so its eventual termination is reaped.
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
    }
}

async fn terminate_contender(contender: Option<tokio::process::Child>) {
    let Some(mut child) = contender else {
        return;
    };
    if child.try_wait().is_ok_and(|status| status.is_some()) {
        return;
    }

    // Falling back starts an independent in-process server. A contender left
    // running here could publish afterwards and race that fallback, so signal
    // it before returning. Reaping remains bounded just like loser cleanup.
    let _ = child.start_kill();
    let _ = timeout(PROXY_LOSER_REAP_TIMEOUT, child.wait()).await;
}

struct ConnectedDaemon {
    pid: u32,
    stream: ClientStream,
}

#[cfg(unix)]
enum ClientStream {
    Uds(tokio::net::UnixStream),
    Tcp(tokio::net::TcpStream),
}

#[cfg(windows)]
enum ClientStream {
    Pipe(tokio::net::windows::named_pipe::NamedPipeClient),
    Tcp(tokio::net::TcpStream),
}

async fn connect_from_metadata(
    paths: &DaemonPaths,
    connect_timeout: Duration,
) -> anyhow::Result<ConnectedDaemon> {
    let encoded = fs::read(&paths.metadata)
        .with_context(|| format!("read daemon metadata {}", paths.metadata.display()))?;
    let metadata: DaemonMetadata = serde_json::from_slice(&encoded)
        .with_context(|| format!("parse daemon metadata {}", paths.metadata.display()))?;

    let stream = match metadata.transport {
        #[cfg(unix)]
        Transport::Uds => ClientStream::Uds(
            timeout(
                connect_timeout,
                tokio::net::UnixStream::connect(&metadata.endpoint),
            )
            .await
            .context("connect daemon UDS timed out")??,
        ),
        #[cfg(windows)]
        Transport::Pipe => ClientStream::Pipe(
            tokio::net::windows::named_pipe::ClientOptions::new()
                .open(&metadata.endpoint)
                .with_context(|| format!("connect daemon pipe {}", metadata.endpoint))?,
        ),
        Transport::Tcp => {
            let mut stream = timeout(
                connect_timeout,
                tokio::net::TcpStream::connect(&metadata.endpoint),
            )
            .await
            .context("connect daemon TCP timed out")??;
            let token = read_client_secret(&paths.secret)?;
            stream
                .write_all(format!("CG-AUTH {token}\n").as_bytes())
                .await
                .context("send daemon TCP authentication")?;
            stream
                .flush()
                .await
                .context("flush daemon TCP authentication")?;
            let mut acknowledgement = [0_u8; AUTH_ACK.len()];
            timeout(connect_timeout, stream.read_exact(&mut acknowledgement))
                .await
                .context("read daemon TCP authentication acknowledgement timed out")?
                .context("read daemon TCP authentication acknowledgement")?;
            if acknowledgement != AUTH_ACK {
                bail!("invalid daemon TCP authentication acknowledgement")
            }
            ClientStream::Tcp(stream)
        }
        #[cfg(not(unix))]
        Transport::Uds => bail!("daemon metadata selects unsupported UDS transport"),
        #[cfg(not(windows))]
        Transport::Pipe => bail!("daemon metadata selects unsupported pipe transport"),
    };

    Ok(ConnectedDaemon {
        pid: metadata.pid,
        stream,
    })
}

fn read_client_secret(path: &Path) -> anyhow::Result<String> {
    // The token is immediately copied into the auth prelude. Do not accept
    // whitespace or alternate encodings around the owner-only file contents.
    let contents = fs::read_to_string(path)
        .with_context(|| format!("read daemon TCP secret {}", path.display()))?;
    let token = match contents.as_bytes() {
        bytes if bytes.len() == 64 => contents.as_str(),
        bytes if bytes.len() == 65 && bytes[64] == b'\n' => &contents[..64],
        _ => "",
    };
    if token.len() != 64
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("daemon TCP secret is not a 64-character lowercase hexadecimal token")
    }
    Ok(token.to_owned())
}

async fn pump_connection(stream: ClientStream) -> anyhow::Result<()> {
    match stream {
        #[cfg(unix)]
        ClientStream::Uds(stream) => pump_bytes(stream).await,
        #[cfg(windows)]
        ClientStream::Pipe(stream) => pump_bytes(stream).await,
        ClientStream::Tcp(stream) => pump_bytes(stream).await,
    }
}

async fn pump_bytes<S>(stream: S) -> anyhow::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut socket_read, mut socket_write) = tokio::io::split(stream);
    let stdin_to_socket = async {
        let mut stdin = tokio::io::stdin();
        tokio::io::copy(&mut stdin, &mut socket_write)
            .await
            .context("proxy stdin to daemon")?;
        socket_write
            .shutdown()
            .await
            .context("shutdown daemon socket after stdin EOF")
    };
    let socket_to_stdout = async {
        let mut stdout = tokio::io::stdout();
        tokio::io::copy(&mut socket_read, &mut stdout)
            .await
            .context("proxy daemon to stdout")?;
        stdout.flush().await.context("flush proxy stdout")
    };

    tokio::pin!(stdin_to_socket);
    tokio::pin!(socket_to_stdout);
    // A broken established stream ends this proxy session immediately, even
    // when the parent still holds stdin open. Conversely, EOF on stdin first
    // half-closes the socket and then drains every daemon byte before exit.
    tokio::select! {
        biased;
        output = &mut socket_to_stdout => output,
        input = &mut stdin_to_socket => {
            input?;
            socket_to_stdout.await
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Transport {
    Uds,
    Pipe,
    Tcp,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct DaemonMetadata {
    pid: u32,
    transport: Transport,
    endpoint: String,
    binary_sha: String,
    started_at: String,
    owner: LockIdentity,
}

impl DaemonMetadata {
    fn new(transport: Transport, endpoint: String, owner: LockIdentity) -> Self {
        Self {
            pid: std::process::id(),
            transport,
            endpoint,
            binary_sha: env!("CODE_GRAPH_GIT_SHA").to_owned(),
            started_at: rfc3339_now(),
            owner,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct LockIdentity {
    pid: u32,
    start_time: u64,
    nonce: String,
}

impl LockIdentity {
    fn current() -> std::io::Result<Self> {
        let pid = std::process::id();
        let start_time = process_start_time(pid).ok_or_else(|| {
            std::io::Error::other("sysinfo could not determine daemon process start time")
        })?;
        Ok(Self {
            pid,
            start_time,
            nonce: random_hex(16)?,
        })
    }
}

struct DaemonLock {
    path: PathBuf,
    contents: String,
    identity: LockIdentity,
    file: Option<std::fs::File>,
}

impl DaemonLock {
    fn acquire(paths: &DaemonPaths) -> std::io::Result<Self> {
        let identity = LockIdentity::current()?;
        let contents = format!(
            "{}\n",
            serde_json::to_string(&identity).map_err(std::io::Error::other)?
        );
        let mut file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&paths.lock)?;
        let initialize = (|| -> std::io::Result<()> {
            file.try_lock_exclusive()?;
            file.write_all(contents.as_bytes())?;
            file.sync_all()
        })();
        if let Err(error) = initialize {
            let _ = FileExt::unlock(&file);
            drop(file);
            return Err(error);
        }
        Ok(Self {
            path: paths.lock.clone(),
            contents,
            identity,
            file: Some(file),
        })
    }

    fn still_owned(&self) -> bool {
        self.file.is_some()
            && fs::read_to_string(&self.path).is_ok_and(|contents| contents == self.contents)
    }

    fn write_identity(&mut self, identity: LockIdentity) -> std::io::Result<()> {
        let contents = format!(
            "{}\n",
            serde_json::to_string(&identity).map_err(std::io::Error::other)?
        );
        let file = self.file.as_mut().expect("locked daemon file");
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        self.contents = contents;
        self.identity = identity;
        Ok(())
    }

    fn release(mut self) {
        if let Some(file) = self.file.take() {
            let _ = FileExt::unlock(&file);
            drop(file);
        }
    }

    fn remove_if_owned(self) {
        let owned = self.still_owned();
        let path = self.path.clone();
        self.release();
        if owned {
            let _ = fs::remove_file(path);
        }
    }
}

/// Starts a daemon rooted at the nearest `.code-graph.toml` ancestor of the
/// current working directory. With no config file, the working directory is
/// the project root, matching [`RootConfig::load`].
pub async fn run(server: CodeGraphServer) -> anyhow::Result<()> {
    let cwd = std::env::current_dir().context("read current directory for daemon root")?;
    let cwd = fs::canonicalize(&cwd).context("canonicalize daemon root")?;
    let (_, root) = RootConfig::load(&cwd).context("discover daemon project root")?;
    slow_test_contender_start(&root).await;
    run_until(server, root, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}

#[cfg(debug_assertions)]
async fn slow_test_contender_start(root: &Path) {
    let Ok(delay_root) = std::env::var("CODE_GRAPH_TEST_DAEMON_DELAY_ROOT") else {
        return;
    };
    let Ok(delay_millis) = std::env::var("CODE_GRAPH_TEST_DAEMON_DELAY_MILLIS") else {
        return;
    };
    let Ok(delay_millis) = delay_millis.parse::<u64>() else {
        return;
    };
    if Path::new(&delay_root) == root {
        tokio::time::sleep(Duration::from_millis(delay_millis)).await;
    }
}

#[cfg(not(debug_assertions))]
async fn slow_test_contender_start(_root: &Path) {}

async fn run_until<F>(server: CodeGraphServer, root: PathBuf, shutdown: F) -> anyhow::Result<()>
where
    F: Future<Output = ()>,
{
    let paths = DaemonPaths::for_root(&root);
    paths.ensure_runtime_dir()?;

    let Some(lock) = acquire_or_detect_live(&paths).await? else {
        eprintln!(
            "code-graph-mcp: daemon contender lost lock for {}",
            root.display()
        );
        return Ok(());
    };

    // A stale metadata/token pair can outlive a crashed lock owner. A new
    // lock owner removes it only after proving the recorded endpoint and
    // process identity are both dead.
    if !prepare_runtime_for_owner(&paths).await {
        lock.remove_if_owned();
        return Ok(());
    }

    let (listener, metadata, tcp_secret) = match bind_listener(&paths, &lock).await {
        Ok(listener) => listener,
        Err(error) => {
            lock.remove_if_owned();
            return Err(error);
        }
    };

    // A contender must still own the exclusive-create lock at publication.
    // In particular, a socket bind is not a substitute for that ownership.
    if !lock.still_owned() {
        drop(listener);
        return Ok(());
    }

    if let Err(error) = write_metadata_atomically(&paths.metadata, &metadata) {
        drop(listener);
        cleanup_owned(&paths, lock, &metadata).await;
        return Err(error);
    }

    serve_listener(listener, server, tcp_secret, shutdown).await;
    cleanup_owned(&paths, lock, &metadata).await;
    Ok(())
}

async fn acquire_or_detect_live(paths: &DaemonPaths) -> anyhow::Result<Option<DaemonLock>> {
    match DaemonLock::acquire(paths) {
        Ok(lock) => Ok(Some(lock)),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&paths.lock)
                .context("open existing daemon lock")?;
            match file.try_lock_exclusive() {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
                Err(error) => return Err(error).context("lock existing daemon lock"),
            };
            let previous_identity = read_lock_identity_from(&file);
            if previous_identity.as_ref().is_some_and(identity_is_alive) {
                FileExt::unlock(&file).ok();
                return Ok(None);
            }

            // The OS lock is the single-instance authority. A dead or
            // malformed on-disk identity cannot own this file, so endpoint
            // reuse is not evidence that its metadata/token remain valid.
            cleanup_stale_runtime(paths, previous_identity.as_ref()).await;
            let identity = previous_identity.unwrap_or(LockIdentity {
                pid: 0,
                start_time: 0,
                nonce: "malformed".to_owned(),
            });
            let mut lock = DaemonLock {
                path: paths.lock.clone(),
                contents: format!("{}\n", serde_json::to_string(&identity)?),
                identity,
                file: Some(file),
            };
            lock.write_identity(LockIdentity::current()?)
                .context("replace stale daemon lock identity")?;
            Ok(Some(lock))
        }
        Err(error) => Err(error).context("create daemon lock"),
    }
}

fn read_lock_identity_from(file: &std::fs::File) -> Option<LockIdentity> {
    let mut file = file.try_clone().ok()?;
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut contents = Vec::new();
    file.read_to_end(&mut contents).ok()?;
    serde_json::from_slice(&contents).ok()
}

fn process_start_time(pid: u32) -> Option<u64> {
    let mut system = System::new_all();
    system.refresh_all();
    system
        .process(sysinfo::Pid::from_u32(pid))
        .map(sysinfo::Process::start_time)
}

fn identity_is_alive(identity: &LockIdentity) -> bool {
    process_start_time(identity.pid) == Some(identity.start_time)
}

async fn prepare_runtime_for_owner(paths: &DaemonPaths) -> bool {
    let Ok(contents) = fs::read(&paths.metadata) else {
        if paths.secret.exists() {
            let _ = fs::remove_file(&paths.secret);
        }
        return true;
    };
    let Ok(metadata) = serde_json::from_slice::<DaemonMetadata>(&contents) else {
        // The caller has the OS lock. Without usable metadata there is no
        // endpoint or owner that could still be live, so clear both files
        // before Windows metadata publication and TCP token creation.
        let _ = fs::remove_file(&paths.metadata);
        let _ = fs::remove_file(&paths.secret);
        return true;
    };
    if probe_endpoint(&metadata).await || identity_is_alive(&metadata.owner) {
        return false;
    }
    cleanup_stale_runtime(paths, Some(&metadata.owner)).await;
    true
}

async fn cleanup_stale_runtime(paths: &DaemonPaths, _owner: Option<&LockIdentity>) {
    // The caller holds the OS lock on daemon.lock. Do not probe an endpoint
    // here: a recycled TCP port or UDS pathname can belong to an unrelated
    // listener, while this locked identity is already known stale/malformed.
    // Metadata and TCP auth are daemon-instance state; a live UDS inode is
    // deliberately preserved for bind-time fallback handling.
    let _ = fs::remove_file(&paths.metadata);
    let _ = fs::remove_file(&paths.secret);
}

async fn probe_endpoint(metadata: &DaemonMetadata) -> bool {
    match metadata.transport {
        #[cfg(unix)]
        Transport::Uds => tokio::net::UnixStream::connect(&metadata.endpoint)
            .await
            .is_ok(),
        #[cfg(windows)]
        Transport::Pipe => tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&metadata.endpoint)
            .is_ok(),
        Transport::Tcp => tokio::net::TcpStream::connect(&metadata.endpoint)
            .await
            .is_ok(),
        #[cfg(not(unix))]
        Transport::Uds => false,
        #[cfg(not(windows))]
        Transport::Pipe => false,
    }
}

#[cfg(unix)]
enum Listener {
    Uds(tokio::net::UnixListener),
    Tcp(tokio::net::TcpListener),
}

#[cfg(windows)]
enum Listener {
    Pipe(tokio::net::windows::named_pipe::NamedPipeServer, String),
    Tcp(tokio::net::TcpListener),
}

async fn bind_listener(
    paths: &DaemonPaths,
    lock: &DaemonLock,
) -> anyhow::Result<(Listener, DaemonMetadata, Option<String>)> {
    #[cfg(unix)]
    match bind_uds(paths, lock).await {
        UdsBind::Listener(listener) => {
            let endpoint = paths.socket.to_string_lossy().into_owned();
            return Ok((
                Listener::Uds(listener),
                DaemonMetadata::new(Transport::Uds, endpoint, lock.identity.clone()),
                None,
            ));
        }
        UdsBind::LiveListener => {
            eprintln!("code-graph-mcp: live UDS listener retained; using loopback TCP")
        }
        UdsBind::Unavailable(error) => {
            eprintln!("code-graph-mcp: UDS unavailable ({error}); using loopback TCP")
        }
    }

    #[cfg(windows)]
    match bind_pipe() {
        Ok((listener, endpoint)) => {
            return Ok((
                Listener::Pipe(listener, endpoint.clone()),
                DaemonMetadata::new(Transport::Pipe, endpoint, lock.identity.clone()),
                None,
            ));
        }
        Err(error) => {
            eprintln!("code-graph-mcp: named pipe unavailable ({error}); using loopback TCP")
        }
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind loopback TCP daemon listener")?;
    let endpoint = listener
        .local_addr()
        .context("read loopback TCP daemon endpoint")?
        .to_string();
    let secret = generate_secret()?;
    write_secret(&paths.secret, &secret)?;
    Ok((
        Listener::Tcp(listener),
        DaemonMetadata::new(Transport::Tcp, endpoint, lock.identity.clone()),
        Some(secret),
    ))
}

#[cfg(unix)]
enum UdsBind {
    Listener(tokio::net::UnixListener),
    LiveListener,
    Unavailable(anyhow::Error),
}

#[cfg(unix)]
async fn bind_uds(paths: &DaemonPaths, lock: &DaemonLock) -> UdsBind {
    match tokio::net::UnixListener::bind(&paths.socket) {
        Ok(listener) => secure_uds_listener(paths, listener).await,
        Err(first_error) => {
            // A pathname is never a liveness signal. Only a refused
            // connection proves that an existing socket inode is orphaned.
            let connection_refused = matches!(
                tokio::net::UnixStream::connect(&paths.socket).await,
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused
            );
            if !connection_refused {
                if tokio::net::UnixStream::connect(&paths.socket).await.is_ok() {
                    return UdsBind::LiveListener;
                }
                return UdsBind::Unavailable(first_error.into());
            }
            if !lock.still_owned() {
                return UdsBind::LiveListener;
            }
            if !socket_inode(&paths.socket) {
                return UdsBind::Unavailable(first_error.into());
            }
            if let Err(error) = fs::remove_file(&paths.socket) {
                return UdsBind::Unavailable(error.into());
            }
            match tokio::net::UnixListener::bind(&paths.socket) {
                Ok(listener) => secure_uds_listener(paths, listener).await,
                Err(error) => UdsBind::Unavailable(error.into()),
            }
        }
    }
}

#[cfg(unix)]
async fn secure_uds_listener(paths: &DaemonPaths, listener: tokio::net::UnixListener) -> UdsBind {
    match fs::set_permissions(&paths.socket, std::fs::Permissions::from_mode(0o600)) {
        Ok(()) => UdsBind::Listener(listener),
        Err(error) => {
            drop(listener);
            remove_orphan_socket(paths).await;
            UdsBind::Unavailable(error.into())
        }
    }
}

#[cfg(unix)]
fn socket_inode(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket())
}

#[cfg(unix)]
async fn remove_orphan_socket(paths: &DaemonPaths) {
    if socket_inode(&paths.socket)
        && matches!(
            tokio::net::UnixStream::connect(&paths.socket).await,
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused
        )
    {
        let _ = fs::remove_file(&paths.socket);
    }
}

#[cfg(windows)]
fn bind_pipe() -> anyhow::Result<(tokio::net::windows::named_pipe::NamedPipeServer, String)> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let endpoint = format!(r"\\.\pipe\code-graph-mcp-{}", std::process::id());
    let listener = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&endpoint)
        .context("create daemon named pipe")?;
    Ok((listener, endpoint))
}

fn generate_secret() -> anyhow::Result<String> {
    random_hex(32).map_err(anyhow::Error::from)
}

fn random_hex(byte_len: usize) -> std::io::Result<String> {
    let mut bytes = vec![0_u8; byte_len];
    getrandom::fill(&mut bytes)
        .map_err(|error| std::io::Error::other(format!("random daemon value: {error:?}")))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn write_secret(path: &Path, secret: &str) -> anyhow::Result<()> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let result = (|| -> anyhow::Result<()> {
        let mut file = options
            .open(path)
            .with_context(|| format!("create daemon secret {}", path.display()))?;
        file.write_all(secret.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        #[cfg(windows)]
        restrict_windows_secret(path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

#[cfg(windows)]
fn restrict_windows_secret(path: &Path) -> anyhow::Result<()> {
    restrict_windows_path(path, "(R,W)", "daemon secret")
}

#[cfg(windows)]
fn restrict_windows_runtime_dir(path: &Path) -> anyhow::Result<()> {
    restrict_windows_path(path, "(OI)(CI)(F)", "daemon runtime directory")
}

#[cfg(windows)]
fn restrict_windows_path(path: &Path, permissions: &str, label: &str) -> anyhow::Result<()> {
    let owner = std::env::var("USERNAME").context("read Windows account for daemon secret ACL")?;
    let reset = std::process::Command::new("icacls")
        .arg(path)
        .arg("/reset")
        .status()
        .with_context(|| format!("reset icacls DACL for {label}"))?;
    if !reset.success() {
        bail!("icacls failed to reset {label} DACL")
    }
    let status = std::process::Command::new("icacls")
        .arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(format!("{owner}:{permissions}"))
        .status()
        .with_context(|| format!("run icacls for {label}"))?;
    if status.success() {
        Ok(())
    } else {
        bail!("icacls failed to restrict {label}")
    }
}

fn write_metadata_atomically(path: &Path, metadata: &DaemonMetadata) -> anyhow::Result<()> {
    let encoded = serde_json::to_vec(metadata).context("serialize daemon metadata")?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = path.with_file_name(format!(".daemon-{}-{nonce}.tmp", std::process::id()));
    let write = || -> anyhow::Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)
            .with_context(|| format!("create daemon metadata temp {}", temp.display()))?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        fs::rename(&temp, path)
            .with_context(|| format!("publish daemon metadata {}", path.display()))?;
        Ok(())
    };
    let result = write();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

async fn serve_listener<F>(
    listener: Listener,
    server: CodeGraphServer,
    tcp_secret: Option<String>,
    shutdown: F,
) where
    F: Future<Output = ()>,
{
    #[cfg(unix)]
    match listener {
        Listener::Uds(listener) => serve_uds(listener, server, shutdown).await,
        Listener::Tcp(listener) => {
            serve_tcp(
                listener,
                server,
                tcp_secret.expect("TCP secret").as_str(),
                shutdown,
            )
            .await
        }
    }

    #[cfg(windows)]
    match listener {
        Listener::Pipe(listener, endpoint) => {
            serve_pipe(listener, endpoint, server, shutdown).await
        }
        Listener::Tcp(listener) => {
            serve_tcp(
                listener,
                server,
                tcp_secret.expect("TCP secret").as_str(),
                shutdown,
            )
            .await
        }
    }
}

#[cfg(unix)]
async fn serve_uds<F>(listener: tokio::net::UnixListener, server: CodeGraphServer, shutdown: F)
where
    F: Future<Output = ()>,
{
    let permits = Arc::new(Semaphore::new(MAX_PENDING_CONNECTIONS));
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    if let Ok(permit) = permits.clone().try_acquire_owned() {
                        spawn_service(server.clone(), stream, permit);
                    }
                }
                Err(error) => eprintln!("code-graph-mcp: accept daemon UDS connection: {error}"),
            }
        }
    }
}

async fn serve_tcp<F>(
    listener: tokio::net::TcpListener,
    server: CodeGraphServer,
    secret: &str,
    shutdown: F,
) where
    F: Future<Output = ()>,
{
    let secret = secret.to_owned();
    let permits = Arc::new(Semaphore::new(MAX_PENDING_CONNECTIONS));
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => match accepted {
                Ok((mut stream, _)) => {
                    let Ok(permit) = permits.clone().try_acquire_owned() else {
                        continue;
                    };
                    let server = server.clone();
                    let secret = secret.clone();
                    tokio::spawn(async move {
                        if authenticate_tcp(&mut stream, &secret).await.is_ok() {
                            serve_service(server, stream, permit).await;
                        }
                    });
                }
                Err(error) => eprintln!("code-graph-mcp: accept daemon TCP connection: {error}"),
            }
        }
    }
}

#[cfg(windows)]
async fn serve_pipe<F>(
    mut listener: tokio::net::windows::named_pipe::NamedPipeServer,
    endpoint: String,
    server: CodeGraphServer,
    shutdown: F,
) where
    F: Future<Output = ()>,
{
    use tokio::net::windows::named_pipe::ServerOptions;

    let permits = Arc::new(Semaphore::new(MAX_PENDING_CONNECTIONS));
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            connected = listener.connect() => match connected {
                Ok(()) => {
                    // This server is now connected and cannot accept again.
                    // Hand it to rmcp before retrying the replacement so a
                    // transient pipe-create error neither spins nor strands
                    // the already attached client.
                    let permit = permits.clone().try_acquire_owned().ok();
                    if let Some(permit) = permit {
                        spawn_service(server.clone(), listener, permit);
                    } else {
                        drop(listener);
                    }
                    loop {
                        match ServerOptions::new().create(&endpoint) {
                            Ok(next) => {
                                listener = next;
                                break;
                            }
                            Err(error) => {
                                eprintln!("code-graph-mcp: create next daemon named pipe: {error}");
                                tokio::select! {
                                    _ = &mut shutdown => return,
                                    _ = tokio::time::sleep(Duration::from_millis(100)) => {},
                                }
                            }
                        }
                    }
                }
                Err(error) => eprintln!("code-graph-mcp: accept daemon named pipe connection: {error}"),
            }
        }
    }
}

fn spawn_service<S>(server: CodeGraphServer, stream: S, permit: OwnedSemaphorePermit)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        serve_service(server, stream, permit).await;
    });
}

async fn serve_service<S>(server: CodeGraphServer, stream: S, _permit: OwnedSemaphorePermit)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    match server.serve(stream).await {
        Ok(service) => {
            if let Err(error) = service.waiting().await {
                eprintln!("code-graph-mcp: daemon MCP service: {error}");
            }
        }
        Err(error) => eprintln!("code-graph-mcp: daemon MCP handshake: {error}"),
    }
}

async fn authenticate_tcp<S>(stream: &mut S, secret: &str) -> anyhow::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut line = [0_u8; AUTH_LINE_LEN];
    timeout(AUTH_TIMEOUT, stream.read_exact(&mut line))
        .await
        .context("read TCP daemon authentication prelude timed out")?
        .context("read TCP daemon authentication prelude")?;
    if !valid_auth_line(&line)
        || !constant_time_token_eq(&line[AUTH_PREFIX.len()..72], secret.as_bytes())
    {
        bail!("invalid TCP daemon authentication prelude")
    }
    stream
        .write_all(AUTH_ACK)
        .await
        .context("write TCP daemon authentication acknowledgement")?;
    stream
        .flush()
        .await
        .context("flush TCP daemon authentication acknowledgement")?;
    Ok(())
}

fn valid_auth_line(line: &[u8; AUTH_LINE_LEN]) -> bool {
    line.starts_with(AUTH_PREFIX)
        && line[72] == b'\n'
        && line[AUTH_PREFIX.len()..72]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn constant_time_token_eq(candidate: &[u8], expected: &[u8]) -> bool {
    if candidate.len() != 64 || expected.len() != 64 {
        return false;
    }
    let mut difference = 0_u8;
    for (&left, &right) in candidate.iter().zip(expected) {
        difference |= left ^ right;
    }
    difference == 0
}

async fn cleanup_owned(paths: &DaemonPaths, lock: DaemonLock, metadata: &DaemonMetadata) {
    if !lock.still_owned() {
        return;
    }
    if fs::read(&paths.metadata)
        .ok()
        .and_then(|contents| serde_json::from_slice::<DaemonMetadata>(&contents).ok())
        .is_some_and(|current| current == *metadata)
    {
        let _ = fs::remove_file(&paths.metadata);
    }
    if metadata.transport == Transport::Tcp {
        let _ = fs::remove_file(&paths.secret);
    }
    #[cfg(unix)]
    if metadata.transport == Transport::Uds {
        remove_orphan_socket(paths).await;
    }
    lock.remove_if_owned();
}

fn rfc3339_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = (seconds / 86_400) as i64;
    let seconds_of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds_of_day / 3_600,
        (seconds_of_day % 3_600) / 60,
        seconds_of_day % 60
    )
}

// Gregorian conversion for days since the Unix epoch. This is integer-only
// and avoids introducing a date/time dependency for daemon metadata.
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    (year + i64::from(month <= 2), month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn test_root() -> PathBuf {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "code-graph-mcp-daemon-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn auth_grammar_and_constant_time_comparison() {
        let token = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let mut valid = [0_u8; AUTH_LINE_LEN];
        valid[..AUTH_PREFIX.len()].copy_from_slice(AUTH_PREFIX);
        valid[AUTH_PREFIX.len()..72].copy_from_slice(token.as_bytes());
        valid[72] = b'\n';
        assert!(valid_auth_line(&valid));
        assert!(constant_time_token_eq(&valid[8..72], token.as_bytes()));

        valid[10] = b'A';
        assert!(!valid_auth_line(&valid));
        assert!(!constant_time_token_eq(&valid[8..72], token.as_bytes()));
    }

    #[test]
    fn sysinfo_recognizes_the_current_process() {
        assert!(identity_is_alive(&LockIdentity::current().unwrap()));
    }

    #[test]
    fn metadata_serializes_transport_and_identity() {
        let metadata = DaemonMetadata::new(
            Transport::Tcp,
            "127.0.0.1:1".to_owned(),
            LockIdentity::current().unwrap(),
        );
        let encoded = serde_json::to_value(&metadata).unwrap();
        assert_eq!(encoded["transport"], "tcp");
        assert_eq!(encoded["endpoint"], "127.0.0.1:1");
        assert!(encoded["started_at"].as_str().unwrap().ends_with('Z'));
        assert!(!encoded["binary_sha"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn stale_lock_is_reclaimed_but_live_lock_is_not() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let stale = LockIdentity {
            pid: 99_999_999,
            start_time: 0,
            nonce: "stale".to_owned(),
        };
        fs::write(&paths.lock, serde_json::to_vec(&stale).unwrap()).unwrap();
        let reclaimed = acquire_or_detect_live(&paths).await.unwrap().unwrap();
        reclaimed.remove_if_owned();
        let live = DaemonLock::acquire(&paths).unwrap();
        assert!(acquire_or_detect_live(&paths).await.unwrap().is_none());
        live.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn malformed_locked_file_is_replaced_in_place() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        fs::write(&paths.lock, b"partial lock write").unwrap();

        let replacement = acquire_or_detect_live(&paths).await.unwrap().unwrap();
        assert!(replacement.still_owned());
        assert_ne!(fs::read(&paths.lock).unwrap(), b"partial lock write");
        replacement.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn stale_identity_beats_reused_tcp_endpoint() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let stale = LockIdentity {
            pid: 99_999_999,
            start_time: 0,
            nonce: "stale".to_owned(),
        };
        fs::write(&paths.lock, serde_json::to_vec(&stale).unwrap()).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let metadata = DaemonMetadata::new(
            Transport::Tcp,
            listener.local_addr().unwrap().to_string(),
            stale,
        );
        write_metadata_atomically(&paths.metadata, &metadata).unwrap();
        fs::write(&paths.secret, b"stale token\n").unwrap();

        let replacement = acquire_or_detect_live(&paths).await.unwrap().unwrap();
        assert!(!paths.metadata.exists());
        assert!(!paths.secret.exists());
        replacement.remove_if_owned();
        drop(listener);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn simultaneous_stale_recovery_has_one_replacement_owner() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let stale = LockIdentity {
            pid: 99_999_999,
            start_time: 0,
            nonce: "stale".to_owned(),
        };
        fs::write(&paths.lock, serde_json::to_vec(&stale).unwrap()).unwrap();

        let mut contenders = Vec::new();
        for _ in 0..8 {
            let paths = paths.clone();
            contenders.push(tokio::spawn(
                async move { acquire_or_detect_live(&paths).await },
            ));
        }
        let mut owners = Vec::new();
        for contender in contenders {
            if let Some(owner) = contender.await.unwrap().unwrap() {
                owners.push(owner);
            }
        }
        assert_eq!(owners.len(), 1, "one OS lock owner replaces stale lock");
        assert!(owners[0].still_owned());
        owners.pop().unwrap().remove_if_owned();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stale_uds_is_unlinked_and_live_uds_is_not() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let lock = DaemonLock::acquire(&paths).unwrap();
        let stale = std::os::unix::net::UnixListener::bind(&paths.socket).unwrap();
        drop(stale);
        let listener = match bind_uds(&paths, &lock).await {
            UdsBind::Listener(listener) => listener,
            _ => panic!("orphaned socket must bind"),
        };
        drop(listener);
        assert!(paths.socket.exists());
        fs::remove_file(&paths.socket).unwrap();

        let live = tokio::net::UnixListener::bind(&paths.socket).unwrap();
        assert!(matches!(
            bind_uds(&paths, &lock).await,
            UdsBind::LiveListener
        ));
        drop(live);
        lock.remove_if_owned();
        fs::remove_file(&paths.socket).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn live_uds_inode_is_retained_while_daemon_falls_back_to_tcp() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let lock = DaemonLock::acquire(&paths).unwrap();
        let live = tokio::net::UnixListener::bind(&paths.socket).unwrap();

        let (listener, metadata, secret) = bind_listener(&paths, &lock).await.unwrap();
        assert!(matches!(&listener, Listener::Tcp(_)));
        assert_eq!(metadata.transport, Transport::Tcp);
        assert!(secret.is_some());
        assert!(socket_inode(&paths.socket));
        drop(live);
        drop(listener);
        fs::remove_file(&paths.secret).unwrap();
        fs::remove_file(&paths.socket).unwrap();
        lock.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn uds_never_unlinks_regular_files_or_symlinks() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let lock = DaemonLock::acquire(&paths).unwrap();
        fs::write(&paths.socket, "not a socket").unwrap();
        assert!(matches!(
            bind_uds(&paths, &lock).await,
            UdsBind::Unavailable(_)
        ));
        assert!(fs::symlink_metadata(&paths.socket)
            .unwrap()
            .file_type()
            .is_file());
        fs::remove_file(&paths.socket).unwrap();
        std::os::unix::fs::symlink("target", &paths.socket).unwrap();
        assert!(matches!(
            bind_uds(&paths, &lock).await,
            UdsBind::Unavailable(_)
        ));
        assert!(fs::symlink_metadata(&paths.socket)
            .unwrap()
            .file_type()
            .is_symlink());
        fs::remove_file(&paths.socket).unwrap();
        lock.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn tcp_auth_refuses_missing_stale_and_wrong_tokens() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let token = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let server = tokio::spawn(async move {
            for expected in [false, false, false, true] {
                let (mut stream, _) = listener.accept().await.unwrap();
                assert_eq!(authenticate_tcp(&mut stream, token).await.is_ok(), expected);
            }
        });
        for line in [
            "",
            "CG-AUTH 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdeg\n",
            "CG-AUTH 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdefx\n",
        ] {
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            use tokio::io::AsyncWriteExt;
            stream.write_all(line.as_bytes()).await.unwrap();
        }
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        use tokio::io::AsyncWriteExt;
        stream
            .write_all(
                b"CG-AUTH 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n",
            )
            .await
            .unwrap();
        let mut acknowledgement = [0_u8; AUTH_ACK.len()];
        stream.read_exact(&mut acknowledgement).await.unwrap();
        assert_eq!(&acknowledgement, AUTH_ACK);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn tcp_proxy_attachment_requires_the_authentication_acknowledgement() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let token = generate_secret().unwrap();
        let metadata = DaemonMetadata::new(
            Transport::Tcp,
            listener.local_addr().unwrap().to_string(),
            LockIdentity::current().unwrap(),
        );
        write_metadata_atomically(&paths.metadata, &metadata).unwrap();
        write_secret(&paths.secret, &token).unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut prelude = [0_u8; AUTH_LINE_LEN];
            stream.read_exact(&mut prelude).await.unwrap();
            assert!(valid_auth_line(&prelude));
            stream.write_all(b"CG-NO\n").await.unwrap();
            stream.flush().await.unwrap();
        });
        assert!(connect_from_metadata(&paths, Duration::from_secs(1))
            .await
            .is_err());
        server.await.unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn tcp_secret_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        write_secret(&paths.secret, &generate_secret().unwrap()).unwrap();
        assert_eq!(
            fs::metadata(&paths.secret).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn runtime_directory_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        assert_eq!(
            fs::metadata(paths.runtime).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn runtime_directory_rejects_symlink_outside_project_root() {
        let root = test_root();
        let outside = test_root();
        let paths = DaemonPaths::for_root(&root);
        std::os::unix::fs::symlink(&outside, &paths.runtime).unwrap();

        assert!(paths.ensure_runtime_dir().is_err());
        assert!(fs::symlink_metadata(&paths.runtime)
            .unwrap()
            .file_type()
            .is_symlink());
        fs::remove_file(&paths.runtime).unwrap();
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[tokio::test]
    async fn owned_metadata_cleanup_preserves_non_owner_files() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let lock = DaemonLock::acquire(&paths).unwrap();
        let metadata = DaemonMetadata::new(
            Transport::Tcp,
            "127.0.0.1:1".to_owned(),
            lock.identity.clone(),
        );
        write_metadata_atomically(&paths.metadata, &metadata).unwrap();
        fs::write(&paths.secret, "secret\n").unwrap();
        cleanup_owned(&paths, lock, &metadata).await;
        assert!(!paths.metadata.exists());
        assert!(!paths.secret.exists());
        assert!(!paths.lock.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn locked_owner_clears_unusable_metadata_and_token() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let lock = DaemonLock::acquire(&paths).unwrap();
        fs::write(&paths.metadata, b"not JSON").unwrap();
        fs::write(&paths.secret, b"orphaned token\n").unwrap();

        assert!(prepare_runtime_for_owner(&paths).await);
        assert!(!paths.metadata.exists());
        assert!(!paths.secret.exists());
        lock.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
    }
}
