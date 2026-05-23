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

use std::process::Command;

fn main() {
    // Re-run when HEAD moves (commit, checkout) or any source file
    // changes. The wildcard `refs/heads` re-trigger catches branch
    // updates without depending on a specific branch name.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");

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
