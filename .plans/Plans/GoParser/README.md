---
title: "Go Language Parser"
type: plan
status: draft
created: 2026-03-22
updated: 2026-03-22
tags: [parser, go, tree-sitter]
related: [Research/multi-language-parsers, Plans/CodeGraphMCP]
phases:
  - id: 1
    title: "Shared Foundation & Go Parser Core"
    status: planned
    doc: "01-Foundation-And-Core.md"
  - id: 2
    title: "Go Test Project & Validation"
    status: planned
    doc: "02-Test-Project-And-Validation.md"
    depends_on: [1]
  - id: 3
    title: "Integration & Registration"
    status: planned
    doc: "03-Integration-And-Registration.md"
    depends_on: [2]
---

# Go Language Parser

## Overview

Add Go language support to code-graph-mcp. This is the first non-C++ parser and also introduces shared type additions (KindInterface) used by subsequent language parsers.

## Architecture

```mermaid
graph TD
    subgraph "Phase 1: Foundation & Core"
        Types["Add KindInterface to types.go"]
        Graph["Update ClassHierarchy filter"]
        GoParser["GoParser struct + queries"]
        Extract["Extraction methods"]
    end

    subgraph "Phase 2: Test Project"
        TestData["testdata/go/ project"]
        UnitTests["Unit test corpus"]
        CLI["Validate via parse-test"]
    end

    subgraph "Phase 3: Integration"
        Register["Register in main.go"]
        IntTests["MCP tool integration tests"]
        Docs["Update README + CLAUDE.md"]
    end

    Types --> Graph --> GoParser --> Extract
    Extract --> TestData --> UnitTests --> CLI
    CLI --> Register --> IntTests --> Docs
```

## Key Decisions

- **Reuse `EdgeIncludes`** for Go imports — semantically different from C++ `#include` but functionally equivalent for the graph. `To` value is the import path string (e.g., `"fmt"`, `"os/exec"`).
- **No inheritance edges** for Go — interfaces are structural. `ClassHierarchy` will return the interface but no bases/derived since that requires type checking.
- **Package name as Namespace** — extracted from `package_clause`.
- **Method receiver as Parent** — `method_declaration`'s receiver type becomes `Symbol.Parent`.

## Dependencies

- `github.com/tree-sitter/tree-sitter-go/bindings/go` v0.25.0
- Existing `go-tree-sitter v0.25.0` runtime (already in project)
