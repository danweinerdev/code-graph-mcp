---
title: "Perforce VCS provider: p4api -sys crate vs. CLI shell-out"
type: research
status: active
created: 2026-08-20
updated: 2026-08-20
tags: [vcs, perforce, p4, ffi, sys-crate, d-0004]
related:
  - Decisions/decisions.md
  - Specs/GraphPlatformExpansion
---

# Perforce VCS provider: p4api -sys crate vs. CLI shell-out

## Context

The workspace's VCS layer was built provider-agnostic: `code-graph-vcs`
defines `VcsProvider` (async trait, exactly four required ops — `blame`,
`revisions_touching`, `read_at`, `resolve_rev` — plus sync `id`/`detect`),
opaque `RevId`, and a registry with working-tree detection
(`crates/code-graph-vcs/src/lib.rs:113-153`). Only git is implemented
(`code-graph-vcs-git`, pure-Rust gix per D-0004). The user asked to explore a
Perforce provider via a `code-graph-vcs-p4-sys` style crate binding the
publisher's C library. Questions: what exactly does Perforce publish, under
what license, with what linkage requirements, and is a -sys crate the right
shape versus alternatives?

## Findings

### Key Insights

1. **There is no C API — it is a C++ API.** The client library's entry
   points are C++ classes: `class ClientApi : public StrDict` and the
   virtual-callback sink `class ClientUser` (`InputData`, `HandleError`,
   `Message`, `OutputInfo`, `OutputBinary`, `OutputText`, `OutputStat`,
   `Prompt`, `Diff`, `Merge`, …). Command results arrive by overriding
   virtuals — tagged output lands in `OutputStat(StrDict*)`. A Rust `-sys`
   crate therefore cannot be plain `bindgen` over headers; it requires a
   hand-written C++ shim exposing a C ABI (or a `cxx`-crate bridge), plus a
   Rust-side vtable-equivalent for the `ClientUser` callbacks. *(Source:
   `client/clientapi.h:164`, `client/clientuser.h:120-154` in
   p4source-2025.1.2907437, inspected 2026-08-20.)*

2. **The client source IS published, under a BSD-2-Clause-style license.**
   `p4source.tgz` (6.7 MB compressed, 31 MB unpacked, 263 `.cc` + 492 `.h`)
   ships per release at `ftp.perforce.com/perforce/<rel>/bin.tools/`. The
   LICENSE is the classic two-clause "redistribution and use in source and
   binary forms … permitted" text (Copyright 1995-2026 Perforce Software,
   Inc.). Vendoring the source in a crate is legally unproblematic with
   notice preservation. README title: "Perforce Open Source Command Line and
   API". *(Source: p4source-2025.1.2907437/LICENSE + README.md, fetched from
   https://ftp.perforce.com/perforce/r25.1/bin.tools/p4source.tgz,
   2026-08-20.)*

3. **The source tree is heavy and toolchain-hostile for a cc-rs build.**
   Modules: `api, blake3, client, diff, dme, dmec, i18n, map, misc, msgs,
   net, rpc, script, sslstub, support, sys, web, zlib`. It vendors its own
   zlib (with SIMD variants), BLAKE3 (with `.S`/`.asm` per-platform
   assembly), and an embedded Lua 5.3 (`script/` — the `p4script` extension
   runtime, stubbable via `clientscript_stub.cc`). Build system is **Jam**,
   not make/cmake; the README documents `-sSMARTHEAP=0` for Windows/Linux
   and OS-version probing. A `p4api-sys` building from source must
   re-derive the file set and defines (e.g. `-DOS_NT`) that Jam computes
   per platform. C++ dialect is old (Jamrules references `-std=c++98`
   heritage; boost variadic templates explicitly disabled) — compilable by
   modern compilers, but ~300 translation units of C++ plus C plus
   assembly is a different order of build than the six self-contained
   tree-sitter `parser.c` archives the workspace compiles today. *(Source:
   tree listing + Jamfile/Jamrules inspection, 2026-08-20.)*

4. **OpenSSL is mandatory for real deployments; the escape hatch is a
   stub.** Since 2017.1 the P4API links OpenSSL by default. The tree ships
   `sslstub/` — no-op implementations of the OpenSSL symbols — so a
   non-SSL build links clean, but such a client cannot connect to `ssl:`
   servers, which is how production Helix Core is typically deployed. So a
   from-source -sys crate faces a fork: link real OpenSSL (external native
   library — exactly what D-0004 exists to prevent, plus the
   openssl-on-Windows toolchain swamp) or ship a client that fails against
   SSL servers. *(Source: README.md build docs + `sslstub/sslstub.cc`,
   2026-08-20.)*

