//! Repository-local daemon transport, ownership, and lifecycle.

use std::fs::{self, OpenOptions, TryLockError};
use std::future::Future;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
#[cfg(unix)]
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use code_graph_core::{paths, RootConfig};
use code_graph_tools::CodeGraphServer;
use rmcp::ServiceExt;
use serde::{Deserialize, Serialize};
use sysinfo::{Signal, System};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::timeout;

#[cfg(unix)]
use rustix::fs::{self as rustix_fs, AtFlags, Mode, OFlags, CWD};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};

const LOCK_FILE: &str = "daemon.lock";
const METADATA_FILE: &str = "daemon.json";
const SECRET_FILE: &str = "secret";
const SOCKET_FILE: &str = "daemon.sock";
const SHUTDOWN_REQUEST_FILE: &str = "shutdown.request";
const SHUTDOWN_ACK_FILE: &str = "shutdown.ack";
#[cfg(target_os = "linux")]
const SHUTDOWN_CONTROL_LOCK_FILE: &str = "shutdown.control.lock";
#[cfg(target_os = "linux")]
const OWNER_RECORD_TEMP_PREFIX: &str = ".daemon-owner-record-";
#[cfg(target_os = "linux")]
const METADATA_TEMP_PREFIX: &str = ".daemon-";
#[cfg(target_os = "linux")]
const METADATA_TEMP_SUFFIX: &str = ".tmp";
const MAX_METADATA_BYTES: usize = 64 * 1024;
const MAX_LOCK_RECORD_BYTES: usize = 4 * 1024;
const MAX_SHUTDOWN_RECORD_BYTES: usize = 4 * 1024;
const MAX_SECRET_RECORD_BYTES: usize = 65;
const AUTH_PREFIX: &[u8] = b"CG-AUTH ";
/// Fixed transport prelude used for TCP authentication and UDS admission.
/// It is consumed before either transport starts its newline-delimited MCP
/// session, so it cannot be mistaken for a JSON-RPC message.
const AUTH_ACK: &[u8] = b"CG-OK\n";
const AUTH_LINE_LEN: usize = 73;
const AUTH_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_PENDING_CONNECTIONS: usize = 128;
const PROXY_ATTACH_DEADLINE: Duration = Duration::from_secs(4);
const PROXY_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const PROXY_RETRY_INTERVAL: Duration = Duration::from_millis(50);
const PROXY_MAX_SPAWN_BACKOFF: Duration = Duration::from_secs(1);
const PROXY_LOSER_REAP_TIMEOUT: Duration = Duration::from_millis(500);
/// An unacknowledged replacement remains below the client's four-second
/// attach budget: two seconds for cooperative drain, then a half-second
/// confirmation after a safe hard kill. This excludes the acknowledged,
/// bounded 30-second drain exception below.
const REPLACEMENT_GRACE: Duration = Duration::from_secs(2);
const REPLACEMENT_KILL_WAIT: Duration = Duration::from_millis(500);
/// Once the owner acknowledges shutdown it gets a bounded 30-second drain
/// window for admitted indexing and cache persistence before escalation.
const REPLACEMENT_DRAIN_GRACE: Duration = Duration::from_secs(30);
const SHUTDOWN_REQUEST_POLL: Duration = Duration::from_millis(50);
/// Post-stdin-EOF drain window for the named-pipe proxy. Pipes cannot
/// half-close, so this bounds how long already-submitted requests may keep
/// flushing replies to stdout before the session handle drops.
#[cfg(windows)]
const PIPE_EOF_DRAIN: Duration = Duration::from_millis(500);
/// Ownership paths are only advisory while the root-inode lock is held, but a
/// short poll bounds the time an old daemon can keep serving after its visible
/// namespace has been replaced.
#[cfg(target_os = "linux")]
const OWNERSHIP_PATH_POLL: Duration = Duration::from_millis(50);

#[derive(Clone, Debug)]
struct DaemonPaths {
    root: PathBuf,
    runtime: PathBuf,
    #[allow(dead_code)]
    lock: PathBuf,
    #[allow(dead_code)]
    metadata: PathBuf,
    #[allow(dead_code)]
    secret: PathBuf,
    socket: PathBuf,
    #[allow(dead_code)]
    shutdown_request: PathBuf,
    #[allow(dead_code)]
    shutdown_ack: PathBuf,
    /// The verified runtime directory is retained immutably so Unix child
    /// operations do not resolve `.code-graph` again after validation.
    #[cfg(unix)]
    runtime_dir: Arc<OnceLock<RuntimeDir>>,
}

