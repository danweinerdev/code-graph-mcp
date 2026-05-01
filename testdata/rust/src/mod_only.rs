//! Mod-only fixture: a file containing nothing but external module
//! declarations. The parser must walk this without error and produce zero
//! Symbol records (mod_items are namespace anchors, not symbols) and zero
//! Edge records (no `use`, no `extern crate`, no calls).

pub mod a;
pub mod b;
