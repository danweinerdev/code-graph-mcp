//! Build script that captures the git SHA + dirty state of the working
//! tree into a `CODE_GRAPH_GIT_SHA` environment variable for the
//! `get_status` MCP tool. Fails gracefully (sets `"unknown"`) when
//! git isn't available, the binary is built outside a working tree
//! (e.g. via `cargo install`), or any git invocation errors.
//!
//! The dirty-state suffix (`-dirty`) matches what the user expects
//! from `git describe --dirty` — useful for verifying "is the running
//! server actually the build I just made" without an
//! independently-tracked build counter.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Re-run when HEAD moves (commit, checkout) or any branch ref
    // updates. `rerun-if-changed` paths are resolved relative to this
    // crate's manifest directory, but the `.git` lives at the workspace
    // root, so we resolve an absolute path via `git rev-parse --git-dir`
    // (handles worktrees and submodules correctly). When git isn't
    // available we skip the hints entirely — emitting a path that
    // doesn't exist would make cargo treat the crate as perpetually
    // dirty and force a rebuild on every invocation.
    let git_dir = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| PathBuf::from(s.trim()))
        .and_then(|p| p.canonicalize().ok());

    if let Some(git_dir) = &git_dir {
        println!("cargo:rerun-if-changed={}/HEAD", git_dir.display());
        println!("cargo:rerun-if-changed={}/refs/heads", git_dir.display());
    }

    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                String::from_utf8(out.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .map(|out| !out.stdout.is_empty())
        .unwrap_or(false);

    let version = if dirty && sha != "unknown" {
        format!("{sha}-dirty")
    } else {
        sha
    };

    println!("cargo:rustc-env=CODE_GRAPH_GIT_SHA={version}");
}
