// Package models — User struct with embedded fields, Named interface,
// and a method on User that exercises a defer call.
//
// Symbol contract for `models/user.go` (asserted by `MANIFEST.md`):
//   Structs:    `Profile`, `User` (2)
//   Interfaces: `Named` (1)
//   Methods:    `User.Name`, `User.Close` (2)
//
// `User` embeds `Profile` and `*sync.Mutex` (both anonymous) — these are
// `field_declaration` nodes with no name field; they MUST NOT produce
// Symbol records and MUST NOT produce `Inherits` edges. Go's structural
// composition (method-set promotion) is not represented as inheritance.
//
// Calls inside this file:
//   - `User.Close -> Unlock` (1, defer with method-call selector)
// (1 Calls edge total)
//
// Imports:
//   - `sync` (1 Includes edge)
package models

import "sync"

// Named is the structural interface a type satisfies by having a `Name()`
// method. Go interfaces are structurally typed — there is no syntactic
// declaration that `User` implements `Named`, and the parser must NOT
// emit an `Inherits` edge for the implicit relationship.
type Named interface {
	Name() string
}

// Profile is embedded into User. The embedded field is anonymous and
// produces no Symbol record (anti-regression for embedded fields).
type Profile struct {
	Email string
}

// User has two embedded fields (Profile and *sync.Mutex) plus one named
// field (login). Both embeds must produce zero Inherits edges.
type User struct {
	Profile
	*sync.Mutex
	login string
}

// Name returns the user's login. Pointer-receiver method on User; the
// concrete type structurally satisfies the `Named` interface, but no
// `Inherits` edge is emitted (Go interfaces are structural).
func (u *User) Name() string {
	return u.login
}

// Close demonstrates a `defer` selector call — the deferred Unlock call
// is captured naturally because defer_statement wraps a call_expression
// already matched by the call query. Edge: User.Close -> Unlock.
func (u *User) Close() {
	defer u.Unlock()
}
