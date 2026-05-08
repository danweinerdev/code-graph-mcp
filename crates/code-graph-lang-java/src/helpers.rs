//! Helper routines for the Java parser.
//!
//! Phase status: Phase 3.1 ships this module empty. Tasks 3.2-3.5 fill
//! in the small structural helpers (e.g. enclosing-class lookup for
//! method-vs-function disambiguation, anonymous-class skipping for
//! Decision 4) used by the upcoming extractors.
//!
//! Cross-language helpers (e.g. `truncate_signature`) are re-exported
//! from `code_graph_lang::helpers` rather than duplicated here, mirroring
//! the post-consolidation shape of the C++/Rust/Go/Python plugins.
