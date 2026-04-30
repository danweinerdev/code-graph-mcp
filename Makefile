# Rust workspace build targets.
#
# The host-target convenience targets (`build`, `test`, etc.) build/run
# against the host triple via plain cargo. The `release-*` targets cross-
# compile via cargo-zigbuild — see the "Rust release builds" block below.

.PHONY: build test lint fmt fmt-check clean \
	rust-build rust-test rust-lint rust-fmt rust-fmt-check rust-clean

# Default `build` is a host-target release build of the binary crate.
build:
	cargo build --release -p code-graph-mcp

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
	rm -rf bin/

# ---- Rust workspace targets (rust-prefixed aliases) -------------------
# These coexist with the unprefixed defaults above for callers that want
# explicit `rust-` naming (kept stable across the Phase 4 cutover).

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

# === Rust release builds (Phase 4.3) ===================================
# Cross-compile the Rust binary for all 6 target platforms from a single
# Linux host using cargo-zigbuild. Output layout:
#   bin/<rust-triple>/code-graph-mcp(.exe)
#
# Prerequisites (one-time, on the build host):
#   rustup target add x86_64-unknown-linux-gnu \
#                     x86_64-unknown-linux-musl \
#                     aarch64-unknown-linux-musl \
#                     x86_64-apple-darwin \
#                     aarch64-apple-darwin \
#                     x86_64-pc-windows-gnu
#   cargo install cargo-zigbuild
#   # zig: dnf install zig  (Fedora) / brew install zig (macOS) / etc.
#
# Stripping is handled by `[profile.release].strip = "symbols"` in the
# workspace Cargo.toml -- no post-build `strip`/`llvm-strip` invocation
# is needed (important for the windows-gnu target, which would otherwise
# need a separate `llvm-strip`).

RUST_BIN := code-graph-mcp
RUST_TARGETS := \
	x86_64-unknown-linux-gnu \
	x86_64-unknown-linux-musl \
	aarch64-unknown-linux-musl \
	x86_64-apple-darwin \
	aarch64-apple-darwin \
	x86_64-pc-windows-gnu

.PHONY: release-all release-host-smoke \
	release-linux-x86_64-gnu release-linux-x86_64-musl release-linux-aarch64-musl \
	release-darwin-x86_64 release-darwin-aarch64 release-windows-x86_64-gnu

# Aggregate target. Run with `-jN` for parallel cross-builds; each recipe
# is independent (separate target dirs / output paths).
release-all: release-linux-x86_64-gnu release-linux-x86_64-musl release-linux-aarch64-musl \
	release-darwin-x86_64 release-darwin-aarch64 release-windows-x86_64-gnu

release-linux-x86_64-gnu:
	cargo zigbuild --release --target x86_64-unknown-linux-gnu -p $(RUST_BIN)
	mkdir -p bin/x86_64-unknown-linux-gnu
	cp target/x86_64-unknown-linux-gnu/release/$(RUST_BIN) bin/x86_64-unknown-linux-gnu/$(RUST_BIN)

release-linux-x86_64-musl:
	cargo zigbuild --release --target x86_64-unknown-linux-musl -p $(RUST_BIN)
	mkdir -p bin/x86_64-unknown-linux-musl
	cp target/x86_64-unknown-linux-musl/release/$(RUST_BIN) bin/x86_64-unknown-linux-musl/$(RUST_BIN)

release-linux-aarch64-musl:
	cargo zigbuild --release --target aarch64-unknown-linux-musl -p $(RUST_BIN)
	mkdir -p bin/aarch64-unknown-linux-musl
	cp target/aarch64-unknown-linux-musl/release/$(RUST_BIN) bin/aarch64-unknown-linux-musl/$(RUST_BIN)

release-darwin-x86_64:
	cargo zigbuild --release --target x86_64-apple-darwin -p $(RUST_BIN)
	mkdir -p bin/x86_64-apple-darwin
	cp target/x86_64-apple-darwin/release/$(RUST_BIN) bin/x86_64-apple-darwin/$(RUST_BIN)

release-darwin-aarch64:
	cargo zigbuild --release --target aarch64-apple-darwin -p $(RUST_BIN)
	mkdir -p bin/aarch64-apple-darwin
	cp target/aarch64-apple-darwin/release/$(RUST_BIN) bin/aarch64-apple-darwin/$(RUST_BIN)

release-windows-x86_64-gnu:
	cargo zigbuild --release --target x86_64-pc-windows-gnu -p $(RUST_BIN)
	mkdir -p bin/x86_64-pc-windows-gnu
	cp target/x86_64-pc-windows-gnu/release/$(RUST_BIN).exe bin/x86_64-pc-windows-gnu/$(RUST_BIN).exe

# Host-only smoke check. Used during Phase 4.3 to confirm the release
# profile builds without invoking the full multi-platform cross-build.
# Falls back to a plain `cargo build --release` if cargo-zigbuild is not
# installed. The binary itself is a stdio MCP server with no CLI flags
# (no `--version`, etc.) -- the smoke check stops at "binary exists and
# is executable"; full smoke testing happens against an MCP client.
release-host-smoke:
	@if command -v cargo-zigbuild >/dev/null 2>&1; then \
		echo ">>> cargo-zigbuild available -- building x86_64-unknown-linux-gnu"; \
		$(MAKE) release-linux-x86_64-gnu; \
		test -x bin/x86_64-unknown-linux-gnu/$(RUST_BIN) && \
			echo ">>> bin/x86_64-unknown-linux-gnu/$(RUST_BIN) built ($$(du -h bin/x86_64-unknown-linux-gnu/$(RUST_BIN) | cut -f1))"; \
	else \
		echo ">>> cargo-zigbuild not found -- falling back to host cargo build"; \
		cargo build --release -p $(RUST_BIN); \
		test -x target/release/$(RUST_BIN) && \
			echo ">>> target/release/$(RUST_BIN) built ($$(du -h target/release/$(RUST_BIN) | cut -f1))"; \
	fi
