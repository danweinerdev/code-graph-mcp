// Package main — entry point for the Go parser corpus.
//
// Exercises every import form Phase 6.4 supports:
//   - single import: `import "fmt"` (in helpers.go and elsewhere)
//   - grouped imports (block below)
//   - aliased import: `umath "code-graph-go-corpus/utils"`
//   - dot import: `. "code-graph-go-corpus/models"` — symbols brought
//     into scope without qualification (we use `Map` unqualified below)
//   - blank import: `_ "image/png"` — side-effect-only import
//
// Symbol contract for `main.go` (asserted by `MANIFEST.md`):
//   Functions: `main`, `init` (2)
//
// Calls inside this file (4 Calls edges total):
//   - `main -> New`     (1, package-qualified `server.New`)
//   - `main -> Run`     (1, method-call selector on the *Server)
//   - `main -> Println` (1, package-qualified `fmt.Println`)
//   - `init -> Add`     (1, aliased package call: `umath.Add`)
//
// Imports (5 Includes edges total):
//   - `fmt`
//   - `code-graph-go-corpus/server`
//   - `code-graph-go-corpus/utils`         (aliased — alias dropped)
//   - `code-graph-go-corpus/models`        (dot — `.` dropped)
//   - `image/png`                          (blank — `_` dropped)
//
// All five forms parse as `import_spec` with the same `path` field, so
// the import query produces five edges — one per spec — regardless of
// the alias / dot / blank prefix. Aliases and `.` / `_` are NEVER
// recorded as the `to` field.
package main

import (
	"fmt"

	"code-graph-go-corpus/server"

	umath "code-graph-go-corpus/utils"

	. "code-graph-go-corpus/models"

	_ "image/png"
)

// init runs before main. Exercises the aliased-import call form: the
// `umath.Add` selector resolves to its trailing field (`Add`) per the
// call-query contract; the alias `umath` is in the operand position
// and is not part of the captured To field.
func init() {
	_ = umath.Add(1, 2)
}

// main constructs a Server, runs it, and prints a summary using the
// dot-imported `Map` from `models`. The dot-import means `Map` is
// referenced unqualified — but we don't actually call `Map` here
// because doing so would add another generic-instantiation edge that
// complicates the count. Instead we keep main's body to three calls
// (`New`, `Run`, `Println`) for predictability.
func main() {
	srv := server.New(":0")
	_ = srv.Run()
	fmt.Println("done")
}
