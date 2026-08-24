//! Human-readable default rendering (Designs/CommandLineInterface,
//! "Output contract").
//!
//! ONE renderer, fed from the payload JSON — the same bytes machine mode
//! prints — never from a second typed path, so a rendering bug cannot mask
//! a payload divergence (the thing AC-11 exists to catch). Rendering keys
//! on the response's STRUCTURAL family, not on which subcommand ran:
//!
//! - `Page<T>` envelopes (`results` + `total`) → aligned columns plus a
//!   paging footer, plus any extra top-level fields (flattened envelopes
//!   like `search_symbols`/`detect_communities`) as footer lines. The
//!   `did you mean:` footer appears ONLY when `suggestions` is present —
//!   the field is absent rather than empty by contract.
//! - Dual-page (`incoming` + `outgoing` pages, no top-level `results`) →
//!   two labelled sections.
//! - Trees (`hierarchy`) → two-space indentation, `(ref)` stubs marked.
//! - Arrays (diagram edges) → one table.
//! - Everything else → labelled `key: value` lines with arrays-of-objects
//!   as sub-tables (find_path hops, blame hunks, history entries).

use std::collections::BTreeSet;

use serde_json::Value;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// A human table cell never exceeds this many terminal columns. Wrapping is
/// display-width-aware (not byte or scalar-count-aware), so CJK and combining
/// text keep subsequent columns aligned.
const MAX_CELL_DISPLAY_WIDTH: usize = 48;

/// Renders a JSON payload for terminal reading. The caller has already
/// decided the payload IS JSON; `ToolOk::Text` bodies bypass this.
pub fn human(payload: &Value) -> String {
    match payload {
        Value::Array(rows) => table_or_list(rows),
        Value::Object(map) => {
            if map.contains_key("results") && map.contains_key("total") {
                page(map)
            } else if map.contains_key("incoming") && map.contains_key("outgoing") {
                dual_page(map)
            } else if map.contains_key("hierarchy") {
                hierarchy(map)
            } else {
                object(map, 0)
            }
        }
        scalar => scalar_text(scalar),
    }
}

fn page(map: &serde_json::Map<String, Value>) -> String {
    let mut out = String::new();
    if let Some(Value::Array(rows)) = map.get("results") {
        out.push_str(&table_or_list(rows));
    }
    // Paging footer, always present in the envelope.
    let mut footer: Vec<String> = Vec::new();
    for key in ["total", "offset", "limit", "truncated", "next_offset"] {
        if let Some(value) = map.get(key) {
            footer.push(format!("{key} {}", scalar_text(value)));
        }
    }
    if !footer.is_empty() {
        out.push_str(&footer.join(" · "));
        out.push('\n');
    }
    // Flattened-envelope extras (search_symbols suggestions,
    // detect_communities metadata, …), in payload order.
    for (key, value) in map {
        if matches!(
            key.as_str(),
            "results" | "total" | "offset" | "limit" | "truncated" | "next_offset"
        ) {
            continue;
        }
        if key == "suggestions" {
            if let Value::Array(names) = value {
                let names: Vec<&str> = names.iter().filter_map(Value::as_str).collect();
                out.push_str(&format!("did you mean: {}\n", names.join(", ")));
            }
            continue;
        }
        out.push_str(&format!("{key}: {}\n", scalar_text(value)));
    }
    out
}

