# Phase 10 platform seams — macOS/Linux touch-point map

Prepared 2026-08-20 on the Windows host, ahead of phase 10 (deferred, needs
native macOS hardware) and the outstanding Linux `make verify` re-run. This is
the exact work surface a future macOS or Linux session builds on. Line numbers
are as of the commit that adds this file and will rot; the durable anchors are
the function names and the greppable markers below.

## How to find every touch point

```
rg -n "SEAM\(phase10-macos\)" crates/          # in-code decision points (9 sites)
rg -n "SEAM\(linux-proctitle\)" crates/        # Linux process-title seam (1 site)
rg -n 'cfg\(target_os = "linux"\)' crates/     # Linux-only mechanisms macOS lacks
rg -n 'cfg\(unix\)' crates/                    # shared POSIX arms macOS inherits
```

The stub test harness is `crates/code-graph-mcp/tests/daemon_macos.rs`
(`#![cfg(target_os = "macos")]`, all `#[ignore]`, one stub per 10.2 coverage
obligation). It parses on every platform, compiles only on macOS, and each
stub's doc comment names the existing suite whose assertions it mirrors.

## The three-tier platform model (read this first)

The daemon's platform code is NOT unix-vs-windows. It is three tiers:

1. **Linux** — full capability model: procfd aliases, retained descriptors,
   root-inode flock ownership, ownership watchdog, serialized owner-file
   exchange under `shutdown.control.lock`, crash-recovery temp scavengers.
2. **Generic unix (= macOS today)** — the `#[cfg(unix)]` arms minus every
   `#[cfg(target_os = "linux")]` mechanism: `openat` capability opens, nlink
   defenses, 0600/0700 permission hygiene, UDS transport, descriptor-based
   lock semantics — but pathname-trust wherever Linux uses a retained inode.
3. **Windows** — named pipes, mandatory file locks, icacls DACLs (phase 11,
   certified).

Phase 10.1's core question at every seam: does macOS accept tier-2 semantics
(document it), or does it need a tier-1 equivalent built from macOS
primitives (kqueue, `F_GETPATH`, `$TMPDIR` sockets)?

## In-code SEAM markers (the 9 macOS decision points)

All in production code, greppable via `SEAM(phase10-macos)`:

| # | Site | File / anchor | What macOS gets today | Phase 10 decision |
|---|---|---|---|---|
| 1 | `DaemonPaths::uds_bind_path` | `crates/code-graph-mcp/src/daemon.rs` | Raw socket path; deep checkouts (> ~104-byte `sun_path`) fail the bind | Accept TCP degrade, or add a `$TMPDIR` short-path socket strategy |
| 2 | `bind_listener` `UdsBind::Unavailable` arm | same file | Loopback-TCP degrade with eprintln breadcrumb — macOS's routine landing spot | Measure real-checkout frequency; keep or fix (CLAUDE.md "revisit in Phase 10") |
| 3 | `refresh_proxy_paths` non-linux arm | same file | No runtime-namespace divergence detection; proxy keeps original paths | dev/ino re-stat equivalent, or pathname-trust documented |
| 4 | `proxy_namespace_is_current` non-linux arm | same file | Always `true` | Follows #3 |
| 5 | Ownership watchdog wiring in `run` | same file | `future::pending()` — replaced project root undetected while serving | kqueue/re-stat watchdog, or document the gap |
| 6 | Final cache save, non-linux `None` arm in `run` | same file | Pathname-based save; a root replaced mid-drain writes into the replacement | Exercise and accept, or anchor |
| 7 | `write_owner_file` non-linux dispatch | same file | Portable `O_EXCL`-create arm, not Linux's serialized temp+rename exchange | Pin the portable arm's concurrency story natively |
| 8 | `ServerInner::ensure_daemon_root_current` non-linux arm (+ the linux-only unanchored-save refusal in `core/analyze.rs::save_cache`) | `crates/code-graph-tools/src/server.rs`, `crates/code-graph-tools/src/core/analyze.rs` | Unconditional `Ok(())` — replaced daemon root not detected before publish; saves always pathname-based | dev/ino comparison (the `MetadataExt` APIs exist under `cfg(unix)`), or pathname-trust documented |
| 9 | `set_process_listing_identity` (daemon startup) | `crates/code-graph-mcp/src/daemon.rs` | No-op on Windows/macOS — process-listing identity there rides only in argv (`--serve <root>`), the Windows posture. **Linux done**: `set_process_listing_identity_linux` sets comm via `prctl(PR_SET_NAME, "code-graph-d")`, verified against real `ps -o comm`/`/proc/<pid>/comm` output | macOS has no supported setproctitle; accept argv-only identity or ship a renamed helper binary. cmdline rewrite (`ps -o args`/`/proc/<pid>/cmdline`) is intentionally NOT done on Linux — it needs pre-runtime argv-buffer capture, meaningfully riskier than the comm rename, left as a distinct future seam if wanted |

## Linux-only mechanisms with no macOS counterpart (tier-1 inventory)

All in `crates/code-graph-mcp/src/daemon.rs` unless noted. A macOS session
does NOT need to port these one-for-one — it needs a deliberate
accept-or-replace decision per row (the SEAM markers above are where those
decisions land in code):

- `/proc/<pid>/fd/…` aliases: `uds_alias` (socket bind past sun_path),
  `root_io_alias` (cache I/O anchored to retained root inode),
  `retained_root_metadata`.
