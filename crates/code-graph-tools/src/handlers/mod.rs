//! Tool handler bodies.
//!
//! The `#[tool]` shells in `crate::server::CodeGraphServer` delegate here
//! so each handler stays focused, testable, and short. Phase 3.4 filled in
//! the P0 handlers; Phase 3.5 filled in P1+P2 plus watch stubs; Phase 4.1
//! has now replaced the watch stubs with the real lifecycle.
//!
//! Module layout:
//! - `analyze` — `analyze_codebase` (the big one: progress bridge, cache,
//!   spawn_blocking).
//! - `symbols` — `get_file_symbols`, `search_symbols`, `get_symbol_detail`,
//!   `get_symbol_summary`.
//! - `query` — `get_callers`, `get_callees`, `get_dependencies`.
//! - `structure` — `detect_cycles`, `get_orphans`, `get_class_hierarchy`,
//!   `get_coupling`, `generate_diagram`.
//! - `watch` — `watch_start` and `watch_stop` (lifecycle: Phase 4.1;
//!   reindex pipeline: Phase 4.2).
//!
//! All public functions in these submodules return `CallToolResult` (never
//! `McpError`), matching the wire-envelope rule the design pinned in
//! `Designs/RustRewrite/README.md`: tool-level errors stay inside
//! `CallToolResult { is_error: true }` so MCP clients see the standard
//! `tools/call` response shape, not a JSON-RPC protocol error.

pub mod analyze;
pub mod query;
pub mod structure;
pub mod symbols;
pub mod watch;

use code_graph_core::{symbol_id, Language, Symbol, SymbolKind};
use rmcp::model::{CallToolResult, Content};
use serde::Serialize;

/// JSON-shape mirror of Go's `symbolResult` in
/// `internal/tools/symbols.go`. Field order, names, and `omitempty` semantics
/// match exactly so wire-format snapshots stay byte-identical.
///
/// The brief-mode behavior is encoded in the field defaults: `column`,
/// `end_line`, and `signature` get zero/empty values when `brief = true`,
/// which the `skip_serializing_if` annotations then drop from the JSON
/// output. This mirrors Go's `omitempty` serialization exactly.
#[derive(Debug, Serialize)]
pub struct SymbolResult {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub column: u32,
    #[serde(skip_serializing_if = "is_zero_u32", rename = "end_line")]
    pub end_line: u32,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub signature: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub namespace: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub parent: String,
}

fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

/// Shared pagination envelope for list-shaped tool responses.
///
/// Field-declaration order — `results`, `total`, `offset`, `limit`,
/// `truncated`, `next_offset` — is the wire-format contract: `serde`
/// serializes derived structs in declaration order, so reordering these
/// fields is a breaking JSON change. New fields may be appended at the end
/// (additive-only); the existing field positions are frozen. The insta
/// snapshot harness alphabetizes keys via `parsed_sorted` before recording,
/// so snapshot files do not preserve declaration order — the struct itself
/// is the source of truth, not the snapshots. Integer fields stay `u32`
/// (not `usize`) so JSON output is byte-identical across platforms.
///
/// `truncated` is `true` when the handler stopped emitting results before
/// reaching `limit` due to a byte-budget cap (see Phase 2 of the
/// `PaginatedResponseSizeSafety` plan). `next_offset` is `Some(n)` when a
/// client should re-request with `offset = n` to continue paging — `None`
/// when there is no further page. The fields always serialize (no
/// `skip_serializing_if`) so MCP clients can rely on a stable envelope
/// shape: `truncated: false` and `next_offset: null` are emitted explicitly
/// when no truncation occurred.
#[derive(Debug, Serialize)]
pub struct Page<T: Serialize> {
    pub results: Vec<T>,
    pub total: u32,
    pub offset: u32,
    pub limit: u32,
    pub truncated: bool,
    pub next_offset: Option<u32>,
}

/// Convert a [`Symbol`] to a [`SymbolResult`]. In `brief` mode, `column`,
/// `end_line`, and `signature` are reset to defaults so they drop out of
/// the JSON output via `skip_serializing_if`. Mirrors Go's `symbolToResult`.
pub fn symbol_to_result(s: &Symbol, brief: bool) -> SymbolResult {
    SymbolResult {
        id: symbol_id(s),
        name: s.name.clone(),
        kind: kind_str(s.kind).to_string(),
        file: s.file.clone(),
        line: s.line,
        column: if brief { 0 } else { s.column },
        end_line: if brief { 0 } else { s.end_line },
        signature: if brief {
            String::new()
        } else {
            s.signature.clone()
        },
        namespace: s.namespace.clone(),
        parent: s.parent.clone(),
    }
}