#[cfg(unix)]
#[derive(Debug)]
struct RuntimeDir {
    /// The verified canonical project-root descriptor remains available after
    /// runtime setup. Linux daemon ownership is locked on this immutable inode,
    /// rather than on the replaceable `.code-graph` child directory.
    #[cfg(target_os = "linux")]
    root: std::fs::File,
    file: std::fs::File,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OwnershipPathChange {
    Intact,
    RuntimeReplaced,
    RootReplaced,
}

/// Persistent serialization lock for shutdown request/ack mutations. It is
/// intentionally never unlinked: every daemon generation reuses the same
/// owner-only inode through the retained runtime-directory descriptor.
#[cfg(target_os = "linux")]
struct ShutdownControlLock {
    file: std::fs::File,
}

#[cfg(target_os = "linux")]
impl Drop for ShutdownControlLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
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
            shutdown_request: runtime.join(SHUTDOWN_REQUEST_FILE),
            shutdown_ack: runtime.join(SHUTDOWN_ACK_FILE),
            runtime,
            #[cfg(unix)]
            runtime_dir: Arc::new(OnceLock::new()),
        }
    }

    fn ensure_runtime_dir(&self) -> anyhow::Result<()> {
        self.establish_runtime_dir(true)
    }

    /// Opens a pre-existing runtime directory for proxy-side operations. This
    /// intentionally does not create runtime state before a contender owns it.
    fn open_runtime_dir_if_present(&self) -> anyhow::Result<()> {
        match fs::symlink_metadata(&self.runtime) {
            Ok(_) => self.establish_runtime_dir(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| {
                format!(
                    "inspect daemon runtime directory {}",
                    self.runtime.display()
                )
            }),
        }
    }

    fn establish_runtime_dir(&self, create: bool) -> anyhow::Result<()> {
        #[cfg(unix)]
        if self.runtime_dir.get().is_some() {
            return Ok(());
        }
        let canonical_root = fs::canonicalize(&self.root)
            .with_context(|| format!("canonicalize daemon project root {}", self.root.display()))?;
        #[cfg(unix)]
        {
            // Establish the root capability first. Its inode must be the
            // canonical root we discovered; `.code-graph` is then created and
            // opened only relative to that verified descriptor.
            let expected_root = fs::metadata(&canonical_root)?;
            let root = rustix_fs::openat(
                CWD,
                &canonical_root,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .with_context(|| format!("open daemon project root {}", canonical_root.display()))?;
            let root = std::fs::File::from(root);
            let opened_root = root.metadata()?;
            if opened_root.dev() != expected_root.dev() || opened_root.ino() != expected_root.ino()
            {
                bail!(
                    "daemon project root {} changed while establishing runtime state",
                    canonical_root.display()
                );
            }
            if create {
                match rustix_fs::mkdirat(&root, ".code-graph", Mode::from_bits_truncate(0o700)) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error).context("create daemon runtime directory"),
                }
            }
            let directory = rustix_fs::openat(
                &root,
                ".code-graph",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .with_context(|| format!("open daemon runtime directory {}", self.runtime.display()))?;
            let directory = std::fs::File::from(directory);
            if !directory
                .metadata()
                .with_context(|| {
                    format!(
                        "inspect daemon runtime directory {}",
                        self.runtime.display()
                    )
                })?
                .is_dir()
            {
                bail!(
                    "daemon runtime path {} must be a real directory",
                    self.runtime.display()
                );
            }
            rustix_fs::fchmod(&directory, Mode::from_bits_truncate(0o700)).with_context(|| {
                format!(
                    "restrict daemon runtime directory {}",
                    self.runtime.display()
                )
            })?;
            let _ = self.runtime_dir.set(RuntimeDir {
                #[cfg(target_os = "linux")]
                root,
                file: directory,
            });
        }
        #[cfg(windows)]
        {
            match fs::symlink_metadata(&self.runtime) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                    bail!(
                        "daemon runtime path {} must be a real directory",
                        self.runtime.display()
                    )
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
                    fs::create_dir(&self.runtime).with_context(|| {
                        format!("create daemon runtime directory {}", self.runtime.display())
                    })?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
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
                );
            }
            restrict_windows_runtime_dir(&self.runtime)?;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn runtime_dir_for_child(&self) -> std::io::Result<&RuntimeDir> {
        if self.runtime_dir.get().is_none() {
            self.open_runtime_dir_if_present()
                .map_err(std::io::Error::other)?;
        }
        self.runtime_dir.get().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "daemon runtime directory is absent",
            )
        })
    }

    /// Opens a distinct descriptor for the retained project-root inode. A
    /// distinct open-file description is required for `flock` to contend with
    /// another local contender as well as another process; cloning the retained
    /// descriptor would share its lock state.
    #[cfg(target_os = "linux")]
    fn open_root_for_ownership(&self) -> std::io::Result<std::fs::File> {
        let runtime = self.runtime_dir_for_child()?;
        let root = rustix_fs::openat(
            &runtime.root,
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        Ok(std::fs::File::from(root))
    }

    /// Compares the visible namespace paths against the descriptors retained
    /// when this daemon became owner. `symlink_metadata` deliberately observes
    /// a replacement symlink itself rather than following it into a new tree.
    #[cfg(target_os = "linux")]
    fn ownership_path_change(&self) -> OwnershipPathChange {
        let Ok(runtime) = self.runtime_dir_for_child() else {
            return OwnershipPathChange::RootReplaced;
        };
        if !same_directory_inode(fs::symlink_metadata(&self.root), runtime.root.metadata()) {
            return OwnershipPathChange::RootReplaced;
        }
        if !same_directory_inode(fs::symlink_metadata(&self.runtime), runtime.file.metadata()) {
            return OwnershipPathChange::RuntimeReplaced;
        }
        OwnershipPathChange::Intact
    }

    /// Proxies retain a runtime descriptor only after the runtime exists. An
    /// absent runtime has no pinned namespace to refresh; a present descriptor
    /// must be replaced once either visible ownership path diverges.
    #[cfg(target_os = "linux")]
    fn proxy_capability_diverged(&self) -> bool {
        self.runtime_dir.get().is_some()
            && self.ownership_path_change() != OwnershipPathChange::Intact
    }

    #[cfg(unix)]
    fn open_child(&self, name: &str, flags: OFlags, mode: Mode) -> std::io::Result<std::fs::File> {
        let runtime = self.runtime_dir_for_child()?;
        let file = rustix_fs::openat(
            &runtime.file,
            name,
            flags | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            mode,
        )?;
        Ok(std::fs::File::from(file))
    }

    #[cfg(unix)]
    fn remove_child(&self, name: &str) -> std::io::Result<()> {
        Ok(rustix_fs::unlinkat(
            self.runtime_dir_for_child()?.file.try_clone()?,
            name,
            AtFlags::empty(),
        )?)
    }

    #[cfg(unix)]
    fn rename_child(&self, from: &str, to: &str) -> std::io::Result<()> {
        let runtime = self.runtime_dir_for_child()?;
        Ok(rustix_fs::renameat(&runtime.file, from, &runtime.file, to)?)
    }

    #[cfg(target_os = "linux")]
    fn sync_runtime_dir(&self) -> std::io::Result<()> {
        self.runtime_dir_for_child()?.file.sync_all()
    }

    #[cfg(target_os = "linux")]
    fn acquire_shutdown_control_lock(&self) -> std::io::Result<ShutdownControlLock> {
        for _ in 0..8 {
            let file = self.open_child(
                SHUTDOWN_CONTROL_LOCK_FILE,
                OFlags::CREATE | OFlags::RDWR,
                Mode::from_bits_truncate(0o600),
            )?;
            let metadata = file.metadata()?;
            if !metadata.is_file() || metadata.nlink() != 1 {
                return Err(std::io::Error::other(
                    "daemon shutdown control lock is not a single-link regular file",
                ));
            }
            rustix_fs::fchmod(&file, Mode::from_bits_truncate(0o600))?;
            file.lock()?;
            let current = self.open_child(
                SHUTDOWN_CONTROL_LOCK_FILE,
                OFlags::RDONLY | OFlags::NONBLOCK,
                Mode::empty(),
            )?;
            let current_metadata = current.metadata()?;
            if metadata.dev() == current_metadata.dev() && metadata.ino() == current_metadata.ino()
            {
                return Ok(ShutdownControlLock { file });
            }
            let _ = file.unlock();
        }
        Err(std::io::Error::other(
            "daemon shutdown control lock changed while acquiring it",
        ))
    }

    /// Reads one daemon record through the retained runtime-directory
    /// descriptor. The opened inode, rather than a path re-resolution, is the
    /// object that is checked and read on Unix.
    fn read_bounded_record(
        &self,
        name: &str,
        max_bytes: usize,
        require_single_link: bool,
    ) -> anyhow::Result<Vec<u8>> {
        #[cfg(unix)]
        {
            let path = self.runtime.join(name);
            let file = self.open_child(name, OFlags::RDONLY | OFlags::NONBLOCK, Mode::empty())?;
            read_bounded_record_from(file, &path, max_bytes, require_single_link)
        }
        #[cfg(not(unix))]
        {
            read_bounded_record_path(&self.ordinary_path(name), max_bytes, require_single_link)
        }
    }

    #[cfg(unix)]
    fn remove_regular_child(&self, name: &str) -> std::io::Result<()> {
        let file = self.open_child(name, OFlags::RDONLY | OFlags::NONBLOCK, Mode::empty())?;
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::other(format!(
                "daemon runtime entry {name} is not a regular file"
            )));
        }
        self.remove_child(name)
    }

    /// Removes only a regular, unlinked-from-everywhere-else runtime record.
    /// Recovery uses this stricter form so a repository entry hard-linked to
    /// an external sentinel is never treated as an interrupted owner record.
    #[cfg(unix)]
    fn remove_single_link_regular_child(&self, name: &str) -> std::io::Result<()> {
        let file = self.open_child(name, OFlags::RDONLY | OFlags::NONBLOCK, Mode::empty())?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(std::io::Error::other(format!(
                "daemon runtime entry {name} is not a single-link regular file"
            )));
        }
        self.remove_child(name)
    }

    /// A process can die after naming an exchange staging inode. A successor
    /// holds the authoritative daemon lock before this runs, and scans through
    /// the retained descriptor rather than resolving the mutable runtime path.
    #[cfg(target_os = "linux")]
    fn scavenge_owner_record_temps(&self) -> std::io::Result<()> {
        let _control = self.acquire_shutdown_control_lock()?;
        self.scavenge_owner_record_temps_locked()
    }

    #[cfg(target_os = "linux")]
    fn scavenge_owner_record_temps_locked(&self) -> std::io::Result<()> {
        let runtime = self.runtime_dir_for_child()?;
        let mut entries = rustix_fs::Dir::read_from(&runtime.file)?;
        for entry in &mut entries {
            let entry = entry?;
            let bytes = entry.file_name().to_bytes();
            if !bytes.starts_with(OWNER_RECORD_TEMP_PREFIX.as_bytes()) {
                continue;
            }
            let Ok(name) = std::str::from_utf8(bytes) else {
                continue;
            };
            // Do not unlink a planted symlink or hard link just because it
            // mimics our prefix. A genuine crashed staging entry is regular
            // and has exactly one link.
            let _ = self.remove_single_link_regular_child(name);
        }
        Ok(())
    }

    /// Removes only exact crash-abandoned metadata publication entries. This
    /// runs after a daemon owns both the project-root and runtime lock inodes;
    /// links, directories, special entries, and near-miss names are retained.
    #[cfg(target_os = "linux")]
    fn scavenge_metadata_temps(&self, lock: &DaemonLock) -> std::io::Result<()> {
        if !lock.has_root_ownership() || !lock.still_owned() {
            return Err(std::io::Error::other(
                "metadata temp scavenging requires authoritative daemon ownership",
            ));
        }
        let runtime = self.runtime_dir_for_child()?;
        let mut entries = rustix_fs::Dir::read_from(&runtime.file)?;
        for entry in &mut entries {
            let entry = entry?;
            let bytes = entry.file_name().to_bytes();
            if !is_metadata_temp_name(bytes) {
                continue;
            }
            let Ok(name) = std::str::from_utf8(bytes) else {
                continue;
            };
            let _ = self.remove_single_link_regular_child(name);
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn ordinary_path(&self, name: &str) -> PathBuf {
        match name {
            LOCK_FILE => self.lock.clone(),
            METADATA_FILE => self.metadata.clone(),
            SECRET_FILE => self.secret.clone(),
            SOCKET_FILE => self.socket.clone(),
            SHUTDOWN_REQUEST_FILE => self.shutdown_request.clone(),
            SHUTDOWN_ACK_FILE => self.shutdown_ack.clone(),
            _ => self.runtime.join(name),
        }
    }

    #[cfg(not(unix))]
    fn remove_regular_child(&self, name: &str) -> std::io::Result<()> {
        fs::remove_file(self.ordinary_path(name))
    }

    #[cfg(not(unix))]
    fn remove_single_link_regular_child(&self, name: &str) -> std::io::Result<()> {
        self.remove_regular_child(name)
    }

    /// Tokio's Unix socket API only accepts a pathname. The procfd alias is
    /// used solely for that boundary; regular runtime files use `openat`.
    #[cfg(target_os = "linux")]
    fn uds_alias(&self) -> std::io::Result<PathBuf> {
        let runtime = self.runtime_dir_for_child()?;
        Ok(PathBuf::from(format!(
            "/proc/{}/fd/{}/{}",
            std::process::id(),
            runtime.file.as_raw_fd(),
            SOCKET_FILE
        )))
    }

    /// Returns a verified procfd alias for the retained project-root inode.
    /// Cache I/O through this path remains anchored if the visible project-root
    /// pathname is renamed or recreated while the daemon drains.
    #[cfg(target_os = "linux")]
    fn root_io_alias(&self) -> std::io::Result<PathBuf> {
        #[cfg(debug_assertions)]
        if debug_force_root_io_alias_failure(&self.root) {
            return Err(std::io::Error::other(
                "retained-root cache I/O alias deliberately unavailable for test",
            ));
        }
        let runtime = self.runtime_dir_for_child()?;
        let alias = PathBuf::from(format!(
            "/proc/{}/fd/{}",
            std::process::id(),
            runtime.root.as_raw_fd()
        ));
        if !same_directory_inode(fs::metadata(&alias), runtime.root.metadata()) {
            return Err(std::io::Error::other(
                "daemon project-root procfd alias does not resolve to the retained root inode",
            ));
        }
        Ok(alias)
    }

    #[cfg(target_os = "linux")]
    fn retained_root_metadata(&self) -> std::io::Result<fs::Metadata> {
        self.runtime_dir_for_child()?.root.metadata()
    }

    #[cfg(unix)]
    fn uds_bind_path(&self) -> std::io::Result<PathBuf> {
        #[cfg(target_os = "linux")]
        {
            self.uds_alias()
        }
        // SEAM(phase10-macos): macOS has no procfd alias, so the raw socket
        // path is bound directly and deep checkouts (> ~104-byte sun_path)
        // fail here, degrading to loopback TCP in `bind_listener`. Phase 10
        // decides whether to accept the documented TCP degrade or add a
        // macOS-specific short-path strategy (e.g. a per-daemon socket under
        // $TMPDIR with a symlink/metadata pointer back to the runtime dir).
        #[cfg(not(target_os = "linux"))]
        Ok(self.socket.clone())
    }
}

/// Root-scoped debug seam for proving that procfd alias unavailability is an
/// environmental fallback rather than a daemon-startup failure. It is omitted
/// from release builds and cannot affect a different concurrent test root.
#[cfg(all(target_os = "linux", debug_assertions))]
fn debug_force_root_io_alias_failure(root: &Path) -> bool {
    std::env::var("CODE_GRAPH_TEST_FAIL_ROOT_IO_ALIAS_ROOT")
        .is_ok_and(|expected| Path::new(&expected) == root)
}

#[cfg(target_os = "linux")]
fn same_directory_inode(
    visible: std::io::Result<fs::Metadata>,
    retained: std::io::Result<fs::Metadata>,
) -> bool {
    let (Ok(visible), Ok(retained)) = (visible, retained) else {
        return false;
    };
    visible.is_dir()
        && retained.is_dir()
        && visible.dev() == retained.dev()
        && visible.ino() == retained.ino()
}

/// Attaches the invoking stdio process to the repository-local daemon.
///
/// The caller has already discovered `root` with [`RootConfig::load`]. This
/// function intentionally does not create `.code-graph`: disabled/direct
/// invocations must remain indistinguishable from the pre-daemon binary.
/// Every retry reads metadata and probes its recorded endpoint anew, because a
/// competing daemon can publish between any two attempts.
pub async fn proxy(root: PathBuf) -> anyhow::Result<()> {
    let mut paths = DaemonPaths::for_root(&root);
    paths.open_runtime_dir_if_present()?;
    let fingerprint = executable_fingerprint()?;
    let mut deadline = std::time::Instant::now() + PROXY_ATTACH_DEADLINE;
    let mut last_error = None;
    let mut contender = None;
    let mut replaced_owner: Option<LockIdentity> = None;
    let mut next_spawn_at = std::time::Instant::now();
    let mut spawn_backoff = PROXY_RETRY_INTERVAL;

    while std::time::Instant::now() < deadline {
        if refresh_proxy_paths(&mut paths, &root)? {
            reset_proxy_after_namespace_change(
                &mut contender,
                &mut replaced_owner,
                &mut next_spawn_at,
                &mut spawn_backoff,
            )
            .await;
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match read_metadata(&paths) {
            Ok(metadata) if metadata_compatible(&metadata, &fingerprint) => {
                if !metadata_owner_is_active(&paths, &metadata) {
                    last_error = Some(anyhow::anyhow!(
                        "daemon metadata owner is not the active lock owner"
                    ));
                } else {
                    match connect_to_metadata(
                        &paths,
                        &metadata,
                        remaining.min(PROXY_CONNECT_TIMEOUT),
                    )
                    .await
                    {
                        Ok(connection) => {
                            // Authentication/connect success does not prove the
                            // owner remained active while the connection was
                            // being established. Do not hand an established
                            // proxy stream to a daemon that has since lost its
                            // lock ownership.
                            if metadata_owner_is_active(&paths, &metadata)
                                && proxy_namespace_is_current(&paths)
                            {
                                settle_contender(contender.take(), connection.pid).await;
                                if metadata_owner_is_active(&paths, &metadata)
                                    && proxy_namespace_is_current(&paths)
                                {
                                    report_tcp_fallback(&metadata);
                                    return finish_proxy(connection.stream).await;
                                }
                                last_error = Some(anyhow::anyhow!(
                                    "daemon namespace changed while settling its contender"
                                ));
                            } else {
                                last_error = Some(anyhow::anyhow!(
                                    "daemon metadata owner changed while connecting"
                                ));
                            }
                            drop(connection);
                        }
                        Err(error) => last_error = Some(error),
                    }
                }
            }
            Ok(metadata) => {
                if !metadata_owner_is_active(&paths, &metadata) {
                    last_error = Some(anyhow::anyhow!(
                        "daemon metadata owner does not match a live lock owner"
                    ));
                } else {
                    if replaced_owner
                        .as_ref()
                        .is_some_and(|owner| owner != &metadata.owner)
                    {
                        terminate_contender(contender).await;
                        return Err(anyhow::anyhow!(
                            "daemon replacement encountered a second incompatible active owner; serving in-process"
                        ));
                    }
                    if let Err(error) = request_replacement(&paths, &metadata).await {
                        terminate_contender(contender).await;
                        return Err(error);
                    }
                    // A cooperative owner may have spent its full drain
                    // grace persisting. Give the normal spawn/probe loop a
                    // fresh budget once it has actually exited.
                    deadline = std::time::Instant::now() + PROXY_ATTACH_DEADLINE;
                    replaced_owner = Some(metadata.owner.clone());
                }
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
    if refresh_proxy_paths(&mut paths, &root)? {
        reset_proxy_after_namespace_change(
            &mut contender,
            &mut replaced_owner,
            &mut next_spawn_at,
            &mut spawn_backoff,
        )
        .await;
    }
    if let Ok(metadata) = read_metadata(&paths) {
        if metadata_compatible(&metadata, &fingerprint)
            && metadata_owner_is_active(&paths, &metadata)
        {
            if let Ok(connection) =
                connect_to_metadata(&paths, &metadata, Duration::from_millis(250)).await
            {
                if metadata_owner_is_active(&paths, &metadata) && proxy_namespace_is_current(&paths)
                {
                    settle_contender(contender.take(), connection.pid).await;
                    if metadata_owner_is_active(&paths, &metadata)
                        && proxy_namespace_is_current(&paths)
                    {
                        report_tcp_fallback(&metadata);
                        return finish_proxy(connection.stream).await;
                    }
                    drop(connection);
                }
            }
        }
    }
    terminate_contender(contender).await;
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("daemon did not publish metadata")))
}

/// Attach-only proxy (Designs/CommandLineInterface Decision 3): attaches to
/// a published, compatible, live daemon or reports failure so the caller
/// serves in-process. Unlike [`proxy`], it NEVER spawns a contender and
/// NEVER initiates the replacement protocol — a drive-by CLI invocation
/// must not leave a resident daemon behind and must not kill another
/// session's daemon.
///
/// Failure is immediate (no retry loop) for the outcomes retrying cannot
/// change without spawning or replacing: no published metadata, a
/// binary-incompatible owner, or metadata with no live lock owner.
/// Connection failures against a LIVE compatible owner retry within the
/// attach deadline — the daemon may be momentarily saturated.
pub async fn proxy_attach_only(root: PathBuf) -> anyhow::Result<()> {
    let mut paths = DaemonPaths::for_root(&root);
    paths.open_runtime_dir_if_present()?;
    let fingerprint = executable_fingerprint()?;
    let deadline = std::time::Instant::now() + PROXY_ATTACH_DEADLINE;
    let mut last_error = None;

    while std::time::Instant::now() < deadline {
        // Namespace changes (runtime dir recreated underneath us) refresh
        // the descriptor exactly as the full proxy does; the next metadata
        // read observes the successor namespace.
        refresh_proxy_paths(&mut paths, &root)?;
        let metadata = read_metadata(&paths)
            .map_err(|error| anyhow::anyhow!("no attachable daemon published: {error}"))?;
        if !metadata_compatible(&metadata, &fingerprint) {
            return Err(anyhow::anyhow!(
                "published daemon is binary-incompatible; attach-only leaves it untouched"
            ));
        }
        if !metadata_owner_is_active(&paths, &metadata) {
            return Err(anyhow::anyhow!(
                "published daemon metadata has no live lock owner"
            ));
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match connect_to_metadata(&paths, &metadata, remaining.min(PROXY_CONNECT_TIMEOUT)).await {
            Ok(connection) => {
                // Same post-connect revalidation as the full proxy: never
                // hand an established stream to a daemon that lost its lock
                // ownership while the connection was being established.
                if metadata_owner_is_active(&paths, &metadata) && proxy_namespace_is_current(&paths)
                {
                    report_tcp_fallback(&metadata);
                    return finish_proxy(connection.stream).await;
                }
                last_error = Some(anyhow::anyhow!(
                    "daemon metadata owner changed while connecting"
                ));
                drop(connection);
            }
            Err(error) => last_error = Some(error),
        }
        tokio::time::sleep(PROXY_RETRY_INTERVAL).await;
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("attach-only deadline elapsed")))
}

/// A proxy normally keeps its verified runtime descriptor to avoid path
/// reopening races. If the visible root or runtime inode diverges, that
/// capability is detached and cannot observe the successor namespace; replace
/// it before the next metadata probe.
fn refresh_proxy_paths(paths: &mut DaemonPaths, root: &Path) -> anyhow::Result<bool> {
    #[cfg(target_os = "linux")]
    if paths.proxy_capability_diverged() {
        *paths = DaemonPaths::for_root(root);
        paths.open_runtime_dir_if_present()?;
        return Ok(true);
    }
    // SEAM(phase10-macos): no retained-descriptor capability model off Linux,
    // so a replaced runtime namespace is never detected here — the proxy
    // keeps its original paths. Phase 10 decides whether macOS needs a
    // dev/ino re-stat equivalent or accepts pathname-trust semantics.
    #[cfg(not(target_os = "linux"))]
    let _ = (paths, root);
    Ok(false)
}

fn proxy_namespace_is_current(paths: &DaemonPaths) -> bool {
    #[cfg(target_os = "linux")]
    {
        paths.ownership_path_change() == OwnershipPathChange::Intact
    }
    // SEAM(phase10-macos): always-current off Linux (see refresh_proxy_paths).
    #[cfg(not(target_os = "linux"))]
    {
        let _ = paths;
        true
    }
}

/// A contender started against a detached daemon namespace cannot publish in
/// the replacement namespace. Reap it before allowing an immediate successor
/// spawn, and discard owner/backoff state learned from that old namespace.
async fn reset_proxy_after_namespace_change(
    contender: &mut Option<tokio::process::Child>,
    replaced_owner: &mut Option<LockIdentity>,
    next_spawn_at: &mut std::time::Instant,
    spawn_backoff: &mut Duration,
) {
    terminate_contender(contender.take()).await;
    *replaced_owner = None;
    *next_spawn_at = std::time::Instant::now();
    *spawn_backoff = PROXY_RETRY_INTERVAL;
}

fn metadata_compatible(metadata: &DaemonMetadata, fingerprint: &str) -> bool {
    sha_compatible_with(
        env!("CODE_GRAPH_GIT_SHA"),
        &metadata.binary_sha,
        fingerprint,
        &metadata.executable_fingerprint,
    )
}

fn sha_compatible_with(
    current_sha: &str,
    daemon_sha: &str,
    current_fingerprint: &str,
    daemon_fingerprint: &str,
) -> bool {
    !current_fingerprint.is_empty()
        && !daemon_fingerprint.is_empty()
        && current_fingerprint == daemon_fingerprint
        && current_sha == daemon_sha
}

fn read_metadata(paths: &DaemonPaths) -> anyhow::Result<DaemonMetadata> {
    let encoded = paths.read_bounded_record(METADATA_FILE, MAX_METADATA_BYTES, false)?;
    serde_json::from_slice(&encoded).context("parse daemon metadata")
}

/// Static-entry regression helper. Runtime callers on every platform go
/// through [`DaemonPaths::read_bounded_record`]; on Unix that opens relative
/// to the retained directory descriptor, elsewhere it takes the path route.
#[cfg(test)]
fn read_metadata_payload(path: &Path) -> anyhow::Result<Vec<u8>> {
    read_bounded_record_path(path, MAX_METADATA_BYTES, false)
}

/// Opens a record by path only where descriptor-relative opening is not
/// available. The initial check avoids opening a static special entry; the
/// descriptor reader repeats validation after open to close substitution races.
fn read_bounded_record_path(
    path: &Path,
    max_bytes: usize,
    require_single_link: bool,
) -> anyhow::Result<Vec<u8>> {
    let entry = fs::symlink_metadata(path)
        .with_context(|| format!("inspect daemon runtime record {}", path.display()))?;
    validate_bounded_record_metadata(&entry, path, max_bytes, require_single_link)?;

    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options
        .open(path)
        .with_context(|| format!("open daemon runtime record {}", path.display()))?;

    read_bounded_record_from(file, path, max_bytes, require_single_link)
}

/// Reads at most `max_bytes + 1` bytes from an already-open daemon JSON
/// record. The descriptor is validated after opening so a replacement cannot
/// turn a validated pathname into a special file or an external target.
fn read_bounded_record_from(
    file: std::fs::File,
    path: &Path,
    max_bytes: usize,
    require_single_link: bool,
) -> anyhow::Result<Vec<u8>> {
    let opened = file
        .metadata()
        .with_context(|| format!("inspect opened daemon runtime record {}", path.display()))?;
    validate_bounded_record_metadata(&opened, path, max_bytes, require_single_link)?;
    let mut encoded = Vec::with_capacity(opened.len() as usize);
    file.take((max_bytes + 1) as u64)
        .read_to_end(&mut encoded)
        .with_context(|| format!("read daemon runtime record {}", path.display()))?;
    if encoded.len() > max_bytes {
        bail!(
            "daemon runtime record {} exceeds {max_bytes}-byte limit",
            path.display()
        );
    }
    Ok(encoded)
}

fn validate_bounded_record_metadata(
    metadata: &fs::Metadata,
    path: &Path,
    max_bytes: usize,
    require_single_link: bool,
) -> anyhow::Result<()> {
    if !metadata.is_file() {
        bail!(
            "daemon runtime record {} is not a regular file",
            path.display()
        );
    }
    #[cfg(unix)]
    if require_single_link && metadata.nlink() != 1 {
        bail!(
            "daemon runtime record {} has multiple links",
            path.display()
        );
    }
    // Stable std exposes no link count on Windows metadata; the runtime
    // directory ACL is the hard-link defense there instead.
    #[cfg(not(unix))]
    let _ = require_single_link;
    if metadata.len() > max_bytes as u64 {
        bail!(
            "daemon runtime record {} exceeds {max_bytes}-byte limit",
            path.display()
        );
    }
    Ok(())
}

fn report_tcp_fallback(metadata: &DaemonMetadata) {
    if metadata.transport == Transport::Tcp {
        eprintln!("code-graph-mcp: local IPC was unavailable; loopback TCP fallback is active");
    }
}

#[cfg(all(test, unix))]
fn read_lock_identity(path: &Path) -> anyhow::Result<LockIdentity> {
    serde_json::from_slice(&read_bounded_record_path(
        path,
        MAX_LOCK_RECORD_BYTES,
        true,
    )?)
    .with_context(|| format!("parse daemon lock {}", path.display()))
}

fn read_lock_identity_child(paths: &DaemonPaths) -> anyhow::Result<LockIdentity> {
    let encoded = paths.read_bounded_record(LOCK_FILE, MAX_LOCK_RECORD_BYTES, true)?;
    serde_json::from_slice(&encoded).context("parse daemon lock")
}

/// `ERROR_LOCK_VIOLATION` (os error 33): the read hit a byte range another
/// handle holds a mandatory lock on. On Windows this is the only way a data
/// read of an actively held `daemon.lock` can end, so callers treat it as the
/// liveness signal rather than corruption. Harmless to probe on other
/// platforms — advisory locks never fail reads with this code.
#[cfg(not(unix))]
fn is_lock_violation(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .any(|io_error| io_error.raw_os_error() == Some(33))
}

fn metadata_owner_is_active(paths: &DaemonPaths, metadata: &DaemonMetadata) -> bool {
    // The before/after exact-identity checks around the actively-held lock
    // probe are the authorization: the metadata owner must still name the
    // lock owner after proving that lock is actively held.
    #[cfg(unix)]
    {
        let Ok(lock) = read_lock_identity_child(paths) else {
            return false;
        };
        metadata.pid == metadata.owner.pid
            && lock == metadata.owner
            && identity_is_alive(&metadata.owner)
            && lock_is_actively_held(paths)
            && read_lock_identity_child(paths).is_ok_and(|owner| owner == metadata.owner)
    }
    // Windows mandatory locking inverts the probe: an actively held lock is
    // unreadable (ERROR_LOCK_VIOLATION), so the failed read IS the held
    // proof and the metadata owner cannot be cross-checked against the lock
    // contents while the owner lives. A readable lock file means no live
    // owner holds it.
    #[cfg(not(unix))]
    {
        match read_lock_identity_child(paths) {
            Ok(_) => false,
            Err(error) if is_lock_violation(&error) => {
                metadata.pid == metadata.owner.pid && identity_is_alive(&metadata.owner)
            }
            Err(_) => false,
        }
    }
}

/// Unix-only advisory-lock probe. Windows callers never probe by re-locking:
/// mandatory locking already surfaces an actively held lock as a
/// lock-violation read failure (see [`is_lock_violation`]), and a probe
/// `try_lock` from a second handle could spuriously steal a lock released
/// between checks.
#[cfg(unix)]
fn lock_is_actively_held(paths: &DaemonPaths) -> bool {
    let file = match paths.open_child(LOCK_FILE, OFlags::RDWR | OFlags::NONBLOCK, Mode::empty()) {
        Ok(file) => file,
        Err(_) => return false,
    };
    if !file
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.nlink() == 1)
    {
        return false;
    }
    match file.try_lock() {
        Err(TryLockError::WouldBlock) => true,
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(_) => false,
    }
}

async fn request_replacement(paths: &DaemonPaths, metadata: &DaemonMetadata) -> anyhow::Result<()> {
    // Revalidate immediately before publication. Metadata is untrusted and a
    // competing replacement can change the owner between probe and request.
    if !metadata_owner_is_active(paths, metadata) {
        return Ok(());
    }
    replace_stale_shutdown_request(paths, &metadata.owner)?;
    write_shutdown_request(paths, &metadata.owner)?;
    // If the owner changed while publishing, leave the request in place. A
    // stale requester must not race a new owner by deleting a newly-published
    // request after a read-then-remove ownership check.
    if !metadata_owner_is_active(paths, metadata) {
        return Ok(());
    }
    let grace_deadline = std::time::Instant::now() + REPLACEMENT_GRACE;
    while std::time::Instant::now() < grace_deadline {
        if !metadata_owner_is_active(paths, metadata) {
            return Ok(());
        }
        if shutdown_ack_matches(paths, &metadata.owner) {
            break;
        }
        tokio::time::sleep(SHUTDOWN_REQUEST_POLL).await;
    }

    // No acknowledgement within the short request grace: a request can only
    // escalate when its exact owner still owns the lock and
    // still has the original PID/start-time identity. Never kill from stale or
    // owner-mismatched discovery metadata.
    if !metadata_owner_is_active(paths, metadata) {
        return Ok(());
    }
    if shutdown_ack_matches(paths, &metadata.owner) {
        let drain_deadline = std::time::Instant::now() + REPLACEMENT_DRAIN_GRACE;
        while std::time::Instant::now() < drain_deadline {
            if !metadata_owner_is_active(paths, metadata) {
                return Ok(());
            }
            tokio::time::sleep(SHUTDOWN_REQUEST_POLL).await;
        }
    }
    if !metadata_owner_is_active(paths, metadata) {
        return Ok(());
    }
    if !kill_identity(&metadata.owner) {
        bail!("daemon replacement request was ignored and safe hard-kill failed");
    }
    let kill_deadline = std::time::Instant::now() + REPLACEMENT_KILL_WAIT;
    while std::time::Instant::now() < kill_deadline {
        if !identity_is_alive(&metadata.owner) {
            bail!("daemon replacement required hard-kill; serving this invocation in-process");
        }
        tokio::time::sleep(SHUTDOWN_REQUEST_POLL).await;
    }
    bail!("daemon replacement hard-kill did not exit within bounded wait")
}

fn replace_stale_shutdown_request(
    paths: &DaemonPaths,
    rejected: &LockIdentity,
) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    let _control = paths
        .acquire_shutdown_control_lock()
        .context("acquire daemon shutdown control lock")?;
    let existing = match read_owner_file(paths, SHUTDOWN_REQUEST_FILE, "request") {
        Ok(existing) => existing,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|source| source.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(())
        }
        // Publication performs the one atomic replacement below. Do not
        // validate this pathname and then unlink it here: a replacement can
        // race that split operation.
        Err(_) => return Ok(()),
    };
    if existing == *rejected {
        return Ok(());
    }
    let active = active_lock_identity(paths);
    if active.as_ref() == Some(&existing) {
        bail!("daemon shutdown request belongs to a different active owner");
    }
    Ok(())
}

