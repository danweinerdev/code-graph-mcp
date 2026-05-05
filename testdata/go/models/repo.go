// Package models — Repo interface (embedding another interface), a
// generic function, and a struct exposing a value-receiver method.
//
// Symbol contract for `models/repo.go` (asserted by `MANIFEST.md`):
//   Structs:    `Memo`, `KV` (2)
//   Interfaces: `Closer`, `Repo` (2)
//   Methods:    `Memo.Get`, `Memo.Hits`, `KV.Set`, `KV.Lookup` (4)
//   Functions:  `Map`, `Filter` (2, both generic)
//
// Edge cases exercised here:
//   - Interface embedding interface: `Repo` embeds `Closer` via a bare
//     `Closer` line in its body. The embedded interface is a
//     `type_elem` containing a `type_identifier`, NOT a `method_elem` —
//     the definition queries don't match either, so no Symbol falls out
//     for the embed and no edge is produced.
//   - Anonymous struct field: `Memo`'s `Cache` field is an anonymous
//     struct value. tree-sitter parses it as a `field_declaration` with
//     a `struct_type` body; no nested Symbol is emitted (the anonymous
//     struct has no name).
//   - Generic function: `Map[T any, U any](in []T, f func(T) U) []U`
//     extracted as Function; signature truncates at the body opener
//     leaving the type parameter list intact.
//
// Calls inside this file (10 Calls edges total):
//   - `Memo.Get  -> Add`    (1, package-qualified `utils.Add`)
//   - `Memo.Hits -> Get`    (1, method-call selector inside Hits)
//   - `KV.Set    -> make`   (1, builtin used to lazy-init the map)
//   - `KV.Lookup -> ok`     (1, identifier-call against a local fn-typed
//                            var — exercises identifier branch on a
//                            non-package, non-method callee)
//   - `Map    -> make`, `Map    -> len`, `Map    -> append`,
//     `Map    -> f`        (4, direct calls — `make` / `len` / `append`
//                            are Go builtins but parse identically to
//                            user functions, so they produce identical
//                            Calls edges; `f` is a closure-typed
//                            parameter invoked by name)
//   - `Filter -> append`,  `Filter -> pred`
//                          (2, body of the second generic function —
//                            again, builtin and closure parameter)
//
// Imports:
//   - `code-graph-go-corpus/utils` (1 Includes edge)
package models

import "code-graph-go-corpus/utils"

// Closer is a single-method interface — the simplest Go interface form.
type Closer interface {
	Close() error
}

// Repo embeds Closer (interface-embedding-interface) and adds Find. The
// embedded `Closer` produces no Symbol and no edge.
type Repo interface {
	Closer
	Find(id string) (*User, error)
}

// Memo is a defined struct with one named field and one ANONYMOUS struct
// field (Cache). The anonymous struct field's inner fields are not
// emitted as nested Symbols — the type body is `struct_type` without a
// name, so the type_spec query never fires on it.
type Memo struct {
	Cache struct {
		hits int
	}
	id string
}

// Get is a value-receiver method on Memo. Exercises the value-receiver
// branch of `extract_receiver_type` and produces one Calls edge to the
// cross-package `utils.Add` (resolved as `Add` because the call query
// captures only the trailing field of the selector).
func (m Memo) Get() int {
	return utils.Add(m.Cache.hits, 1)
}

// Hits is a pointer-receiver method on Memo that defers to Get. Two
// methods on the same type with different receiver kinds (value vs
// pointer) exercise both branches of `extract_receiver_type` against
// the same parent.
func (m *Memo) Hits() int {
	return m.Get()
}

// KV is a defined struct with a single named map field. Used to
// exercise an additional concrete-type-with-methods sample.
type KV struct {
	store map[string]int
}

// Set is a pointer-receiver method on KV. Lazy-inits the map via
// `make(...)`.
func (k *KV) Set(key string, val int) {
	if k.store == nil {
		k.store = make(map[string]int)
	}
	k.store[key] = val
}

// Lookup is a value-receiver method on KV that uses an inline
// callback. The local `ok` is a function-typed local var; calling it
// produces a direct Calls edge.
func (k KV) Lookup(key string) bool {
	ok := func(s string) bool { return s != "" }
	return ok(key)
}

// Map is a Go 1.18+ generic function. The signature must truncate at the
// body opener with the `[T any, U any]` type parameter list intact. The
// body has four direct calls (make, len, append, f).
func Map[T any, U any](in []T, f func(T) U) []U {
	out := make([]U, 0, len(in))
	for _, v := range in {
		out = append(out, f(v))
	}
	return out
}

// Filter is a second generic function — single-type-param form. Body
// produces two Calls edges (append, pred).
func Filter[T any](in []T, pred func(T) bool) []T {
	out := []T{}
	for _, v := range in {
		if pred(v) {
			out = append(out, v)
		}
	}
	return out
}
