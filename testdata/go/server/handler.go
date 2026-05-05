// Package server — HTTP handler functions and a closure pattern used by
// the corpus to exercise:
//   - free function `handle` (called via `go handle(...)` in server.go)
//   - a package-level function literal assigned to a global var (call
//     inside it falls back to the bare file path as `from`)
//   - a closure inside a method body (call inside resolves to the
//     enclosing method)
//
// Symbol contract for `server/handler.go` (asserted by `MANIFEST.md`):
//   Functions: `handle`, `withLog` (2)
//   Methods:   none
//
// Calls inside this file (4 Calls edges total):
//   - `handle  -> Println` (1, fmt.Println in handle)
//   - file-path -> Println (1, package-level closure assigned to var
//     `Logger` — no enclosing function, falls back to file path)
//   - `withLog -> Println` (1, fmt.Println inside the inner func_literal
//     returned by withLog — closure-transparent walk attributes the
//     call to `withLog`, NOT to the inner literal)
//   - `withLog -> inner`   (1, the parameter `inner` is invoked by
//     name inside the inner closure body; same enclosing-fn rule)
//
// Imports:
//   - `fmt` (single)
// (1 Includes edge total)
package server

import "fmt"

// Logger is a package-level closure assigned to a var. The call inside
// has NO enclosing function/method declaration; the parser must fall
// back to the bare file path as `from`. Anti-regression for the
// package-level closure rule.
var Logger = func(msg string) {
	fmt.Println("[log]", msg)
}

// handle is invoked from `Server.Run` via `go handle(addr)`. Plain free
// function so the goroutine-call test exercises the direct-call query.
func handle(addr string) {
	fmt.Println("handling", addr)
}

// withLog is a higher-order function that wraps `inner` with a logging
// prefix. Inside the returned closure, the call to `Println` resolves
// to the closure's enclosing function (`withLog`), not to the bare
// file path — closures are transparent in the parent walk when an
// enclosing function/method declaration exists.
func withLog(inner func()) func() {
	return func() {
		fmt.Println("before")
		inner()
	}
}
