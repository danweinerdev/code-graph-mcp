---
title: "Rust Language Parser"
type: plan
status: draft
created: 2026-03-22
updated: 2026-03-22
tags: [parser, rust, tree-sitter]
related: [Research/multi-language-parsers, Plans/GoParser, Plans/PythonParser]
phases:
  - id: 1
    title: "Rust Parser Core"
    status: planned
    doc: "01-Parser-Core.md"
  - id: 2
    title: "Rust Test Project & Validation"
    status: planned
    doc: "02-Test-Project-And-Validation.md"
    depends_on: [1]
  - id: 3
    title: "Integration & Registration"
    status: planned
    doc: "03-Integration-And-Registration.md"
    depends_on: [2]
---

# Rust Language Parser

## Overview

Add Rust language support. Rust is the most complex of the three new parsers due to `impl` blocks, trait impls, `use` tree traversal, and macro invocations. Requires `KindTrait` added to types.

## Key Decisions

- **`KindTrait`** — separate from `KindInterface` (Go). Traits are Rust's interface concept but with different semantics (explicit `impl Trait for Type`).
- **`impl_item` context** — Methods inside `impl` blocks get Parent from `impl_item > type:` field, analogous to C++ class body context.
- **Trait impl edges** — `impl Trait for Type` produces an EdgeInherits from Type to Trait.
- **`use` declarations** — Record top-level module path. Nested `use_list` (`use foo::{Bar, Baz}`) expanded recursively.
- **Macro invocations** — `macro_invocation` produces call edges (macros are effectively function-like).

## Dependencies

- `github.com/tree-sitter/tree-sitter-rust/bindings/go` v0.24.1
- Go and Python parser plans completed first (KindInterface already added)
