---
title: "Adding Go, Python, and Rust Parsers"
type: research
status: active
created: 2026-03-22
updated: 2026-03-22
tags: [parser, go, python, rust, tree-sitter]
related: [Designs/CodeGraphMCP, Brainstorm/code-graph-mcp-architecture]
---

# Adding Go, Python, and Rust Parsers

## Context

The code-graph-mcp server currently supports C++ via tree-sitter. The Parser interface is pluggable by design. This research evaluates what it takes to add Go, Python, and Rust support.

## Findings

### Grammar Availability

All three have official tree-sitter grammars with Go bindings in the same pattern as `tree-sitter-cpp`:

| Language | Module | Version | Go Bindings |
|----------|--------|---------|-------------|
| Go | `github.com/tree-sitter/tree-sitter-go/bindings/go` | v0.25.0 | Yes |
| Python | `github.com/tree-sitter/tree-sitter-python/bindings/go` | v0.25.0 | Yes |
| Rust | `github.com/tree-sitter/tree-sitter-rust/bindings/go` | v0.24.1 | Yes |

All are ABI-compatible with `go-tree-sitter v0.25.0` already in the project. CGO is already required.

### Interface Changes Needed

**Parser interface:** No changes. `Extensions()`, `ParseFile()`, `Close()` work for all languages.

**New SymbolKind values:**
- `KindInterface` — Go interfaces, Rust traits
- `KindTrait` — Rust traits (if kept separate from interface)
- `KindModule` — Rust `mod_item` (optional)

**EdgeKind decision:** Use existing `EdgeIncludes` for imports (Go, Python, Rust all use module imports, not file includes). The `To` value is the import path string. `get_dependencies` works unchanged.

**Graph changes:** `ClassHierarchy` filter needs `KindInterface`/`KindTrait` added alongside `KindClass`/`KindStruct`.

### Per-Language Breakdown

#### Go (Effort: 0.6x of C++)

**Extensions:** `.go`

**Key node types:**
| Concept | Node Type | Query Pattern |
|---------|-----------|---------------|
| Function | `function_declaration` | `name: (identifier) @func.name` |
| Method | `method_declaration` | `name: (field_identifier) @method.name` |
| Struct | `type_spec > struct_type` | `name: (type_identifier) @struct.name` |
| Interface | `type_spec > interface_type` | `name: (type_identifier) @interface.name` |
| Call | `call_expression` | `function: (identifier) @call.name` |
| Method call | `call_expression > selector_expression` | `field: (field_identifier) @call.name` |
| Import | `import_spec` | `path: (interpreted_string_literal) @import.path` |

**Unique challenge:** Method receiver extraction. `method_declaration` has a `receiver` field containing a `parameter_list` with the receiver type. Must walk into this to set `Symbol.Parent`. No inheritance — Go interfaces are structural.

**Namespace:** Package name from `package_clause`.

#### Python (Effort: 0.7x of C++)

**Extensions:** `.py`, `.pyi`

**Key node types:**
| Concept | Node Type | Query Pattern |
|---------|-----------|---------------|
| Function | `function_definition` | `name: (identifier) @func.name` |
| Class | `class_definition` | `name: (identifier) @class.name` |
| Base class | `class_definition > argument_list` | `(identifier) @base.name` |
| Call | `call` (not `call_expression`) | `function: (identifier) @call.name` |
| Method call | `call > attribute` | `attribute: (identifier) @call.name` |
| Import | `import_statement` | `name: (dotted_name) @import.path` |
| From-import | `import_from_statement` | `module_name: (dotted_name) @import.module` |

**Unique challenges:**
- `decorated_definition` wraps functions and classes — query still matches inner node
- Methods vs free functions distinguished by checking for enclosing `class_definition` (same technique as C++ inline methods)
- Two import forms: `import foo` and `from foo import bar`
- `__init__` is the constructor convention

#### Rust (Effort: 1.2x of C++)

**Extensions:** `.rs`

**Key node types:**
| Concept | Node Type | Query Pattern |
|---------|-----------|---------------|
| Function | `function_item` | `name: (identifier) @func.name` |
| Method | `function_item` inside `declaration_list` | Same — context distinguishes |
| Struct | `struct_item` | `name: (type_identifier) @struct.name` |
| Enum | `enum_item` | `name: (type_identifier) @enum.name` |
| Trait | `trait_item` | `name: (type_identifier) @trait.name` |
| Impl block | `impl_item` | `type: (type_identifier)`, `trait: (type_identifier)` |
| Call | `call_expression` | `function: (identifier) @call.name` |
| Path call | `call_expression` | `function: (scoped_identifier) @call.qname` |
| Macro call | `macro_invocation` | `macro: (identifier) @call.name` |
| Use | `use_declaration` | Complex — `use_tree` can be nested |
| Trait impl | `impl_item` with `trait:` | Both `trait` and `type` fields |

**Unique challenges:**
- `impl_item` context walking to set Parent (analogous to C++ class body)
- `use_declaration` has nested `use_list` for `use foo::{Bar, Baz}` — needs recursive walk
- Trait impl produces inheritance edges: `impl Trait for Type`
- Macros (`macro_invocation`) should produce call edges

### Effort Estimates

| Language | Effort | Why |
|----------|--------|-----|
| **Go** | 0.6x | Simple grammar, no preprocessor/templates/overloading. One tricky part: receiver type extraction. |
| **Python** | 0.7x | Simple grammar but dual import forms, decorated definitions, method/function distinction by context. |
| **Rust** | 1.2x | Most complex: impl blocks, use tree traversal, trait impls, macro tracking. |

All are easier than C++ was because the architecture, patterns, and test harness already exist.

### Recommended Order

1. **Go** — simplest, immediately useful for this project itself
2. **Python** — well-understood, lots of test code available
3. **Rust** — most complex, patterns from Go/Python will be solidified

### File Structure Per Language

```
internal/lang/{go,python,rust}/
  {lang}.go       # Parser struct, NewXParser(), ParseFile(), extraction methods
  queries.go      # Tree-sitter query string constants
  {lang}_test.go  # Unit tests with C++ test corpus pattern
```

Plus registration in `cmd/code-graph-mcp/main.go` and testdata projects.

## Open Questions

1. **`EdgeIncludes` vs `EdgeImports`** — reuse or add new kind? Reusing is simpler; adding is cleaner semantically.
2. **`KindInterface` vs `KindTrait`** — unify or keep separate? Both work for `ClassHierarchy`.
3. **Go generics (1.18+)** — does `tree-sitter-go v0.25.0` parse `func Foo[T any]()` correctly?
4. **Python `decorated_definition`** — verify `findEnclosingKind` doesn't get confused by decorator wrappers.
5. **Rust `use_list` expansion** — how deep to recurse? `use std::{io::{self, Read}, collections::HashMap}` is valid.
