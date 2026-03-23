---
title: "C++ Parser"
type: phase
plan: CodeGraphMCP
phase: 2
status: planned
created: 2026-03-22
updated: 2026-03-22
deliverable: "CppParser that extracts symbols, calls, includes, and inheritance from C++ files using tree-sitter"
tasks:
  - id: "2.1"
    title: "CppParser struct and tree-sitter initialization"
    status: planned
    verification: "NewCppParser() succeeds; Extensions() returns [.cpp, .cc, .cxx, .c, .h, .hpp, .hxx]; Close() releases all query objects without panic; parser satisfies the Parser interface"
  - id: "2.2"
    title: "Query string definitions"
    status: planned
    verification: "All four query categories (definitions, calls, includes, inheritance) compile against the pinned tree-sitter-cpp grammar without error"
    depends_on: ["2.1"]
  - id: "2.3"
    title: "Symbol definition extraction"
    status: planned
    verification: "Extracts: free functions, qualified methods (Class::method), classes with bodies, structs with bodies, enums, typedefs. Each symbol has correct Name, Kind, File, Line, Column, EndLine, Signature, Namespace, and Parent. Namespace is populated from enclosing namespace_definition. Tested with snippets covering each symbol type including nested namespaces."
    depends_on: ["2.2"]
  - id: "2.4"
    title: "Call site extraction"
    status: planned
    verification: "Extracts: free function calls (foo()), method calls (obj.foo(), obj->foo()), qualified calls (ns::foo()), template function calls (foo<T>()), template method calls (obj.foo<T>()). Each edge has From (enclosing function), To (callee name), Kind=calls, File, Line. Tested with snippets covering each call pattern."
    depends_on: ["2.2"]
  - id: "2.5"
    title: "Include directive extraction"
    status: planned
    verification: "Extracts both quoted includes (#include \"foo.h\") and system includes (#include <vector>). Edge kind is 'includes'. Quoted include paths have quotes stripped. System include paths have angle brackets stripped. Tested with both forms."
    depends_on: ["2.2"]
  - id: "2.6"
    title: "Inheritance extraction"
    status: planned
    verification: "Extracts base classes from both class_specifier and struct_specifier. Handles simple bases (class Derived : public Base) and qualified bases (class Derived : public ns::Base). Edge kind is 'inherits', From is derived class, To is base class name. Tested with single and multiple inheritance."
    depends_on: ["2.2"]
  - id: "2.7"
    title: "Enclosing context resolution"
    status: planned
    verification: "Call edges correctly identify the enclosing function as the 'From' field (not just the file). Methods inside class bodies have correct Parent. Nested namespace resolution produces dotted namespace strings (ns1::ns2). Functions inside anonymous namespaces get empty Namespace. Error nodes (node.HasError()) are skipped without crashing."
    depends_on: ["2.3", "2.4"]
  - id: "2.8"
    title: "Unit test corpus"
    status: planned
    verification: "Every query pattern has at least one test case. Test corpus covers: free functions, methods, classes, structs, enums, typedefs, nested namespaces, multiple inheritance, all 5 call site patterns, both include forms, forward declarations (should NOT produce symbols — only definitions with bodies/implementations). go vet and -race pass."
    depends_on: ["2.3", "2.4", "2.5", "2.6", "2.7"]
---

# Phase 2: C++ Parser

## Overview

Implement `CppParser` in `internal/lang/cpp/` using go-tree-sitter with the C++ grammar. This is the core extraction engine — it takes a C++ source file and returns a `FileGraph` with all symbols and relationships.

Each extraction capability (definitions, calls, includes, inheritance) is built and tested incrementally. The parser is fully testable without the graph engine or MCP server.

## 2.1: CppParser struct and tree-sitter initialization

