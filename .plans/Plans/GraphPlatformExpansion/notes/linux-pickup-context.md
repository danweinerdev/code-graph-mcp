# Linux pickup context — post-phase-12 session handoff

Prepared 2026-08-25 on the Windows host at the end of the session that
completed phase 12, closed KNOWN_ISSUES F2/F3, and produced the F6 WSL2
finding. Written for a COLD START on a Linux system: everything you need to
know is here or pointed at from here; nothing assumes the prior conversation.
Commit hashes are durable anchors; line numbers are avoided.

## 1. Where the repo stands

Branch: `feature/shared-process`, clean tree, HEAD = `6dc7bf1`
("docs(plan): mark phase 12 complete"). Not pushed. Everything below is on
this branch.

- **Phase 12 (Accepted Technical Debt Closure) is COMPLETE** via the
  evidence-gated `sdd phase complete` transition: 11/11 tasks with recorded
  checkpoints, 10/10 acceptance criteria checked (AC-8 as amended — see §4),
  Phase Completion Evidence recorded, four-lane frozen gate resolved
  **Aligned** at `reviews/24-graphplatformexpansion-code-review-f9ca2ca.md`
  (frozen code range `0b41bbd..f9ca2ca`, reviewed planning revision
  `162cdd2`). Artifact 23 at the same range is the first persisted
  resolution, superseded (never edited) by 24 after legitimate lifecycle
  bookkeeping — the supersession note in the phase doc's evidence section
  explains it.
- `sdd validate --scope .plans/Plans/GraphPlatformExpansion` → Valid.
  `sdd next` → nothing actionable (only phase 10 macOS remains, deferred).
- **`make verify` on native Windows is fully green at the final candidate**
  (clippy `-D warnings`, fmt, all workspace tests incl. the eight dogfood
  baselines with submodules initialized, snapshots, plugin mirrors).

### The final candidate vs. HEAD

The phase's reviewed CODE identity ends at `f9ca2ca`. Everything after it is
docs/plan bookkeeping **except one production change**: `622949a`
(`scripts/sync-plugin-skills.sh --check` now exits 2 with an install hint
when `diff` is missing, instead of misreporting mirror staleness — found via
a diffutils-less WSL image). Treat `622949a` as ordinary post-phase repo
work; it is deliberately outside the frozen review.

## 2. What landed this session (newest last)

| Commit | What |
|---|---|
| `32e5b9a` | Windows/CI fixes: unix-gated daemon test helper (clippy dead-code), go_resolution path separators, **`canonicalize_allowing_missing`** in code-graph-vcs-git (8.3 short-form convergence for missing paths), typed_core_consumer license |
| `85fd1d6` | OpenCode plugin: commands-directory registration |
| `1b833e4` | vcs-git pins for `canonicalize_allowing_missing` (incl. derived 8.3 short form via `cmd %~s` + `raw_arg`) |
| `5725dc2` | Task 12.8 Windows pins: NTFS casing, icacls-saved SDDL exact-SID, bounded shutdown diagnostics, `make dogfood-required` |
| `cd4e19b` | KNOWN_ISSUES **F3** first step: terminal `@bound-receiver::` markers discarded via new `LanguagePlugin::discard_unresolved_call` |
| `68abe00` | KNOWN_ISSUES **F2** Option B: parse-only `CallShape` (Free/SelfReceiver/Receiver) gates the resolver's sole-candidate shortcut; receiver-typed sole-candidate picks are now `Heuristic/1`; **no CACHE_VERSION bump** (v13 unreleased — decision 2026-08-24) |
| `834520d` | Research: Option C (receiver type inference) cache/RAM cost model — memory is a non-blocker (~+3-4% cache interned); land its cache shape before v13 ships |
| `69bb87c` | **Owner's Linux run**: fixed the one Linux-only clippy break (test import rescoped into its `#[cfg(windows)]` test) |
| `f2d001d`..`f9ca2ca` | Five review-gate repair commits (see §3): D-0007 doc surfaces reconciled, Python/Java/C++ receiver-verification hardening, full pin coverage |
| `622949a` | sync script fails honestly without `diff` |
| `2f4d51c` | KNOWN_ISSUES **F6** recorded (see §5) |
| rest | plan/README/review lifecycle bookkeeping through `6dc7bf1` |

## 3. The F2 resolver hardening — what you need to know cold

