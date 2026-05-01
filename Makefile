# Rust workspace build targets. Build natively on each platform you need
# the binary for — `make release` produces a host-target release build.

.PHONY: build release test lint fmt fmt-check clean \
	rust-build rust-test rust-lint rust-fmt rust-fmt-check rust-clean

# Default `build` is a host-target release build of the binary crate.
build:
	cargo build --release -p code-graph-mcp
	@echo ">>> target/release/code-graph-mcp built ($$(du -h target/release/code-graph-mcp | cut -f1))"

# `release` is an alias for `build` — kept as the canonical name for
# producing a distributable host binary.
release: build

test:
	cargo test --workspace

lint:
	cargo clippy --workspace --all-targets -- -D warnings

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all --check

clean:
	cargo clean

# ---- Rust workspace targets (rust-prefixed aliases) -------------------
# These coexist with the unprefixed defaults above for callers that want
# explicit `rust-` naming.

rust-build:
	cargo build --workspace

rust-test:
	cargo test --workspace

rust-lint:
	cargo clippy --workspace --all-targets -- -D warnings

rust-fmt:
	cargo fmt --all

rust-fmt-check:
	cargo fmt --all --check

rust-clean:
	cargo clean