### Subtasks
- [ ] `internal/lang/cpp/cpp.go` — `CppParser` struct with `language`, `defQuery`, `callQuery`, `inclQuery`, `inhQuery` fields
- [ ] `NewCppParser() (*CppParser, error)` — initialize language from `tree_sitter_cpp.Language()`, compile all query objects
- [ ] `Extensions()` — return C/C++ extensions
- [ ] `Close()` — close all cached query objects
- [ ] Verify `CppParser` satisfies `parser.Parser` interface (compile-time check via `var _ parser.Parser = (*CppParser)(nil)`)

### Notes
Query objects are compiled once and reused across `ParseFile` calls. They are thread-safe for reads. The `tree_sitter.Parser` (not to be confused with our `parser.Parser` interface) is created per-`ParseFile` call since it is NOT thread-safe.

## 2.2: Query string definitions

### Subtasks
- [ ] `internal/lang/cpp/queries.go` — define query strings as Go `const` blocks
- [ ] `definitionQueries` — free functions, qualified methods, classes, structs, enums, typedefs, namespace definitions
- [ ] `callQueries` — all 5 call site patterns (free, method, qualified, template free, template method)
- [ ] `includeQueries` — preproc_include with string_literal and system_lib_string
- [ ] `inheritanceQueries` — class and struct base_class_clause with type_identifier and qualified_identifier alternation
- [ ] Verify all queries compile: test that `tree_sitter.NewQuery(lang, queryString)` returns no error for each

### Notes
The query strings come directly from the design document's Query Patterns section. The critical pattern is `@method.qname` — capture the full `qualified_identifier` text and split `scope::name` in Go code, rather than relying on grammar-version-specific child field names.

## 2.3: Symbol definition extraction

### Subtasks
- [ ] `extractDefinitions(root *tree_sitter.Node, content []byte, fg *parser.FileGraph)`
- [ ] Run `defQuery` via `QueryCursor.Matches()`, iterate matches
- [ ] For `@func.name` captures: create Symbol with Kind=function, extract name text
- [ ] For `@method.qname` captures: split on `::`, set Parent from scope, Name from leaf, Kind=method
- [ ] For `@class.name`, `@struct.name`, `@enum.name`, `@typedef.name`: create appropriate Symbol
- [ ] For `@ns.name`: track current namespace context for populating Symbol.Namespace
- [ ] Populate Line, Column, EndLine from node positions (tree-sitter is 0-based rows; convert to 1-based lines)
- [ ] Populate Signature by extracting the declaration text (truncated to ~200 chars)
- [ ] Skip nodes where `node.HasError()` is true

### Notes
Namespace tracking: tree-sitter queries are flat (no stack), so we need a helper that walks up from a symbol node to find enclosing `namespace_definition` ancestors. This is a tree walk, not a query — for each symbol node, walk `node.Parent()` until we find namespace definitions or the root.

Forward declarations (function declarations without a body) should NOT produce symbols. The definition query anchors on `function_definition` (which has a body), not `declaration`.

## 2.4: Call site extraction

### Subtasks
- [ ] `extractCalls(root *tree_sitter.Node, content []byte, fg *parser.FileGraph)`
- [ ] Run `callQuery` via `QueryCursor.Matches()`, iterate matches
- [ ] For each call match, determine the enclosing function (walk up `node.Parent()` until `function_definition`)
- [ ] For `@call.name` captures: create Edge with To=callee name text
- [ ] For `@call.qname` captures: use full qualified text, split on `::` for scope-aware matching later
- [ ] Edge.From = enclosing function's symbol ID (file:funcname); if no enclosing function (top-level call), use file path
- [ ] Edge.Kind = "calls", Edge.File = current file path, Edge.Line = call node start line

### Notes
The "enclosing function" walk is shared with definition extraction's namespace walk — consider a helper `findEnclosing(node, kinds []string) *Node` that walks parents looking for any of the given node kinds.

Top-level calls (e.g., global initializers like `int x = compute()`) should still produce edges, with File as the From.

## 2.5: Include directive extraction

