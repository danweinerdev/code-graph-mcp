//! Embed the binary identity published by daemon metadata.
//!
//! The git-SHA/dirty logic is single-sourced in `build-support/git-identity.rs`
//! and shared with `code-graph-tools`' build script.

include!("../../build-support/git-identity.rs");

fn main() {
    emit_git_identity();
}
