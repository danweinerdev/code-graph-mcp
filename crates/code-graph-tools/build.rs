//! Capture the git SHA + dirty state for the `get_status` MCP tool.
//!
//! The git-SHA/dirty logic is single-sourced in `build-support/git-identity.rs`
//! and shared with `code-graph-mcp`'s build script.

include!("../../build-support/git-identity.rs");

fn main() {
    emit_git_identity();
}