5. **Prebuilt binaries exist but are a per-toolchain matrix.** Windows:
   `p4api_vs2005…vs2022 × static/dyn × vsdebug × openssl1.1.1/3` (88 zips
   on r25.1/bin.ntx64 alone) plus mingw64 variants; Linux: per-glibc
   (2.3/2.12) × OpenSSL; macOS 12+ universal. Binary-only distribution is
   governed by Perforce's EULA rather than the BSD source license, and a
   build.rs that downloads-or-locates the right zip for the host MSVC
   version is the classic fragile-sys-crate antipattern. *(Source:
   https://ftp.perforce.com/perforce/r25.1/bin.ntx64/ and
   …/bin.linux26x86_64/ listings, 2026-08-20.)*

6. **Perforce's own precedent for wrapping is a per-language C++ shim over
   the prebuilt libs.** P4Go (bin.tools/p4go.tgz, ~70 KB of source) is
   cgo + hand-written shim (`p4go.cpp`, `p4goclientuser.cpp`, …) linking
   `-lp4api -lssl -lcrypto` plus platform extras (Windows: `crypt32
   ws2_32 ole32 shell32 user32 advapi32`; macOS: `ApplicationServices,
   Foundation, Security` frameworks). The user must download the matching
   p4api and OpenSSL themselves and point build flags at them. P4Python /
   P4Ruby / P4Perl follow the same pattern. This is what a
   `code-graph-vcs-p4-sys` would look like — and the dependency UX it
   would inherit. *(Source: p4go-2025.1.2786684/README.md, 2026-08-20.)*

7. **No existing Rust binding to build on.** crates.io search "perforce"
   (2026-08-20, 18 results): only CLI wrappers — `p4cli-20251` (active
   2025-2026, auto-detects system `p4` or downloads the official binary,
   platform sub-crates) and `p4-cmd` (2018, one release, stale). Nothing
   binds the C++ API. An absence claim: searched crates.io API
   `?q=perforce` and `?q=p4`; no `-sys` crate exists.