#[cfg(unix)]
fn active_lock_identity(paths: &DaemonPaths) -> Option<LockIdentity> {
    let owner = read_lock_identity_child(paths).ok()?;
    (identity_is_alive(&owner)
        && lock_is_actively_held(paths)
        && read_lock_identity_child(paths).is_ok_and(|current| current == owner))
    .then_some(owner)
}

/// Windows cannot read an actively held lock's contents (mandatory locking),
/// so the live owner identity comes from the published metadata record once
/// the lock-violation read failure proves someone holds the lock.
#[cfg(not(unix))]
fn active_lock_identity(paths: &DaemonPaths) -> Option<LockIdentity> {
    match read_lock_identity_child(paths) {
        Ok(_) => None,
        Err(error) if is_lock_violation(&error) => read_metadata(paths)
            .ok()
            .map(|metadata| metadata.owner)
            .filter(identity_is_alive),
        Err(_) => None,
    }
}

fn kill_identity(identity: &LockIdentity) -> bool {
    // Safe Rust has no portable pidfd/process-handle primitive here. The
    // caller immediately revalidates metadata + actively-held lock, and this
    // final sysinfo start-time check narrows PID reuse without unsafe or
    // platform-specific APIs. Targeted refresh — see `process_start_time`.
    let target = sysinfo::Pid::from_u32(identity.pid);
    let mut system = System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[target]), true);
    let Some(process) = system.process(target) else {
        return false;
    };
    if process.start_time() != identity.start_time {
        return false;
    }
    process.kill_with(Signal::Kill).unwrap_or(false)
}

async fn finish_proxy(stream: ClientStream) -> anyhow::Result<()> {
    // Once connected, MCP framing may already be in flight. A second server
    // cannot safely reconstruct that session, so an established-stream error
    // ends this process rather than falling back to a new in-process server.
    // The exit still reports success (the host would misread a nonzero code
    // as a tool failure), so a mid-session daemon death must at least leave
    // a stderr breadcrumb to distinguish it from a graceful shutdown.
    if let Err(error) = pump_connection(stream).await {
        eprintln!("code-graph-mcp: daemon connection ended mid-session ({error}); exiting");
    }
    Ok(())
}

/// The host hands this proxy its stdio pipe ends with `HANDLE_FLAG_INHERIT`
/// set — that is how they crossed `CreateProcess` in the first place — and
/// Windows copies every inheritable handle into every child spawned with
/// handle inheritance enabled (stable std `Command` exposes no handle-list
/// control). Without this seal the daemon contender would silently retain
/// the host↔proxy stdio pipe ends for its entire lifetime, so the host would
/// never observe EOF on the proxy's stdout/stderr after the proxy exits —
/// observed as an MCP host (or test harness) hanging on session teardown
/// until the daemon's idle timeout fires.
#[cfg(windows)]
#[allow(unsafe_code)]
fn seal_standard_handles_from_inheritance() {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT};

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    for handle in [
        stdin.as_raw_handle(),
        stdout.as_raw_handle(),
        stderr.as_raw_handle(),
    ] {
        if handle.is_null() {
            continue;
        }
        // SAFETY: each handle is one of this process's own standard handles,
        // owned for the life of the process; clearing the inherit flag
        // neither closes nor otherwise invalidates it. A failed call (e.g. a
        // console pseudo-handle rejecting the request) fails open: the flag
        // stays set and behavior is unchanged from before this call.
        unsafe {
            SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
        }
    }
}

async fn spawn_contender(root: &Path) -> anyhow::Result<tokio::process::Child> {
    #[cfg(windows)]
    seal_standard_handles_from_inheritance();
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

#[cfg(test)]
async fn connect_from_metadata(
    paths: &DaemonPaths,
    connect_timeout: Duration,
) -> anyhow::Result<ConnectedDaemon> {
    let metadata = read_metadata(paths)?;
    connect_to_metadata(paths, &metadata, connect_timeout).await
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

async fn connect_to_metadata(
    paths: &DaemonPaths,
    metadata: &DaemonMetadata,
    connect_timeout: Duration,
) -> anyhow::Result<ConnectedDaemon> {
    let stream = match metadata.transport {
        #[cfg(unix)]
        Transport::Uds => {
            let mut stream = timeout(
                connect_timeout,
                tokio::net::UnixStream::connect(paths.uds_bind_path()?),
            )
            .await
            .context("connect daemon UDS timed out")??;
            read_acknowledgement(&mut stream, connect_timeout, "UDS admission").await?;
            ClientStream::Uds(stream)
        }
        #[cfg(windows)]
        Transport::Pipe => {
            let mut stream = tokio::net::windows::named_pipe::ClientOptions::new()
                .open(&metadata.endpoint)
                .with_context(|| format!("connect daemon pipe {}", metadata.endpoint))?;
            // A successful pipe open is not admission: the daemon may be
            // saturated or racing an idle-timeout shutdown. Only the CG-OK
            // prelude proves an attached MCP service is behind the stream;
            // its absence lets the proxy retry or fall back instead of
            // byte-pumping a dead pipe and exiting 0.
            read_acknowledgement(&mut stream, connect_timeout, "pipe admission").await?;
            ClientStream::Pipe(stream)
        }
        Transport::Tcp => {
            let mut stream = timeout(
                connect_timeout,
                tokio::net::TcpStream::connect(&metadata.endpoint),
            )
            .await
            .context("connect daemon TCP timed out")??;
            let token = read_client_secret(paths)?;
            stream
                .write_all(format!("CG-AUTH {token}\n").as_bytes())
                .await
                .context("send daemon TCP authentication")?;
            stream
                .flush()
                .await
                .context("flush daemon TCP authentication")?;
            read_acknowledgement(&mut stream, connect_timeout, "TCP authentication").await?;
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

fn read_client_secret(paths: &DaemonPaths) -> anyhow::Result<String> {
    // The token is immediately copied into the auth prelude. Do not accept
    // whitespace or alternate encodings around the owner-only file contents.
    let contents = String::from_utf8(paths.read_bounded_record(
        SECRET_FILE,
        MAX_SECRET_RECORD_BYTES,
        false,
    )?)
    .context("read daemon TCP secret")?;
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
        ClientStream::Pipe(stream) => pump_bytes_without_half_close(stream).await,
        ClientStream::Tcp(stream) => pump_bytes(stream).await,
    }
}

/// Named pipes cannot half-close: `poll_shutdown` on the client's write half
/// is a no-op, so the UDS/TCP pattern — shutdown after stdin EOF, then drain
/// until the daemon closes — would hang this proxy forever. Stdin EOF is the
/// host's session teardown, so the pipe session instead ends by flushing
/// pending writes and dropping the duplex handle; the daemon observes the
/// close and releases the connection. A response still in flight at that
/// point is dropped, which matches how a host treats a session it has
/// already torn down.
#[cfg(windows)]
async fn pump_bytes_without_half_close<S>(stream: S) -> anyhow::Result<()>
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
            .flush()
            .await
            .context("flush daemon pipe after stdin EOF")
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
    // A broken established stream still ends the session immediately. EOF on
    // stdin ends it after the flush above plus a bounded drain: without
    // half-close the daemon cannot signal end-of-responses, so replies to
    // already-submitted requests get a short window to reach stdout before
    // the handle drops.
    tokio::select! {
        biased;
        output = &mut socket_to_stdout => output,
        input = &mut stdin_to_socket => {
            input?;
            match timeout(PIPE_EOF_DRAIN, &mut socket_to_stdout).await {
                Ok(output) => output,
                Err(_elapsed) => Ok(()),
            }
        }
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
    /// Stable fingerprint of the executable bytes that published this record.
    /// It is a compatibility discriminator, not a cryptographic integrity or
    /// security primitive; the length-prefixed FNV-style hash can collide.
    #[serde(default)]
    executable_fingerprint: String,
    started_at: String,
    owner: LockIdentity,
}

impl DaemonMetadata {
    fn new(transport: Transport, endpoint: String, owner: LockIdentity) -> anyhow::Result<Self> {
        Ok(Self {
            pid: std::process::id(),
            transport,
            endpoint,
            binary_sha: env!("CODE_GRAPH_GIT_SHA").to_owned(),
            executable_fingerprint: executable_fingerprint()?,
            started_at: rfc3339_now(),
            owner,
        })
    }
}

/// Computes a deterministic, dependency-free content fingerprint for the
/// current executable. Including the byte length prevents simple prefix
/// ambiguity; this is deliberately non-cryptographic and only separates local
/// daemon build identities.
fn executable_fingerprint() -> anyhow::Result<String> {
    let path = std::env::current_exe().context("locate current executable for daemon identity")?;
    let bytes = fs::read(&path).with_context(|| {
        format!(
            "read current executable for daemon identity {}",
            path.display()
        )
    })?;
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let length = bytes.len();
    for byte in (length as u64).to_le_bytes().into_iter().chain(bytes) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    Ok(format!("{length:016x}-{hash:016x}"))
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
    #[cfg(not(unix))]
    path: PathBuf,
    paths: DaemonPaths,
    contents: String,
    identity: LockIdentity,
    file: Option<std::fs::File>,
    /// Production Linux acquisition holds this separate root-inode lock for
    /// the entire daemon lifetime. Direct `DaemonLock::acquire` remains the
    /// runtime-lock test helper, so it deliberately leaves this absent.
    #[cfg(target_os = "linux")]
    root_file: Option<std::fs::File>,
}

impl DaemonLock {
    fn acquire(paths: &DaemonPaths) -> std::io::Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::OpenOptionsExt;

        let identity = LockIdentity::current()?;
        let contents = format!(
            "{}\n",
            serde_json::to_string(&identity).map_err(std::io::Error::other)?
        );
        let mut options = OpenOptions::new();
        options.create_new(true).read(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        #[cfg(unix)]
        let mut file = paths.open_child(
            LOCK_FILE,
            OFlags::CREATE | OFlags::EXCL | OFlags::RDWR,
            Mode::from_bits_truncate(0o600),
        )?;
        #[cfg(not(unix))]
        let mut file = options.open(&paths.lock)?;
        let initialize = (|| -> std::io::Result<()> {
            file.try_lock()?;
            file.write_all(contents.as_bytes())?;
            file.sync_all()
        })();
        if let Err(error) = initialize {
            let _ = file.unlock();
            drop(file);
            return Err(error);
        }
        Ok(Self {
            #[cfg(not(unix))]
            path: paths.lock.clone(),
            paths: paths.clone(),
            contents,
            identity,
            file: Some(file),
            #[cfg(target_os = "linux")]
            root_file: None,
        })
    }

    #[cfg(target_os = "linux")]
    fn with_root_ownership(mut self, root_file: std::fs::File) -> Self {
        debug_assert!(self.root_file.is_none());
        self.root_file = Some(root_file);
        self
    }

    #[cfg(target_os = "linux")]
    fn has_root_ownership(&self) -> bool {
        self.root_file.is_some()
    }

    #[cfg(unix)]
    fn still_owned(&self) -> bool {
        self.file.is_some()
            && self
                .paths
                .read_bounded_record(LOCK_FILE, MAX_LOCK_RECORD_BYTES, true)
                .is_ok_and(|contents| contents == self.contents.as_bytes())
    }

    /// Windows file locks are mandatory: while this owner holds the
    /// whole-file lock, a data read through any other handle fails with
    /// `ERROR_LOCK_VIOLATION`, so the contents cannot be re-verified the way
    /// the Unix advisory-lock path does. That read failure is itself the
    /// ownership proof — the pathname still names an actively held lock. A
    /// readable lock file means the lock is no longer held, and a missing
    /// file means it was removed.
    #[cfg(not(unix))]
    fn still_owned(&self) -> bool {
        if self.file.is_none() {
            return false;
        }
        match self
            .paths
            .read_bounded_record(LOCK_FILE, MAX_LOCK_RECORD_BYTES, true)
        {
            Ok(contents) => contents == self.contents.as_bytes(),
            Err(error) => is_lock_violation(&error),
        }
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

    fn release_files(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = file.unlock();
            drop(file);
        }
        #[cfg(target_os = "linux")]
        if let Some(root_file) = self.root_file.take() {
            let _ = root_file.unlock();
            drop(root_file);
        }
    }

    fn release(mut self) {
        self.release_files();
    }

    #[cfg(unix)]
    fn remove_if_owned(self) {
        self.remove_if_owned_before_release(|| {});
    }

    /// Unlink an owned Unix lock while its inode remains exclusively locked.
    /// A successor can then exclusively create and lock a new pathname before
    /// this owner releases the unlinked inode; releasing cannot remove that
    /// successor.
    #[cfg(unix)]
    fn remove_if_owned_before_release(self, after_unlink: impl FnOnce()) {
        let owned = self.still_owned();
        if owned {
            let _ = self.paths.remove_child(LOCK_FILE);
        }
        after_unlink();
        self.release();
    }

    // Windows cannot unlink an open locked file with the Unix handoff
    // ordering. Keep the prior ownership check and best-effort cleanup until
    // native Windows lock handoff semantics are implemented.
    #[cfg(not(unix))]
    fn remove_if_owned(self) {
        let owned = self.still_owned();
        let path = self.path.clone();
        self.release();
        if owned {
            let _ = fs::remove_file(path);
        }
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        self.release_files();
    }
}

/// Starts a daemon rooted at the nearest `.code-graph.toml` ancestor of the
/// current working directory. With no config file, the working directory is
/// the project root, matching [`RootConfig::load`].
pub async fn run(server: CodeGraphServer) -> anyhow::Result<()> {
    let cwd = std::env::current_dir().context("read current directory for daemon root")?;
    let cwd = paths::canonicalize(&cwd).context("canonicalize daemon root")?;
    let (config, root) = RootConfig::load(&cwd).context("discover daemon project root")?;
    slow_test_contender_start(&root).await;
    run_until(
        server,
        root,
        Duration::from_secs(config.daemon.idle_timeout_secs),
        async {
            let _ = tokio::signal::ctrl_c().await;
        },
    )
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

async fn run_until<F>(
    server: CodeGraphServer,
    root: PathBuf,
    idle_timeout: Duration,
    shutdown: F,
) -> anyhow::Result<()>
where
    F: Future<Output = ()>,
{
    if let Err(existing_root) = server.bind_daemon_project_root(root.clone()) {
        bail!(
            "daemon server is already bound to project root {}; cannot start for {}",
            existing_root.display(),
            root.display()
        );
    }
    let paths = DaemonPaths::for_root(&root);
    paths.ensure_runtime_dir()?;
    #[cfg(target_os = "linux")]
    server
        .bind_daemon_retained_root(&paths.retained_root_metadata()?)
        .map_err(|_| anyhow::anyhow!("daemon retained project-root identity was already bound"))?;

    let Some(lock) = acquire_or_detect_live(&paths).await? else {
        eprintln!(
            "code-graph-mcp: daemon contender lost lock for {}",
            root.display()
        );
        return Ok(());
    };

    // Root ownership and runtime locking both succeeded through retained
    // descriptors, but their visible paths may have been substituted in the
    // handoff window. Treat that detached owner as a loser before scavenging
    // or publishing anything into either namespace.
    #[cfg(target_os = "linux")]
    if paths.ownership_path_change() != OwnershipPathChange::Intact {
        lock.remove_if_owned();
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        let cache_io_root = match paths.root_io_alias() {
            Ok(alias) => Some(alias),
            Err(error) => {
                eprintln!(
                    "code-graph-mcp: retained-root cache I/O alias unavailable for {}; logical cache saves remain guarded: {error}",
                    root.display()
                );
                None
            }
        };
        *server.inner.cache_io_root.write() = cache_io_root;
    }

    #[cfg(target_os = "linux")]
    if let Err(error) = paths.scavenge_owner_record_temps() {
        lock.remove_if_owned();
        return Err(error).context("scavenge daemon owner-record temps after lock acquisition");
    }

    if let Err(error) = clear_shutdown_request(&paths) {
        lock.remove_if_owned();
        return Err(error);
    }
    if let Err(error) = clear_shutdown_ack(&paths) {
        lock.remove_if_owned();
        return Err(error);
    }

    // A stale metadata/token pair can outlive a crashed lock owner. The OS
    // lock is authoritative: after acquiring it, preserve runtime metadata
    // only for a genuinely live recorded owner, never for a recycled endpoint.
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
    #[cfg(target_os = "linux")]
    if paths.ownership_path_change() != OwnershipPathChange::Intact {
        drop(listener);
        lock.remove_if_owned();
        return Ok(());
    }

    if let Err(error) = write_metadata_atomically_at(&paths, &metadata) {
        drop(listener);
        cleanup_owned(&paths, lock, &metadata).await;
        return Err(error);
    }

    #[cfg(target_os = "linux")]
    let ownership_watchdog = ownership_path_watchdog(&paths, &server);
    // SEAM(phase10-macos): no ownership watchdog off Linux — a renamed or
    // recreated project root is not detected while serving. Phase 10 decides
    // whether macOS gets a kqueue/re-stat watchdog or documents the gap.
    #[cfg(not(target_os = "linux"))]
    let ownership_watchdog = std::future::pending::<()>();
    let shutdown_or_idle = async {
        tokio::select! {
            _ = shutdown_with_request(&paths, &lock.identity, shutdown) => {},
            claimed = server.inner.persist.wait_for_idle_shutdown(idle_timeout) => {
                if claimed {
                    eprintln!("code-graph-mcp: daemon idle timeout reached for {}", root.display());
                }
            },
            _ = ownership_watchdog => {},
        }
    };
    serve_listener(listener, server.clone(), tcp_secret, shutdown_or_idle).await;
    graceful_shutdown(&server, &root).await;
    cleanup_owned(&paths, lock, &metadata).await;
    Ok(())
}

/// Closes admission as soon as a visible ownership path diverges from its
/// retained inode. Cache writes remain anchored through the daemon's retained
/// root alias, so either kind of replacement can still drain safely.
#[cfg(target_os = "linux")]
async fn ownership_path_watchdog(paths: &DaemonPaths, server: &CodeGraphServer) {
    loop {
        tokio::time::sleep(OWNERSHIP_PATH_POLL).await;
        match paths.ownership_path_change() {
            OwnershipPathChange::Intact => {}
            OwnershipPathChange::RuntimeReplaced => {
                eprintln!(
                    "code-graph-mcp: daemon runtime path changed for {}; shutting down",
                    paths.root.display()
                );
                server.inner.persist.close_admission();
                return;
            }
            OwnershipPathChange::RootReplaced => {
                eprintln!(
                    "code-graph-mcp: daemon project root path changed for {}; shutting down",
                    paths.root.display()
                );
                server.inner.persist.close_admission();
                return;
            }
        }
    }
}

/// Drains graph mutation before runtime cleanup. Existing analyses finish and
/// may persist; then the watcher and any watch reindex drain before one
/// exclusive final cache save captures the current graph through the daemon's
/// retained-root I/O path when available. The watcher task is awaited before
/// the index lock, so no queued watch batch can mutate after that save.
async fn graceful_shutdown(server: &CodeGraphServer, _daemon_root: &Path) {
    server.inner.persist.close_analyze_and_wait().await;
    let handle = { server.inner.watch.write().take() };
    if let Some(handle) = handle {
        let code_graph_tools::WatchHandle {
            debouncer,
            cancel,
            task,
        } = handle;
        // Cancel first; the loop prioritizes cancellation once it reaches its
        // select, then release the OS watcher off this Tokio worker.
        let _ = cancel.send(());
        let _ = task.await;
        let _ = tokio::task::spawn_blocking(move || drop(debouncer)).await;
    }
    // A normal `watch_stop` may already have moved its handle into detached
    // cooperative cleanup. Wait for that handoff before taking index_lock so
    // no queued watch batch can mutate after the final save.
    server.inner.persist.close_watch_cleanup_and_wait().await;
    let _index_guard = server.inner.index_lock.lock().await;
    server.inner.persist.close_persist_and_wait().await;
    let cache_root = server.inner.cache_root.read().clone();
    if server
        .inner
        .indexed
        .load(std::sync::atomic::Ordering::Acquire)
    {
        if let Some(cache_root) = cache_root {
            let graph = server.inner.graph.read();
            let cache_io_root = server.inner.cache_io_root.read().clone();
            let cache_io_root = match cache_io_root {
                Some(alias) => alias,
                None if server.inner.daemon_project_root.get().is_none() => cache_root,
                // SEAM(phase10-macos): no retained-root cache-I/O alias off
                // Linux — the final save always goes through the pathname, so
                // a root replaced mid-drain writes into the replacement.
                // Phase 10 exercises this and either accepts or anchors it.
                #[cfg(not(target_os = "linux"))]
                None => cache_root,
                #[cfg(target_os = "linux")]
                None => {
                    eprintln!(
                        "code-graph-mcp: skipped final daemon cache save because retained-root cache I/O is unavailable"
                    );
                    return;
                }
            };
            if let Err(error) = graph.save(&cache_io_root) {
                eprintln!("code-graph-mcp: final daemon cache save failed: {error}");
            }
        }
    }
}

async fn shutdown_with_request<F>(paths: &DaemonPaths, owner: &LockIdentity, shutdown: F)
where
    F: Future<Output = ()>,
{
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = tokio::time::sleep(SHUTDOWN_REQUEST_POLL) => {
                if accept_shutdown_request(paths, owner) {
                    return;
                }
            }
        }
    }
}

fn clear_shutdown_request(paths: &DaemonPaths) -> anyhow::Result<()> {
    clear_owner_file(paths, SHUTDOWN_REQUEST_FILE, "request")
}

fn clear_shutdown_ack(paths: &DaemonPaths) -> anyhow::Result<()> {
    clear_owner_file(paths, SHUTDOWN_ACK_FILE, "acknowledgement")
}

fn clear_owner_file(paths: &DaemonPaths, name: &str, label: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    let _control = paths
        .acquire_shutdown_control_lock()
        .with_context(|| format!("acquire daemon shutdown control lock for {label}"))?;
    match paths.remove_single_link_regular_child(name) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove stale daemon shutdown {label}")),
    }
}

fn write_shutdown_request(paths: &DaemonPaths, owner: &LockIdentity) -> anyhow::Result<()> {
    write_owner_file(paths, SHUTDOWN_REQUEST_FILE, "request", owner)
}

#[cfg(not(target_os = "linux"))]
fn write_shutdown_ack(paths: &DaemonPaths, owner: &LockIdentity) -> anyhow::Result<()> {
    write_owner_file(paths, SHUTDOWN_ACK_FILE, "acknowledgement", owner)
}

fn write_owner_file(
    paths: &DaemonPaths,
    name: &str,
    label: &str,
    owner: &LockIdentity,
) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        write_owner_file_linux(paths, name, label, owner)
    }
    // SEAM(phase10-macos): macOS takes the portable O_EXCL-create path, not
    // Linux's serialized temp+rename exchange under shutdown.control.lock.
    // Phase 10 exercises concurrent shutdown-request publication on macOS
    // and decides whether the portable arm's races are acceptable there.
    #[cfg(not(target_os = "linux"))]
    {
        write_owner_file_portable(paths, name, label, owner)
    }
}