`Resolved` on a call edge now means VERIFIED; `Heuristic` covers both
multi-candidate scope-rule picks AND receiver-typed sole-candidate guesses
(`Heuristic/1`). `candidates: 1` no longer implies `Resolved`. The gate is
the parse-only `Edge.shape` (`CallShape`) consumed by
`default_scope_aware_resolve`; nothing rides the cache (v13 layout
untouched). Five gate cycles drove per-language correctness:

- Python: `self`/`cls` verify only as the enclosing function's FIRST
  parameter, with `@staticmethod`/splat rejection and a rebinding scan
  covering assignment/walrus/for/as-patterns/match/imports/del/type-alias/
  nested defs/lambdas; bare calls to parameter- or body-bound callables
  degrade to Receiver.
- Java: static-import-bound unqualified calls and non-`this` method
  references degrade; `this::x` verifies.
- C++: SelfReceiver compares the caller ID's FULL parent
  (`caller_id_full_parent`, matching `split_qualified`'s rfind); the
  inline-vs-out-of-line nested-parent mismatch remains a documented
  conservative downgrade.
- Accepted residuals (documented in KNOWN_ISSUES F2): callable-value
  parameters in Rust/C++/C#/Java still classify Free (accepted pending
  Option C); C++ implicit-`this` decision; module-scope rebound callables
  in Python.

Every claim above is pinned: `f2_*` tests in code-graph-lang and the five
parser crates, `receiver_shape_resolution.rs` end-to-end, plus regenerated
tools_list snapshots. The quality lane independently probe-verified the
Python fixes 11/11 against the production `parse_file` entry.

## 4. What a Linux system should verify (the actionable list)

### 4a. Full gate at HEAD — upgrades AC-8 from "amended" to unqualified

```
git clone <repo> && cd code-graph-mcp
git checkout 6dc7bf1        # or feature/shared-process HEAD
make verify                 # needs a C compiler (grammar crates) + diffutils
```

Expected: fully green. If it is, the phase-12 AC-8 amendment (phase doc,
Acceptance Criteria, the bracketed 2026-08-25 note) can be superseded by an
unqualified native-Linux evidence row — add a `make verify (native Linux)`
row to the phase's Phase Completion Evidence table. NOTE the SDD174
constraint before touching any INTENT text (AC wording, Notes prose, README
row prose): evidence-table rows, checkbox state, and lifecycle frontmatter
are lifecycle-normalized (safe to add post-review); intent-text changes
require re-running the four-lane review at a new planning revision (that is
exactly why review 24 superseded 23). Adding an evidence row is safe;
rewording AC-8 is not — prefer the evidence row.

Also worthwhile on Linux: `make submodules && make dogfood-required`
(8 baselines; auto-skip is promoted to failure), never yet run on Linux.

### 4b. F6 — confirm the classification (one command)

```
cargo test -p code-graph-mcp --bin code-graph-mcp run_until_uses_the_idle_future
```

Expected on a real (non-WSL) kernel: PASSES. If it does, F6's
"environmental" classification is confirmed — update the F6 entry's
follow-up 1 in `.plans/KNOWN_ISSUES.md` with the kernel/distro evidence.
If it FAILS on real Linux, the classification is WRONG: it is a genuine
Linux bug in the daemon idle path and becomes a product defect —
re-open with the diagnostics below as the starting map.

### 4c. F6 — identify the mechanism (needs strace, ~15 minutes)

Read `.plans/KNOWN_ISSUES.md` §F6 first; it is the condensed form of this:

- The failing test livelocks ONLY on WSL2 (Fedora, kernel
  `6.18.33.2-microsoft-standard-WSL2`): the daemon starts cleanly, the
  serve select is polled exactly twice, then the runtime thread spins at
  100% CPU in USERSPACE (wchan=0, empty /proc syscall) with zero further
  task polls and zero timer deliveries — 20ms/50ms/50ms timers all dead —
  yet the test's OUTER 5s `tokio::time::timeout` fires (late, 5.25-5.5s).
  A 5000ms timer delivered while a 20ms timer on the same runtime is not.
- Ruled out empirically (each by direct experiment): phase-12 regression
  (reproduces at range start `0b41bbd`), tokio version (1.52.1 == 1.53.1),
  current-thread starvation (multi_thread flavor fails identically), the
  procfd-aliased UDS listener alone, the nested select/loop shape alone,
  the ownership watchdog, the shutdown-request poller, the idle machinery
  (a bare `sleep(20ms)` substituted in ALSO never fires), the Linux
  retained-root/procfd machinery. Every component passes in isolation;
  every bisect of the composition still fails.
