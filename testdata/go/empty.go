// Package main — anti-regression fixture for the empty-file edge case.
//
// This file contains only the `package` clause. It MUST produce zero
// symbols (the package_clause itself is consumed without emitting a
// Symbol — the package name lives only in `Symbol.namespace` on other
// symbols in the same package) and zero edges (no imports, no calls).
//
// Symbol contract for `empty.go` (asserted by `MANIFEST.md`):
//   - 0 symbols
//   - 0 edges
package main
