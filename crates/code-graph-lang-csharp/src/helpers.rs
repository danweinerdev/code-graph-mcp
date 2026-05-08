//! Helper routines for the C# parser.
//!
//! Phase 2.1 ships this module empty. The cross-language helpers
//! ([`code_graph_lang::helpers::truncate_signature`],
//! [`code_graph_lang::helpers::find_enclosing_kind`]) are already
//! consolidated in `code-graph-lang` and will be re-exported here on
//! demand as Phases 2.2-2.5 land their extractors. Keeping the file
//! present (even if empty) preserves the same module shape as the four
//! shipped plugins so future helpers slot in without churn.

// Re-export shared helpers from code-graph-lang::helpers as needed.
