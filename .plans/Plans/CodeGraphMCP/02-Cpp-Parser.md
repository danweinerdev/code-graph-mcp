---
title: "C++ Parser"
type: phase
plan: CodeGraphMCP
phase: 2
status: complete
created: 2026-03-22
updated: 2026-03-22
deliverable: "CppParser that extracts symbols, calls, includes, and inheritance from C++ files using tree-sitter"
tasks:
  - id: "2.1"
    title: "CppParser struct and tree-sitter initialization"
    status: complete
    verification: "NewCppParser() succeeds; Extensions() returns [.cpp, .cc, .cxx, .c, .h, .hpp, .hxx]; Close() releases all query objects without panic; parser satisfies the Parser interface"
  - id: "2.2"
    title: "Query string definitions"
    status: complete
    verification: "All four query categories (definitions, calls, includes, inheritance) compile against the pinned tree-sitter-cpp grammar without error"
    depends_on: ["2.1"]
  - id: "2.3"
    title: "Symbol definition extraction"
    status: complete
    verification: "Extracts: free functions, qualified methods (Class::method), classes with bodies, structs with bodies, enums, typedefs. Each symbol has correct Name, Kind, File, Line, Column, EndLine, Signature, Namespace, and Parent. Namespace is populated from enclosing namespace_definition. Tested with snippets covering each symbol type including nested namespaces."
    depends_on: ["2.2"]
  - id: "2.4"
    title: "Call site extraction"
    status: complete
    verification: "Extracts: free function calls (foo()), method calls (obj.foo(), obj->foo()), qualified calls (ns::foo()), template function calls (foo<T>()). Each edge has From (enclosing function), To (callee name), Kind=calls, File, Line. Tested with snippets covering each call pattern."
    depends_on: ["2.2"]
  - id: "2.5"
    title: "Include directive extraction"
    status: complete
    verification: "Extracts both quoted includes and system includes. Edge kind is 'includes'. Quoted include paths have quotes stripped. System include paths have angle brackets stripped. Tested with both forms."
    depends_on: ["2.2"]
  - id: "2.6"
    title: "Inheritance extraction"
    status: complete
    verification: "Extracts base classes from both class_specifier and struct_specifier. Handles simple bases and qualified bases (ns::Base). Edge kind is 'inherits', From is derived class, To is base class name. Tested with single and multiple inheritance."
    depends_on: ["2.2"]
  - id: "2.7"
    title: "Enclosing context resolution"
    status: complete
    verification: "Call edges correctly identify the enclosing function as the 'From' field. Nested namespace resolution produces joined strings (a::b). Functions inside anonymous namespaces get empty Namespace. Error nodes skipped without crashing."
    depends_on: ["2.3", "2.4"]
  - id: "2.8"
    title: "Unit test corpus"
    status: complete
    verification: "23 tests covering: free functions, methods, classes, structs, enums, typedefs, nested namespaces, multiple inheritance, 4 call site patterns, both include forms, forward declarations excluded, top-level calls, anonymous namespaces, signature truncation, helper functions. go vet and -race pass."
    depends_on: ["2.3", "2.4", "2.5", "2.6", "2.7"]
---

# Phase 2: C++ Parser

## Overview

Implement `CppParser` in `internal/lang/cpp/` using go-tree-sitter with the C++ grammar. This is the core extraction engine — it takes a C++ source file and returns a `FileGraph` with all symbols and relationships.

## 2.1: CppParser struct and tree-sitter initialization

### Subtasks
- [x] `internal/lang/cpp/cpp.go` — `CppParser` struct with `language`, `defQuery`, `callQuery`, `inclQuery`, `inhQuery` fields
- [x] `NewCppParser() (*CppParser, error)` — initialize language from `tree_sitter_cpp.Language()`, compile all query objects
- [x] `Extensions()` — return C/C++ extensions
- [x] `Close()` — close all cached query objects
- [x] Verify `CppParser` satisfies `parser.Parser` interface (compile-time check via `var _ parser.Parser = (*CppParser)(nil)`)

