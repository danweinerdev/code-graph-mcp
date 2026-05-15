# Rust workspace build targets. Build natively on each platform you need
# the binary for — `make release` produces a host-target release build.

.PHONY: build release test lint fmt fmt-check clean \
	snapshot-clean snapshot-accept snapshot-audit install-hooks submodules \
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

# Accept exactly one pending insta snapshot by name. `cargo insta
# accept --snapshot <name>` silently no-ops in this repo's layout (the
# CLI's name matching does not line up with the nested snapshot path),
# so a single snapshot has to be promoted by hand: `mv X.snap.new
# X.snap`. This target does that safely — it refuses to act unless
# EXACTLY one `*.snap.new` matches the given stem, so a typo or an
# over-broad fragment can't silently promote the wrong file or sweep
# several. After promoting it runs the `snapshot-clean` gate so any
# OTHER still-pending snapshot is surfaced immediately rather than
# riding along in the next commit.
#
#   make snapshot-accept FILE=snapshot_tools_list__tools_list_get_orphans
#
# FILE is the snapshot stem; a trailing .snap or .snap.new is tolerated.
snapshot-accept:
	@if [ -z "$(FILE)" ]; then \
		echo "✗ FILE is required: make snapshot-accept FILE=<snapshot-stem>"; \
		exit 1; \
	fi
	@stem=$$(echo "$(FILE)" | sed -e 's/\.snap\.new$$//' -e 's/\.snap$$//'); \
	matches=$$(find crates -type f -name "$$stem.snap.new" 2>/dev/null); \
	count=$$(printf '%s' "$$matches" | grep -c . || true); \
	if [ "$$count" -eq 0 ]; then \
		echo "✗ No pending snapshot matching '$$stem.snap.new' under crates/."; \
		echo "  (run the failing snapshot test first, or check the stem)"; \
		exit 1; \
	elif [ "$$count" -gt 1 ]; then \
		echo "✗ '$$stem' is ambiguous — $$count pending files match:"; \
		echo "$$matches" | sed 's/^/    /'; \
		echo "  Pass the full snapshot stem so exactly one matches."; \
		exit 1; \
	fi; \
	target=$$(printf '%s' "$$matches" | sed 's/\.snap\.new$$/.snap/'); \
	mv "$$matches" "$$target"; \
	echo "✓ Accepted $$target"
	@$(MAKE) --no-print-directory snapshot-clean

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
# curl, abseil-cpp, efcore, commons-lang). Each submodule is pinned to
# a specific upstream tag — see `.gitmodules` and `tests/baselines/*.txt`
# / `testdata/<lang>/<name>-baseline.txt` for the recorded pin + symbol
# count.
#
# Dogfood tests auto-skip when their submodule is not initialized, so
# this target is opt-in: clone what you want to dogfood against. The
# `--depth 1` flag below keeps clones small — full-depth would be
# multiple hundreds of MB (dominated by curl, abseil-cpp, and efcore).
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