#[cfg(target_os = "linux")]
fn write_owner_file_linux(
    paths: &DaemonPaths,
    name: &str,
    label: &str,
    owner: &LockIdentity,
) -> anyhow::Result<()> {
    let _control = paths
        .acquire_shutdown_control_lock()
        .context("acquire daemon shutdown control lock")?;
    write_owner_file_linux_locked(paths, name, label, owner)
}

#[cfg(target_os = "linux")]
fn write_owner_file_linux_locked(
    paths: &DaemonPaths,
    name: &str,
    label: &str,
    owner: &LockIdentity,
) -> anyhow::Result<()> {
    let encoded =
        serde_json::to_vec(owner).with_context(|| format!("serialize daemon shutdown {label}"))?;
    match read_owner_file(paths, name, label) {
        Ok(existing) if existing == *owner => return Ok(()),
        Ok(existing) if active_lock_identity(paths).as_ref() == Some(&existing) => {
            bail!("daemon shutdown {label} belongs to a different active owner")
        }
        Ok(_) | Err(_) => ensure_single_link_regular_owner_record_or_absent(paths, name, label)?,
    }
    if active_lock_identity(paths).as_ref() != Some(owner) {
        bail!("daemon shutdown {label} target owner is not active")
    }

    let (temp, mut file) = create_owner_record_temp(paths, name)?;
    let result = (|| -> anyhow::Result<()> {
        file.write_all(&encoded)
            .with_context(|| format!("write daemon shutdown {label} temp"))?;
        file.sync_all()
            .with_context(|| format!("sync daemon shutdown {label} temp"))?;
        // The control lock serializes all final-name mutations, so ordinary
        // descriptor-relative rename can atomically replace only authorized
        // stale/malformed state or install an absent record.
        if active_lock_identity(paths).as_ref() != Some(owner) {
            bail!("daemon shutdown {label} target owner changed before publication")
        }
        paths
            .rename_child(&temp, name)
            .with_context(|| format!("publish daemon shutdown {label}"))?;
        paths
            .sync_runtime_dir()
            .context("sync daemon shutdown runtime directory")
    })();
    if result.is_err() {
        let _ = paths.remove_child(&temp);
    }
    result
}

#[cfg(target_os = "linux")]
fn create_owner_record_temp(
    paths: &DaemonPaths,
    final_name: &str,
) -> anyhow::Result<(String, std::fs::File)> {
    for _ in 0..8 {
        let name = format!(
            "{OWNER_RECORD_TEMP_PREFIX}{final_name}-{}-{}.tmp",
            std::process::id(),
            random_hex(16)?
        );
        match paths.open_child(
            &name,
            OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY,
            Mode::from_bits_truncate(0o600),
        ) {
            Ok(file) => return Ok((name, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("create daemon shutdown temp"),
        }
    }
    bail!("could not allocate a unique daemon shutdown temp name")
}

/// Deferred native platforms retain the pre-hardening create-new publication
/// semantics: no existing record is clobbered, an exact owner is idempotent,
/// and reads remain bounded. Linux's stronger exchange protocol is deliberately
/// isolated above until native atomic replacement is completed there.
#[cfg(not(target_os = "linux"))]
fn write_owner_file_portable(
    paths: &DaemonPaths,
    name: &str,
    label: &str,
    owner: &LockIdentity,
) -> anyhow::Result<()> {
    #[cfg(unix)]
    let created = paths.open_child(
        name,
        OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY,
        Mode::from_bits_truncate(0o600),
    );
    #[cfg(not(unix))]
    let created = {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        options.open(paths.ordinary_path(name))
    };
    match created {
        Ok(mut file) => {
            file.write_all(&serde_json::to_vec(owner)?)?;
            file.sync_all()?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing: LockIdentity = serde_json::from_slice(&paths.read_bounded_record(
                name,
                MAX_SHUTDOWN_RECORD_BYTES,
                false,
            )?)
            .with_context(|| format!("parse existing daemon shutdown {label}"))?;
            if existing == *owner {
                Ok(())
            } else {
                bail!("daemon shutdown {label} belongs to a different owner")
            }
        }
        Err(error) => Err(error).with_context(|| format!("create daemon shutdown {label}")),
    }
}

/// Reads a control record only when it is a normal owner-only file. A writer
/// never adopts a hard-linked or symlinked entry as its own idempotent record.
fn read_owner_file(paths: &DaemonPaths, name: &str, label: &str) -> anyhow::Result<LockIdentity> {
    let encoded = paths.read_bounded_record(name, MAX_SHUTDOWN_RECORD_BYTES, true)?;
    serde_json::from_slice(&encoded)
        .with_context(|| format!("parse existing daemon shutdown {label}"))
}

#[cfg(target_os = "linux")]
fn ensure_single_link_regular_owner_record_or_absent(
    paths: &DaemonPaths,
    name: &str,
    label: &str,
) -> anyhow::Result<()> {
    let file = match paths.open_child(name, OFlags::RDONLY | OFlags::NONBLOCK, Mode::empty()) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context(format!("inspect daemon shutdown {label}")),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        bail!("daemon shutdown {label} is not a single-link regular file");
    }
    Ok(())
}

fn accept_shutdown_request(paths: &DaemonPaths, owner: &LockIdentity) -> bool {
    #[cfg(debug_assertions)]
    if std::env::var("CODE_GRAPH_TEST_DAEMON_IGNORE_REQUEST_ROOT")
        .ok()
        .is_some_and(|root| Path::new(&root) == paths.root)
    {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        let Ok(_control) = paths.acquire_shutdown_control_lock() else {
            return false;
        };
        let matches = read_owner_file(paths, SHUTDOWN_REQUEST_FILE, "request")
            .ok()
            .is_some_and(|requested| requested == *owner);
        if matches {
            if let Err(error) =
                write_owner_file_linux_locked(paths, SHUTDOWN_ACK_FILE, "acknowledgement", owner)
            {
                eprintln!("code-graph-mcp: publish daemon shutdown acknowledgement: {error}");
                return false;
            }
            let _ = paths.remove_single_link_regular_child(SHUTDOWN_REQUEST_FILE);
        }
        matches
    }
    #[cfg(not(target_os = "linux"))]
    {
        let matches = read_owner_file(paths, SHUTDOWN_REQUEST_FILE, "request")
            .ok()
            .is_some_and(|requested| requested == *owner);
        if matches {
            if let Err(error) = write_shutdown_ack(paths, owner) {
                eprintln!("code-graph-mcp: publish daemon shutdown acknowledgement: {error}");
                return false;
            }
            let _ = paths.remove_single_link_regular_child(SHUTDOWN_REQUEST_FILE);
        }
        matches
    }
}

fn shutdown_ack_matches(paths: &DaemonPaths, owner: &LockIdentity) -> bool {
    read_owner_file(paths, SHUTDOWN_ACK_FILE, "acknowledgement")
        .ok()
        .is_some_and(|acknowledged| acknowledged == *owner)
}

async fn acquire_or_detect_live(paths: &DaemonPaths) -> anyhow::Result<Option<DaemonLock>> {
    acquire_or_detect_live_with_initial_file(paths, None).await
}

/// `initial_file` makes the unlinked-inode handoff regression deterministic;
/// normal daemon acquisition always starts from the current pathname.
async fn acquire_or_detect_live_with_initial_file(
    paths: &DaemonPaths,
    mut initial_file: Option<std::fs::File>,
) -> anyhow::Result<Option<DaemonLock>> {
    #[cfg(target_os = "linux")]
    let root_file = match acquire_root_ownership(paths)? {
        Some(file) => file,
        // The root inode, not the mutable runtime directory, is the daemon
        // namespace authority. A contender that cannot lock it must not inspect
        // or publish state in a replacement `.code-graph` directory.
        None => return Ok(None),
    };
    loop {
        match DaemonLock::acquire(paths) {
            Ok(lock) => {
                #[cfg(target_os = "linux")]
                let lock = lock.with_root_ownership(root_file);
                #[cfg(target_os = "linux")]
                if paths.ownership_path_change() != OwnershipPathChange::Intact {
                    lock.remove_if_owned();
                    return Ok(None);
                }
                #[cfg(target_os = "linux")]
                if let Err(error) = paths.scavenge_metadata_temps(&lock) {
                    lock.remove_if_owned();
                    return Err(error)
                        .context("scavenge daemon metadata temps after lock acquisition");
                }
                return Ok(Some(lock));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let file = match initial_file.take() {
                    Some(file) => file,
                    None => match open_existing_lock(paths) {
                        Ok(file) => file,
                        // The prior owner can remove the lock between our
                        // failed exclusive create and this open. The pathname
                        // is free again; retry from creation.
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => return Err(error).context("open existing daemon lock"),
                    },
                };
                match file.try_lock() {
                    Ok(()) => {}
                    Err(TryLockError::WouldBlock) => return Ok(None),
                    Err(TryLockError::Error(error)) => {
                        return Err(error).context("lock existing daemon lock")
                    }
                }
                #[cfg(unix)]
                if !opened_lock_matches_path(&file, paths) {
                    // The old owner may have unlinked this inode and a successor
                    // may now own the pathname. Never clean runtime state for
                    // the detached inode; restart from the current pathname.
                    let _ = file.unlock();
                    drop(file);
                    continue;
                }
                #[cfg(unix)]
                {
                    rustix_fs::fchmod(&file, Mode::from_bits_truncate(0o600))
                        .context("restrict recovered daemon lock permissions")?;
                }
                let previous_identity = read_lock_identity_from(&file);

                // The OS lock is the single-instance authority. Once it is ours,
                // lock-file contents cannot retain ownership, even if a stale
                // identity happens to name a currently live process.
                cleanup_stale_runtime(paths, previous_identity.as_ref()).await;
                let identity = previous_identity.unwrap_or(LockIdentity {
                    pid: 0,
                    start_time: 0,
                    nonce: "malformed".to_owned(),
                });
                let mut lock = DaemonLock {
                    #[cfg(not(unix))]
                    path: paths.lock.clone(),
                    paths: paths.clone(),
                    contents: format!("{}\n", serde_json::to_string(&identity)?),
                    identity,
                    file: Some(file),
                    #[cfg(target_os = "linux")]
                    root_file: None,
                };
                lock.write_identity(LockIdentity::current()?)
                    .context("replace stale daemon lock identity")?;
                #[cfg(target_os = "linux")]
                let lock = lock.with_root_ownership(root_file);
                #[cfg(target_os = "linux")]
                if paths.ownership_path_change() != OwnershipPathChange::Intact {
                    lock.remove_if_owned();
                    return Ok(None);
                }
                #[cfg(target_os = "linux")]
                if let Err(error) = paths.scavenge_metadata_temps(&lock) {
                    lock.remove_if_owned();
                    return Err(error)
                        .context("scavenge daemon metadata temps after lock recovery");
                }
                return Ok(Some(lock));
            }
            Err(error) => return Err(error).context("create daemon lock"),
        }
    }
}

/// Acquires the crash-released daemon namespace lock from a fresh descriptor
/// for the verified project-root inode. It is intentionally taken before any
/// runtime `daemon.lock` create or recovery, and its caller keeps it across
/// every detached-inode retry.
#[cfg(target_os = "linux")]
fn acquire_root_ownership(paths: &DaemonPaths) -> std::io::Result<Option<std::fs::File>> {
    let root = paths.open_root_for_ownership()?;
    match root.try_lock() {
        Ok(()) => Ok(Some(root)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

#[cfg(target_os = "linux")]
fn is_metadata_temp_name(name: &[u8]) -> bool {
    let Some(name) = name
        .strip_prefix(METADATA_TEMP_PREFIX.as_bytes())
        .and_then(|name| name.strip_suffix(METADATA_TEMP_SUFFIX.as_bytes()))
    else {
        return false;
    };
    let Some(separator) = name.iter().position(|byte| *byte == b'-') else {
        return false;
    };
    let (pid, nonce_with_separator) = name.split_at(separator);
    let nonce = &nonce_with_separator[1..];
    canonical_decimal_u32(pid) && canonical_decimal_u128(nonce)
}

#[cfg(target_os = "linux")]
fn canonical_decimal_u32(value: &[u8]) -> bool {
    let Ok(value) = std::str::from_utf8(value) else {
        return false;
    };
    value
        .parse::<u32>()
        .is_ok_and(|parsed| parsed.to_string() == value)
}

#[cfg(target_os = "linux")]
fn canonical_decimal_u128(value: &[u8]) -> bool {
    let Ok(value) = std::str::from_utf8(value) else {
        return false;
    };
    value
        .parse::<u128>()
        .is_ok_and(|parsed| parsed.to_string() == value)
}

#[cfg(unix)]
fn opened_lock_matches_path(file: &std::fs::File, paths: &DaemonPaths) -> bool {
    let Ok(current) = paths.open_child(LOCK_FILE, OFlags::RDONLY | OFlags::NONBLOCK, Mode::empty())
    else {
        return false;
    };
    let Ok(opened) = file.metadata() else {
        return false;
    };
    let Ok(current) = current.metadata() else {
        return false;
    };
    opened.is_file()
        && opened.nlink() == 1
        && current.is_file()
        && current.nlink() == 1
        && opened.dev() == current.dev()
        && opened.ino() == current.ino()
}

fn open_existing_lock(paths: &DaemonPaths) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    let file = paths.open_child(LOCK_FILE, OFlags::RDWR | OFlags::NONBLOCK, Mode::empty())?;
    #[cfg(not(unix))]
    let file = {
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        options.open(&paths.lock)?
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::other(format!(
            "existing daemon lock {} is not a regular file",
            paths.lock.display()
        )));
    }
    #[cfg(unix)]
    if metadata.nlink() != 1 {
        return Err(std::io::Error::other(format!(
            "existing daemon lock {} has multiple links",
            paths.lock.display()
        )));
    }
    Ok(file)
}

fn read_lock_identity_from(file: &std::fs::File) -> Option<LockIdentity> {
    let mut file = file.try_clone().ok()?;
    file.seek(SeekFrom::Start(0)).ok()?;
    let contents =
        read_bounded_record_from(file, Path::new(LOCK_FILE), MAX_LOCK_RECORD_BYTES, true).ok()?;
    serde_json::from_slice(&contents).ok()
}

fn process_start_time(pid: u32) -> Option<u64> {
    // Targeted refresh: this runs inside 50ms client attach/replacement
    // poll loops, where a full-system process/disk/network scan
    // (`System::new_all` + `refresh_all`) is a sustained busy loop.
    let target = sysinfo::Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[target]), true);
    system.process(target).map(sysinfo::Process::start_time)
}

