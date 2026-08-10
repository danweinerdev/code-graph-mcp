//! Embed the binary identity published by daemon metadata.

use std::path::PathBuf;
use std::process::Command;

fn main() {
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
