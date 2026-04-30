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

use codegraph_core::{symbol_id, Language, Symbol, SymbolKind};
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
        // a fall-back; tests in codegraph-core lock the existing variants in.
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
pub fn suggest_symbols(graph: &codegraph_graph::Graph, name: &str, limit: usize) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::{Language, SymbolKind};

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
        assert!(parse_language("ruby").is_none());
        assert!(parse_language("").is_none());
    }
}
