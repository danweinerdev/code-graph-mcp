//! Helper routines for the Java parser.
//!
//! Currently empty. The cross-language helpers
//! ([`code_graph_lang::helpers::truncate_signature`],
//! [`code_graph_lang::helpers::find_enclosing_kind`]) are imported
//! directly from `code-graph-lang` at their use sites in `lib.rs` rather
//! than re-exported through this module. The Java-specific helpers that
//! Phase 3.2 needed (`enclosing_named_type_kind`,
//! `enclosing_named_type_name`, `enclosing_type_name`) live as private
//! functions in `lib.rs` rather than here, because they are tightly
//! coupled to the extractor's tree-walking strategy. This module exists
//! as a per-plugin landing spot for any Java-specific helpers later
//! phases introduce (e.g. package-path joining, qualified-name
//! flattening for inheritance edges); keeping the file present preserves
//! the same module shape as the C++/Rust/Go/Python/C# plugins so future
//! helpers slot in without churn.