8. **The four `VcsProvider` ops map cleanly onto four p4 commands.**
   - `resolve_rev(spec)` → `p4 changes -m1 <spec>` (changelist number as
     `RevId`; specs like `@2026/08/01`, `#head`, plain CL numbers).
   - `revisions_touching(path, limit)` → `p4 filelog -m <limit> -t <path>`
     (per-file history; follows renames only via `-i` integrations —
     policy choice mirrors git provider's no-rename-follow D-0005 stance).
   - `read_at(rev, path)` → `p4 print -q <path>@<CL>`.
   - `blame(rev, path, range)` → `p4 annotate -c -q <path>@<CL>` gives the
     introducing changelist per line; author + timestamp require a join
     with `p4 changes`/`describe -s` output (annotate alone reports CLs,
     `-u` adds user/date). `-I` optionally follows integrations — richer
     than git blame for branch-heavy (UE-style) depots.
   All are read-only and line-oriented; the CLI's `-G` flag emits Python
   marshal dicts (stable, documented) and `-ztag` field output parses
   trivially. *(Source: p4 command reference knowledge; trait surface
   verified against `crates/code-graph-vcs/src/lib.rs`, 2026-08-20.)*

9. **Detection differs from git in kind: it needs config, and truth needs
   a server.** There is no `.git`-equivalent directory. Working-tree
   detection = `P4CONFIG` file upward walk (`.p4config`) / `P4CLIENT`+
   `P4PORT` env / `p4 info` round-trip; definitive client-root mapping
   requires talking to the server. `detect()` must stay offline-cheap
   (config presence only) and let the four ops surface connectivity
   errors — which the tool layer already renders as success-shaped
   unavailability (`available: false` + `reason`, FR-36).

### Sources

- p4source-2025.1.2907437 (LICENSE, README.md, Jamrules, client/, sslstub/,
  script/, zlib/, blake3/) — https://ftp.perforce.com/perforce/r25.1/bin.tools/p4source.tgz, fetched 2026-08-20.
- p4go-2025.1.2786684 (README.md build flags, shim file layout) — same host, bin.tools/p4go.tgz, fetched 2026-08-20.
- Prebuilt matrix listings — https://ftp.perforce.com/perforce/r25.1/bin.ntx64/, …/bin.linux26x86_64/, …/ (root), fetched 2026-08-20.
- crates.io search API `?q=perforce` — fetched 2026-08-20 (18 results, none binding p4api).
- Workspace: `crates/code-graph-vcs/src/lib.rs` (trait), CLAUDE.md D-0004 statement, `crates/code-graph-vcs-git` (provider precedent).

## Analysis

### Implications

- **A true `code-graph-vcs-p4-sys` is feasible but is the workspace's
  worst-case dependency shape.** Both variants break the D-0004 boundary
  as CLAUDE.md states it ("nothing links against an external library"):
  prebuilt-lib linkage imports the MSVC-version matrix, OpenSSL, and EULA
  redistribution questions; vendored-source build imports ~300 C++ TUs +
  zlib + BLAKE3 assembly + Lua under a hand-replicated Jam configuration,
  and STILL needs real OpenSSL to talk to `ssl:` servers (the sslstub
  build is a lab toy for this use case). Either way the C++-only surface
  forces a shim layer (cxx or extern-C wrapper) with a `ClientUser`
  vtable bridge — the exact piece Perforce hand-writes per language.
- **A CLI provider (`code-graph-vcs-p4`, spawning `p4 -G`/`-ztag`) does
  not violate D-0004** — a spawned subprocess links nothing into our
  binary (same reasoning as the git-CLI *test fixture harness* already in
  the workspace, and the daemon's captured `icacls`/`whoami` calls on
  Windows). It fits the trait's `spawn_blocking` shape, works against
  `ssl:` servers for free (the user's own p4 handles TLS + tickets), and
  its per-call process overhead (~tens of ms) is congruent with
  blame/history call rates (these tools are already seconds-scale through
  the git provider on large files).
- **The wire contract needs zero changes.** `RevId` is opaque (changelist
  numbers fit), unavailability is success-shaped, and the fingerprint
  cache is keyed on provider+revision — a second provider slots into the
  registry with no handler edits. This was the design intent and it holds.
- **Auth/session is the real product risk for any variant, not the
  bindings.** Tickets expire (`p4 login`), servers are remote (latency on
  `annotate` of large files), and Windows Unreal shops often run
  case-insensitive servers with mixed path casing — the provider's
  belongs-to-this-working-tree check (mirroring the git provider's
  submodule guard) has to be built on `p4 where` semantics, not string
  prefixing.

### Recommendations

1. **Do not start with a -sys crate.** Build `code-graph-vcs-p4` as a CLI
   provider first: `detect()` = P4CONFIG/env presence; four ops =
   `changes`/`filelog`/`print`/`annotate` via captured-output subprocess
   with `-G` marshal (or `-ztag`) parsing; all blocking calls inside
   `spawn_blocking`, injected by the binary alongside git. This ships
   Perforce support with zero build-system risk and no D-0004 exception.
2. **Record the decision before implementing** (ledger entry): provider
   strategy = subprocess CLI, D-0004 untouched (spawn ≠ link), with the
   -sys route explicitly named as rejected-for-now and the reasons (C++-
   only API, OpenSSL linkage, toolchain matrix, EULA on prebuilt libs).
3. **If native bindings ever become necessary** (latency at scale, or
   long-lived-connection reuse), the least-bad shape is vendored-source +
   `cxx` shim + real OpenSSL via `openssl-sys`, structured exactly like
   P4Go's shim, behind an off-by-default cargo feature and an explicit
   D-0004 supersession in the ledger. Budget it as a multi-week build
   engineering task, not a bindgen afternoon.
4. **Prototype scope for the CLI provider:** blame + filelog + print
   against a local `p4d` fixture (p4d is a single binary; a hermetic
   test harness mirroring `code-graph-vcs-git`'s git-CLI fixture pattern
   is straightforward), plus a ticket-expired error-mapping test.

## Open Questions

- `p4 annotate` cost on very large files against a remote server — is
  per-call latency acceptable for `symbol_history`'s up-to-500-revision
  walk, or does the provider need result caching beyond the existing
  fingerprint sidecar? (The window walk multiplies `read_at` calls, not
  `annotate` calls, so `print` throughput matters more.)
- Changelist-vs-revision granularity: `revisions_touching` wants per-file
  touch points; `p4 filelog` gives file revisions (`#n`) each tied to a
  CL. Use CLs as `RevId` uniformly, or file revisions? (CLs recommended —
  they align with blame's `-c` output and `describe` joins.)
- Case-insensitive server + Windows client path normalization: does
  `normalize_user_path` + `p4 where` round-trip suffice, or does the
  provider need its own casing canonicalization for depot paths?
- Does the sandbox/CI environment for tests ship `p4`/`p4d`, or does the
  fixture harness download them (p4cli-20251 precedent) — and is that
  acceptable for hermetic CI?
- Stream depots and workspaces with non-trivial view mappings: `p4 where`
  handles the mapping, but exclusionary lines (`-//depot/...`) need a
  belongs-to-tree test fixture.
