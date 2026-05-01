//! Library root for the Rust parser corpus.
//!
//! Module declarations + a couple of crate-level type aliases so the parser
//! exercises both `mod foo;` declarations (no body) and `type Alias = T;`
//! at the crate root.
//!
//! Symbol contract for `lib.rs` (asserted by `MANIFEST.md`):
//!   - 2 type aliases (`Pair`, `Result2`)
//!   - 0 functions, structs, enums, traits, methods
//!   - 0 mod_item symbols (modules are namespace anchors, not symbols)

pub mod errors;
pub mod models;
pub mod traits;
pub mod utils;

pub type Pair = (i32, i32);
pub type Result2<T> = std::result::Result<T, errors::AppError>;
