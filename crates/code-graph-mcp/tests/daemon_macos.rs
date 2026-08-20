//! Phase 10.2 macOS daemon coverage stubs.
//!
//! SEAM(phase10-macos): this file is the test-side half of the macOS seam
//! inventory (see `.plans/Plans/GraphPlatformExpansion/notes/10-platform-seams.md`
//! for the full touch-point map). Every test below names one coverage
//! obligation from plan task 10.2 and is deliberately `#[ignore]`d with an
//! `unimplemented!` body: the file parses on every platform, compiles only on
//! macOS, and fails loudly if someone un-ignores a stub without implementing
//! it. A native macOS session implements each stub against the real OS
//! transport and permission behavior — mocked path or socket tests do not
//! satisfy 10.2 (see the task's Notes).
//!
//! Porting guidance: `tests/daemon_serve.rs` and `tests/daemon_proxy.rs`
//! carry the assertion patterns to mirror (their unix arms already run on
//! macOS); the Linux-only suites named per-stub below carry the semantics to
//! reproduce where macOS has no procfd/control-lock equivalent.

#![cfg(target_os = "macos")]

/// 10.2: "Exercise UDS modes [and] owner boundary."
/// Mirror `daemon_serve.rs::serve_publishes_owner_only_ipc_and_serves_mcp`'s
/// unix arm natively: socket file type via `FileTypeExt::is_socket`, socket
/// and secret 0600, runtime dir 0700, MCP round-trip over the UDS endpoint.
#[test]
#[ignore = "phase 10.2: implement on a native macOS runner"]
fn uds_publication_is_owner_only_and_serves_mcp() {
    unimplemented!("phase 10.2 stub — see notes/10-platform-seams.md");
}

/// 10.2 / production seam `DaemonPaths::uds_bind_path` (daemon.rs): a project
/// root deep enough that `<root>/.code-graph/daemon.sock` exceeds the
/// ~104-byte macOS `sun_path` limit must degrade to authenticated loopback
/// TCP with the `eprintln!` breadcrumb, not fail startup. This is macOS's
/// routine landing spot — Linux dodges it via the procfd alias.
#[test]
#[ignore = "phase 10.2: implement on a native macOS runner"]
fn sun_path_overflow_degrades_to_authenticated_tcp() {
    unimplemented!("phase 10.2 stub — see notes/10-platform-seams.md");
}

/// 10.2: "simultaneous attachment." Mirror
/// `daemon_proxy.rs::root_and_nested_clients_share_index_watch_and_async_slot`
/// (already `cfg(any(unix, windows))` — verify it actually passes natively
/// rather than assuming) plus a UDS-specific concurrent-admission check.
#[test]
#[ignore = "phase 10.2: implement on a native macOS runner"]
fn simultaneous_attachment_shares_one_daemon() {
    unimplemented!("phase 10.2 stub — see notes/10-platform-seams.md");
}

/// 10.2: "replacement." Binary-identity mismatch triggers grace → drain →
/// hard-kill with owner revalidation. Mirror
/// `daemon_proxy.rs::clean_binary_mismatch_recovers_a_truncated_request_and_replaces_the_old_owner`,
/// which is Linux-gated because its shutdown-record semantics ride the
/// shutdown.control.lock; macOS uses the portable owner-file arm
/// (`write_owner_file_portable`), so this stub must pin the portable arm's
/// concurrency story natively.
#[test]
#[ignore = "phase 10.2: implement on a native macOS runner"]
fn replacement_protocol_converges_under_portable_owner_files() {
    unimplemented!("phase 10.2 stub — see notes/10-platform-seams.md");
}

/// 10.2: "idle exit/cache reuse." Idle timeout closes admission, saves the
/// cache, removes runtime files; a warm restart reloads the saved graph.
/// Mirror the ungated idle-timeout suite in `daemon_serve.rs` natively and
/// add the macOS-specific check that the final save lands at the pathname
/// (no retained-root alias exists off Linux — see the SEAM comment at the
/// final-save site in daemon.rs).
#[test]
#[ignore = "phase 10.2: implement on a native macOS runner"]
fn idle_exit_persists_cache_and_warm_restart_reuses_it() {
    unimplemented!("phase 10.2 stub — see notes/10-platform-seams.md");
}

/// 10.2: "authenticated TCP fallback." Force the fallback (occupy the socket
/// path, as `daemon_serve.rs::force_tcp`'s unix arm does), then verify the
/// CG-AUTH/CG-OK handshake, constant-time token compare behavior (wrong
/// token → close without ack), secret 0600, and loopback-only endpoint.
#[test]
#[ignore = "phase 10.2: implement on a native macOS runner"]
fn tcp_fallback_authenticates_and_stays_loopback_only() {
    unimplemented!("phase 10.2 stub — see notes/10-platform-seams.md");
}

/// 10.2: "stale/live socket-inode handling." A stale (orphaned) UDS inode is
/// unlinked and rebound; a live listener's inode is retained while the
/// contender degrades to TCP. Mirror daemon.rs unit tests
/// `stale_uds_is_unlinked_and_live_uds_is_not` /
/// `live_uds_inode_is_retained_while_daemon_falls_back_to_tcp` (cfg(unix) —
/// they should compile and run on macOS; this stub is the native-evidence
/// record that they actually did, per the 10.1/10.2 no-inference rule).
#[test]
#[ignore = "phase 10.2: implement on a native macOS runner"]
fn socket_inode_staleness_is_handled_natively() {
    unimplemented!("phase 10.2 stub — see notes/10-platform-seams.md");
}

/// 10.2: "mandatory denial from a separately provisioned local account."
/// Requires a second local macOS account (manual provisioning; cannot be
/// fully automated in CI). The second account must be denied the UDS
/// endpoint (0600/0700), the TCP secret (0600), and therefore the fallback
/// endpoint. NOTE: D-0014 scoped this OUT for Windows phase 11; phase 10's
/// task text predates that decision — reconcile with D-0014 before
/// implementing (either the task drops this bullet per the ledger, or the
/// user explicitly re-expands scope for macOS).
#[test]
#[ignore = "phase 10.2: blocked on a D-0014 scope decision, then a second local account"]
fn second_account_denial_or_d0014_rescope() {
    unimplemented!("phase 10.2 stub — see notes/10-platform-seams.md");
}

/// 10.2 adjunct: the FSEvents watcher backend. The remove-misclassification
/// regression in code-graph-tools `handlers/watch.rs` simulates the event on
/// every platform; this stub runs a REAL watch_start → edit → reindex cycle
/// on the native FSEvents backend (mirror
/// `tests/watch_cpp_macro_strip.rs`'s sentinel-then-discriminator pattern).
#[test]
#[ignore = "phase 10.2: implement on a native macOS runner"]
fn fsevents_watch_reindex_round_trips() {
    unimplemented!("phase 10.2 stub — see notes/10-platform-seams.md");
}