fn dual_page(map: &serde_json::Map<String, Value>) -> String {
    let mut out = String::new();
    for section in ["incoming", "outgoing"] {
        if let Some(Value::Object(inner)) = map.get(section) {
            out.push_str(section);
            out.push_str(":\n");
            for line in page(inner).lines() {
                out.push_str("  ");
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out
}

fn hierarchy(map: &serde_json::Map<String, Value>) -> String {
    let mut out = String::new();
    if let Some(root) = map.get("hierarchy") {
        hierarchy_node(root, 0, &mut out);
    }
    let mut footer: Vec<String> = Vec::new();
    for key in ["truncated", "max_nodes", "total_nodes_seen"] {
        if let Some(value) = map.get(key) {
            footer.push(format!("{key} {}", scalar_text(value)));
        }
    }
    if !footer.is_empty() {
        out.push_str(&footer.join(" · "));
        out.push('\n');
    }
    out
}

fn hierarchy_node(node: &Value, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    let name = node
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("<unnamed>");
    let marker = if node.get("ref").and_then(Value::as_bool) == Some(true) {
        " (ref)"
    } else {
        ""
    };
    out.push_str(&format!("{indent}{name}{marker}\n"));
    for (arm, label) in [("bases", "bases:"), ("derived", "derived:")] {
        if let Some(Value::Array(children)) = node.get(arm) {
            out.push_str(&format!("{indent}  {label}\n"));
            for child in children {
                hierarchy_node(child, depth + 2, out);
            }
        }
    }
}

/// A uniform array: objects render as an aligned table using the deterministic
/// union of keys from every object row; anything else renders one entry per
/// line. Cells hard-wrap at [`MAX_CELL_DISPLAY_WIDTH`] terminal columns.
fn table_or_list(rows: &[Value]) -> String {
    if rows.is_empty() {
        return "(no results)\n".to_string();
    }
    let Some(_) = rows.first().and_then(Value::as_object) else {
        let mut out = String::new();
        for row in rows {
            out.push_str(&scalar_text(row));
            out.push('\n');
        }
        return out;
    };
    let mut keys = BTreeSet::new();
    for row in rows.iter().filter_map(Value::as_object) {
        keys.extend(row.keys().cloned());
    }
    let columns: Vec<String> = keys.into_iter().collect();
    let headers: Vec<Vec<String>> = columns.iter().map(|column| wrap_cell(column)).collect();
    let mut widths: Vec<usize> = headers
        .iter()
        .map(|header| {
            header
                .iter()
                .map(|part| UnicodeWidthStr::width(part.as_str()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut cells: Vec<Vec<Vec<String>>> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut line: Vec<Vec<String>> = Vec::with_capacity(columns.len());
        for (i, column) in columns.iter().enumerate() {
            let text = row
                .get(column.as_str())
                .map(scalar_text)
                .unwrap_or_default();
            let wrapped = wrap_cell(&text);
            widths[i] = widths[i].max(
                wrapped
                    .iter()
                    .map(|part| UnicodeWidthStr::width(part.as_str()))
                    .max()
                    .unwrap_or(0),
            );
            line.push(wrapped);
        }
        cells.push(line);
    }
    let mut out = String::new();
    let header_height = headers.iter().map(Vec::len).max().unwrap_or(1);
    for line_index in 0..header_height {
        let rendered: Vec<String> = headers
            .iter()
            .enumerate()
            .map(|(column, header)| {
                pad_display(
                    header.get(line_index).map(String::as_str).unwrap_or(""),
                    widths[column],
                )
            })
            .collect();
        out.push_str(rendered.join("  ").trim_end());
        out.push('\n');
    }
    for row in cells {
        let height = row.iter().map(Vec::len).max().unwrap_or(1);
        for line_index in 0..height {
            let rendered: Vec<String> = row
                .iter()
                .enumerate()
                .map(|(column, cell)| {
                    pad_display(
                        cell.get(line_index).map(String::as_str).unwrap_or(""),
                        widths[column],
                    )
                })
                .collect();
            out.push_str(rendered.join("  ").trim_end());
            out.push('\n');
        }
    }
    out
}

fn pad_display(value: &str, width: usize) -> String {
    let padding = width.saturating_sub(UnicodeWidthStr::width(value));
    format!("{value}{}", " ".repeat(padding))
}

fn wrap_cell(value: &str) -> Vec<String> {
    let mut lines = vec![String::new()];
    let mut width = 0;
    for character in value.chars() {
        if character == '\n' {
            lines.push(String::new());
            width = 0;
            continue;
        }
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if width > 0 && width + character_width > MAX_CELL_DISPLAY_WIDTH {
            lines.push(String::new());
            width = 0;
        }
        lines.last_mut().expect("one initial line").push(character);
        width += character_width;
    }
    lines
}

/// Labelled `key: value` lines; arrays of objects become sub-tables.
fn object(map: &serde_json::Map<String, Value>, depth: usize) -> String {
    let indent = "  ".repeat(depth);
    let mut out = String::new();
    for (key, value) in map {
        match value {
            Value::Array(rows) if rows.iter().any(Value::is_object) => {
                out.push_str(&format!("{indent}{key}:\n"));
                for line in table_or_list(rows).lines() {
                    out.push_str(&format!("{indent}  {line}\n"));
                }
            }
            Value::Object(inner) => {
                out.push_str(&format!("{indent}{key}:\n"));
                out.push_str(&object(inner, depth + 1));
            }
            scalar => out.push_str(&format!("{indent}{key}: {}\n", scalar_text(scalar))),
        }
    }
    out
}

/// Scalars render bare (no JSON quoting); nested values render compact.
fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use unicode_width::UnicodeWidthStr;

    use super::{human, MAX_CELL_DISPLAY_WIDTH};

    #[test]
    fn table_includes_columns_present_only_in_later_rows() {
        let rendered = human(&serde_json::json!([
            { "first": "one" },
            { "later": "two" }
        ]));
        let mut lines = rendered.lines();
        assert_eq!(lines.next(), Some("first  later"));
        assert_eq!(lines.next(), Some("one"));
        assert_eq!(lines.next(), Some("       two"));
    }

    #[test]
    fn table_alignment_uses_unicode_display_width() {
        let rendered = human(&serde_json::json!([
            { "name": "表", "value": "x" },
            { "name": "e\u{301}", "value": "y" }
        ]));
        let lines: Vec<_> = rendered.lines().collect();
        for (line, marker) in [(lines[1], "x"), (lines[2], "y")] {
            let before = line.strip_suffix(marker).expect("value marker");
            assert_eq!(UnicodeWidthStr::width(before), 6, "aligned: {line}");
        }
    }

    #[test]
    fn table_cells_hard_wrap_to_the_display_width_cap() {
        let value = "x".repeat(MAX_CELL_DISPLAY_WIDTH + 2);
        let rendered = human(&serde_json::json!([{ "value": value }]));
        let lines: Vec<_> = rendered.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(UnicodeWidthStr::width(lines[1]), MAX_CELL_DISPLAY_WIDTH);
        assert_eq!(UnicodeWidthStr::width(lines[2]), 2);
    }

    #[test]
    fn table_headers_hard_wrap_to_the_display_width_cap() {
        let key = "k".repeat(MAX_CELL_DISPLAY_WIDTH + 2);
        let rendered = human(&serde_json::json!([{ (key): "value" }]));
        let lines: Vec<_> = rendered.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(UnicodeWidthStr::width(lines[0]), MAX_CELL_DISPLAY_WIDTH);
        assert_eq!(UnicodeWidthStr::width(lines[1]), 2);
        assert_eq!(lines[2], "value");
    }
}
