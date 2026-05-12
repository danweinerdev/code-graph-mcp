//! Shared types for the code-graph-mcp workspace.
//!
//! These mirror the canonical Go types in `internal/parser/types.go` and
//! `internal/graph/graph.go`. The JSON wire format **adds** a `language`
//! field to [`Symbol`] and [`FileGraph`] versus the Go binary's shape; all
//! other fields and JSON tags match Go exactly so MCP tool responses stay
//! backward-compatible for agents that only read the existing fields.
//!
//! These types are **not** designed to deserialize Go-produced cache files.
//! `Symbol` and `FileGraph` require `language`, which Go output does not
//! produce. Phase 4 of the Rust rewrite bumps the on-disk cache format to
//! v2; older Go-written caches are detected by the version tag and trigger
//! a silent re-index rather than being parsed.

mod config;

pub use config::{ConfigError, DiscoveryConfig, ExtensionsConfig, ParsingConfig, RootConfig};

use serde::{Deserialize, Serialize};

/// Source language identifier. Used to tag every [`Symbol`] and [`FileGraph`]
/// so cross-language queries can filter by language without parsing paths.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Language {
    Cpp,
    Rust,
    Go,
    Python,
    CSharp,
    Java,
}

/// Kind of code symbol. Mirrors the Go `parser.SymbolKind` constants and
/// adds `Interface` and `Trait` up front so future Go/Rust support does not
/// require a JSON-format bump.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Typedef,
    Interface,
    Trait,
}

/// Kind of edge in the code graph.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum EdgeKind {
    Calls,
    Includes,
    Inherits,
}

/// A named code entity (function, class, etc.). The shape mirrors the Go
/// `parser.Symbol` exactly (snake_case JSON field names, `namespace`/`parent`
/// elided when empty) and adds the `language` tag.
#[derive(Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub signature: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub namespace: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub parent: String,
    pub language: Language,
}

/// A relationship between symbols or files. Mirrors the Go `parser.Edge`.
#[derive(Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
    pub file: String,
    pub line: u32,
}

/// Result of parsing a single source file. Mirrors the Go `parser.FileGraph`
/// and adds the `language` tag (the Go shape did not need it because each
/// parser produced its own homogeneous output).
#[derive(Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
pub struct FileGraph {
    pub path: String,
    pub language: Language,
    pub symbols: Vec<Symbol>,
    pub edges: Vec<Edge>,
}

/// Stable string identifier for a [`Symbol`] in the graph. The format is
/// `path:Name` for free symbols and `path:Parent::Name` for methods, matching
/// Go's `graph.SymbolID` byte-for-byte.
pub type SymbolId = String;

/// Generate the graph key for a symbol. Mirrors Go's `graph.SymbolID` exactly:
/// `file:Name` when `parent` is empty, otherwise `file:Parent::Name`.
pub fn symbol_id(s: &Symbol) -> SymbolId {
    if s.parent.is_empty() {
        format!("{}:{}", s.file, s.name)
    } else {
        format!("{}:{}::{}", s.file, s.parent, s.name)
    }
}

