---
title: "MCP Server & P0 Tools"
type: phase
plan: CodeGraphMCP
phase: 5
status: planned
created: 2026-03-22
updated: 2026-03-22
deliverable: "Working MCP server with analyze_codebase and all P0 query tools"
tasks:
  - id: "5.1"
    title: "Entry point and server setup"
    status: planned
    verification: "Binary starts, serves stdio MCP, and responds to tool list requests"
  - id: "5.2"
    title: "Tools struct, Register, state guards"
    status: planned
    verification: "All P0 tools registered; requireIndexed guard returns error before analyze_codebase is called"
  - id: "5.3"
    title: "analyze_codebase handler with worker pool"
    status: planned
    verification: "Walks directory, parses files concurrently, builds graph, returns summary"
  - id: "5.4"
    title: "P0 query tool handlers"
    status: planned
    verification: "get_file_symbols, get_callers, get_callees, get_dependencies, search_symbols, get_symbol_detail all return correct JSON"
  - id: "5.5"
    title: "Integration tests"
    status: planned
    verification: "End-to-end: analyze_codebase on testdata, then query tools return expected results"
---

# Phase 5: MCP Server & P0 Tools

## Overview

Wire the parser (Phase 2-3) and graph engine (Phase 4) into a working MCP server. To be broken down in detail before implementation.

## Acceptance Criteria
- [ ] MCP server starts and serves tools over stdio
- [ ] analyze_codebase indexes a directory and returns a summary
- [ ] All P0 query tools return correct results
- [ ] Integration tests pass