- To finish: `strace -f -o /tmp/tr.txt <test binary> run_until_uses_the_idle_future`
  during the 5s window, find the storming fd or spinning syscall pattern,
  then file upstream (microsoft/WSL if kernel epoll/timer delivery;
  tokio if it mishandles legal kernel behavior). The bisect matrix above
  is most of the reproduction writeup.
- A warm diagnostic clone survives on the Windows host's WSL at
  `~/cg-verify` (rustup 1.98 installed, target/ built). The WSL image
  lacks strace/gdb/diffutils and sudo needs a password — that is what
  stopped the diagnosis. `dnf install strace diffutils` unblocks it.

### 4d. sdd (claude-sdd-planner) — the checker fixes

Separate repo, `D:/devel/git/upstreams/claude-sdd-planner` on the Windows
host, branch `main`, commit `1338cf2` ("fix(rules): tolerate conventional
identity-section layout in SDD157/SDD158"): the completed-identity checks
rejected the layout the tool's own workflow produces (the appended
`Final aligned review` line, blank separators, annotated entries, annotated
checkpoint values) — 8 of 11 completed phases in THIS repo failed SDD157
under the unfixed binary. Full `make test` green there. The repo had other
uncommitted user work which was deliberately left untouched. Not pushed.
If you rebuild sdd on Linux, `go install ./cmd/sdd` from that commit or
later — an older binary will re-report the 8 false SDD157 findings here.
One genuine finding it surfaced was fixed in THIS repo: task 11.3's
identity checkpoint (`c135bc3`).

## 5. Known-issues ledger state (`.plans/KNOWN_ISSUES.md`)

| Entry | State |
|---|---|
| F2 | FIXED (`68abe00` + gate repairs); accepted residuals documented; Option C is the long-term direction with its cost model in `Research/option-c-type-inference-memory-impact.md` (memory non-blocker; decide its cache shape BEFORE v13 ships to users — no version bump exists yet) |
| F3 | First step done (`cd4e19b`); remaining: measure marker counts on a large Go repo before any broader compaction |
| F4/F5 | Deferred by design (Go manifest metadata; benchmark-gated; fold together) |
| F6 | Open — §4b/§4c above ARE its follow-ups |

## 6. Traps for the next session

- **sdd section set** replaces only up to the FIRST subsection heading —
  it duplicated F2/F3 in KNOWN_ISSUES and the 12.8 section in the phase doc
  before this was understood (both since repaired). Prefer `sdd apply`
  (needs `--type <kind>`; strip tool-owned frontmatter keys) or targeted
  edits for section bodies containing `###` subsections.
- **SDD157's entry grammar** (after the fix): every non-blank line in
  `### Completed task identities` must be ``- `<id>`: `<full40>` `` with an
  optional trailing `(...)`, or the `Final aligned review` line; each
  checkpoint must equal the task's recorded `Revision / checkpoint`
  (leading backticked token when annotated). Prose paragraphs must live
  ABOVE the heading.
- **SDD173/SDD174**: the completion gate verifies the COMMITTED copy at
  HEAD and requires a clean worktree; the review's planning revision must
  cover all current intent text (see §4a).
- **Windows-only test knowledge** that cost time this session: Windows
  `cmd` needs `raw_arg` (std's MSVC quoting breaks it); `ends_with` on
  symbol IDs needs separator normalization; missing-path canonicalization
  falls back to the nearest existing ancestor (`canonicalize_allowing_missing`).
- The model-router relay for review-lane subagents intermittently echoed
  placeholder prompts; retries eventually went through. Lane evidence
  source texts live under the orchestrator temp dir and inside the
  resolved review artifacts.

## 7. Quick command reference (Linux)

```
make verify                      # full gate: clippy -D warnings, fmt, tests, snapshots, mirrors
make submodules                  # init the 8 dogfood pins (~shallow clones)
make dogfood-required            # baselines with auto-skip promoted to failure
cargo test -p code-graph-tools --test receiver_shape_resolution   # F2 end-to-end pin
cargo test -p code-graph-lang-python f2_                          # Python receiver pins (5)
cargo test -p code-graph-mcp --bin code-graph-mcp run_until_uses_the_idle_future  # F6 probe
sdd validate --scope .plans/Plans/GraphPlatformExpansion          # plan health
```