/// Recover the file path prefix of a [`SymbolId`] produced by [`symbol_id`].
///
/// This is the **public id-recovery contract** for MCP consumers: any consumer
/// that needs to map a `SymbolId` back to the originating file path MUST use
/// this function rather than rolling its own split. Phase 4 of the
/// `PaginatedResponseSizeSafety` plan references this as the canonical inverse
/// of [`symbol_id`].
///
/// # Algorithm
///
/// Walks `id` from right to left and returns the slice before the **rightmost
/// `:` that is not part of a `::` token**. Concretely, a colon at byte index
/// `i` qualifies iff `bytes[i - 1] != b':'` AND `bytes[i + 1] != b':'`. This is
/// exact-inverse of the two id formats:
///
/// - Free symbol: `format!("{file}:{name}")` — the single separator colon.
/// - Method: `format!("{file}:{parent}::{name}")` — the separator colon sits
///   to the LEFT of `Parent::name`, so the rightmost-not-part-of-`::` colon is
///   the separator, never one of the colons in `::`.
///
/// Symbol names in the six supported languages (C++, Rust, Go, Python, C#,
/// Java) cannot contain a `:`, so there is no ambiguity inside the name
/// portion.
///
/// # Edge cases
///
/// - **Windows drive letters** (`C:\Users\foo\bar.cs:Baz::qux`): the drive
///   colon at index `1` is followed by `\`, not `:`, so it qualifies as a
///   separator colon. But the rightmost-not-part-of-`::` colon is the one
///   between `bar.cs` and `Baz`, so the drive colon is not reached.
/// - **Filename containing `:`** (`/project/foo:bar.rs:func`): same reasoning;
///   the rightmost qualifying colon is between `bar.rs` and `func`, so
///   `foo:bar.rs` is preserved in the file portion.
/// - **Malformed id with no separator colon**: returns `""` rather than
///   panicking. Callers that need to detect this case should check
///   `id_to_file(id).is_empty()`.
pub fn id_to_file(id: &str) -> &str {
    let bytes = id.as_bytes();
    let mut i = bytes.len();
    while i > 0 {
        i -= 1;
        if bytes[i] == b':' {
            let prev_is_colon = i > 0 && bytes[i - 1] == b':';
            let next_is_colon = i + 1 < bytes.len() && bytes[i + 1] == b':';
            if !prev_is_colon && !next_is_colon {
                return &id[..i];
            }
        }
    }
    ""
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn sample_symbol(name: &str, kind: SymbolKind, language: Language) -> Symbol {
        Symbol {
            name: name.to_string(),
            kind,
            file: "src/foo.cpp".to_string(),
            line: 10,
            column: 4,
            end_line: 20,
            signature: format!("void {name}()"),
            namespace: String::new(),
            parent: String::new(),
            language,
        }
    }

    #[test]
    fn language_serializes_lowercase() {
        let cases = [
            (Language::Cpp, "cpp"),
            (Language::Rust, "rust"),
            (Language::Go, "go"),
            (Language::Python, "python"),
            (Language::CSharp, "csharp"),
            (Language::Java, "java"),
        ];
        for (lang, expected) in cases {
            let v = serde_json::to_value(lang).unwrap();
            assert_eq!(v, Value::String(expected.to_string()));
            let back: Language = serde_json::from_value(v).unwrap();
            assert_eq!(back, lang);
        }
    }

    #[test]
    fn symbol_kind_serializes_lowercase_all_variants() {
        let cases = [
            (SymbolKind::Function, "function"),
            (SymbolKind::Method, "method"),
            (SymbolKind::Class, "class"),
            (SymbolKind::Struct, "struct"),
            (SymbolKind::Enum, "enum"),
            (SymbolKind::Typedef, "typedef"),
            (SymbolKind::Interface, "interface"),
            (SymbolKind::Trait, "trait"),
        ];
        for (kind, expected) in cases {
            let v = serde_json::to_value(kind).unwrap();
            assert_eq!(v, Value::String(expected.to_string()));
            let back: SymbolKind = serde_json::from_value(v).unwrap();
            assert_eq!(back, kind);
        }
    }

    #[test]
    fn edge_kind_serializes_lowercase_all_variants() {
        let cases = [
            (EdgeKind::Calls, "calls"),
            (EdgeKind::Includes, "includes"),
            (EdgeKind::Inherits, "inherits"),
        ];
        for (kind, expected) in cases {
            let v = serde_json::to_value(kind).unwrap();
            assert_eq!(v, Value::String(expected.to_string()));
            let back: EdgeKind = serde_json::from_value(v).unwrap();
            assert_eq!(back, kind);
        }
    }

    #[test]
    fn symbol_round_trip_omits_empty_namespace_and_parent() {
        let s = sample_symbol("foo", SymbolKind::Function, Language::Cpp);
        let v = serde_json::to_value(&s).unwrap();
        // Empty namespace and parent must NOT appear in output.
        let obj = v.as_object().expect("symbol serializes as object");
        assert!(
            !obj.contains_key("namespace"),
            "empty namespace must be elided, got: {v}"
        );
        assert!(
            !obj.contains_key("parent"),
            "empty parent must be elided, got: {v}"
        );

        // Required fields are present.
        for key in [
            "name",
            "kind",
            "file",
            "line",
            "column",
            "end_line",
            "signature",
            "language",
        ] {
            assert!(obj.contains_key(key), "missing required key: {key}");
        }

        // Round-trip preserves equality (deserializing fills the empty strings via #[serde(default)]).
        let back: Symbol = serde_json::from_value(v).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn symbol_round_trip_includes_populated_namespace_and_parent() {
        let s = Symbol {
            name: "do_thing".to_string(),
            kind: SymbolKind::Method,
            file: "src/widget.cpp".to_string(),
            line: 12,
            column: 0,
            end_line: 18,
            signature: "void Widget::do_thing()".to_string(),
            namespace: "ns::inner".to_string(),
            parent: "Widget".to_string(),
            language: Language::Cpp,
        };
        let v = serde_json::to_value(&s).unwrap();
        let obj = v.as_object().unwrap();
        assert_eq!(
            obj.get("namespace").and_then(|x| x.as_str()),
            Some("ns::inner")
        );
        assert_eq!(obj.get("parent").and_then(|x| x.as_str()), Some("Widget"));

        let back: Symbol = serde_json::from_value(v).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn symbol_round_trip_every_kind_and_language() {
        let kinds = [
            SymbolKind::Function,
            SymbolKind::Method,
            SymbolKind::Class,
            SymbolKind::Struct,
            SymbolKind::Enum,
            SymbolKind::Typedef,
            SymbolKind::Interface,
            SymbolKind::Trait,
        ];
        let languages = [
            Language::Cpp,
            Language::Rust,
            Language::Go,
            Language::Python,
            Language::CSharp,
            Language::Java,
        ];
        for (i, kind) in kinds.iter().enumerate() {
            for language in languages {
                let s = sample_symbol(&format!("sym_{i}"), *kind, language);
                let v = serde_json::to_value(&s).unwrap();
                let back: Symbol = serde_json::from_value(v).unwrap();
                assert_eq!(back, s);
            }
        }
    }

    #[test]
    fn edge_round_trip_every_kind() {
        for kind in [EdgeKind::Calls, EdgeKind::Includes, EdgeKind::Inherits] {
            let e = Edge {
                from: "src/a.cpp:foo".to_string(),
                to: "src/b.cpp:bar".to_string(),
                kind,
                file: "src/a.cpp".to_string(),
                line: 42,
            };
            let v = serde_json::to_value(&e).unwrap();
            let back: Edge = serde_json::from_value(v).unwrap();
            assert_eq!(back, e);
        }
    }

    #[test]
    fn file_graph_empty_collections_serialize_as_arrays_not_null() {
        let fg = FileGraph {
            path: "src/empty.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![],
            edges: vec![],
        };
        let v = serde_json::to_value(&fg).unwrap();
        // Critical wire-format invariant: empty Vecs MUST serialize as [] not null.
        assert_eq!(v["symbols"], json!([]));
        assert_eq!(v["edges"], json!([]));
        assert!(
            !v["symbols"].is_null(),
            "symbols must never serialize as null"
        );
        assert!(!v["edges"].is_null(), "edges must never serialize as null");

        let back: FileGraph = serde_json::from_value(v).unwrap();
        assert_eq!(back, fg);
    }

    #[test]
    fn file_graph_round_trip_populated() {
        let symbol = sample_symbol("main", SymbolKind::Function, Language::Cpp);
        let edge = Edge {
            from: "src/main.cpp:main".to_string(),
            to: "src/util.cpp:helper".to_string(),
            kind: EdgeKind::Calls,
            file: "src/main.cpp".to_string(),
            line: 7,
        };
        let fg = FileGraph {
            path: "src/main.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![symbol],
            edges: vec![edge],
        };
        let v = serde_json::to_value(&fg).unwrap();
        let obj = v.as_object().unwrap();
        for key in ["path", "language", "symbols", "edges"] {
            assert!(obj.contains_key(key), "missing key {key}");
        }
        assert_eq!(obj["language"], Value::String("cpp".to_string()));

        let back: FileGraph = serde_json::from_value(v).unwrap();
        assert_eq!(back, fg);
    }

    #[test]
    fn symbol_id_free_symbol_uses_path_colon_name() {
        // Mirrors the Go SymbolID fixture: free function → "file:Name".
        let s = Symbol {
            name: "free_fn".to_string(),
            kind: SymbolKind::Function,
            file: "src/util.cpp".to_string(),
            line: 1,
            column: 0,
            end_line: 3,
            signature: "void free_fn()".to_string(),
            namespace: String::new(),
            parent: String::new(),
            language: Language::Cpp,
        };
        assert_eq!(symbol_id(&s), "src/util.cpp:free_fn");
    }

    #[test]
    fn symbol_id_method_uses_parent_double_colon_name() {
        // Mirrors the Go SymbolID fixture: method → "file:Parent::Name".
        let s = Symbol {
            name: "do_it".to_string(),
            kind: SymbolKind::Method,
            file: "src/widget.cpp".to_string(),
            line: 10,
            column: 0,
            end_line: 12,
            signature: "void Widget::do_it()".to_string(),
            namespace: "ns".to_string(),
            parent: "Widget".to_string(),
            language: Language::Cpp,
        };
        assert_eq!(symbol_id(&s), "src/widget.cpp:Widget::do_it");
    }

    #[test]
    fn symbol_id_ignores_namespace_when_parent_empty() {
        // Go's graph.SymbolID only branches on Parent — a populated namespace
        // with empty parent must still produce "file:Name", not "file:ns::Name".
        let s = Symbol {
            name: "free_in_ns".to_string(),
            kind: SymbolKind::Function,
            file: "src/util.cpp".to_string(),
            line: 5,
            column: 0,
            end_line: 7,
            signature: "void acme::free_in_ns()".to_string(),
            namespace: "acme".to_string(),
            parent: String::new(),
            language: Language::Cpp,
        };
        assert_eq!(symbol_id(&s), "src/util.cpp:free_in_ns");
    }

    // -------------------------------------------------------------------
    // id_to_file recovery tests
    //
    // These tests pin the rightmost-not-part-of-`::` contract documented on
    // `id_to_file`. Phase 4 of PaginatedResponseSizeSafety references this
    // function as the canonical inverse of `symbol_id`; consumers must use it
    // rather than rolling their own split.
    // -------------------------------------------------------------------

    #[test]
    fn id_to_file_unix_free_function() {
        // Free function id format: "file:name" → file is everything before
        // the single separator colon.
        assert_eq!(id_to_file("/a/b.rs:foo"), "/a/b.rs");
    }

    #[test]
    fn id_to_file_unix_method() {
        // Method id format: "file:Parent::name" → the separator colon is the
        // rightmost colon NOT part of `::`. The `::` between Parent and name
        // must be skipped.
        assert_eq!(id_to_file("/a/b.rs:Foo::bar"), "/a/b.rs");
    }

    #[test]
    fn id_to_file_windows_path() {
        // Windows drive-letter colon (`C:\...`) must NOT be confused with the
        // separator. The drive colon's right neighbor is `\`, not `:`, so the
        // colon qualifies as a separator candidate — but it's never reached
        // because a rightmost qualifying colon exists between `b.cs` and `Baz`.
        assert_eq!(id_to_file(r"C:\a\b.cs:Baz::qux"), r"C:\a\b.cs");
    }

    #[test]
    fn id_to_file_malformed_no_separator() {
        // Defensive: id with no `:` at all → empty &str, NOT a panic.
        assert_eq!(id_to_file("noseparator"), "");
    }

    #[test]
    fn id_to_file_round_trip() {
        // Construct ids the same way symbol_id does and assert id_to_file
        // recovers the file path. Covers a representative spread of path
        // shapes (Unix abs, Unix rel, Windows abs, deeply nested).
        let cases = [
            ("/a/b.rs", "foo"),
            ("src/util.cpp", "do_thing"),
            (r"C:\Users\dev\widget.cs", "Render"),
            ("crates/code-graph-core/src/lib.rs", "id_to_file"),
        ];
        for (file, name) in cases {
            let id = format!("{file}:{name}");
            assert_eq!(id_to_file(&id), file, "round-trip failed for id={id}");
        }

        // Method form: "{file}:{parent}::{name}" must also round-trip.
        let method_cases = [
            ("/a/b.rs", "Foo", "bar"),
            ("src/widget.cpp", "Widget", "do_it"),
        ];
        for (file, parent, name) in method_cases {
            let id = format!("{file}:{parent}::{name}");
            assert_eq!(
                id_to_file(&id),
                file,
                "method-form round-trip failed for id={id}"
            );
        }
    }

    #[test]
    fn id_to_file_unix_filename_with_colon() {
        // Documents that the rightmost-single-colon rule is the contract,
        // not an accident. A filename containing `:` (legal on Unix) is
        // preserved in the file portion because the separator colon is to
        // its right.
        assert_eq!(
            id_to_file("/project/foo:bar.rs:func"),
            "/project/foo:bar.rs"
        );
    }
}