/// Lowercase string for a [`SymbolKind`]. Matches the JSON serialization
/// of `SymbolKind` (`#[serde(rename_all = "lowercase")]`) so the wire
/// format is consistent across all surfaces that emit a kind name.
pub fn kind_str(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function => "function",
        SymbolKind::Method => "method",
        SymbolKind::Class => "class",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Typedef => "typedef",
        SymbolKind::Interface => "interface",
        SymbolKind::Trait => "trait",
        // SymbolKind is `#[non_exhaustive]`. New variants would surface here as
        // a fall-back; tests in code-graph-core lock the existing variants in.
        _ => "unknown",
    }
}

/// Parse a lowercase [`SymbolKind`] string into the enum, or `None` if the
/// string does not name a known kind. Matches the variants accepted by the
/// Go `search_symbols` `kind` parameter.
pub fn parse_kind(s: &str) -> Option<SymbolKind> {
    match s {
        "function" => Some(SymbolKind::Function),
        "method" => Some(SymbolKind::Method),
        "class" => Some(SymbolKind::Class),
        "struct" => Some(SymbolKind::Struct),
        "enum" => Some(SymbolKind::Enum),
        "typedef" => Some(SymbolKind::Typedef),
        "interface" => Some(SymbolKind::Interface),
        "trait" => Some(SymbolKind::Trait),
        _ => None,
    }
}

/// Parse a lowercase [`Language`] string into the enum, or `None` if the
/// string does not name a known language.
pub fn parse_language(s: &str) -> Option<Language> {
    match s {
        "cpp" => Some(Language::Cpp),
        "rust" => Some(Language::Rust),
        "go" => Some(Language::Go),
        "python" => Some(Language::Python),
        "csharp" => Some(Language::CSharp),
        "java" => Some(Language::Java),
        _ => None,
    }
}

/// Convenience: build a tool-level error `CallToolResult` from a string.
/// Mirrors `mcp.NewToolResultError(msg)` in the Go binary.
pub fn tool_error(msg: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![Content::text(msg.into())])
}

/// Convenience: serialize a value to compact JSON and wrap it in a
/// successful `CallToolResult`. Compact (not pretty) so the wire format
/// matches Go's `json.Marshal` byte-for-byte.
pub fn tool_success_json<T: Serialize>(v: &T) -> CallToolResult {
    // serde_json::to_string can only fail on cycles or non-serializable
    // types; everything we serialize from these handlers is plain data so
    // the unwrap path is unreachable in practice. Use `.unwrap_or_default()`
    // so we never panic in production: an empty body is still a valid
    // (if degenerate) tool result.
    let body = serde_json::to_string(v).unwrap_or_default();
    CallToolResult::success(vec![Content::text(body)])
}

/// Build a did-you-mean suggestion string for an unknown symbol ID.
///
/// Pulls a 100-candidate pool from the graph (matching Go's
/// `suggestSymbols` behavior in `internal/tools/tools.go`), keeps the top
/// `limit` results, and joins their IDs with `, `. Returns an empty string
/// when no candidates exist — callers prepend the prefix only when this
/// returns non-empty.
pub fn suggest_symbols(graph: &code_graph_graph::Graph, name: &str, limit: usize) -> String {
    let candidates = graph.search_symbols(name, None);
    if candidates.is_empty() {
        return String::new();
    }
    let take = candidates.len().min(limit);
    let mut out = String::new();
    for (i, s) in candidates.iter().take(take).enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&symbol_id(s));
    }
    out
}

/// Reserved bytes for the [`Page<T>`] envelope wrapper itself when sizing a
/// response against `max_bytes`. Reserves headroom for the JSON envelope
/// wrapper
/// (`{"results":[...],"total":N,"offset":M,"limit":L,"truncated":false,"next_offset":null}`
/// ≈ 100 bytes) plus a 5× safety margin. The slack absorbs inter-record
/// commas, large `total`/`offset`/`limit` integer widths, and any future
/// envelope-shape additions without forcing a constant bump.
///
/// `#[allow(dead_code)]` is intentional for Phase 1 of the
/// `PaginatedResponseSizeSafety` plan: the constant and its sole consumer
/// [`byte_budget_take`] ship in Phase 1, but handlers do not wire them up
/// until Phase 2. Without the allow, `cargo clippy --workspace
/// --all-targets -- -D warnings` would fail. The allow is removed in Phase 2
/// once the first handler consumes the helper.
#[allow(dead_code)]
pub(super) const ENVELOPE_OVERHEAD_BYTES: usize = 512;

