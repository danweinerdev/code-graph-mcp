// Package utils — free functions, type aliases, defined-type form, and
// init(). Also exercises a `type Handler func(...)` defined-type alias
// (a `type_spec` whose body is `function_type`, not `struct_type` or
// `interface_type`, so the extractor falls through to Typedef).
//
// Symbol contract for `utils/helpers.go` (asserted by `MANIFEST.md`):
//   Functions: `Add`, `Mul`, `Greet`, `Sub`, `Apply`, `init` (6)
//   Typedefs:  `Op`, `Handler`, `Count` (3)
//     - `Op`      — type alias (`type X = T`,    parsed as `type_alias`)
//     - `Handler` — defined-type with function_type body (`type_spec`)
//     - `Count`   — defined-type with named-type body  (`type_spec`)
//
// Calls inside this file (4 Calls edges total):
//   - `Greet  -> Println` (1, package-qualified `fmt.Println`)
//   - `Apply  -> op`      (1, parameter invocation by name)
//   - `init   -> Add`     (1, direct call sequencing init)
//   - `init   -> Mul`     (1, direct call sequencing init)
//
// Imports (1 Includes edge):
//   - `fmt`
package utils

import "fmt"

// Op is a type alias for a binary integer operator. Exercises the Go 1.9+
// `type X = T` form (parsed as `type_alias`, not `type_spec`).
type Op = func(int, int) int

// Handler is a defined-type with a function_type body. tree-sitter parses
// this as a `type_spec` whose `type` field is `function_type`; the
// extractor falls through the struct/interface dispatch and emits Typedef.
type Handler func(int) error

// Count is a defined-type with a named-type body — `type X int`. Same
// dispatch fall-through as Handler.
type Count int

// Add returns the sum of two ints.
func Add(a, b int) int {
	return a + b
}

// Mul returns the product of two ints.
func Mul(a, b int) int {
	return a * b
}

// Sub returns the difference of two ints.
func Sub(a, b int) int {
	return a - b
}

// Apply applies an Op to two ints.
func Apply(op Op, a, b int) int {
	return op(a, b)
}

// Greet writes a greeting via fmt.Println — exercises the package-qualified
// selector-call edge (To=Println).
func Greet(name string) {
	fmt.Println("hello,", name)
}

// init exercises the special init() function — extracted as an ordinary
// Function (no special-casing). The body has two direct calls (Add, Mul)
// so the corpus pins init's behavior in the call graph.
func init() {
	_ = Add(1, 2)
	_ = Mul(3, 4)
}
