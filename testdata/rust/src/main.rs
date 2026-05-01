//! Binary entry-point for the Rust parser corpus.
//!
//! Exercises:
//!   - `use` declarations (simple, scoped, grouped, wildcard, alias, self-in-list)
//!   - `extern crate` declaration (legacy form, captured as Includes edge)
//!   - direct call (`run()`), scoped call (`models::Vec2::new`), method call
//!     (`engine.update`), macro invocation (`println!`)
//!
//! Symbol contract for `main.rs` (asserted by `MANIFEST.md`):
//!   - 1 function (`main`)
//!   - 0 of every other kind

extern crate alloc;

use code_graph_rust_corpus::errors::AppError;
use code_graph_rust_corpus::models::{self, Vec2};
use code_graph_rust_corpus::traits::*;
use code_graph_rust_corpus::utils as utils_mod;
use std::collections::HashMap;
use std::io::{self, Read};

fn main() {
    let pos = Vec2::new(0, 0);
    let _ = pos;
    let _ = HashMap::<String, i32>::new();
    let _: io::Result<()> = Ok(());
    let _ = Read::bytes; // method-pointer reference, not a call edge
    let _ = utils_mod::add(1, 2);
    let _: Result<(), AppError> = Ok(());
    println!("corpus main");
    let _ = models::nested_helper(); // scoped call into models module
}