/// Apply `offset` + `limit` pagination to `iter` while enforcing a
/// JSON-serialized byte budget on the returned page. This is the drop-in
/// replacement for the `.skip(offset).take(limit).collect()` pattern used by
/// the four materializing handlers (orphans, file_symbols, callers, callees)
/// today; Phase 2 of the `PaginatedResponseSizeSafety` plan wires it through.
///
/// Behavior:
/// - Skips the first `offset` items from `iter`, then accumulates up to
///   `limit` items while the running JSON byte count stays under
///   `max_bytes - ENVELOPE_OVERHEAD_BYTES`.
/// - Each candidate is pre-serialized with `serde_json::to_string`; its
///   serialized length (plus one byte for the inter-record comma) is added
///   to the running total.
/// - If a candidate would push the total over budget, it is NOT included,
///   the function returns early with `truncated = true` and
///   `next_offset = Some(offset + kept_count)`.
/// - If `limit` is reached, or `iter` is exhausted, before the budget
///   bites, the function returns `truncated = false` and `next_offset = None`.
/// - Pathological case: if the very first candidate alone exceeds the
///   budget, the helper returns 0 records, `truncated = true`, and
///   `next_offset = Some(offset)` — never panics, never makes forward
///   progress impossible. Callers should treat `next_offset == Some(offset)`
///   with an empty `results` as "budget too tight for any record at this
///   position" and surface a meaningful error if needed.
///
/// Return tuple is `(kept_records, total_kept, truncated, next_offset)`
/// where `total_kept == kept_records.len() as u32`.
///
/// Note on `total_kept` vs `Page<T>.total`: the second tuple element is the
/// count of records actually emitted on THIS page (`results.len() as u32`),
/// NOT the pre-pagination match count. The handler is responsible for
/// computing the latter separately (typically via `.count()` on the source
/// iterator before this helper is called).
///
/// `#[allow(dead_code)]` is intentional for Phase 1 of the
/// `PaginatedResponseSizeSafety` plan — see [`ENVELOPE_OVERHEAD_BYTES`].
#[allow(dead_code)]
pub(super) fn byte_budget_take<T: Serialize, I: IntoIterator<Item = T>>(
    iter: I,
    offset: u32,
    limit: u32,
    max_bytes: usize,
) -> (Vec<T>, u32, bool, Option<u32>) {
    // `limit == 0` is always a caller bug here: every handler resolves
    // limit defaults (typically via `resolve_pagination(limit, DEFAULT, MAX)`)
    // before invoking this helper. A 0 limit would cause the first
    // `kept.len() >= limit` check to return immediately with an empty page
    // and `truncated=false`, silently swallowing all records. Surface that
    // mistake loudly in debug builds; release builds still behave (empty
    // page, no continuation) but the panic catches the bug in CI.
    debug_assert!(
        limit > 0,
        "byte_budget_take called with limit=0; callers must resolve limit defaults before invocation"
    );

    // Reserve envelope overhead so `{"results": [...], ...}` always fits.
    // saturating_sub guards against a pathological `max_bytes <
    // ENVELOPE_OVERHEAD_BYTES` (including the boundary `max_bytes == 0`
    // case) — budget becomes 0, the loop's first comparison rejects every
    // candidate, and the helper returns 0 records with truncated=true.
    let budget = max_bytes.saturating_sub(ENVELOPE_OVERHEAD_BYTES);

    let mut kept: Vec<T> = Vec::new();
    let mut running_bytes: usize = 0;

    for item in iter.into_iter().skip(offset as usize) {
        if (kept.len() as u32) >= limit {
            // Hit the count cap before the byte budget — clean page, no
            // continuation token. Anything beyond `limit` is the next call's
            // responsibility, signalled by the caller-supplied `offset+limit`,
            // not by the helper.
            return (kept, limit, false, None);
        }
        // Production `T` types (SymbolResult, CallChain) are infallible
        // serializers — they hold only plain owned data with no cycles or
        // custom Serialize impls that can error. The `unwrap_or(0)` fallback
        // exists solely to satisfy the generic `T: Serialize` bound. On a
        // hypothetical failure the record is admitted as zero-cost (the
        // running total does not move) and the budget will still bite on
        // the next iteration, so we never silently emit unbounded bytes.
        let serialized_len = serde_json::to_string(&item).map(|s| s.len()).unwrap_or(0);
        // +1 covers the inter-record comma between this candidate and the
        // previous one. The first candidate has no leading comma, so this
        // over-counts by 1 byte on the first record; intentional headroom
        // over ENVELOPE_OVERHEAD_BYTES — over-estimation favors safety.
        let projected = running_bytes
            .saturating_add(serialized_len)
            .saturating_add(1);
        if projected > budget {
            // Budget bites: drop this candidate, stop here.
            let kept_len = kept.len() as u32;
            return (kept, kept_len, true, Some(offset.saturating_add(kept_len)));
        }
        running_bytes = projected;
        kept.push(item);
    }

    // Iterator exhausted before either trigger fired. No continuation.
    let kept_len = kept.len() as u32;
    (kept, kept_len, false, None)
}

