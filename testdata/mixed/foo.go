// Mixed-language fixture (Phase 6.6): Go side of the cross-language
// `helper` collision used by `crates/codegraph-tools/tests/mixed_language.rs`.
//
// Defines exactly one symbol — `helper` — so the cross-language search test
// can assert (Symbol{ name="helper", language=Go }) appears alongside the
// C++-side counterpart in `foo.cpp` and the Rust-side counterpart in
// `foo.rs`. Keep this file minimal; new symbols here will skew the
// snapshot and the search-by-language assertions.

package mixed

func helper() {}
