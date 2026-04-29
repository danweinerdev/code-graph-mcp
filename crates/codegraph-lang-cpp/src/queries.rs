//! Tree-sitter query patterns for C++ symbol extraction.
//!
//! These are ported verbatim from `internal/lang/cpp/queries.go`. They are
//! validated against tree-sitter-cpp v0.23.4. Any change here must be matched
//! by a corresponding change in the Go side until Phase 4 retires Go.

/// Definition queries: functions, methods, classes, structs, enums, typedefs,
/// operator overloads, and inline methods.
pub(crate) const DEFINITION_QUERIES: &str = r#"
; Free functions — function_declarator holds the name
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @func.name)) @func.def

; Inline methods inside class body — use field_identifier
(function_definition
  declarator: (function_declarator
    declarator: (field_identifier) @inline.name)) @inline.def

; Methods (qualified: Class::method or ns::func)
; Capture full qualified_identifier text; split scope::name in Go.
(function_definition
  declarator: (function_declarator
    declarator: (qualified_identifier) @method.qname)) @method.def

; Operator overloads — operator+ etc.
(function_definition
  declarator: (function_declarator
    declarator: (operator_name) @operator.name)) @operator.def

; Classes with body
(class_specifier
  name: (type_identifier) @class.name
  body: (_)) @class.def

; Structs with body
(struct_specifier
  name: (type_identifier) @struct.name
  body: (_)) @struct.def

; Enums (covers both plain enum and enum class)
(enum_specifier
  name: (type_identifier) @enum.name) @enum.def

; Simple typedefs: typedef int MyInt;
(type_definition
  declarator: (type_identifier) @typedef.name) @typedef.def

; Function pointer typedefs: typedef void (*Callback)(int);
; The type_identifier is nested inside pointer_declarator > parenthesized_declarator
(type_definition
  declarator: (function_declarator
    declarator: (parenthesized_declarator
      (pointer_declarator
        declarator: (type_identifier) @typedef.name)))) @typedef.def

; Type alias: using Callback = void(*)(int);
(alias_declaration
  name: (type_identifier) @typedef.name) @typedef.def
"#;

/// Call queries: free, method, qualified, and template free function calls.
pub(crate) const CALL_QUERIES: &str = r#"
; Free function call: foo()
(call_expression
  function: (identifier) @call.name) @call.expr

; Method call: obj.foo() or obj->foo()
(call_expression
  function: (field_expression
    field: (field_identifier) @call.name)) @call.expr

; Qualified call: ns::foo() or Class::staticMethod()
(call_expression
  function: (qualified_identifier) @call.qname) @call.expr

; Template free function call: foo<T>()
(call_expression
  function: (template_function
    name: (identifier) @call.name)) @call.expr
"#;

/// Include queries: `#include` directives, both quoted and system forms.
pub(crate) const INCLUDE_QUERIES: &str = r#"
(preproc_include
  path: [(string_literal) (system_lib_string)] @include.path)
"#;

/// Inheritance queries: base classes for class_specifier and struct_specifier.
/// Handles simple (Base) and qualified (ns::Base) base names.
pub(crate) const INHERITANCE_QUERIES: &str = r#"
(class_specifier
  name: (type_identifier) @derived.name
  (base_class_clause
    [(type_identifier) (qualified_identifier)] @base.name))

(struct_specifier
  name: (type_identifier) @derived.name
  (base_class_clause
    [(type_identifier) (qualified_identifier)] @base.name))
"#;
