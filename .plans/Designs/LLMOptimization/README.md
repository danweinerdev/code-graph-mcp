---
title: "LLM-Optimized Query Output"
type: design
status: implemented
created: 2026-03-23
updated: 2026-04-28
tags: [optimization, llm, token-efficiency, usability]
related: [Designs/CodeGraphMCP]
---

# LLM-Optimized Query Output

## Overview

Feedback from real-world usage on a 8,908-file / 100K-symbol C++ codebase revealed that query outputs are too verbose for LLM consumption. Signatures contain full class bodies (200+ chars), results lack namespace filtering, and there's no way to get a high-level codebase orientation without reading all symbols.

This design covers 7 changes ordered by impact, all following the principle established by the `generate_diagram` refactor: **default to the LLM-optimized format, keep the verbose version behind a flag.**

## Changes

### 1. Signature Truncation (Priority: Highest)

**Problem:** The `Signature` field often contains 200+ characters including the full body `{ ... }`. For a class, this can be the entire class body. LLMs waste tokens parsing signatures they don't need.

**Change:** Truncate signatures at the opening `{` or `;` — whichever comes first. Keep only the declaration line.

**Implementation:**
- Modify `truncate()` in `internal/lang/cpp/cpp.go`
- Instead of truncating at byte count, find the first `{` or `;` and truncate there
- If neither found within 200 chars, truncate at 200 chars with `...`
- This is a parser-level change — affects all languages

```go
func truncateSignature(s string) string {
    for i, c := range s {
        if c == '{' || c == ';' {
            return strings.TrimRight(s[:i], " \t\n")
        }
        if i >= 200 {
            return s[:200] + "..."
        }
    }
    return s
}
```

**Affected files:** `internal/lang/cpp/cpp.go` (and future parsers)

**Testing:** Existing tests verify Signature field — update to expect truncated output. Add test for class body truncation, function with long parameter list, one-liner function.

---

### 2. Namespace Filter on search_symbols (Priority: High)

**Problem:** Searching for "Session" returns results from HTTP, Database, NFS, etc. namespaces. No way to scope by namespace.

**Change:** Add `namespace` parameter to `search_symbols` — substring match against `Symbol.Namespace`.

**Implementation:**
- Add `namespace` param to tool registration in `tools.go`
- Pass to `Graph.SearchSymbols()` — add `namespace string` parameter
- Filter: `strings.Contains(strings.ToLower(symbol.Namespace), strings.ToLower(namespace))`

```go
// Updated signature
func (g *Graph) SearchSymbols(pattern string, kind SymbolKind, namespace string) []Symbol
```

**Affected files:** `internal/graph/graph.go`, `internal/tools/symbols.go`, `internal/tools/tools.go`

**Testing:** Test with namespace filter returns only matching namespace. Test case-insensitive. Test empty namespace (no filter).

---

### 3. Brief Mode on search_symbols (Priority: High)

**Problem:** Search results include full signature, column, end_line for every symbol. LLMs usually just need name, file, line, kind to decide what to look at next.

**Change:** Add `brief` boolean parameter (default `true`). When brief, return only: `id`, `name`, `kind`, `file`, `line`, `namespace`, `parent`. When `brief=false`, include full detail (signature, column, end_line).

**Implementation:**
- Add `brief` param to tool registration
- In `handleSearchSymbols`, conditionally include/exclude fields
- Create a `briefSymbolResult` struct or use `omitempty` on optional fields

```go
type symbolResult struct {
    ID        string `json:"id"`
    Name      string `json:"name"`
    Kind      string `json:"kind"`
    File      string `json:"file"`
    Line      int    `json:"line"`
    Column    int    `json:"column,omitempty"`     // omitted in brief mode
    EndLine   int    `json:"end_line,omitempty"`   // omitted in brief mode
    Signature string `json:"signature,omitempty"`  // omitted in brief mode
    Namespace string `json:"namespace,omitempty"`
    Parent    string `json:"parent,omitempty"`
}
```

**Affected files:** `internal/tools/symbols.go`, `internal/tools/tools.go`

**Testing:** Test brief mode omits signature/column/end_line. Test brief=false includes everything.

---

### 4. Pagination on search_symbols (Priority: Medium)

**Problem:** Hardcoded cap of 100 results. No way to page through or set limit.

**Change:** Add `limit` (default 20) and `offset` (default 0) parameters.

**Implementation:**
- Add params to tool registration
- Pass to `Graph.SearchSymbols()` — replace hardcoded 100 with limit
- Apply offset by skipping first N matches
- Return total match count in response alongside results

```json
{"results": [...], "total": 147, "offset": 0, "limit": 20}
```

**Affected files:** `internal/graph/graph.go`, `internal/tools/symbols.go`, `internal/tools/tools.go`

**Testing:** Test limit caps results. Test offset skips. Test total count reflects all matches.

---

### 5. Top-Level-Only Filter on get_file_symbols (Priority: Medium)

**Problem:** Returns all symbols including nested structs, methods, typedefs. Noisy when you want the class-level overview.

**Change:** Add `top_level_only` boolean parameter (default `false`). When true, filter to symbols where `Parent == ""`. Also adds `brief` parameter (default `true`) consistent with Decision 1 — `get_file_symbols` is LLM-facing too.

