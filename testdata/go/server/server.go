// Package server — Server struct with pointer + value receivers, an
// interface that the Server structurally satisfies (no edge emitted),
// and a method that uses both `go` (goroutine) and `defer`.
//
// Symbol contract for `server/server.go` (asserted by `MANIFEST.md`):
//   Structs:    `Server` (1)
//   Interfaces: `Runner` (1)
//   Methods:    `Server.Run`, `Server.Stop`, `Server.Status`,
//               `Server.cleanup` (4 — Server has both pointer-receiver
//               and value-receiver methods on the same type)
//   Functions:  `New` (1, constructor — free fn returning *Server)
//
// Server structurally satisfies the Runner interface (it has a `Run()`
// method), but Go interfaces are structural — no `Inherits` edge is
// emitted between Server and Runner.
//
// Calls inside this file (6 Calls edges total):
//   - `New        -> Println` (1, package-qualified `fmt.Println`)
//   - `Server.Run -> handle`  (1, `go handle(...)` direct goroutine call)
//   - `Server.Run -> Greet`   (1, package-qualified `utils.Greet`)
//   - `Server.Stop -> cleanup` (1, deferred selector call:
//     `defer s.cleanup()`)
//   - `Server.Stop -> Println` (1, package-qualified `fmt.Println`)
//   - `Server.Status -> len`   (1, builtin `len(s.peers)` direct call)
//
// Imports:
//   - `fmt` (single)
//   - `code-graph-go-corpus/utils` (single)
// (2 Includes edges total)
package server

import (
	"fmt"

	"code-graph-go-corpus/utils"
)

// Runner is the interface Server structurally satisfies. No edge is
// emitted between Server and Runner — Go interfaces are structural.
type Runner interface {
	Run() error
}

// Server is the concrete type. Pointer-receiver methods make it satisfy
// the Runner interface; the parser does NOT emit any Inherits edge.
type Server struct {
	addr  string
	peers []string
}

// New is a free function (constructor) returning a pointer to Server.
// Demonstrates a free fn that the corpus uses as a top-level helper.
func New(addr string) *Server {
	fmt.Println("constructing", addr)
	return &Server{addr: addr}
}

// Run is a pointer-receiver method on Server. Exercises:
//   - `go handle(...)` — goroutine direct-call edge (To=handle)
//   - `utils.Greet(...)` — package-qualified call edge (To=Greet)
//
// Calls produced:
//   - To=handle (goroutine)
//   - To=Greet  (package-qualified)
func (s *Server) Run() error {
	go handle(s.addr)
	utils.Greet("server")
	return nil
}

// Stop exercises a value-receiver method on Server. Calls produced:
//   - To=cleanup (deferred selector call: `defer s.cleanup()`)
//   - To=Println (direct selector call: `fmt.Println(...)`)
//
// Note: Stop is intentionally a value receiver to exercise both the
// pointer and value branches of `extract_receiver_type` against the
// SAME concrete type within one fixture file.
func (s Server) Stop() {
	defer s.cleanup()
	fmt.Println("stopping", s.addr)
}

// Status returns the number of peers — exercises a `len(...)` call on
// a slice and an additional pointer-receiver method.
//
// Calls produced:
//   - To=len (builtin, direct call)
func (s *Server) Status() int {
	return len(s.peers)
}

// cleanup is an unexported helper method invoked by Stop. Pointer
// receiver; produces no calls of its own (kept empty so the call-edge
// math stays simple).
func (s *Server) cleanup() {}