/// Test-only helpers shared across handler submodules. Lifted out of each
/// submodule's `mod tests` to avoid identical copy-paste in every paginated
/// handler test file. Submodules opt in via `use super::test_helpers::*`.
#[cfg(test)]
pub(super) mod test_helpers {
    use rmcp::model::CallToolResult;

    /// Extract the text body from a successful (or error) `CallToolResult`.
    /// Returns the empty string if the result has no text content.
    pub fn body_text(r: &CallToolResult) -> String {
        r.content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default()
    }

    /// Parse the shared `Page<T>` envelope into `(results_array, total,
    /// offset, limit)` for assertion convenience. Used by every paginated
    /// tool's tests (`get_orphans`, `get_file_symbols`, `get_callers`,
    /// `get_callees`).
    pub fn page_parts(r: &CallToolResult) -> (Vec<serde_json::Value>, u32, u32, u32) {
        let parsed: serde_json::Value = serde_json::from_str(&body_text(r)).unwrap();
        let results = parsed["results"].as_array().cloned().unwrap_or_default();
        let total = parsed["total"].as_u64().unwrap_or(0) as u32;
        let offset = parsed["offset"].as_u64().unwrap_or(0) as u32;
        let limit = parsed["limit"].as_u64().unwrap_or(0) as u32;
        (results, total, offset, limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_graph_core::{Language, SymbolKind};

    fn sym() -> Symbol {
        Symbol {
            name: "do_thing".to_string(),
            kind: SymbolKind::Method,
            file: "/a.cpp".to_string(),
            line: 10,
            column: 4,
            end_line: 20,
            signature: "void Widget::do_thing()".to_string(),
            namespace: "ns".to_string(),
            parent: "Widget".to_string(),
            language: Language::Cpp,
        }
    }

    #[test]
    fn brief_mode_zeroes_fields_so_omitempty_drops_them() {
        let r = symbol_to_result(&sym(), true);
        let v = serde_json::to_value(&r).unwrap();
        let obj = v.as_object().unwrap();
        // brief: column / end_line / signature absent.
        assert!(!obj.contains_key("column"));
        assert!(!obj.contains_key("end_line"));
        assert!(!obj.contains_key("signature"));
        // line and namespace and parent always present (line is non-zero;
        // namespace and parent are non-empty for this sample).
        assert_eq!(obj["line"], serde_json::json!(10));
        assert_eq!(obj["namespace"], serde_json::json!("ns"));
        assert_eq!(obj["parent"], serde_json::json!("Widget"));
    }

    #[test]
    fn full_mode_includes_all_fields() {
        let r = symbol_to_result(&sym(), false);
        let v = serde_json::to_value(&r).unwrap();
        let obj = v.as_object().unwrap();
        assert_eq!(obj["column"], serde_json::json!(4));
        assert_eq!(obj["end_line"], serde_json::json!(20));
        assert_eq!(
            obj["signature"],
            serde_json::json!("void Widget::do_thing()")
        );
    }

    #[test]
    fn empty_namespace_and_parent_omitted() {
        let mut s = sym();
        s.namespace.clear();
        s.parent.clear();
        let r = symbol_to_result(&s, false);
        let v = serde_json::to_value(&r).unwrap();
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("namespace"));
        assert!(!obj.contains_key("parent"));
    }

    #[test]
    fn kind_str_is_lowercase_for_every_known_variant() {
        for (k, expected) in [
            (SymbolKind::Function, "function"),
            (SymbolKind::Method, "method"),
            (SymbolKind::Class, "class"),
            (SymbolKind::Struct, "struct"),
            (SymbolKind::Enum, "enum"),
            (SymbolKind::Typedef, "typedef"),
            (SymbolKind::Interface, "interface"),
            (SymbolKind::Trait, "trait"),
        ] {
            assert_eq!(kind_str(k), expected);
            // Round-trip via parse_kind.
            assert_eq!(parse_kind(expected), Some(k));
        }
    }

    #[test]
    fn parse_kind_unknown_returns_none() {
        assert!(parse_kind("not_a_kind").is_none());
        assert!(parse_kind("").is_none());
        // Mixed case is not accepted — Go's binding is case-sensitive too.
        assert!(parse_kind("Function").is_none());
    }

    #[test]
    fn parse_language_handles_all_languages() {
        assert_eq!(parse_language("cpp"), Some(Language::Cpp));
        assert_eq!(parse_language("rust"), Some(Language::Rust));
        assert_eq!(parse_language("go"), Some(Language::Go));
        assert_eq!(parse_language("python"), Some(Language::Python));
        assert_eq!(parse_language("csharp"), Some(Language::CSharp));
        assert_eq!(parse_language("java"), Some(Language::Java));
        assert!(parse_language("ruby").is_none());
        assert!(parse_language("").is_none());
    }

    /// Tiny serializable record with predictable JSON size, used to make the
    /// byte-budget thresholds in the tests below easy to reason about.
    ///
    /// `Rec { id: N }` serializes as `{"id":N}`:
    /// - id `0..=9`   → 8 bytes
    /// - id `10..=99` → 9 bytes
    ///
    /// With the helper's +1 inter-record comma overhead, each single-digit
    /// record costs 9 bytes against the budget; the byte budget itself is
    /// `max_bytes - ENVELOPE_OVERHEAD_BYTES` (512).
    #[derive(Serialize)]
    struct Rec {
        id: u32,
    }

    #[test]
    fn byte_budget_take_fits_under_budget() {
        // 5 records, generous budget — all kept, no truncation.
        let items: Vec<Rec> = (0..5).map(|id| Rec { id }).collect();
        let (kept, total_kept, truncated, next_offset) = byte_budget_take(items, 0, 100, 10_000);
        assert_eq!(kept.len(), 5);
        assert_eq!(total_kept, 5);
        assert!(!truncated);
        assert_eq!(next_offset, None);
    }

    #[test]
    fn byte_budget_take_truncates_on_overflow() {
        // Records sized so exactly 2 fit before the 3rd blows the budget.
        // Each `{"id":N}` for single-digit N is 8 bytes; helper adds +1
        // comma per record. Two records cost 9+9 = 18 bytes against budget.
        // Set budget = 20 → 3rd record's projected total is 18+9 = 27 > 20.
        // max_bytes = ENVELOPE_OVERHEAD_BYTES (512) + 20 = 532.
        let items: Vec<Rec> = (0..5).map(|id| Rec { id }).collect();
        let (kept, total_kept, truncated, next_offset) =
            byte_budget_take(items, 0, 100, ENVELOPE_OVERHEAD_BYTES + 20);
        assert_eq!(kept.len(), 2);
        assert_eq!(total_kept, 2);
        assert!(truncated);
        assert_eq!(next_offset, Some(2));
    }

    #[test]
    fn byte_budget_take_max_bytes_zero() {
        // Pathological: max_bytes = 0 cannot even hold the envelope. Helper
        // returns empty results, truncated=true, next_offset preserves the
        // caller's offset so re-paging from that position is still possible
        // once `max_bytes` is raised. offset is within iter range so the
        // first post-skip candidate is actually evaluated (and rejected).
        let items: Vec<Rec> = (0..5).map(|id| Rec { id }).collect();
        let (kept, total_kept, truncated, next_offset) = byte_budget_take(items, 0, 100, 0);
        assert!(kept.is_empty());
        assert_eq!(total_kept, 0);
        assert!(truncated);
        assert_eq!(next_offset, Some(0));
    }

    #[test]
    fn byte_budget_take_iter_shorter_than_limit() {
        // 3 records, limit=100, budget never tested. Iterator exhaustion
        // wins: truncated=false, next_offset=None.
        let items: Vec<Rec> = (0..3).map(|id| Rec { id }).collect();
        let (kept, total_kept, truncated, next_offset) = byte_budget_take(items, 0, 100, 10_000);
        assert_eq!(kept.len(), 3);
        assert_eq!(total_kept, 3);
        assert!(!truncated);
        assert_eq!(next_offset, None);
    }

    #[test]
    fn byte_budget_take_first_record_exceeds_budget() {
        // Single record whose serialized form alone blows past the
        // envelope-overhead-adjusted budget. With budget = 5 bytes (after
        // subtracting overhead), an 8-byte record cannot fit. Expected:
        // 0 records kept, truncated=true, next_offset=Some(offset).
        //
        // Uses offset=3 with enough records that skip(3) lands on a real
        // candidate — proves the "first post-skip candidate too big" path,
        // not the "iter exhausted" path.
        let items: Vec<Rec> = (0..5).map(|id| Rec { id }).collect();
        let (kept, total_kept, truncated, next_offset) =
            byte_budget_take(items, 3, 100, ENVELOPE_OVERHEAD_BYTES + 5);
        assert!(kept.is_empty());
        assert_eq!(total_kept, 0);
        assert!(truncated);
        assert_eq!(next_offset, Some(3));
    }

    #[test]
    fn byte_budget_take_respects_offset_when_skipping() {
        // Sanity: offset=2 skips the first 2 items before applying budget.
        // 5 records, offset=2, generous budget → 3 records kept (ids 2,3,4).
        let items: Vec<Rec> = (0..5).map(|id| Rec { id }).collect();
        let (kept, total_kept, truncated, next_offset) = byte_budget_take(items, 2, 100, 10_000);
        assert_eq!(kept.len(), 3);
        assert_eq!(kept[0].id, 2);
        assert_eq!(kept[2].id, 4);
        assert_eq!(total_kept, 3);
        assert!(!truncated);
        assert_eq!(next_offset, None);
    }

    #[test]
    fn byte_budget_take_limit_cap_before_budget() {
        // limit caps before budget bites. 5 records, limit=2, generous
        // budget → exactly 2 kept, truncated=false (caller decides whether
        // to re-page via offset+limit).
        let items: Vec<Rec> = (0..5).map(|id| Rec { id }).collect();
        let (kept, total_kept, truncated, next_offset) = byte_budget_take(items, 0, 2, 10_000);
        assert_eq!(kept.len(), 2);
        assert_eq!(total_kept, 2);
        assert!(!truncated);
        assert_eq!(next_offset, None);
    }

    #[test]
    fn byte_budget_take_exact_fit_includes_record() {
        // Boundary: when the projected serialized total EXACTLY equals the
        // post-envelope budget, the helper uses `>` (strict) for rejection,
        // so the record must be INCLUDED, not dropped.
        //
        // Each `Rec { id: N }` for single-digit N serializes as `{"id":N}`
        // (8 bytes); the helper adds +1 for the comma → 9 bytes per record.
        // To make exactly 1 record land on the boundary, set the budget to
        // 9 bytes (max_bytes = ENVELOPE_OVERHEAD_BYTES + 9). Projected
        // running total after admitting record 0 = 0 + 8 + 1 = 9 == budget;
        // `9 > 9` is false → record kept. The next record's projected total
        // would be 9 + 8 + 1 = 18 > 9 → truncated.
        let items: Vec<Rec> = (0..5).map(|id| Rec { id }).collect();
        let (kept, total_kept, truncated, next_offset) =
            byte_budget_take(items, 0, 100, ENVELOPE_OVERHEAD_BYTES + 9);
        assert_eq!(kept.len(), 1, "exact-fit record must be admitted");
        assert_eq!(total_kept, 1);
        assert!(truncated, "second record's projected total exceeds budget");
        assert_eq!(next_offset, Some(1));
    }

    #[test]
    #[should_panic(expected = "limit=0")]
    fn byte_budget_take_panics_on_zero_limit_in_debug() {
        // Caller bug: passing `limit=0` bypasses pagination defaulting
        // (handlers always resolve via `resolve_pagination` first) and would
        // silently return an empty page. The `debug_assert!` makes that
        // mistake visible in test builds. In release builds the assertion
        // is compiled out and the helper still returns cleanly (empty page,
        // no continuation), so this contract is debug-only by design.
        let items: Vec<Rec> = (0..5).map(|id| Rec { id }).collect();
        let _ = byte_budget_take(items, 0, 0, 10_000);
    }
}
