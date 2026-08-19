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

use serde_json::Value;

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

/// A uniform array: objects render as an aligned table on the first row's
/// key order; anything else renders one entry per line.
fn table_or_list(rows: &[Value]) -> String {
    if rows.is_empty() {
        return "(no results)\n".to_string();
    }
    let Some(first) = rows.first().and_then(Value::as_object) else {
        let mut out = String::new();
        for row in rows {
            out.push_str(&scalar_text(row));
            out.push('\n');
        }
        return out;
    };
    let columns: Vec<&String> = first.keys().collect();
    let mut widths: Vec<usize> = columns.iter().map(|c| c.chars().count()).collect();
    let mut cells: Vec<Vec<String>> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut line: Vec<String> = Vec::with_capacity(columns.len());
        for (i, column) in columns.iter().enumerate() {
            let text = row
                .get(column.as_str())
                .map(scalar_text)
                .unwrap_or_default();
            widths[i] = widths[i].max(text.chars().count());
            line.push(text);
        }
        cells.push(line);
    }
    let mut out = String::new();
    let header: Vec<String> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{:<width$}", c, width = widths[i]))
        .collect();
    out.push_str(header.join("  ").trim_end());
    out.push('\n');
    for line in cells {
        let rendered: Vec<String> = line
            .iter()
            .enumerate()
            .map(|(i, cell)| format!("{:<width$}", cell, width = widths[i]))
            .collect();
        out.push_str(rendered.join("  ").trim_end());
        out.push('\n');
    }
    out
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
