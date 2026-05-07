# Mixed-language fixture (Phase 7.7): Python side of the cross-language
# `helper` collision used by `crates/codegraph-tools/tests/mixed_language.rs`.
#
# Defines exactly one symbol — `helper` — so the cross-language search test
# can assert (Symbol{ name="helper", language=Python }) appears alongside
# the C++/Rust/Go-side counterparts in `foo.cpp`, `foo.rs`, and `foo.go`.
# Keep this file minimal; new symbols here will skew the snapshot and the
# search-by-language assertions.


def helper():
    pass
