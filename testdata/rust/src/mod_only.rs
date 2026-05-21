//! Mod-only fixture: a file containing nothing but external module
//! declarations. The parser walks this without error and produces zero
//! Symbol records (mod_items are namespace anchors, not symbols).
//!
//! Two provisional `Includes` edges do emit here — one per `pub mod a;` /
//! `pub mod b;` — with the bare modname token as `to`. They are
//! intentionally dropped at edge-resolution time because the default
//! basename matcher in the indexer's `resolve_include` can't find a `.rs`
//! file named just `a` or `b` in the `FileIndex`; an extension is
//! required to match. A future resolver step will rewrite these to
//! concrete child-file paths (`a.rs`, `a/mod.rs`, `#[path]` override),
//! at which point the edges will survive into the final graph. Until
//! then, the steady state for this fixture remains zero surviving
//! Includes edges.

pub mod a;
pub mod b;
