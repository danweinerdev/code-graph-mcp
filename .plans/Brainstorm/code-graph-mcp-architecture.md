---
title: "Code Graph MCP Server Architecture"
type: brainstorm
status: draft
created: 2026-03-22
updated: 2026-03-22
tags: [architecture, mcp, code-graph, tree-sitter, golang]
related: []
---

# Code Graph MCP Server Architecture

## Problem Statement

AI agents working with large codebases waste significant time and tokens performing exhaustive file-by-file searches to understand code structure, dependencies, and call graphs. We need an MCP server that builds and maintains a semantic code graph, enabling agents to query relationships (callers, callees, dependencies, inheritance, etc.) in real time.

**Constraints:**
- Must be written in Go, following the same patterns as `lldb-debug-mcp` (mcp-go, stdio transport, `internal/` layout)
- Must support C++ first, but the parser layer must be pluggable for future languages
- Must handle large legacy codebases efficiently
- The graph must be queryable in real time without re-parsing the entire codebase on each request
- Local use only — stdio transport, no cloud/HTTP mode

**Reference:** [LegacyGraph-MCP](https://github.com/RohitYadav34980/LegacyGraph-MCP) — Python implementation using tree-sitter + NetworkX, exposing tools like `get_callers`, `get_callees`, `detect_cycles`, `get_orphan_functions`, `generate_mermaid_graph`.

---

## Decision Area 1: Parsing Strategy

### Idea 1A: go-tree-sitter (CGo bindings)

**Description:** Use [go-tree-sitter](https://github.com/tree-sitter/go-tree-sitter) — the official Go bindings for tree-sitter. Produces full ASTs from source files. We write tree-sitter queries (S-expression patterns) to extract function definitions, call sites, class/struct declarations, includes, etc. Each language gets its own query file and extractor.

**Pros:**
- Battle-tested parser used by GitHub, Neovim, Zed, and the reference project
- Incremental parsing — can re-parse only changed regions
- C++ grammar handles macros, templates, preprocessor directives
- Rich query language (S-expressions) lets us declaratively extract nodes
- 100+ language grammars available — natural path to multi-language support
- Each new language = new grammar + new query file, no new parser code

**Cons:**
- CGo dependency adds build complexity (requires C toolchain)
- C++ grammar is large and can be slow on very large files
- Query authoring requires learning tree-sitter's S-expression syntax
- Some C++ edge cases (heavily macro'd code, complex template metaprogramming) may produce incomplete ASTs

**Effort:** Medium

### Idea 1B: libclang via CGo

**Description:** Use libclang's C API through CGo bindings to parse C++ with full semantic understanding. Libclang understands the C++ type system, template instantiation, name lookup, and produces a semantically-rich AST.

**Pros:**
- Clang understands C++ completely — templates, SFINAE, ADL, overload resolution
- Produces semantic information (resolved types, template instantiations) not just syntax
- Can resolve `#include` dependencies accurately
- The "gold standard" for C++ tooling

**Cons:**
- Heavy dependency — requires libclang installed on the system
- Single-language: only C/C++/Objective-C — no path to Go, Python, Rust, etc.
- Significantly slower than tree-sitter (full semantic analysis)
- Requires compilation database (`compile_commands.json`) for accurate results
- CGo bindings for libclang are poorly maintained in the Go ecosystem
- **Fundamentally violates the "pluggable parser" requirement**

**Effort:** High

### Idea 1C: ctags/cscope external process

**Description:** Shell out to Universal Ctags or cscope to generate symbol indices, then parse the output to build the graph. These tools produce flat symbol tables (function definitions, references, etc.).

**Pros:**
- No CGo — pure Go, just exec an external process
- ctags is fast and widely available
- Simple to implement initially

**Cons:**
- Coarse-grained: ctags gives definitions but not call relationships
- cscope gives callers/callees but is C-only and poorly maintained
- Requires external tool installation
- No AST — can't extract structural info like class hierarchies, template parameters
- Limited extensibility to other languages
- Results are less accurate than AST-based approaches

**Effort:** Low

### Idea 1D: LSP Client

**Description:** Act as an LSP client, connecting to language servers (clangd for C++, gopls for Go, etc.) to extract references, definitions, call hierarchies, and type information.

**Pros:**
- Language servers provide semantically-accurate results
- Naturally pluggable: swap the language server for each language
- Call hierarchy, references, and type hierarchy are standard LSP methods
- Language servers are well-maintained by their respective communities

**Cons:**
- LSP protocol is request/response — no bulk "give me everything" operation
- Building a full graph requires O(N) requests (one per symbol), which is slow
- Language servers need a workspace and may require build system integration
- Startup latency: language servers need to index the project first
- Heavyweight dependency for each language
- Fragile: different servers implement different subsets of the protocol

**Effort:** High

---

## Decision Area 2: Graph Storage

### Idea 2A: In-Memory Directed Graph (Custom)

**Description:** Build a simple in-memory directed graph using Go maps. Nodes are symbols (functions, classes, files). Edges are relationships (calls, includes, inherits). Serialize to disk as JSON/gob for persistence between sessions.

**Pros:**
- Zero dependencies
- Fastest possible query performance (everything in RAM)
- Simple to implement and debug
- Full control over data structures and query patterns
- Easy to serialize/deserialize for persistence
- Matches the reference project's approach (NetworkX is in-memory)

**Cons:**
- Memory usage scales with codebase size (typically fine — even 100K functions < 100MB)
- Must implement graph algorithms ourselves (cycle detection, transitive closure, topological sort)
- No built-in query language

**Effort:** Medium

### Idea 2B: Embedded Graph Database (Cayley / BoltDB-backed)

**Description:** Use an embedded graph database like [Cayley](https://github.com/cayleygraph/cayley) or build a simple triple store on top of BoltDB/Badger for persistent, queryable graph storage.

**Pros:**
- Built-in persistence without manual serialization
- Cayley offers Gremlin/GraphQL query languages
- Can handle very large graphs that don't fit in memory

**Cons:**
- Cayley is effectively unmaintained (last release 2020)
- Adds significant dependency weight
- Query overhead vs. in-memory (disk I/O, serialization)
- Over-engineered for the expected graph sizes
- BoltDB-backed triple store still requires custom graph algorithm implementation

**Effort:** High

### Idea 2C: SQLite with Recursive CTEs

**Description:** Store nodes and edges in SQLite tables. Use recursive CTEs for graph traversal queries (transitive callers/callees, dependency chains). Use `modernc.org/sqlite` for pure-Go SQLite.

**Pros:**
- Pure Go via modernc.org/sqlite (no CGo)
- Built-in persistence, ACID transactions
- Recursive CTEs handle transitive closure, reachability
- SQL is well-understood; easy to add new query types
- Good tooling for debugging (can open DB with any SQLite client)

**Cons:**
- SQL is awkward for graph operations beyond basic traversal
- Recursive CTEs can be slow on deep graphs
- More complex schema management than in-memory maps
- Slight overhead compared to pure in-memory

**Effort:** Medium

---

## Decision Area 3: Language Plugin Architecture

### Idea 3A: Interface-Based Plugin (Compile-Time)

**Description:** Define a Go `Parser` interface that each language implements. Language parsers are compiled into the binary. A registry maps file extensions to parsers.

```go
type Parser interface {
    Extensions() []string
    ParseFile(path string, content []byte) (*FileGraph, error)
}
```

**Pros:**
- Simple, idiomatic Go
- Type-safe at compile time
- No runtime overhead
- Easy to test each parser independently
- Follows Go stdlib patterns (e.g., `database/sql` driver registration)

**Cons:**
- Adding a new language requires recompilation
- All language grammars bundled in the binary (larger binary)

**Effort:** Low

### Idea 3B: Plugin System (hashicorp/go-plugin)

**Description:** Use HashiCorp's go-plugin to run language parsers as separate processes communicating over gRPC. Users can add new languages by dropping in a plugin binary.

**Pros:**
- True runtime extensibility — add languages without recompiling the server
- Process isolation — a parser crash doesn't take down the MCP server
- Can be written in any language (not just Go)

**Cons:**
- Massive complexity increase (gRPC, process management, plugin discovery)
- Serialization overhead for AST data across process boundaries
- Much harder to debug
- Overkill for the foreseeable future (we'll have < 5 languages)

**Effort:** High

### Idea 3C: Tree-Sitter Query Files (Data-Driven)

**Description:** If using tree-sitter (Idea 1A), each language is defined by: (1) its grammar `.so`, and (2) a set of `.scm` query files that extract symbols and relationships. A single generic extractor runs queries against any grammar. Adding a language = adding query files.

**Pros:**
- Extremely lightweight language addition — just query files, no Go code
- Single extractor implementation shared across all languages
- Query files are declarative and easy to audit
- Natural fit if tree-sitter is chosen for parsing

**Cons:**
- Only viable if tree-sitter is the parsing backend
- Complex relationships may be hard to express in pure S-expression queries
- May need some Go "post-processing" code per language for edge cases

**Effort:** Low (given tree-sitter choice)

---

## Decision Area 4: MCP Tools to Expose

Based on the reference project and the goal of enabling efficient agent queries:

### Core Query Tools
| Tool | Description | Priority |
|------|-------------|----------|
| `analyze_codebase` | Index a directory, build the graph | P0 |
| `get_file_symbols` | List all symbols (functions, classes, etc.) in a file | P0 |
| `get_callers` | Who calls this function? (upstream) | P0 |
| `get_callees` | What does this function call? (downstream) | P0 |
| `get_dependencies` | File-level dependency graph (#include / import) | P0 |
| `search_symbols` | Find symbols by name/pattern | P0 |
| `get_symbol_detail` | Full signature, location, doc comment for a symbol | P0 |

### Structural Analysis Tools
| Tool | Description | Priority |
|------|-------------|----------|
| `detect_cycles` | Find circular dependency chains | P1 |
| `get_orphans` | Functions/classes with no callers | P1 |
| `get_class_hierarchy` | Inheritance tree for a class | P1 |
| `get_coupling` | Cross-file coupling metrics | P1 |

### Visualization Tools
| Tool | Description | Priority |
|------|-------------|----------|
| `generate_mermaid` | Bounded Mermaid diagram of a subgraph | P2 |

---

## Evaluation

### Parsing Strategy Matrix

| Criteria | 1A: tree-sitter | 1B: libclang | 1C: ctags | 1D: LSP |
|----------|-----------------|--------------|-----------|---------|
| C++ accuracy | High | Highest | Low | High |
| Multi-language | Excellent | None | Limited | Good |
| Performance | Fast | Slow | Fastest | Slow |
| Build complexity | Medium (CGo) | High (CGo + libclang) | Low | Medium |
| Incremental update | Yes | No | No | Partial |
| Ecosystem maturity | Excellent | Good (C++ only) | Stale | Varies |

### Graph Storage Matrix

| Criteria | 2A: In-Memory | 2B: Embedded DB | 2C: SQLite |
|----------|---------------|-----------------|------------|
| Query speed | Fastest | Medium | Fast |
| Implementation effort | Medium | High | Medium |
| Persistence | Manual (JSON/gob) | Built-in | Built-in |
| Dependencies | None | Heavy | One (pure Go) |
| Debugging | Print maps | Complex | SQL client |
| Scalability | ~100K nodes fine | Unlimited | Very large |

### Plugin Architecture Matrix

| Criteria | 3A: Interface | 3B: go-plugin | 3C: Query Files |
|----------|---------------|---------------|-----------------|
| Simplicity | High | Low | High |
| Extensibility | Recompile | Runtime | Data files |
| Effort | Low | High | Low |
| Type safety | Compile-time | gRPC schema | N/A |

---

## Recommendation

**Parsing: 1A (go-tree-sitter)** — Best balance of accuracy, multi-language extensibility, and performance. The CGo cost is acceptable (tree-sitter's C core is small and stable). This is the proven approach used by the reference project and major code intelligence tools.

**Graph Storage: 2A (In-Memory) with JSON persistence** — Matches the reference project's approach, keeps things simple, and is fast enough for any realistic codebase. Graph algorithms (BFS, DFS, cycle detection via Tarjan's) are straightforward to implement. If we later need persistence, we can add SQLite as a cache layer without changing the query interface.

**Plugin Architecture: 3A (Interface-Based) + 3C (Query Files)** — Define a Go `Parser` interface for type safety and testability. For tree-sitter-backed parsers, implement a generic `TreeSitterParser` that is parameterized by grammar + query files (Idea 3C). This gives us the best of both: compile-time interfaces for the Go layer, and data-driven language addition for the tree-sitter layer.

### Recommended Architecture

```
AI Agent <-stdio/MCP-> [Go MCP Server (mcp-go)]
                              |
                     +--------+--------+
                     |                 |
              [Tool Handlers]    [Graph Engine]
              (internal/tools)   (internal/graph)
                     |                 |
              [Parser Registry]   [In-Memory Graph]
              (internal/parser)   Nodes + Edges + Algorithms
                     |
              +------+------+
              |             |
         [C++ Parser]  [Future: Go, Rust, ...]
         tree-sitter    tree-sitter
         + cpp.scm      + lang.scm
```

### Why This Combination Works

1. **Follows established patterns**: Same `mcp-go` + `internal/` structure as `lldb-debug-mcp`
2. **Proven approach**: tree-sitter + in-memory graph mirrors the reference project, translated to Go
3. **Pluggable by design**: Adding Go support = `go.scm` query file + `NewTreeSitterParser(goLang, goQueries)`
4. **Token-efficient for agents**: Structured graph queries replace file-by-file grep
5. **Incremental-ready**: tree-sitter supports incremental parsing; the graph can be updated without full rebuilds

---

## Next Steps

1. Create a technical design document (`Designs/code-graph-mcp.md`) detailing the graph schema, parser interface, and tool APIs
2. Scaffold the Go project structure following `lldb-debug-mcp` patterns
3. Implement the C++ tree-sitter parser with basic symbol extraction
4. Build the in-memory graph engine with core algorithms
5. Wire up P0 MCP tools and test against a sample C++ project
