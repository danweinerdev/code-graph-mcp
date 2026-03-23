---
title: "Python Language Parser"
type: plan
status: draft
created: 2026-03-22
updated: 2026-03-22
tags: [parser, python, tree-sitter]
related: [Research/multi-language-parsers, Plans/GoParser]
phases:
  - id: 1
    title: "Python Parser Core"
    status: planned
    doc: "01-Parser-Core.md"
  - id: 2
    title: "Python Test Project & Validation"
    status: planned
    doc: "02-Test-Project-And-Validation.md"
    depends_on: [1]
  - id: 3
    title: "Integration & Registration"
    status: planned
    doc: "03-Integration-And-Registration.md"
    depends_on: [2]
---

# Python Language Parser

## Overview

Add Python language support to code-graph-mcp. Python's dynamic nature means call resolution is noisier than statically-typed languages, but the grammar is simple and well-supported.

## Key Decisions

- **`call` not `call_expression`** — Python's tree-sitter grammar uses `call` as the node kind for function calls.
- **`decorated_definition` handling** — Functions/classes inside decorators are still matched by inner queries; no special unwrapping needed.
- **Methods vs functions** — Distinguished by checking for enclosing `class_definition`, same technique as C++ inline methods.
- **Two import forms** — Both `import foo` and `from foo import bar` produce include edges.
- **Inheritance** — `class_definition` has `superclasses: (argument_list)` with base class identifiers.

## Dependencies

- `github.com/tree-sitter/tree-sitter-python/bindings/go` v0.25.0
- Go parser plan completed first (shared KindInterface already added)