fn identity_is_alive(identity: &LockIdentity) -> bool {
    process_start_time(identity.pid) == Some(identity.start_time)
}

async fn prepare_runtime_for_owner(paths: &DaemonPaths) -> bool {
    let Ok(metadata) = read_metadata(paths) else {
        // Malformed and oversized regular files are stale instance state and
        // can be removed. Unsafe entries are retained: they are unavailable to
        // this daemon, but must not redirect cleanup through a symlink or
        // mutate a non-regular repository entry.
        let _ = paths.remove_regular_child(METADATA_FILE);
        let _ = paths.remove_regular_child(SECRET_FILE);
        return true;
    };
    // The caller already owns daemon.lock, so metadata identity is only
    // stale instance data. A reused PID/start time must not veto cleanup.
    cleanup_stale_runtime(paths, Some(&metadata.owner)).await;
    true
}

async fn cleanup_stale_runtime(paths: &DaemonPaths, _owner: Option<&LockIdentity>) {
    // The caller holds the OS lock on daemon.lock. Do not probe an endpoint
    // here: a recycled TCP port or UDS pathname can belong to an unrelated
    // listener, while this locked identity is already known stale/malformed.
    // Metadata and TCP auth are daemon-instance state; a live UDS inode is
    // deliberately preserved for bind-time fallback handling.
    let _ = paths.remove_regular_child(METADATA_FILE);
    let _ = paths.remove_regular_child(SECRET_FILE);
    let _ = clear_shutdown_request(paths);
    let _ = clear_shutdown_ack(paths);
    #[cfg(target_os = "linux")]
    let _ = paths.scavenge_owner_record_temps();
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

/// Test-only transport forcing for process-level TCP coverage. Unix tests
/// force the fallback by occupying the UDS pathname (a production-shaped
/// cause), but Windows pipe names are per-PID and cannot be occupied
/// externally, so the TCP authentication/rotation path needs an explicit
/// seam. The root match keeps concurrent test roots isolated; release
/// builds omit the seam entirely.
#[cfg(debug_assertions)]
fn debug_force_tcp(paths: &DaemonPaths) -> bool {
    let Ok(root) = std::env::var("CODE_GRAPH_TEST_FORCE_TCP_ROOT") else {
        return false;
    };
    Path::new(&root) == paths.root
}

#[cfg(not(debug_assertions))]
fn debug_force_tcp(_paths: &DaemonPaths) -> bool {
    false
}

async fn bind_listener(
    paths: &DaemonPaths,
    lock: &DaemonLock,
) -> anyhow::Result<(Listener, DaemonMetadata, Option<String>)> {
    if debug_force_tcp(paths) {
        eprintln!("code-graph-mcp: test seam forced daemon transport; using loopback TCP");
    } else {
        #[cfg(unix)]
        match bind_uds(paths, lock).await {
            UdsBind::Listener(listener) => {
                let endpoint = paths.socket.to_string_lossy().into_owned();
                return Ok((
                    Listener::Uds(listener),
                    DaemonMetadata::new(Transport::Uds, endpoint, lock.identity.clone())?,
                    None,
                ));
            }
            UdsBind::LiveListener => {
                eprintln!("code-graph-mcp: live UDS listener retained; using loopback TCP")
            }
            // SEAM(phase10-macos): on macOS this is the routine landing spot
            // for deeply nested checkouts — the raw socket path exceeds the
            // ~104-byte sun_path limit at `uds_bind_path`, so the daemon
            // degrades to authenticated loopback TCP. Logged, not silent.
            // Phase 10 measures how often real checkouts hit this and
            // whether the degrade stays acceptable (CLAUDE.md "revisit in
            // Phase 10").
            UdsBind::Unavailable(error) => {
                eprintln!("code-graph-mcp: UDS unavailable ({error}); using loopback TCP")
            }
        }

        #[cfg(windows)]
        match bind_pipe() {
            Ok((listener, endpoint)) => {
                return Ok((
                    Listener::Pipe(listener, endpoint.clone()),
                    DaemonMetadata::new(Transport::Pipe, endpoint, lock.identity.clone())?,
                    None,
                ));
            }
            Err(error) => {
                eprintln!("code-graph-mcp: named pipe unavailable ({error}); using loopback TCP")
            }
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
    write_secret_at(paths, &secret)?;
    Ok((
        Listener::Tcp(listener),
        DaemonMetadata::new(Transport::Tcp, endpoint, lock.identity.clone())?,
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
    let socket = match paths.uds_bind_path() {
        Ok(socket) => socket,
        Err(error) => return UdsBind::Unavailable(error.into()),
    };
    match std::os::unix::net::UnixListener::bind(&socket) {
        Ok(listener) => secure_uds_listener(paths, listener).await,
        Err(first_error) => {
            // A pathname is never a liveness signal. Only a refused
            // connection proves that an existing socket inode is orphaned.
            let connection_refused = matches!(
                tokio::net::UnixStream::connect(&socket).await,
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused
            );
            if !connection_refused {
                if tokio::net::UnixStream::connect(&socket).await.is_ok() {
                    return UdsBind::LiveListener;
                }
                return UdsBind::Unavailable(first_error.into());
            }
            if !lock.still_owned() {
                return UdsBind::LiveListener;
            }
            if !socket_inode(&socket) {
                return UdsBind::Unavailable(first_error.into());
            }
            if let Err(error) = fs::remove_file(&socket) {
                return UdsBind::Unavailable(error.into());
            }
            match std::os::unix::net::UnixListener::bind(&socket) {
                Ok(listener) => secure_uds_listener(paths, listener).await,
                Err(error) => UdsBind::Unavailable(error.into()),
            }
        }
    }
}

#[cfg(unix)]
async fn secure_uds_listener(
    paths: &DaemonPaths,
    listener: std::os::unix::net::UnixListener,
) -> UdsBind {
    let result = (|| -> anyhow::Result<_> {
        let runtime = paths.runtime_dir_for_child()?;
        let socket = paths.uds_bind_path()?;
        let metadata = fs::symlink_metadata(&socket)?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
            bail!("daemon socket entry is no longer the bound socket");
        }
        // Binding has installed this path's socket inode. Capture that exact
        // no-follow entry before chmod and require it to survive publication.
        let before = rustix_fs::statat(&runtime.file, SOCKET_FILE, AtFlags::SYMLINK_NOFOLLOW)?;
        // Linux does not support fchmod on an AF_UNIX listener. The no-follow
        // entry check above rejects substitutions before this chmod; the inode
        // comparison below rejects a replacement racing the permission step.
        fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        let after = rustix_fs::statat(&runtime.file, SOCKET_FILE, AtFlags::SYMLINK_NOFOLLOW)?;
        if after.st_dev != before.st_dev || after.st_ino != before.st_ino {
            bail!("daemon socket entry changed while permissions were secured");
        }
        listener.set_nonblocking(true)?;
        Ok(tokio::net::UnixListener::from_std(listener)?)
    })();
    match result {
        Ok(listener) => UdsBind::Listener(listener),
        Err(error) => {
            remove_orphan_socket(paths).await;
            UdsBind::Unavailable(error)
        }
    }
}

#[cfg(unix)]
fn socket_inode(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket())
}

#[cfg(unix)]
async fn remove_orphan_socket(paths: &DaemonPaths) {
    let Ok(socket) = paths.uds_bind_path() else {
        return;
    };
    if socket_inode(&socket)
        && matches!(
            tokio::net::UnixStream::connect(&socket).await,
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused
        )
    {
        let _ = fs::remove_file(socket);
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

#[cfg_attr(unix, allow(dead_code))]
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

#[cfg(unix)]
fn write_secret_at(paths: &DaemonPaths, secret: &str) -> anyhow::Result<()> {
    let mut file = paths.open_child(
        SECRET_FILE,
        OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY,
        Mode::from_bits_truncate(0o600),
    )?;
    let result = (|| -> anyhow::Result<()> {
        file.write_all(secret.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = paths.remove_child(SECRET_FILE);
    }
    result
}

#[cfg(not(unix))]
fn write_secret_at(paths: &DaemonPaths, secret: &str) -> anyhow::Result<()> {
    write_secret(&paths.secret, secret)
}

#[cfg(windows)]
fn restrict_windows_secret(path: &Path) -> anyhow::Result<()> {
    restrict_windows_path(path, "(R,W)", "daemon secret")
}

#[cfg(windows)]
fn restrict_windows_runtime_dir(path: &Path) -> anyhow::Result<()> {
    restrict_windows_path(path, "(OI)(CI)(F)", "daemon runtime directory")
}

/// Prefer the current user's SID for the `icacls` grant: a bare `USERNAME`
/// principal is ambiguous on domain-joined machines (a local `alice` shadows
/// `DOMAIN\alice`) and often fails outright for AzureAD accounts, while
/// `icacls` accepts a `*S-1-…` SID directly and unambiguously. The
/// environment-variable form stays as the fallback so an exotic `whoami`
/// failure degrades to the previous behavior instead of disabling the daemon.
#[cfg(windows)]
fn windows_owner_grant_principal() -> anyhow::Result<String> {
    if let Some(sid) = current_user_sid() {
        return Ok(format!("*{sid}"));
    }
    std::env::var("USERNAME").context("read Windows account for daemon secret ACL")
}

#[cfg(windows)]
fn current_user_sid() -> Option<String> {
    // `.output()` (never `.status()`/inherited stdio): child chatter must
    // not reach the MCP stdout stream when this runs inside the proxy.
    let output = std::process::Command::new("whoami")
        .args(["/user", "/fo", "csv", "/nh"])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    // /fo csv /nh emits one line: "domain\user","S-1-5-21-…"
    let text = String::from_utf8_lossy(&output.stdout);
    let sid = text
        .trim()
        .rsplit(',')
        .next()?
        .trim()
        .trim_matches('"')
        .to_owned();
    sid.starts_with("S-1-").then_some(sid)
}

#[cfg(windows)]
fn restrict_windows_path(path: &Path, permissions: &str, label: &str) -> anyhow::Result<()> {
    let owner = windows_owner_grant_principal()?;
    // `.output()` (never `.status()`): icacls chatters "processed file: …" on
    // success, and inherited stdio would inject that line into the middle of
    // the MCP stdout stream when this runs inside the proxy, corrupting
    // JSON-RPC framing for the attached host.
    let reset = std::process::Command::new("icacls")
        .arg(path)
        .arg("/reset")
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("reset icacls DACL for {label}"))?;
    if !reset.status.success() {
        bail!(
            "icacls failed to reset {label} DACL: {}",
            String::from_utf8_lossy(&reset.stderr).trim()
        )
    }
    let restrict = std::process::Command::new("icacls")
        .arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(format!("{owner}:{permissions}"))
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("run icacls for {label}"))?;
    if restrict.status.success() {
        Ok(())
    } else {
        bail!(
            "icacls failed to restrict {label}: {}",
            String::from_utf8_lossy(&restrict.stderr).trim()
        )
    }
}

#[cfg_attr(unix, allow(dead_code))]
fn write_metadata_atomically(path: &Path, metadata: &DaemonMetadata) -> anyhow::Result<()> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let encoded = serde_json::to_vec(metadata).context("serialize daemon metadata")?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = path.with_file_name(format!(".daemon-{}-{nonce}.tmp", std::process::id()));
    let write = || -> anyhow::Result<()> {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options
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

#[cfg(unix)]
fn write_metadata_atomically_at(
    paths: &DaemonPaths,
    metadata: &DaemonMetadata,
) -> anyhow::Result<()> {
    let encoded = serde_json::to_vec(metadata).context("serialize daemon metadata")?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = format!(".daemon-{}-{nonce}.tmp", std::process::id());
    let result = (|| -> anyhow::Result<()> {
        let mut file = paths.open_child(
            &temp,
            OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY,
            Mode::from_bits_truncate(0o600),
        )?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        paths.rename_child(&temp, METADATA_FILE)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = paths.remove_child(&temp);
    }
    result
}

#[cfg(not(unix))]
fn write_metadata_atomically_at(
    paths: &DaemonPaths,
    metadata: &DaemonMetadata,
) -> anyhow::Result<()> {
    write_metadata_atomically(&paths.metadata, metadata)
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
                Ok((mut stream, _)) => {
                    let Ok(permit) = permits.clone().try_acquire_owned() else {
                        continue;
                    };
                    let Ok(connection) = server.inner.persist.begin_connection() else {
                        // The idle timer may have claimed shutdown after this
                        // accept completed. Reject this racing stream rather
                        // than reviving the daemon after listener closure.
                        continue;
                    };
                    let server = server.clone();
                    tokio::spawn(async move {
                        // A connected UDS peer is not attached until both
                        // guards above are held and this prelude is delivered.
                        // A saturated or shutdown-racing peer sees EOF here,
                        // which lets its proxy retry or fall back instead of
                        // byte-pumping a dead MCP stream.
                        if write_acknowledgement(&mut stream).await.is_ok() {
                            serve_service(server, stream, permit, connection).await;
                        }
                    });
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
                        if validate_tcp_auth(&mut stream, &secret).await.is_ok() {
                            // TCP peers do not affect idle accounting until
                            // they have proved possession of the daemon secret.
                            // The permit still bounds unauthenticated handshakes.
                            if let Ok(connection) = server.inner.persist.begin_connection() {
                                // Admission atomically loses to an idle claim.
                                // A peer that loses must not receive CG-OK.
                                if write_acknowledgement(&mut stream).await.is_ok() {
                                    serve_service(server, stream, permit, connection).await;
                                }
                            }
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
                    let connection = server.inner.persist.begin_connection().ok();
                    let permit = permits.clone().try_acquire_owned().ok();
                    if let (Some(permit), Some(connection)) = (permit, connection) {
                        spawn_service(server.clone(), listener, permit, connection);
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

#[cfg(windows)]
fn spawn_service<S>(
    server: CodeGraphServer,
    mut stream: S,
    permit: OwnedSemaphorePermit,
    connection: code_graph_tools::ConnectionGuard,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        // A connected pipe peer is not attached until both guards are held
        // and this prelude is delivered — the same admission contract as the
        // UDS and TCP paths. A peer that never sees CG-OK observes pipe
        // closure and retries or falls back.
        if write_acknowledgement(&mut stream).await.is_ok() {
            serve_service(server, stream, permit, connection).await;
        }
    });
}

async fn serve_service<S>(
    server: CodeGraphServer,
    stream: S,
    _permit: OwnedSemaphorePermit,
    _connection: code_graph_tools::ConnectionGuard,
) where
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

#[cfg(test)]
async fn authenticate_tcp<S>(stream: &mut S, secret: &str) -> anyhow::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    validate_tcp_auth(stream, secret).await?;
    write_acknowledgement(stream).await
}

async fn validate_tcp_auth<S>(stream: &mut S, secret: &str) -> anyhow::Result<()>
where
    S: tokio::io::AsyncRead + Unpin,
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
    Ok(())
}

async fn read_acknowledgement<S>(
    stream: &mut S,
    acknowledgement_timeout: Duration,
    transport: &str,
) -> anyhow::Result<()>
where
    S: tokio::io::AsyncRead + Unpin,
{
    let mut acknowledgement = [0_u8; AUTH_ACK.len()];
    timeout(
        acknowledgement_timeout,
        stream.read_exact(&mut acknowledgement),
    )
    .await
    .with_context(|| format!("read daemon {transport} acknowledgement timed out"))?
    .with_context(|| format!("read daemon {transport} acknowledgement"))?;
    if acknowledgement != AUTH_ACK {
        bail!("invalid daemon {transport} acknowledgement")
    }
    Ok(())
}

async fn write_acknowledgement<S>(stream: &mut S) -> anyhow::Result<()>
where
    S: tokio::io::AsyncWrite + Unpin,
{
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
    if read_metadata(paths).is_ok_and(|current| current == *metadata) {
        let _ = paths.remove_regular_child(METADATA_FILE);
    }
    if metadata.transport == Transport::Tcp {
        let _ = paths.remove_regular_child(SECRET_FILE);
    }
    remove_shutdown_request_if_owned(paths, &metadata.owner);
    remove_shutdown_ack_if_owned(paths, &metadata.owner);
    #[cfg(unix)]
    if metadata.transport == Transport::Uds {
        remove_orphan_socket(paths).await;
    }
    lock.remove_if_owned();
}

fn remove_shutdown_request_if_owned(paths: &DaemonPaths, owner: &LockIdentity) {
    remove_owner_file_if_owned(paths, SHUTDOWN_REQUEST_FILE, owner);
}

fn remove_shutdown_ack_if_owned(paths: &DaemonPaths, owner: &LockIdentity) {
    remove_owner_file_if_owned(paths, SHUTDOWN_ACK_FILE, owner);
}

fn remove_owner_file_if_owned(paths: &DaemonPaths, name: &str, owner: &LockIdentity) {
    #[cfg(target_os = "linux")]
    let Ok(_control) = paths.acquire_shutdown_control_lock() else {
        return;
    };
    let belongs_to_owner = read_owner_file(paths, name, "cleanup")
        .ok()
        .is_some_and(|requested| requested == *owner);
    if belongs_to_owner {
        let _ = paths.remove_single_link_regular_child(name);
    }
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

    #[cfg(all(debug_assertions, target_os = "linux"))]
    struct EnvVarGuard {
        name: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    #[cfg(all(debug_assertions, target_os = "linux"))]
    impl EnvVarGuard {
        fn set_path(name: &'static str, value: &Path) -> Self {
            let previous = std::env::var_os(name);
            std::env::set_var(name, value);
            Self { name, previous }
        }
    }

    #[cfg(all(debug_assertions, target_os = "linux"))]
    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = self.previous.take() {
                std::env::set_var(self.name, value);
            } else {
                std::env::remove_var(self.name);
            }
        }
    }

    fn test_root() -> PathBuf {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "code-graph-mcp-daemon-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[cfg(all(debug_assertions, target_os = "linux"))]
    #[tokio::test]
    async fn run_until_starts_when_retained_root_cache_alias_is_unavailable() {
        let root = test_root();
        let _alias_failure =
            EnvVarGuard::set_path("CODE_GRAPH_TEST_FAIL_ROOT_IO_ALIAS_ROOT", &root);
        let server = CodeGraphServer::new(code_graph_lang::LanguageRegistry::new());
        let observer = server.clone();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let daemon = tokio::spawn(run_until(
            server,
            root.clone(),
            Duration::ZERO,
            async move {
                let _ = shutdown_rx.await;
            },
        ));
        wait_for_path(&root.join(".code-graph").join(METADATA_FILE)).await;
        assert!(
            observer.inner.cache_io_root.read().is_none(),
            "alias fallback keeps retained-root cache I/O unset"
        );
        shutdown_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), daemon)
            .await
            .expect("alias-fallback daemon shuts down")
            .unwrap()
            .unwrap();

        fs::remove_dir_all(root).unwrap();
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
        )
        .unwrap();
        let encoded = serde_json::to_value(&metadata).unwrap();
        assert_eq!(encoded["transport"], "tcp");
        assert_eq!(encoded["endpoint"], "127.0.0.1:1");
        assert!(encoded["started_at"].as_str().unwrap().ends_with('Z'));
        assert!(!encoded["binary_sha"].as_str().unwrap().is_empty());
    }

    #[test]
    fn metadata_payload_rejects_a_directory_without_opening_it() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        fs::create_dir(&paths.metadata).unwrap();

        let started = std::time::Instant::now();
        assert!(read_metadata_payload(&paths.metadata).is_err());
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a non-regular metadata entry fails without blocking"
        );
        assert!(paths.metadata.is_dir(), "the directory remains untouched");

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn metadata_payload_rejects_a_symlink_without_reading_its_target() {
        let root = test_root();
        let outside = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let target = outside.join("metadata-target");
        let contents = b"external daemon metadata target\n";
        fs::write(&target, contents).unwrap();
        std::os::unix::fs::symlink(&target, &paths.metadata).unwrap();

        let started = std::time::Instant::now();
        assert!(read_metadata_payload(&paths.metadata).is_err());
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a metadata symlink fails without blocking"
        );
        assert_eq!(fs::read(&target).unwrap(), contents, "target is untouched");
        assert!(fs::symlink_metadata(&paths.metadata)
            .unwrap()
            .file_type()
            .is_symlink());

        fs::remove_file(&paths.metadata).unwrap();
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn metadata_payload_rejects_an_oversized_file_without_reading_it() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let contents = vec![b'x'; MAX_METADATA_BYTES + 1];
        fs::write(&paths.metadata, &contents).unwrap();

        let started = std::time::Instant::now();
        assert!(read_metadata_payload(&paths.metadata).is_err());
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "oversized metadata fails without blocking"
        );
        assert_eq!(fs::read(&paths.metadata).unwrap(), contents);

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn oversized_lock_record_fails_bounded_without_mutation() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let contents = vec![b'x'; MAX_LOCK_RECORD_BYTES + 1];
        fs::write(&paths.lock, &contents).unwrap();

        let started = std::time::Instant::now();
        assert!(read_lock_identity_child(&paths).is_err());
        let opened = open_existing_lock(&paths).unwrap();
        assert!(
            read_lock_identity_from(&opened).is_none(),
            "an already-open oversized lock is bounded too"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "oversized lock record fails without blocking"
        );
        assert_eq!(fs::read(&paths.lock).unwrap(), contents);

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn oversized_shutdown_records_fail_bounded_without_mutation() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let owner = LockIdentity::current().unwrap();
        let request = vec![b'r'; MAX_SHUTDOWN_RECORD_BYTES + 1];
        let acknowledgement = vec![b'a'; MAX_SHUTDOWN_RECORD_BYTES + 1];
        fs::write(&paths.shutdown_request, &request).unwrap();
        fs::write(&paths.shutdown_ack, &acknowledgement).unwrap();

        let started = std::time::Instant::now();
        assert!(write_shutdown_request(&paths, &owner).is_err());
        assert!(!accept_shutdown_request(&paths, &owner));
        assert!(!shutdown_ack_matches(&paths, &owner));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "oversized shutdown records fail without blocking"
        );
        assert_eq!(fs::read(&paths.shutdown_request).unwrap(), request);
        assert_eq!(fs::read(&paths.shutdown_ack).unwrap(), acknowledgement);

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn oversized_secret_record_fails_bounded_without_mutation() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let contents = vec![b'a'; MAX_SECRET_RECORD_BYTES + 1];
        fs::write(&paths.secret, &contents).unwrap();

        let started = std::time::Instant::now();
        assert!(read_client_secret(&paths).is_err());
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "oversized secret record fails without blocking"
        );
        assert_eq!(fs::read(&paths.secret).unwrap(), contents);

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn replaced_lock_record_descriptor_fails_without_touching_the_symlink_target() {
        let root = test_root();
        let outside = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let owner = LockIdentity::current().unwrap();
        fs::write(&paths.lock, serde_json::to_vec(&owner).unwrap()).unwrap();
        let opened = paths
            .open_child(LOCK_FILE, OFlags::RDONLY | OFlags::NONBLOCK, Mode::empty())
            .unwrap();
        let sentinel = outside.join("lock-sentinel");
        let contents = b"external lock record sentinel\n";
        fs::write(&sentinel, contents).unwrap();

        fs::remove_file(&paths.lock).unwrap();
        std::os::unix::fs::symlink(&sentinel, &paths.lock).unwrap();

        let started = std::time::Instant::now();
        assert!(
            read_bounded_record_from(opened, &paths.lock, MAX_LOCK_RECORD_BYTES, true,).is_err()
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a substituted lock record fails without blocking"
        );
        assert_eq!(fs::read(&sentinel).unwrap(), contents);
        assert!(fs::symlink_metadata(&paths.lock)
            .unwrap()
            .file_type()
            .is_symlink());

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tcp_fallback_metadata_is_loopback_only() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let lock = DaemonLock::acquire(&paths).unwrap();
        fs::write(&paths.socket, b"force TCP fallback").unwrap();
        let (listener, metadata, secret) = bind_listener(&paths, &lock).await.unwrap();
        let address: std::net::SocketAddr = metadata.endpoint.parse().unwrap();
        assert_eq!(metadata.transport, Transport::Tcp);
        assert!(
            address.ip().is_loopback(),
            "TCP fallback never binds a LAN address"
        );
        drop(listener);
        drop(secret);
        let _ = fs::remove_file(&paths.secret);
        let _ = fs::remove_file(&paths.socket);
        lock.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
    }

    /// Windows forces TCP by occupying the daemon's per-PID pipe name rather
    /// than planting a socket file. Safe against parallel in-process tests:
    /// every other unit test calling `bind_listener` is unix/linux-gated, so
    /// nothing else binds this process's pipe name on Windows.
    #[cfg(windows)]
    #[tokio::test]
    async fn tcp_fallback_metadata_is_loopback_only() {
        let endpoint = format!(r"\\.\pipe\code-graph-mcp-{}", std::process::id());
        let _occupied = tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(true)
            .create(&endpoint)
            .unwrap();

        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let lock = DaemonLock::acquire(&paths).unwrap();
        let (listener, metadata, secret) = bind_listener(&paths, &lock).await.unwrap();
        let address: std::net::SocketAddr = metadata.endpoint.parse().unwrap();
        assert_eq!(metadata.transport, Transport::Tcp);
        assert!(
            address.ip().is_loopback(),
            "TCP fallback never binds a LAN address"
        );
        assert!(secret.is_some(), "TCP fallback publishes a secret");
        drop(listener);
        drop(secret);
        let _ = fs::remove_file(&paths.secret);
        lock.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn uds_client_uses_its_own_runtime_descriptor_alias() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let lock = DaemonLock::acquire(&paths).unwrap();
        let (listener, metadata, secret) = bind_listener(&paths, &lock).await.unwrap();
        assert_eq!(metadata.transport, Transport::Uds);
        assert_eq!(metadata.endpoint, paths.socket.to_string_lossy());
        assert!(paths
            .uds_bind_path()
            .unwrap()
            .starts_with(format!("/proc/{}/fd/", std::process::id())));
        let Listener::Uds(listener) = listener else {
            panic!("test UDS binding selected TCP fallback");
        };
        let acknowledgement = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            write_acknowledgement(&mut stream).await.unwrap();
        });
        let connection = connect_to_metadata(&paths, &metadata, Duration::from_secs(1))
            .await
            .unwrap();
        assert!(matches!(connection.stream, ClientStream::Uds(_)));
        acknowledgement.await.unwrap();
        drop(secret);
        let _ = paths.remove_child(SOCKET_FILE);
        lock.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn socket_leaf_substitution_fails_without_touching_the_external_sentinel() {
        let root = test_root();
        let outside = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let socket = paths.uds_bind_path().unwrap();
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let sentinel = outside.join("socket-sentinel");
        let contents = b"socket leaf sentinel\n";
        fs::write(&sentinel, contents).unwrap();

        paths.remove_child(SOCKET_FILE).unwrap();
        std::os::unix::fs::symlink(&sentinel, &paths.socket).unwrap();
        assert!(matches!(
            secure_uds_listener(&paths, listener).await,
            UdsBind::Unavailable(_)
        ));
        assert_eq!(fs::read(&sentinel).unwrap(), contents);

        fs::remove_file(&paths.socket).unwrap();
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn binary_sha_gate_accepts_only_equal_clean_builds() {
        assert!(sha_compatible_with("clean-a", "clean-a", "fp-a", "fp-a"));
        assert!(!sha_compatible_with("clean-a", "clean-b", "fp-a", "fp-a"));
        assert!(!sha_compatible_with(
            "dirty-a-dirty",
            "dirty-a-dirty",
            "fp-a",
            "fp-b"
        ));
        assert!(sha_compatible_with(
            "dirty-a-dirty",
            "dirty-a-dirty",
            "fp-a",
            "fp-a"
        ));
        assert!(!sha_compatible_with("clean-a", "clean-a", "", ""));
    }

    #[test]
    fn matching_metadata_without_an_actively_held_lock_is_not_attachable() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let owner = LockIdentity::current().unwrap();
        fs::write(&paths.lock, serde_json::to_vec(&owner).unwrap()).unwrap();
        let metadata = DaemonMetadata {
            pid: owner.pid,
            transport: Transport::Tcp,
            endpoint: "127.0.0.1:1".to_owned(),
            binary_sha: env!("CODE_GRAPH_GIT_SHA").to_owned(),
            executable_fingerprint: executable_fingerprint().unwrap(),
            started_at: rfc3339_now(),
            owner,
        };
        assert!(!metadata_owner_is_active(&paths, &metadata));
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn graceful_shutdown_saves_only_the_active_cache_project_root() {
        let daemon_root = test_root();
        let active_project = test_root();
        let server = CodeGraphServer::new(code_graph_lang::LanguageRegistry::new());
        server
            .inner
            .indexed
            .store(true, std::sync::atomic::Ordering::Release);
        *server.inner.cache_root.write() = Some(active_project.clone());
        graceful_shutdown(&server, &daemon_root).await;
        assert!(code_graph_graph::cache_path(&active_project).exists());
        assert!(!code_graph_graph::cache_path(&daemon_root).exists());
        fs::remove_dir_all(daemon_root).unwrap();
        fs::remove_dir_all(active_project).unwrap();
    }

    #[tokio::test]
    async fn run_until_uses_the_idle_future_to_close_and_cleanup_the_listener() {
        let root = test_root();
        let server = CodeGraphServer::new(code_graph_lang::LanguageRegistry::new());
        tokio::time::timeout(
            Duration::from_secs(5),
            run_until(
                server,
                root.clone(),
                Duration::from_millis(20),
                std::future::pending(),
            ),
        )
        .await
        .expect("idle future shuts the listener down")
        .unwrap();
        assert!(!root.join(".code-graph/daemon.lock").exists());
        assert!(!root.join(".code-graph/daemon.json").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn runtime_replacement_watchdog_exits_then_a_successor_reacquires() {
        let root = test_root();
        let runtime = root.join(".code-graph");
        let old_server = CodeGraphServer::new(code_graph_lang::LanguageRegistry::new());
        old_server
            .inner
            .indexed
            .store(true, std::sync::atomic::Ordering::Release);
        *old_server.inner.cache_root.write() = Some(root.clone());
        let old = tokio::spawn(run_until(
            old_server,
            root.clone(),
            Duration::ZERO,
            std::future::pending(),
        ));
        wait_for_path(&runtime.join(METADATA_FILE)).await;

        let displaced = root.join("displaced-runtime");
        fs::rename(&runtime, &displaced).unwrap();
        fs::create_dir(&runtime).unwrap();
        tokio::time::timeout(Duration::from_secs(5), old)
            .await
            .expect("runtime watchdog shuts down the old daemon")
            .unwrap()
            .unwrap();
        assert!(
            code_graph_graph::cache_path(&root).exists(),
            "a runtime-only replacement permits the old root's final cache save"
        );

        let successor_server = CodeGraphServer::new(code_graph_lang::LanguageRegistry::new());
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let successor = tokio::spawn(run_until(
            successor_server,
            root.clone(),
            Duration::ZERO,
            async move {
                let _ = shutdown_rx.await;
            },
        ));
        wait_for_path(&runtime.join(METADATA_FILE)).await;
        shutdown_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), successor)
            .await
            .expect("successor daemon shuts down")
            .unwrap()
            .unwrap();

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn root_replacement_watchdog_saves_only_through_the_retained_root_alias() {
        let root = test_root();
        let runtime = root.join(".code-graph");
        let server = CodeGraphServer::new(code_graph_lang::LanguageRegistry::new());
        server
            .inner
            .indexed
            .store(true, std::sync::atomic::Ordering::Release);
        *server.inner.cache_root.write() = Some(root.clone());
        let daemon = tokio::spawn(run_until(
            server,
            root.clone(),
            Duration::ZERO,
            std::future::pending(),
        ));
        wait_for_path(&runtime.join(METADATA_FILE)).await;

        let relocated = root.with_extension("relocated");
        fs::rename(&root, &relocated).unwrap();
        fs::create_dir(&root).unwrap();
        tokio::time::timeout(Duration::from_secs(5), daemon)
            .await
            .expect("root watchdog shuts down the old daemon")
            .unwrap()
            .unwrap();
        assert!(
            !code_graph_graph::cache_path(&root).exists(),
            "the old daemon never saves its cache through the replacement root path"
        );
        let mut retained_cache = code_graph_graph::Graph::new();
        assert!(
            retained_cache.load(&relocated).unwrap(),
            "the final cache is written through the retained original root inode"
        );

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(relocated).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proxy_refreshes_a_detached_runtime_descriptor_for_successor_metadata() {
        let root = test_root();
        let mut proxy_paths = DaemonPaths::for_root(&root);
        proxy_paths.ensure_runtime_dir().unwrap();
        let old_metadata = DaemonMetadata::new(
            Transport::Tcp,
            "127.0.0.1:1".to_owned(),
            LockIdentity::current().unwrap(),
        )
        .unwrap();
        write_metadata_atomically_at(&proxy_paths, &old_metadata).unwrap();

        let displaced = root.join("displaced-runtime");
        fs::rename(&proxy_paths.runtime, &displaced).unwrap();
        fs::create_dir(&proxy_paths.runtime).unwrap();
        let successor_paths = DaemonPaths::for_root(&root);
        successor_paths.ensure_runtime_dir().unwrap();
        let successor_metadata = DaemonMetadata::new(
            Transport::Tcp,
            "127.0.0.1:2".to_owned(),
            LockIdentity::current().unwrap(),
        )
        .unwrap();
        write_metadata_atomically_at(&successor_paths, &successor_metadata).unwrap();

        assert!(proxy_paths.proxy_capability_diverged());
        assert!(refresh_proxy_paths(&mut proxy_paths, &root).unwrap());
        assert_eq!(read_metadata(&proxy_paths).unwrap(), successor_metadata);

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn proxy_namespace_refresh_reaps_contender_and_resets_spawn_state() {
        let child = tokio::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("start stale contender");
        let pid = child.id().expect("child pid");
        let start_time = process_start_time(pid).expect("child start time");
        let mut contender = Some(child);
        let mut replaced_owner = Some(LockIdentity::current().unwrap());
        let mut next_spawn_at = std::time::Instant::now() + Duration::from_secs(30);
        let mut spawn_backoff = PROXY_MAX_SPAWN_BACKOFF;

        reset_proxy_after_namespace_change(
            &mut contender,
            &mut replaced_owner,
            &mut next_spawn_at,
            &mut spawn_backoff,
        )
        .await;

        assert!(contender.is_none(), "detached contender is discarded");
        assert!(replaced_owner.is_none(), "old namespace owner is discarded");
        assert_eq!(spawn_backoff, PROXY_RETRY_INTERVAL);
        assert!(
            next_spawn_at <= std::time::Instant::now(),
            "a successor is eligible immediately after namespace refresh"
        );
        assert!(
            !identity_is_alive(&LockIdentity {
                pid,
                start_time,
                nonce: String::new(),
            }),
            "reset reaps the stale contender process"
        );
    }

    #[cfg(target_os = "linux")]
    async fn wait_for_path(path: &Path) {
        for _ in 0..100 {
            if path.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for daemon path {}", path.display());
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_request_requires_a_regular_file_and_exact_owner() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let lock = DaemonLock::acquire(&paths).unwrap();
        std::os::unix::fs::symlink("outside", &paths.shutdown_request).unwrap();
        assert!(write_shutdown_request(&paths, &lock.identity).is_err());
        assert!(clear_shutdown_request(&paths).is_err());
        fs::remove_file(&paths.shutdown_request).unwrap();

        let wrong_owner = LockIdentity {
            pid: lock.identity.pid,
            start_time: lock.identity.start_time,
            nonce: "different".to_owned(),
        };
        fs::write(
            &paths.shutdown_request,
            serde_json::to_vec(&wrong_owner).unwrap(),
        )
        .unwrap();
        assert!(!accept_shutdown_request(&paths, &lock.identity));
        assert!(
            paths.shutdown_request.exists(),
            "foreign request is retained"
        );
        clear_shutdown_request(&paths).unwrap();
        write_shutdown_request(&paths, &lock.identity).unwrap();
        assert!(accept_shutdown_request(&paths, &lock.identity));
        assert!(
            !paths.shutdown_request.exists(),
            "owned request is consumed"
        );
        lock.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn truncated_shutdown_request_and_ack_recover_for_the_active_owner() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let lock = DaemonLock::acquire(&paths).unwrap();
        assert_eq!(active_lock_identity(&paths).as_ref(), Some(&lock.identity));

        let truncated_request = b"{\"pid\":";
        fs::write(&paths.shutdown_request, truncated_request).unwrap();
        let different_owner = LockIdentity {
            pid: lock.identity.pid,
            start_time: lock.identity.start_time,
            nonce: "different-owner".to_owned(),
        };
        assert!(write_shutdown_request(&paths, &different_owner).is_err());
        assert_eq!(
            fs::read(&paths.shutdown_request).unwrap(),
            truncated_request,
            "a malformed record remains when the target does not own the active lock"
        );
        assert_no_owner_record_temps(&paths);

        write_shutdown_request(&paths, &lock.identity).unwrap();
        assert_eq!(
            read_owner_file(&paths, SHUTDOWN_REQUEST_FILE, "request").unwrap(),
            lock.identity
        );

        fs::write(&paths.shutdown_ack, b"{\"start_time\":").unwrap();
        write_owner_file_linux(&paths, SHUTDOWN_ACK_FILE, "acknowledgement", &lock.identity)
            .unwrap();
        assert_eq!(
            read_owner_file(&paths, SHUTDOWN_ACK_FILE, "acknowledgement").unwrap(),
            lock.identity
        );
        assert_no_owner_record_temps(&paths);

        lock.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn owner_record_replacement_leaves_stale_state_until_serialized_publication() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let lock = DaemonLock::acquire(&paths).unwrap();
        let stale = LockIdentity {
            pid: lock.identity.pid,
            start_time: lock.identity.start_time,
            nonce: "stale-owner".to_owned(),
        };
        let stale_bytes = serde_json::to_vec(&stale).unwrap();
        fs::write(&paths.shutdown_request, &stale_bytes).unwrap();

        replace_stale_shutdown_request(&paths, &lock.identity).unwrap();
        assert_eq!(
            fs::read(&paths.shutdown_request).unwrap(),
            stale_bytes,
            "authorization leaves stale final state for serialized publication"
        );
        write_shutdown_request(&paths, &lock.identity).unwrap();
        assert_eq!(
            read_owner_file(&paths, SHUTDOWN_REQUEST_FILE, "request").unwrap(),
            lock.identity
        );
        assert_no_owner_record_temps(&paths);

        assert!(write_shutdown_request(&paths, &lock.identity).is_ok());
        assert!(write_shutdown_request(&paths, &stale).is_err());
        assert_eq!(
            read_owner_file(&paths, SHUTDOWN_REQUEST_FILE, "request").unwrap(),
            lock.identity,
            "a different owner cannot replace an active final record"
        );
        assert_no_owner_record_temps(&paths);

        lock.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn shutdown_control_lock_serializes_publishers_and_blocks_owner_transition() {
        use std::sync::mpsc;

        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let mut lock = DaemonLock::acquire(&paths).unwrap();
        let target = lock.identity.clone();
        let control = paths.acquire_shutdown_control_lock().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            let paths = paths.clone();
            let owner = target.clone();
            scope.spawn(move || {
                started_tx.send(()).unwrap();
                done_tx
                    .send(write_shutdown_request(&paths, &owner))
                    .unwrap();
            });
            started_rx.recv().unwrap();
            assert!(
                matches!(
                    done_rx.recv_timeout(Duration::from_millis(50)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ),
                "a competing publisher waits on the persistent control lock"
            );
            drop(control);
            done_rx.recv().unwrap().unwrap();
        });
        assert_eq!(
            read_owner_file(&paths, SHUTDOWN_REQUEST_FILE, "request").unwrap(),
            target
        );
        assert_no_owner_record_temps(&paths);

        let stale = LockIdentity {
            pid: target.pid,
            start_time: target.start_time,
            nonce: "stale-owner".to_owned(),
        };
        fs::write(&paths.shutdown_request, serde_json::to_vec(&stale).unwrap()).unwrap();
        let control = paths.acquire_shutdown_control_lock().unwrap();
        let successor = LockIdentity {
            pid: target.pid,
            start_time: target.start_time,
            nonce: "successor-owner".to_owned(),
        };
        lock.write_identity(successor).unwrap();
        drop(control);
        assert!(write_shutdown_request(&paths, &target).is_err());
        assert_eq!(
            read_owner_file(&paths, SHUTDOWN_REQUEST_FILE, "request").unwrap(),
            stale,
            "a target owner transition prevents stale-record overwrite"
        );
        assert_no_owner_record_temps(&paths);

        lock.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn locked_successor_scavenges_only_single_link_owner_record_temps() {
        let root = test_root();
        let outside = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let lock = DaemonLock::acquire(&paths).unwrap();
        let leaked = paths.runtime.join(format!(
            "{OWNER_RECORD_TEMP_PREFIX}shutdown.request-crashed.tmp"
        ));
        fs::write(&leaked, b"completed but unexchanged").unwrap();

        let sentinel = outside.join("owner-record-temp-sentinel");
        let contents = b"external owner-record temp sentinel\n";
        fs::write(&sentinel, contents).unwrap();
        let hardlinked = paths.runtime.join(format!(
            "{OWNER_RECORD_TEMP_PREFIX}shutdown.request-hardlink.tmp"
        ));
        fs::hard_link(&sentinel, &hardlinked).unwrap();

        paths.scavenge_owner_record_temps().unwrap();
        assert!(
            !leaked.exists(),
            "single-link crashed staging inode is removed"
        );
        assert_eq!(fs::read(&sentinel).unwrap(), contents);
        assert!(hardlinked.exists(), "hard-linked sentinel is retained");

        fs::remove_file(hardlinked).unwrap();
        lock.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn authoritative_lock_scavenges_only_exact_metadata_temps() {
        let root = test_root();
        let outside = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();

        let leaked = paths.runtime.join(".daemon-123-456.tmp");
        fs::write(&leaked, b"crashed metadata publication").unwrap();
        let unrelated_names = [
            ".daemon--456.tmp",
            ".daemon-123-.tmp",
            ".daemon-pid-456.tmp",
            ".daemon-123-nonce.tmp",
            ".daemon-001-456.tmp",
            ".daemon-123-00456.tmp",
            ".daemon-4294967296-456.tmp",
            ".daemon-123-340282366920938463463374607431768211456.tmp",
            ".daemon-123-456.tmp.bak",
            ".daemon-123-456-789.tmp",
        ];
        for name in unrelated_names {
            fs::write(paths.runtime.join(name), b"unrelated runtime entry").unwrap();
        }
        let directory = paths.runtime.join(".daemon-234-567.tmp");
        fs::create_dir(&directory).unwrap();

        let sentinel = outside.join("metadata-temp-sentinel");
        let sentinel_contents = b"external metadata temp sentinel\n";
        fs::write(&sentinel, sentinel_contents).unwrap();
        let hardlinked = paths.runtime.join(".daemon-345-678.tmp");
        fs::hard_link(&sentinel, &hardlinked).unwrap();
        let symlinked = paths.runtime.join(".daemon-456-789.tmp");
        std::os::unix::fs::symlink(&sentinel, &symlinked).unwrap();

        let lock = acquire_or_detect_live(&paths).await.unwrap().unwrap();
        assert!(!leaked.exists(), "exact single-link temp is scavenged");
        for name in unrelated_names {
            assert!(
                paths.runtime.join(name).exists(),
                "near miss {name} is retained"
            );
        }
        assert!(directory.is_dir(), "matching directory is retained");
        assert_eq!(fs::read(&sentinel).unwrap(), sentinel_contents);
        assert!(hardlinked.exists(), "matching hard link is retained");
        assert!(fs::symlink_metadata(&symlinked)
            .unwrap()
            .file_type()
            .is_symlink());

        lock.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn fresh_daemon_lock_scavenges_owner_record_temps_before_control_cleanup() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let leaked = paths.runtime.join(format!(
            "{OWNER_RECORD_TEMP_PREFIX}shutdown.request-fresh-lock.tmp"
        ));
        fs::write(&leaked, b"crashed named staging record").unwrap();
        let server = CodeGraphServer::new(code_graph_lang::LanguageRegistry::new());

        run_until(
            server,
            root.clone(),
            Duration::from_millis(20),
            std::future::pending(),
        )
        .await
        .unwrap();
        assert!(
            !leaked.exists(),
            "a fresh lock scavenges prefixed crash state before control cleanup"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn concurrent_owner_record_publishers_converge_without_clobbering() {
        use std::sync::{Arc, Barrier};

        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let lock = DaemonLock::acquire(&paths).unwrap();
        let owner = lock.identity.clone();
        let barrier = Arc::new(Barrier::new(8));
        std::thread::scope(|scope| {
            let mut publishers = Vec::new();
            for _ in 0..8 {
                let paths = paths.clone();
                let owner = owner.clone();
                let barrier = barrier.clone();
                publishers.push(scope.spawn(move || {
                    barrier.wait();
                    write_shutdown_request(&paths, &owner)
                }));
            }
            for publisher in publishers {
                publisher.join().unwrap().unwrap();
            }
        });
        assert_eq!(
            read_owner_file(&paths, SHUTDOWN_REQUEST_FILE, "request").unwrap(),
            owner
        );
        assert_no_owner_record_temps(&paths);
        lock.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn owner_record_publication_preserves_external_symlink_and_hardlink_sentinels() {
        let root = test_root();
        let outside = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let lock = DaemonLock::acquire(&paths).unwrap();

        let symlink_sentinel = outside.join("shutdown-request-symlink-sentinel");
        let symlink_contents = b"external shutdown request sentinel\n";
        fs::write(&symlink_sentinel, symlink_contents).unwrap();
        std::os::unix::fs::symlink(&symlink_sentinel, &paths.shutdown_request).unwrap();
        assert!(write_shutdown_request(&paths, &lock.identity).is_err());
        assert_eq!(fs::read(&symlink_sentinel).unwrap(), symlink_contents);
        assert!(fs::symlink_metadata(&paths.shutdown_request)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_no_owner_record_temps(&paths);
        fs::remove_file(&paths.shutdown_request).unwrap();

        let hardlink_sentinel = outside.join("shutdown-request-hardlink-sentinel");
        let hardlink_contents = b"external shutdown request hardlink sentinel\n";
        fs::write(&hardlink_sentinel, hardlink_contents).unwrap();
        fs::hard_link(&hardlink_sentinel, &paths.shutdown_request).unwrap();
        assert!(write_shutdown_request(&paths, &lock.identity).is_err());
        assert_eq!(fs::read(&hardlink_sentinel).unwrap(), hardlink_contents);
        assert_eq!(
            fs::metadata(&paths.shutdown_request).unwrap().nlink(),
            2,
            "hard-linked sentinel remains installed rather than being recovered"
        );
        assert_no_owner_record_temps(&paths);

        fs::remove_file(&paths.shutdown_request).unwrap();
        lock.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn owner_record_cleanup_refuses_linked_final_sentinels() {
        let root = test_root();
        let outside = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let lock = DaemonLock::acquire(&paths).unwrap();
        let encoded = serde_json::to_vec(&lock.identity).unwrap();

        let symlink_sentinel = outside.join("cleanup-symlink-sentinel");
        fs::write(&symlink_sentinel, &encoded).unwrap();
        std::os::unix::fs::symlink(&symlink_sentinel, &paths.shutdown_request).unwrap();
        remove_shutdown_request_if_owned(&paths, &lock.identity);
        assert!(fs::symlink_metadata(&paths.shutdown_request)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(&symlink_sentinel).unwrap(), encoded);
        fs::remove_file(&paths.shutdown_request).unwrap();

        let hardlink_sentinel = outside.join("cleanup-hardlink-sentinel");
        fs::write(&hardlink_sentinel, &encoded).unwrap();
        fs::hard_link(&hardlink_sentinel, &paths.shutdown_ack).unwrap();
        remove_shutdown_ack_if_owned(&paths, &lock.identity);
        assert!(paths.shutdown_ack.exists());
        assert_eq!(fs::read(&hardlink_sentinel).unwrap(), encoded);

        fs::remove_file(&paths.shutdown_ack).unwrap();
        lock.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(target_os = "linux")]
    fn assert_no_owner_record_temps(paths: &DaemonPaths) {
        let leaked: Vec<_> = fs::read_dir(&paths.runtime)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with(OWNER_RECORD_TEMP_PREFIX))
            .collect();
        assert!(
            leaked.is_empty(),
            "leaked owner-record temporary entries: {leaked:?}"
        );
    }

    #[test]
    fn shutdown_owner_publication_never_clobbers_another_owner() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let lock = DaemonLock::acquire(&paths).unwrap();
        let first = lock.identity.clone();
        let second = LockIdentity {
            pid: first.pid,
            start_time: first.start_time,
            nonce: "second-owner".to_owned(),
        };

        write_shutdown_request(&paths, &first).unwrap();
        assert!(write_shutdown_request(&paths, &second).is_err());
        let published: LockIdentity =
            serde_json::from_slice(&fs::read(&paths.shutdown_request).unwrap()).unwrap();
        assert_eq!(published, first);

        lock.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn stale_lock_is_reclaimed_but_live_lock_is_not() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let stale = LockIdentity::current().unwrap();
        fs::write(&paths.lock, serde_json::to_vec(&stale).unwrap()).unwrap();
        let reclaimed = acquire_or_detect_live(&paths).await.unwrap().unwrap();
        reclaimed.remove_if_owned();
        let live = DaemonLock::acquire(&paths).unwrap();
        assert!(acquire_or_detect_live(&paths).await.unwrap().is_none());
        live.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn owned_lock_is_unlinked_before_release_and_handoff_preserves_successor() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let owner = DaemonLock::acquire(&paths).unwrap();
        let mut successor = None;

        owner.remove_if_owned_before_release(|| {
            assert!(
                !paths.lock.exists(),
                "owned pathname is gone while the original lock remains held"
            );
            successor = Some(DaemonLock::acquire(&paths).unwrap());
        });

        let successor = successor.expect("successor acquires the unlinked pathname");
        assert!(
            successor.still_owned(),
            "releasing the original unlinked inode cannot remove the successor"
        );
        successor.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn lock_cleanup_preserves_a_non_owner_replacement() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let owner = DaemonLock::acquire(&paths).unwrap();
        fs::remove_file(&paths.lock).unwrap();
        fs::write(&paths.lock, b"replacement owner\n").unwrap();

        owner.remove_if_owned();

        assert_eq!(fs::read(&paths.lock).unwrap(), b"replacement owner\n");
        fs::remove_file(&paths.lock).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn existing_lock_recovery_refuses_symlink_without_touching_target() {
        let root = test_root();
        let outside = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let sentinel = outside.join("sentinel");
        let contents = b"external sentinel contents\n";
        fs::write(&sentinel, contents).unwrap();
        std::os::unix::fs::symlink(&sentinel, &paths.lock).unwrap();

        assert!(acquire_or_detect_live(&paths).await.is_err());
        assert_eq!(fs::read(&sentinel).unwrap(), contents);

        fs::remove_file(&paths.lock).unwrap();
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn existing_lock_recovery_refuses_hardlink_without_touching_target() {
        let root = test_root();
        let outside = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let sentinel = outside.join("sentinel");
        let contents = b"hard-linked external sentinel contents\n";
        fs::write(&sentinel, contents).unwrap();
        fs::hard_link(&sentinel, &paths.lock).unwrap();

        assert!(acquire_or_detect_live(&paths).await.is_err());
        assert_eq!(fs::read(&sentinel).unwrap(), contents);

        fs::remove_file(&paths.lock).unwrap();
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[tokio::test]
    async fn existing_lock_recovery_refuses_non_regular_entry() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        fs::create_dir(&paths.lock).unwrap();

        assert!(acquire_or_detect_live(&paths).await.is_err());
        assert!(paths.lock.is_dir(), "non-regular lock entry is untouched");

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn existing_regular_lock_recovery_repairs_owner_only_mode() {
        use std::os::unix::fs::PermissionsExt;

        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        fs::write(&paths.lock, b"partial lock write").unwrap();
        fs::set_permissions(&paths.lock, std::fs::Permissions::from_mode(0o644)).unwrap();

        let lock = acquire_or_detect_live(&paths).await.unwrap().unwrap();
        assert_eq!(
            fs::metadata(&paths.lock).unwrap().permissions().mode() & 0o777,
            0o600
        );

        lock.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn recovered_detached_lock_does_not_cleanup_successor_runtime() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let stale = LockIdentity {
            pid: 99_999_999,
            start_time: 0,
            nonce: "old-inode".to_owned(),
        };
        fs::write(&paths.lock, serde_json::to_vec(&stale).unwrap()).unwrap();
        let old_inode = open_existing_lock(&paths).unwrap();

        fs::remove_file(&paths.lock).unwrap();
        let successor = DaemonLock::acquire(&paths).unwrap();
        let successor_metadata = DaemonMetadata::new(
            Transport::Tcp,
            "127.0.0.1:1".to_owned(),
            successor.identity.clone(),
        )
        .unwrap();
        write_metadata_atomically(&paths.metadata, &successor_metadata).unwrap();

        assert!(
            acquire_or_detect_live_with_initial_file(&paths, Some(old_inode))
                .await
                .unwrap()
                .is_none(),
            "the detached inode is dropped and acquisition retries against the locked successor"
        );
        assert!(successor.still_owned(), "successor lock remains held");
        assert_eq!(
            read_lock_identity(&paths.lock).unwrap(),
            successor.identity,
            "the current lock pathname remains the successor"
        );
        assert_eq!(
            read_metadata(&paths).unwrap(),
            successor_metadata,
            "successor metadata survives detached-inode recovery"
        );

        fs::remove_file(&paths.metadata).unwrap();
        successor.remove_if_owned();
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
        #[cfg(unix)]
        {
            assert_ne!(fs::read(&paths.lock).unwrap(), b"partial lock write");
            replacement.remove_if_owned();
        }
        // Windows mandatory locking makes the held lock unreadable, which is
        // itself the replaced-in-place proof (the malformed bytes were only
        // readable because nothing held them). Release without removing to
        // verify the rewritten identity, then clean up the pathname.
        #[cfg(not(unix))]
        {
            let error = fs::read(&paths.lock).unwrap_err();
            assert_eq!(
                error.raw_os_error(),
                Some(33),
                "held replacement lock is unreadable through the pathname"
            );
            replacement.release();
            assert_ne!(fs::read(&paths.lock).unwrap(), b"partial lock write");
            fs::remove_file(&paths.lock).unwrap();
        }
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
        )
        .unwrap();
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
    async fn tcp_idle_claim_rejects_valid_auth_without_success_acknowledgement() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let token = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let server = CodeGraphServer::new(code_graph_lang::LanguageRegistry::new());
        assert!(
            server
                .inner
                .persist
                .wait_for_idle_shutdown(Duration::from_millis(1))
                .await,
            "test setup claims idle shutdown before TCP admission"
        );
        let (stop, shutdown) = tokio::sync::oneshot::channel();
        let service = server.clone();
        let server = tokio::spawn(async move {
            serve_tcp(listener, service, token, async move {
                let _ = shutdown.await;
            })
            .await;
        });

        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream
            .write_all(
                b"CG-AUTH 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n",
            )
            .await
            .unwrap();
        let mut acknowledgement = [0_u8; AUTH_ACK.len()];
        assert!(
            matches!(
                timeout(
                    Duration::from_secs(1),
                    stream.read_exact(&mut acknowledgement)
                )
                .await,
                Ok(Err(_))
            ),
            "a valid TCP peer racing an idle claim must close without CG-OK"
        );
        stop.send(()).unwrap();
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
        )
        .unwrap();
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
    fn runtime_files_are_owner_only_for_local_user_exclusion() {
        use std::os::unix::fs::PermissionsExt;

        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let lock = DaemonLock::acquire(&paths).unwrap();
        let metadata = DaemonMetadata::new(
            Transport::Tcp,
            "127.0.0.1:1".to_owned(),
            lock.identity.clone(),
        )
        .unwrap();
        write_metadata_atomically(&paths.metadata, &metadata).unwrap();
        write_shutdown_request(&paths, &lock.identity).unwrap();
        #[cfg(target_os = "linux")]
        write_owner_file_linux(&paths, SHUTDOWN_ACK_FILE, "acknowledgement", &lock.identity)
            .unwrap();
        #[cfg(not(target_os = "linux"))]
        write_shutdown_ack(&paths, &lock.identity).unwrap();

        // This is the strongest Linux-runnable evidence available without
        // manufacturing a second UID: other users cannot traverse the 0700
        // runtime directory, and every sensitive regular file is 0600. It
        // intentionally does not claim a second-UID runtime exercise.
        assert_eq!(
            fs::metadata(&paths.runtime).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let mut sensitive_paths = vec![
            paths.lock.clone(),
            paths.metadata.clone(),
            paths.shutdown_request.clone(),
            paths.shutdown_ack.clone(),
        ];
        #[cfg(target_os = "linux")]
        sensitive_paths.push(paths.runtime.join(SHUTDOWN_CONTROL_LOCK_FILE));
        for path in sensitive_paths {
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600,
                "{} must be owner-only",
                path.display()
            );
        }
        lock.remove_if_owned();
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

    #[cfg(target_os = "linux")]
    #[test]
    fn verified_runtime_handle_keeps_metadata_publication_out_of_a_symlink_replacement() {
        let root = test_root();
        let outside = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let sentinel = outside.join(METADATA_FILE);
        let contents = b"external metadata sentinel\n";
        fs::write(&sentinel, contents).unwrap();
        let displaced = root.join("displaced-runtime");

        fs::rename(&paths.runtime, &displaced).unwrap();
        std::os::unix::fs::symlink(&outside, &paths.runtime).unwrap();

        let metadata = DaemonMetadata::new(
            Transport::Tcp,
            "127.0.0.1:1".to_owned(),
            LockIdentity::current().unwrap(),
        )
        .unwrap();
        write_metadata_atomically_at(&paths, &metadata).unwrap();
        assert_eq!(fs::read(&sentinel).unwrap(), contents);

        fs::remove_file(&paths.runtime).unwrap();
        fs::rename(displaced, &paths.runtime).unwrap();
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn retained_runtime_descriptor_keeps_an_open_lock_out_of_a_replacement_directory() {
        let root = test_root();
        let outside = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let mut lock = DaemonLock::acquire(&paths).unwrap();
        let sentinel = outside.join(LOCK_FILE);
        let contents = b"external lock sentinel\n";
        fs::write(&sentinel, contents).unwrap();
        let displaced = root.join("displaced-runtime");

        fs::rename(&paths.runtime, &displaced).unwrap();
        std::os::unix::fs::symlink(&outside, &paths.runtime).unwrap();

        lock.write_identity(LockIdentity::current().unwrap())
            .unwrap();
        assert_eq!(fs::read(&sentinel).unwrap(), contents);
        lock.remove_if_owned();
        assert!(!displaced.join(LOCK_FILE).exists());
        assert_eq!(fs::read(&sentinel).unwrap(), contents);

        fs::remove_file(&paths.runtime).unwrap();
        fs::rename(displaced, &paths.runtime).unwrap();
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn root_ownership_refuses_a_recreated_runtime_namespace_until_crash_release() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let old = acquire_or_detect_live(&paths).await.unwrap().unwrap();
        let old_metadata = DaemonMetadata::new(
            Transport::Tcp,
            "127.0.0.1:1".to_owned(),
            old.identity.clone(),
        )
        .unwrap();
        write_metadata_atomically_at(&paths, &old_metadata).unwrap();
        let cache = code_graph_graph::cache_path(&root);
        let cache_contents = b"old daemon cache state\n";
        fs::write(&cache, cache_contents).unwrap();

        let displaced = root.join("displaced-runtime");
        fs::rename(&paths.runtime, &displaced).unwrap();
        fs::create_dir(&paths.runtime).unwrap();
        let replacement_paths = DaemonPaths::for_root(&root);
        replacement_paths.ensure_runtime_dir().unwrap();

        assert!(
            acquire_or_detect_live(&replacement_paths)
                .await
                .unwrap()
                .is_none(),
            "the old daemon's root lock rejects a replacement runtime namespace"
        );
        assert_eq!(read_metadata(&paths).unwrap(), old_metadata);
        assert!(
            read_metadata(&replacement_paths).is_err(),
            "a refused contender cannot publish replacement metadata"
        );
        assert!(
            !replacement_paths.lock.exists(),
            "a refused contender does not create a replacement lock"
        );
        assert_eq!(fs::read(&cache).unwrap(), cache_contents);

        // Dropping models a crash: both the detached runtime lock and the root
        // inode lock are released by the kernel, letting exactly one successor
        // establish the replacement namespace.
        drop(old);
        let successor = acquire_or_detect_live(&replacement_paths)
            .await
            .unwrap()
            .unwrap();
        let successor_metadata = DaemonMetadata::new(
            Transport::Tcp,
            "127.0.0.1:2".to_owned(),
            successor.identity.clone(),
        )
        .unwrap();
        write_metadata_atomically_at(&replacement_paths, &successor_metadata).unwrap();
        assert_eq!(
            read_metadata(&replacement_paths).unwrap(),
            successor_metadata
        );
        assert_eq!(fs::read(&cache).unwrap(), cache_contents);

        successor.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn root_ownership_blocks_normal_runtime_lock_replacement_until_release() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let old = acquire_or_detect_live(&paths).await.unwrap().unwrap();

        // The runtime lock handoff normally makes a fresh pathname available
        // before the old descriptor closes. Root ownership keeps that window
        // from becoming a second daemon namespace.
        fs::remove_file(&paths.lock).unwrap();
        assert!(
            acquire_or_detect_live(&paths).await.unwrap().is_none(),
            "the root lock remains authoritative after runtime lock unlink"
        );
        old.release();

        let successor = acquire_or_detect_live(&paths).await.unwrap().unwrap();
        assert!(successor.still_owned());
        successor.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn verified_runtime_descriptor_survives_project_root_substitution_without_touching_external_metadata(
    ) {
        let root = test_root();
        let outside = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let sentinel = outside.join(METADATA_FILE);
        let contents = b"external root substitution sentinel\n";
        fs::write(&sentinel, contents).unwrap();
        let relocated = root.with_extension("relocated");

        fs::rename(&root, &relocated).unwrap();
        std::os::unix::fs::symlink(&outside, &root).unwrap();

        let metadata = DaemonMetadata::new(
            Transport::Tcp,
            "127.0.0.1:1".to_owned(),
            LockIdentity::current().unwrap(),
        )
        .unwrap();
        write_metadata_atomically_at(&paths, &metadata).unwrap();
        assert_eq!(fs::read(&sentinel).unwrap(), contents);

        fs::remove_file(&root).unwrap();
        fs::rename(&relocated, &root).unwrap();
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn absent_runtime_proxy_handle_is_acquired_from_the_verified_root_when_it_appears() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.open_runtime_dir_if_present().unwrap();
        assert!(paths.runtime_dir.get().is_none());

        fs::create_dir(&paths.runtime).unwrap();
        let runtime = paths.runtime_dir_for_child().unwrap();
        assert!(paths.runtime_dir.get().is_some());
        assert!(runtime.file.metadata().unwrap().is_dir());

        fs::remove_dir_all(root).unwrap();
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
        )
        .unwrap();
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

    #[tokio::test]
    async fn locked_owner_discards_dead_metadata_with_a_recycled_tcp_endpoint() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let lock = DaemonLock::acquire(&paths).unwrap();
        let unrelated_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead_owner = LockIdentity {
            pid: 99_999_999,
            start_time: 0,
            nonce: "dead-owner".to_owned(),
        };
        let metadata = DaemonMetadata::new(
            Transport::Tcp,
            unrelated_listener.local_addr().unwrap().to_string(),
            dead_owner,
        )
        .unwrap();

        // Simulate stale metadata arriving after this contender acquired the
        // authoritative OS lock. The accepting TCP listener is unrelated and
        // must not block stale runtime cleanup.
        write_metadata_atomically(&paths.metadata, &metadata).unwrap();
        fs::write(&paths.secret, b"stale token\n").unwrap();
        assert!(prepare_runtime_for_owner(&paths).await);
        assert!(!paths.metadata.exists());
        assert!(!paths.secret.exists());

        drop(unrelated_listener);
        lock.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn locked_owner_clears_metadata_with_a_current_looking_identity() {
        let root = test_root();
        let paths = DaemonPaths::for_root(&root);
        paths.ensure_runtime_dir().unwrap();
        let lock = DaemonLock::acquire(&paths).unwrap();
        let metadata = DaemonMetadata::new(
            Transport::Tcp,
            "127.0.0.1:1".to_owned(),
            lock.identity.clone(),
        )
        .unwrap();
        write_metadata_atomically(&paths.metadata, &metadata).unwrap();
        fs::write(&paths.secret, b"live token\n").unwrap();

        assert!(prepare_runtime_for_owner(&paths).await);
        assert!(!paths.metadata.exists());
        assert!(!paths.secret.exists());

        lock.remove_if_owned();
        fs::remove_dir_all(root).unwrap();
    }
}