- `shutdown.control.lock` publisher serialization: `acquire_shutdown_control_lock`,
  `ShutdownControlLock`, control-lock steps in shutdown request/ack/clear.
- Retained root-inode flock ownership: `RuntimeDir.root`,
  `open_root_for_ownership`, `acquire_root_ownership`, `DaemonLock.root_file`,
  `with_root_ownership`/`has_root_ownership`.
- Ownership-path watchdog: `OwnershipPathChange`, `ownership_path_watchdog`,
  `proxy_capability_diverged`.
- Crash-recovery scavengers: `scavenge_owner_record_temps(_locked)`,
  `scavenge_metadata_temps`, temp-name validators.
- Serialized owner-file exchange: `write_owner_file_linux` and helpers.
- `code-graph-tools`: `DaemonRootIdentity` (dev/ino), `bind_daemon_retained_root`,
  linux-gated unanchored-save refusal in `core/analyze.rs`.

## Shared `cfg(unix)` arms macOS inherits (tier-2, compile-first surface)

These should compile and run on macOS unchanged; 10.1 verifies rather than
assumes. Highest-risk items first:

- UDS machinery: `bind_uds`, `secure_uds_listener` (0600 + inode-swap guard),
  `socket_inode` (`FileTypeExt`), `remove_orphan_socket`, `serve_uds`
  (CG-OK admission prelude), `ClientStream::Uds`.
- `openat`-based runtime dir: `establish_runtime_dir` unix arm (NOFOLLOW,
  inode compare, fchmod 0700 — uses `rustix::fs`; rustix supports macOS but
  10.1 confirms these exact flag combinations), `open_child`/`remove_child`/
  `rename_child`, `read_bounded_record_from` (`O_NOFOLLOW|O_NONBLOCK`),
  nlink != 1 rejections.
- `DaemonLock` descriptor semantics: `acquire` (0600 + `O_CREAT|O_EXCL`),
  `still_owned` (descriptor-vs-path inode), `remove_if_owned`
  (unlink-before-release handoff), `opened_lock_matches_path`,
  `open_existing_lock`, `recover_lock` unix arms.
- Secret/metadata writes: `write_secret_at`, `write_metadata_atomically_at`
  unix descriptor versions, 0600 modes.
- Signal-based `stop_daemon` test helpers (SIGINT paths in `daemon_proxy.rs`,
  `daemon_serve.rs`, `code-graph-cli/tests/cli.rs`).

## Platform-gated tests: who runs what

- **Linux-only** (`cfg(target_os = "linux")`) — will NOT run on macOS; their
  semantics are the reference for SEAM decisions: ~26 daemon.rs unit tests
  (oversized-record bounds, procfd aliases, watchdog, control-lock,
  scavenging, root-ownership), 3 `daemon_proxy.rs` process tests
  (`saturated_uds_attachment…`, `clean_binary_mismatch…`,
  `root_replacement_during_admitted_persist…`), 2 code-graph-tools tests
  (`watch_start_rejects_a_replaced_daemon_root`,
  `reindex_rejects_a_replaced_daemon_root_without_mutating_graph`).
- **`cfg(unix)` — run on BOTH Linux and macOS**: ~17 daemon.rs unit tests
  (symlink/hardlink/lock-recovery/UDS/permission suites), the unix
  `tcp_fallback_metadata_is_loopback_only`, tools `walk_warnings_surface`,
  `index_directory_surfaces_read_errors_as_warnings`,
  `file_symbols_through_symlink_requires_canonical_path` (doc comment already
  claims Linux+macOS coverage), graph persist's 3 tmp-link tests.
- **`cfg(any(unix, windows))`**: all of `daemon_proxy.rs`; `daemon_serve.rs`
  and `cli.rs` run everywhere via per-OS helper arms (`stop_daemon`,
  `ipc_connect`, `force_tcp`).
- **Windows-only** (already certified, phase 11): DACL check, verbatim-prefix
  strips, pipe-occupancy TCP fallback, path-trie drive-letter keys.
- **macOS stubs**: `crates/code-graph-mcp/tests/daemon_macos.rs` — 9 ignored
  stubs mapping 1:1 to task 10.2's verification bullets.

## Task mapping

- **10.1 (compile + repair)**: `rustup` + C toolchain on the mac; `make verify`;
  expect the tier-2 list above to be the failure surface if anything breaks.
  Repairs land AT the SEAM markers, never by deleting a linux gate (the
  phase's Linux-suite AC).
- **10.2 (daemon runtime)**: implement the 9 stubs in `daemon_macos.rs`.
  One stub (`second_account_denial_or_d0014_rescope`) is blocked on a D-0014
  scope decision — the task text predates the ledger entry; reconcile before
  implementing.
- **10.3 (parity matrix)**: pattern exists — mirror
  `notes/11-windows-certification-matrix.md`.

## Linux touch points (the other outstanding platform item)

Linux code is complete; the outstanding work is verification, not stubbing:

1. Run `make verify` on a Linux host at (or after) this commit — the standing
   follow-up from gate artifacts 21/22. Everything since `ccd9e11` (the last
   known Linux-green revision) has only been verified natively on Windows.
2. The Linux-only suites listed above are the regression net for any phase 10
   repair — the phase 10 AC requires a post-repair Linux run (or an explicit
   deferral naming it).
3. No Linux code stubs are needed: every `SEAM(phase10-macos)` site keeps its
   linux arm untouched, and `daemon_macos.rs` is cfg'd out of Linux builds
   entirely (parse-only).
