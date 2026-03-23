package cpp

// Tree-sitter query patterns for C++ symbol extraction.
// Validated against tree-sitter-cpp v0.23.4.

// definitionQueries extracts symbol definitions: functions, methods, classes,
// structs, enums, typedefs, and namespace definitions.
const definitionQueries = `
; Free functions — function_declarator holds the name
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @func.name)) @func.def

; Methods (qualified: Class::method or ns::func)
; Capture full qualified_identifier text; split scope::name in Go.
(function_definition
  declarator: (function_declarator
    declarator: (qualified_identifier) @method.qname)) @method.def

; Classes with body
(class_specifier
  name: (type_identifier) @class.name
  body: (_)) @class.def

; Structs with body
(struct_specifier
  name: (type_identifier) @struct.name
  body: (_)) @struct.def

; Enums
(enum_specifier
  name: (type_identifier) @enum.name) @enum.def

; Typedefs
(type_definition
  declarator: (type_identifier) @typedef.name) @typedef.def
`

// callQueries extracts function and method call sites.
const callQueries = `
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
`

// includeQueries extracts #include directives.
const includeQueries = `
(preproc_include
  path: [(string_literal) (system_lib_string)] @include.path)
`

// inheritanceQueries extracts base class relationships from class and struct
// specifiers. Handles both simple (Base) and qualified (ns::Base) base names.
const inheritanceQueries = `
(class_specifier
  name: (type_identifier) @derived.name
  (base_class_clause
    [(type_identifier) (qualified_identifier)] @base.name))

(struct_specifier
  name: (type_identifier) @derived.name
  (base_class_clause
    [(type_identifier) (qualified_identifier)] @base.name))
`
