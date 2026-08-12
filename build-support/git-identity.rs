// Shared build-script fragment: capture the git SHA + dirty state of the
// working tree into a `CODE_GRAPH_GIT_SHA` environment variable.
//
// `include!`d by `crates/code-graph-tools/build.rs` (feeds the `get_status`
// MCP tool) and `crates/code-graph-mcp/build.rs` (feeds daemon metadata's
// binary identity). Both consumers need their own compile-time `env!` read,
// but the generation logic must stay single-sourced so a fix (e.g. a
// git-worktree edge case) cannot land in one crate and silently miss the
// other.
//
// Fails gracefully (sets `"unknown"`) when git isn't available, the crate
// is built outside a working tree (e.g. via `cargo install`), or any git
// invocation errors. The dirty-state suffix (`-dirty`) matches what the
// user expects from `git describe --dirty` — useful for verifying "is the
// running server actually the build I just made" without an
// independently-tracked build counter.

use std::path::PathBuf;
use std::process::Command;

fn emit_git_identity() {
    // Re-run when this shared fragment itself changes; cargo only tracks
    // build.rs automatically, not files it includes.
    println!(
        "cargo:rerun-if-changed={}",
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../build-support/git-identity.rs")
    );

    // Re-run when HEAD moves (commit, checkout) or any branch ref
    // updates. `rerun-if-changed` paths are resolved relative to the
    // including crate's manifest directory, but the `.git` lives at the
    // workspace root, so we resolve an absolute path via
    // `git rev-parse --git-dir` (handles worktrees and submodules
    // correctly). When git isn't available we skip the hints entirely —
    // emitting a path that doesn't exist would make cargo treat the crate
    // as perpetually dirty and force a rebuild on every invocation.
    let git_dir = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|path| PathBuf::from(path.trim()))
        .and_then(|path| path.canonicalize().ok());

    if let Some(git_dir) = &git_dir {
        println!("cargo:rerun-if-changed={}/HEAD", git_dir.display());
        println!("cargo:rerun-if-changed={}/refs/heads", git_dir.display());
        println!("cargo:rerun-if-changed={}/index", git_dir.display());
    }

    let workspace_root = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|path| PathBuf::from(path.trim()))
        .and_then(|path| path.canonicalize().ok());
    if let Some(root) = &workspace_root {
        let tracked = Command::new("git")
            .args(["-C", root.to_string_lossy().as_ref(), "ls-files", "-z"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| output.stdout)
            .unwrap_or_default();
        for path in tracked
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            let relative = PathBuf::from(String::from_utf8_lossy(path).as_ref());
            let path = root.join(relative);
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }

    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        })
        .map(|sha| sha.trim().to_string())
        .filter(|sha| !sha.is_empty())
        .unwrap_or_else(|| "unknown".to_owned());

    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .map(|output| !output.stdout.is_empty())
        .unwrap_or(false);

    let version = if dirty && sha != "unknown" {
        format!("{sha}-dirty")
    } else {
        sha
    };
    println!("cargo:rustc-env=CODE_GRAPH_GIT_SHA={version}");
}
