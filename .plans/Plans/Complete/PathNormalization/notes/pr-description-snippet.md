# PR description snippet — CI-coverage caveat

Append this section to the PR description for any PR carrying changes from `PathNormalization` (whether one bundled PR for the full plan or one per phase). The snippet must live in the PR **body**, not the commit message — commit messages are easy to lose to history fragmentation; reviewers and future archaeologists read the PR body.

## Suggested PR-body section

```markdown
## CI-coverage caveat

This change targets Windows-only behavior (`\\?\` extended-path prefix). The
existing CI matrix is Linux-only; `dunce::simplified` is documented to be a
no-op on non-Windows targets, so the Linux test suite cannot directly exercise
the strip logic.

**Load-bearing automated checks:**

- `#[cfg(windows)]`-gated unit tests in `crates/code-graph-core/src/paths.rs`
  (`simplify_strips_extended_disk_prefix`, `simplify_leaves_verbatim_unc_unchanged`)
  run only when a developer/contributor invokes `cargo test` on a Windows host.
- `#[cfg(windows)]` ground-truth assertions inside `Graph::load` cache-migration
  tests in `crates/code-graph-graph/src/persist.rs::tests` validate that
  `paths::simplify` actually performs observable work on `\\?\D:\…` planted
  fixtures — without this, every cross-platform assertion would be vacuous.
- The dotty-path test (`four_file_taking_tools_resolve_dot_segment_paths` in
  `crates/code-graph-tools/tests/path_normalization.rs`) is the strongest
  cross-platform regression target: it supplies a `./sub/../<file>` path that
  `Path::new` would fail to resolve but `normalize_user_path` canonicalizes
  back to the indexed key. Removing the wrap from any of the 4 handlers fails
  this test on Linux.

**Supplementary verification:** manual smoke against a UE-style fixture on
Windows before release.

**Tracked follow-up:** add `windows-latest` to the CI matrix so the
`#[cfg(windows)]` tests + the strip-correctness ground-truth assertions all
run automatically. Until that lands, the dotty-path test is the load-bearing
guarantee on CI.

**Known limitation (also documented in CLAUDE.md):** watch-mode reindex via
`notify-debouncer-full` may receive `\\?\`-prefixed event paths on Windows
that re-contaminate a clean post-PathNormalization graph. Filed as a deferred
follow-up plan; this PR does NOT address it. See `Designs/PathNormalization/README.md`
Non-Goals.
```

## Why this matters

The PathNormalization plan fixes a Windows-only user-visible bug (`\\?\D:\…`
leaking into every symbol ID and breaking subsequent file-path lookups). The
fix is a `dunce` integration plus a `normalize_user_path` helper at four
handler call sites. CI runs on Linux, where `dunce::simplified` is
documented-identity, so the Linux test suite passing does **not** prove the
fix works on the target platform.

Hiding this gap would invite a regression in a future refactor where someone
removes the `normalize_user_path` wrap, all Linux CI checks pass, and the
release ships broken on Windows. The PR-body disclosure makes the gap
visible at review time so reviewers can weigh the test coverage they see
against the platform-specific behavior they're approving.

## Usage

Copy the section above (starting from `## CI-coverage caveat`) verbatim into
the PR description. Adjust the test-file references if subsequent work changes
file paths. Do NOT shorten the disclosure to a one-liner — the verbose form
exists because the next reader doesn't have this conversation as context.