### Subtasks
- [ ] `extractIncludes(root *tree_sitter.Node, content []byte, fg *parser.FileGraph)`
- [ ] Run `inclQuery` via `QueryCursor.Matches()`
- [ ] For `@include.path` captures: extract the path text
- [ ] Strip surrounding quotes (`"engine.h"` → `engine.h`) or angle brackets (`<vector>` → `vector`)
- [ ] Create Edge with From=current file path, To=raw include path, Kind="includes"

### Notes
Include paths are stored as raw strings — resolution to absolute paths happens later in the graph engine (basename matching against indexed files). System includes are recorded but won't resolve to indexed files in most cases.

## 2.6: Inheritance extraction

### Subtasks
- [ ] `extractInheritance(root *tree_sitter.Node, content []byte, fg *parser.FileGraph)`
- [ ] Run `inhQuery` via `QueryCursor.Matches()`
- [ ] For each match: capture `@derived.name` and `@base.name`
- [ ] For `type_identifier` base nodes: use the name directly
- [ ] For `qualified_identifier` base nodes: use the full text (e.g., `ns::Base`)
- [ ] Create Edge with From=derived class name, To=base class name, Kind="inherits"
- [ ] Handle multiple inheritance: a class with `class D : public A, public B` produces two edges

### Notes
The alternation `[(type_identifier) (qualified_identifier)]` in the query handles both simple and qualified base class names. Multiple `@base.name` captures in a single match represent multiple base classes.

## 2.7: Enclosing context resolution

### Subtasks
- [ ] Implement `findEnclosing(node, content, kindSet) *Node` helper — walks `node.Parent()` chain
- [ ] Use for: namespace resolution (find enclosing `namespace_definition`), call site enclosing function (find `function_definition`), method parent class (find `class_specifier` / `struct_specifier`)
- [ ] Handle anonymous namespaces: `namespace { ... }` has no `name` child — set Namespace to ""
- [ ] Handle nested namespaces: `namespace a { namespace b { ... } }` → Namespace = "a::b"
- [ ] Gracefully handle error nodes: if any ancestor `HasError()`, continue walking up

## 2.8: Unit test corpus

### Subtasks
- [ ] Test file: `internal/lang/cpp/cpp_test.go`
- [ ] Test helper: parse a C++ string, return FileGraph, assert symbols/edges
- [ ] Test cases for definitions:
  - [ ] Free function: `void foo() {}`
  - [ ] Method definition: `void MyClass::doWork() {}`
  - [ ] Class with body: `class MyClass { void method(); };`
  - [ ] Struct with body: `struct Point { int x; int y; };`
  - [ ] Enum: `enum Color { Red, Green, Blue };`
  - [ ] Typedef: `typedef int MyInt;`
  - [ ] Nested namespace: `namespace a { namespace b { void foo() {} } }`
  - [ ] Forward declaration (negative test): `void foo();` should NOT produce a symbol
- [ ] Test cases for calls:
  - [ ] Free call: `void caller() { foo(); }`
  - [ ] Method call: `void f() { obj.method(); }`
  - [ ] Arrow call: `void f() { ptr->method(); }`
  - [ ] Qualified call: `void f() { ns::foo(); }`
  - [ ] Template call: `void f() { make<int>(); }`
- [ ] Test cases for includes:
  - [ ] Quoted: `#include "engine.h"`
  - [ ] System: `#include <vector>`
- [ ] Test cases for inheritance:
  - [ ] Single: `class D : public B {};`
  - [ ] Multiple: `class D : public A, public B {};`
  - [ ] Qualified: `class D : public ns::B {};`
- [ ] Run `go vet` and `go test -race` on the package

## Acceptance Criteria
- [ ] `CppParser` implements `parser.Parser` interface
- [ ] All 8 definition types extracted correctly (free function, method, class, struct, enum, typedef, namespace, and forward decl correctly excluded)
- [ ] All 5 call site patterns produce correct edges
- [ ] Both include forms parsed
- [ ] Single and multiple inheritance extracted
- [ ] `go test -race ./internal/lang/cpp/` — all pass
- [ ] `go vet ./...` clean