**Implementation:**
- Add `top_level_only` and `brief` params to tool registration
- In `handleGetFileSymbols`, filter results where `Parent == ""`; pass `brief` through to `symbolToResult`
- This is a handler-level filter — no graph engine change needed

**Affected files:** `internal/tools/symbols.go`, `internal/tools/tools.go`

**Testing:** Test top_level_only=true excludes methods and nested types. Test default includes all.

---

### 6. Symbol Summary by Namespace (Priority: Medium)

**Problem:** No way to get a high-level orientation of a codebase — how many classes, functions, enums per namespace?

**Change:** New tool `get_symbol_summary` that returns counts grouped by namespace and kind.

**Implementation:**
- New handler in `internal/tools/symbols.go`
- Iterate all nodes in graph, group by namespace, count by kind
- Register in `tools.go`

```json
{
  "Ark::Nfs::V4": {"class": 15, "function": 120, "enum": 8, "struct": 3},
  "Ark::Nfs::V4::Internal": {"class": 5, "function": 45},
  "": {"function": 12, "class": 2}
}
```

**Graph method:**
```go
// file is optional; pass "" for whole-graph summary.
func (g *Graph) SymbolSummary(file string) map[string]map[SymbolKind]int
```

**Affected files:** `internal/graph/graph.go`, `internal/tools/symbols.go`, `internal/tools/tools.go`

**Testing:** Test returns correct counts per namespace. Test empty graph returns empty map.

---

### 7. Class Hierarchy Depth (Priority: Low)

**Problem:** `ClassHierarchy` returns only direct bases and derived. No transitive traversal.

**Change:** Add `depth` parameter (default 1). Walk inheritance edges transitively up to N levels, reusing the BFS pattern from `DiagramInheritance`.

**Implementation:**
- Add `depth` param to tool registration and handler
- Modify `Graph.ClassHierarchy()` to accept depth and BFS through inheritance edges
- Build tree structure recursively up to depth

**Affected files:** `internal/graph/graph.go`, `internal/tools/structure.go`, `internal/tools/tools.go`

**Testing:** Test depth=1 returns direct only. Test depth=2 returns grandparent/grandchild. Test cycle safety.

---

## Design Decisions

### Decision 1: Brief Mode Default

**Context:** Should `brief` default to true or false?

**Decision:** Default `true`. LLMs are the primary consumer and rarely need full signatures in search results. Agents that need detail can call `get_symbol_detail` for a specific symbol.

**Rationale:** Matches the generate_diagram precedent — default to LLM-optimized, verbose behind a flag.

### Decision 2: Signature Truncation Location

**Context:** Where to truncate — parser level or handler level?

**Decision:** Parser level. Change `truncateSignature()` to stop at `{` or `;`.

**Rationale:** The full body is never useful in a signature field. Truncating at the parser means all consumers (search, file symbols, detail) benefit without per-handler logic. The `get_symbol_detail` tool can add a separate `body` field later if full source is needed.

### Decision 3: Symbol Summary Granularity

**Context:** Should summary be per-namespace, per-file, or both?

**Decision:** Per-namespace. Add optional `file` parameter to scope to one file.

**Rationale:** Namespace-first matches how developers think about large codebases. Per-file is already served by `get_file_symbols`.

---

## Error Handling

No new error categories. Existing patterns apply:
- Invalid parameters → `mcp.NewToolResultError`
- Unknown namespace → return empty results (not an error)
- Pagination beyond results → return empty array with correct total

---

## Testing Strategy

Each change has isolated tests. Additionally:
- Integration test: analyze testdata, search with namespace filter + brief mode + limit, verify token count reduction
- Regression: all existing tests must pass unchanged (except signature truncation updates)

### Structural Verification
- `go vet ./...` after each change
- `go test -race ./...` after each change

---

## Migration / Rollout

All changes are backward-compatible additions (new parameters with defaults that preserve current behavior) except:

**Breaking change: Signature truncation** — existing consumers that depend on full class bodies in the `signature` field will see shorter values. This is intentional and desirable. The `get_symbol_detail` tool still provides the full symbol for cases where detail is needed.

**Rollout order:** Follow the priority ranking. Each change is independently deployable.

1. Signature truncation — parser change, rebuild
2. Namespace filter — graph + handler change
3. Brief mode — handler change
4. Pagination — graph + handler change
5. Top-level filter — handler change
6. Symbol summary — new tool
7. Hierarchy depth — graph + handler change

---

## Post-implementation fixes

Issues caught in code review and fixed in the same diff:

- **`truncateSignature` UTF-8 boundary** — fallback used `s[:200]`, which sliced through multi-byte runes. Fixed to `s[:i] + "..."` (`i` is always a rune boundary).
- **`buildHierarchy` diamond inheritance** — a global `visited` map caused shared ancestors to be stubbed on the second branch. Replaced with per-DFS-path tracking so siblings each fully expand a shared ancestor; true cycles are still broken.
- **`handleGetFileSymbols` `null` vs `[]`** — uninitialized slice serialized as `null` when filtered to empty. Now uses `make([]symbolResult, 0, len(symbols))`.
- **`suggestSymbols` candidate cap** — `Search` defaults `Limit` to 20, which silently shrank the pre-existing 100-candidate pool used by did-you-mean. Now passes `Limit: 100` explicitly.
