//! Helper routines for the C# parser.
//!
//! Currently empty. The cross-language helpers
//! ([`code_graph_lang::helpers::truncate_signature`],
//! [`code_graph_lang::helpers::find_enclosing_kind`]) are imported
//! directly from `code-graph-lang` at their use sites in `lib.rs` rather
//! than re-exported through this module. This module exists as a
//! per-plugin landing spot for any C#-specific helpers later phases
//! introduce (e.g. namespace-path joining, qualified-name flattening
//! for inheritance edges); keeping the file present preserves the same
//! module shape as the four shipped plugins so future helpers slot in
//! without churn.
