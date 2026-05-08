# Rust workspace build targets. Build natively on each platform you need
# the binary for — `make release` produces a host-target release build.

.PHONY: build release test lint fmt fmt-check clean \
	snapshot-clean snapshot-audit install-hooks submodules \
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

# Verify no pending insta snapshots in the working tree. `*.snap.new`
# files exist when a snapshot test produced new output that hasn't been
# accepted via `cargo insta review`. Forgetting that step ships a
# "passing" commit whose snapshot is actually stale — CI will fail on
# the next clean checkout. This target is the gate; it's also what the
# pre-commit hook (scripts/hooks/pre-commit) calls.
#
# Uses `find` rather than `cargo insta pending-snapshots` so the check
# is fast and works even if `cargo-insta` isn't installed.
snapshot-clean:
	@pending=$$(find crates -type f -name '*.snap.new' 2>/dev/null); \
	if [ -n "$$pending" ]; then \
		echo "✗ Pending insta snapshots:"; \
		echo "$$pending" | sed 's/^/    /'; \
		echo ""; \
		echo "Run 'cargo insta review' to accept or reject before committing."; \
		exit 1; \
	fi
	@echo "✓ No pending snapshots."

# Snapshot delta audit: assert that any modified/untracked snapshot
# file matches an expected name fragment. Catches accidental cross-tool
# effects when a wire-format change "improvement" silently regenerates
# an unrelated tool's snapshot. Invoked per-phase with the known
# expected fragments — see `scripts/snapshot-audit.sh` for usage.
#
# Pass fragments via ARGS:
#   make snapshot-audit ARGS="response_get_orphans tools_list_get_orphans"
snapshot-audit:
	@scripts/snapshot-audit.sh $(ARGS)

# Initialize the optional `external/<repo>` git submodules used by the
# per-language dogfood baseline tests (logrus, requests, ripgrep, fmt,
# curl, abseil-cpp). Each submodule is pinned to a specific upstream
# tag — see `.gitmodules` and `tests/baselines/*.txt` /
# `testdata/<lang>/<name>-baseline.txt` for the recorded pin + symbol
# count.
#
# Dogfood tests auto-skip when their submodule is not initialized, so
# this target is opt-in: clone what you want to dogfood against. Use
# `--depth 1` to keep clones small (~55MB total at full depth, mostly
# curl + abseil).
#
# Single submodule: `git submodule update --init external/<name>`.
submodules:
	@git submodule update --init --depth 1 external/
	@echo "✓ External submodules initialized — dogfood tests will now run."

# One-time setup: point git at the tracked hook scripts under
# scripts/hooks/ so pre-commit checks fire on every commit. Run this
# once after cloning. The hooks themselves are tracked in the repo, so
# updates land via `git pull` without re-running this command.
install-hooks:
	@git config core.hooksPath scripts/hooks
	@echo "✓ Hooks installed (core.hooksPath = scripts/hooks)"
	@echo "  Active hooks: $$(ls scripts/hooks/ | tr '\n' ' ')"

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
