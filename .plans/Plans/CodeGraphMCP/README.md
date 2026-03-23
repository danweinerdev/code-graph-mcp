---
title: "Code Graph MCP Server"
type: plan
status: complete
created: 2026-03-22
updated: 2026-03-22
tags: [mcp, code-graph, tree-sitter, golang, cpp]
related: [Designs/CodeGraphMCP, Brainstorm/code-graph-mcp-architecture]
phases:
  - id: 1
    title: "Scaffold & Core Types"
    status: complete
    doc: "01-Scaffold-And-Core-Types.md"
  - id: 2
    title: "C++ Parser"
    status: complete
    doc: "02-Cpp-Parser.md"
    depends_on: [1]
  - id: 3
    title: "Real-World Validation"
    status: complete
    doc: "03-Real-World-Validation.md"
    depends_on: [2]
  - id: 4
    title: "Graph Engine"
    status: complete
    doc: "04-Graph-Engine.md"
    depends_on: [1]
  - id: 5
    title: "MCP Server & P0 Tools"
    status: complete
    doc: "05-MCP-Server-And-P0-Tools.md"
    depends_on: [3, 4]
  - id: 6
    title: "P1 Tools & Polish"
    status: complete
    doc: "06-P1-Tools-And-Polish.md"
    depends_on: [5]
---

# Code Graph MCP Server

## Overview

Build a Go MCP server that constructs an in-memory semantic code graph from source files using tree-sitter, exposing graph query tools over stdio. Enables AI agents to query callers, callees, dependencies, and class hierarchies in real time instead of exhaustive file searching.

The plan is structured so that the parser layer is built and validated against real-world C++ code **before** the MCP integration is wired up. This ensures query pattern accuracy before anything depends on it.

## Architecture

```mermaid
graph TD
    subgraph "Phase 1: Scaffold"
        Types["Core Types<br/>(parser.Symbol, Edge, FileGraph)"]
        Interface["Parser Interface"]
        Registry["Parser Registry"]
    end

    subgraph "Phase 2: C++ Parser"
        CPP["CppParser<br/>(tree-sitter)"]
        Queries["Query Patterns<br/>(definitions, calls, includes, inheritance)"]
    end

    subgraph "Phase 3: Validation Gate"
        Harness["CLI Test Harness"]
        RealCode["Real-World C++ Projects"]
    end

    subgraph "Phase 4: Graph Engine"
        Graph["In-Memory Graph"]
        Algo["Algorithms<br/>(BFS, Tarjan, orphan)"]
    end

    subgraph "Phase 5: MCP Integration"
        Server["MCP Server"]
        Tools["P0 Tool Handlers"]
        Analyze["analyze_codebase<br/>(worker pool)"]
    end

    subgraph "Phase 6: P1 & Polish"
        P1["P1 Tools"]
        Polish["Error polish, CLAUDE.md"]
    end

    Types --> Interface
    Interface --> Registry
    Registry --> CPP
    CPP --> Queries
    Queries --> Harness
    Harness --> RealCode
    Types --> Graph
    Graph --> Algo
    RealCode --> Server
    Algo --> Server
    Server --> Tools
    Tools --> Analyze
    Server --> P1
    P1 --> Polish
```

## Key Decisions

- **Parser-first development:** Phases 2-3 are fully testable without MCP. A CLI harness lets us inspect parsed output and fix queries before building the graph or server.
- **Phase 4 parallel to Phase 2-3:** The graph engine depends only on Phase 1 types, not on the parser implementation. It can be developed in parallel with parser validation.
- **Validation gate (Phase 3):** Explicit checkpoint where parser accuracy is confirmed against real C++ code. No MCP work starts until this passes.

## Dependencies

- `github.com/mark3labs/mcp-go` v0.45+ — MCP server framework
- `github.com/tree-sitter/go-tree-sitter` — Official tree-sitter Go bindings (CGo)
- `github.com/tree-sitter/tree-sitter-cpp/bindings/go` — C++ grammar
- CGo toolchain (C compiler) required at build time