## 2.2: Query string definitions

### Subtasks
- [x] `internal/lang/cpp/queries.go` — define query strings as Go `const` blocks
- [x] `definitionQueries` — free functions, qualified methods, classes, structs, enums, typedefs
- [x] `callQueries` — 4 call site patterns (free, method, qualified, template free)
- [x] `includeQueries` — preproc_include with string_literal and system_lib_string
- [x] `inheritanceQueries` — class and struct base_class_clause with type_identifier and qualified_identifier alternation
- [x] Verify all queries compile: NewCppParser() succeeds (compiles all queries)

### Notes
Template method call pattern (`obj.foo<T>()` via `template_method`) was dropped from queries — the `template_method` node type does not exist in tree-sitter-cpp v0.23.4. Template method calls fall through to the regular method call pattern. This will be revisited in Phase 3 validation.

## 2.3: Symbol definition extraction

### Subtasks
- [x] `extractDefinitions(root, content, path, fg)` implemented
- [x] `@func.name` captures → Kind=function
- [x] `@method.qname` captures → split on `::`, Kind=method, Parent set
- [x] `@class.name`, `@struct.name`, `@enum.name`, `@typedef.name` → appropriate kinds
- [x] Namespace populated by walking parent chain for `namespace_definition`
- [x] Line/Column/EndLine from node positions (0-based → 1-based)
- [x] Signature truncated to 200 chars
- [x] Error nodes skipped via `HasError()`

## 2.4: Call site extraction

### Subtasks
- [x] `extractCalls(root, content, path, fg)` implemented
- [x] Enclosing function resolved via `findEnclosingKind` → `enclosingFunctionID`
- [x] `@call.name` → direct callee name
- [x] `@call.qname` → full qualified text preserved
- [x] From = `path:funcName` or `path` for top-level calls
- [x] Kind = "calls", File, Line populated

## 2.5: Include directive extraction

### Subtasks
- [x] `extractIncludes(root, content, path, fg)` implemented
- [x] `@include.path` captures stripped of quotes/angle brackets
- [x] Edge: From=file path, To=cleaned include path, Kind="includes"

## 2.6: Inheritance extraction

### Subtasks
- [x] `extractInheritance(root, content, path, fg)` implemented
- [x] `@derived.name` and `@base.name` captured per match
- [x] Handles both `type_identifier` and `qualified_identifier` base nodes
- [x] Multiple inheritance produces multiple edges

## 2.7: Enclosing context resolution

### Subtasks
- [x] `findEnclosingKind(node, kind)` — walks parent chain
- [x] `resolveNamespace(node, content)` — collects namespace_definition ancestors, reverses, joins with `::`
- [x] `enclosingFunctionID(node, content, path)` — returns `path:funcName` or `path`
- [x] Anonymous namespaces handled (no name child → skipped → empty namespace)
- [x] Nested namespaces produce `a::b`

## 2.8: Unit test corpus

### Subtasks
- [x] Test file: `internal/lang/cpp/cpp_test.go`
- [x] Test helpers: `parse()`, `findSymbol()`, `findEdge()`, `findEdgeFrom()`
- [x] Definition tests: free function, method, class, struct, enum, typedef, nested namespace, forward decl excluded
- [x] Call tests: free call, method call, arrow call, qualified call, template call
- [x] Include tests: quoted, system
- [x] Inheritance tests: single, multiple, qualified
- [x] Edge case tests: top-level call, anonymous namespace, signature truncation
- [x] Helper tests: splitQualified, stripIncludePath
- [x] `go vet ./...` passes, `go test -race` passes (23 tests)

## Acceptance Criteria
- [x] `CppParser` implements `parser.Parser` interface
- [x] Definition types extracted correctly (forward decl excluded)
- [x] 4 call site patterns produce correct edges
- [x] Both include forms parsed
- [x] Single, multiple, and qualified inheritance extracted
- [x] `go test -race ./internal/lang/cpp/` — 23 tests pass
- [x] `go vet ./...` clean
