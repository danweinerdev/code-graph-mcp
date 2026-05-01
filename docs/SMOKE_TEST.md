# Manual Smoke Test (MCP client)

This document describes the manual end-to-end smoke test for `code-graph-mcp` against a real MCP client. The same behaviors have automated coverage:

- `crates/codegraph-tools/tests/watch_race.rs` — watch + analyze concurrency, atomic-save coalescing, removal end-to-end
- `crates/codegraph-tools/tests/watch_dangling_edges.rs` — re-index does not leave dangling cross-file edges after rename
- `crates/codegraph-tools/tests/testdata_cpp_baseline.rs` — analyze_codebase baseline counts on `testdata/cpp`

Documented for human verification — automated coverage of the same behaviors lives in `tests/watch_race.rs` and `tests/watch_dangling_edges.rs`.

## Prerequisites

1. A working `code-graph-mcp` binary on PATH (see [`../README.md`](../README.md#installation)).
2. An MCP-compatible client (Claude Desktop, Claude Code, or any MCP client). Register the binary as an MCP server per the README's "MCP client configuration" section.
3. The repo's `testdata/cpp/` directory available locally — this smoke test indexes it.

## Steps

### 1. Index a small project

From the MCP client, invoke `analyze_codebase` against `testdata/cpp`:

```
analyze_codebase(path="/absolute/path/to/code-graph-mcp/testdata/cpp")
```

Expect a response with non-zero `files`, `symbols`, and `edges` counts (the locked-in baseline from `testdata_cpp_baseline.rs` is the source of truth for exact numbers).

### 2. Start watch mode and modify a file

```
watch_start()
```

Confirm the response indicates watch mode is active. From a separate shell:

```bash
echo 'void g() {}' >> /absolute/path/to/code-graph-mcp/testdata/cpp/<some-file>.cpp
```

Wait at least 1 second for the 250ms debounce window plus reindex time.

### 3. Verify the new symbol is visible

```
get_file_symbols(file="/absolute/path/to/code-graph-mcp/testdata/cpp/<some-file>.cpp")
```

Expect `g` to appear in the symbols list. (Make sure to revert the file edit afterwards, e.g. `git checkout testdata/cpp/<some-file>.cpp`.)

### 4. Stop watch mode

```
watch_stop()
```

Confirm the response indicates watch mode stopped successfully. A second `watch_stop()` should report watch mode is not active.

### 5. Restart binary, verify cache hit

Quit the MCP client (or restart the server entry). Re-issue:

```
analyze_codebase(path="/absolute/path/to/code-graph-mcp/testdata/cpp")
```

Expect zero parse time / a "cache hit" indication in the response — the JSON cache at `testdata/cpp/.code-graph-cache.json` should be loaded directly since no files have changed by mtime.

## Pass criteria

- All five steps complete without errors.
- The new symbol `g` appears in `get_file_symbols` after the file edit (step 3).
- Restarting the binary loads the cache without re-parsing (step 5).

## Rust parser dogfood pass

In addition to the MCP smoke test above, the `parse-test` developer harness can be run against the workspace itself to confirm the Rust parser handles a real production codebase. This was the Phase 5.5 dogfood gate.

### Procedure

```bash
cargo build --release -p codegraph-parse-test
./target/release/codegraph-parse-test crates/
```

### Pass criteria

- 0 crashes (the binary exits with status 0).
- 0 warnings (the `=== Warnings (N) ===` section is absent or empty).
- The trailing `Done:` line reports a non-zero file/symbol/edge count.
- Spot-check that `LanguagePlugin` appears as a `[trait]` symbol in `crates/codegraph-lang/src/lib.rs`.
- Spot-check that an `[inherits]` edge `CppParser -> LanguagePlugin` is present.
- Spot-check that an `[inherits]` edge `RustParser -> LanguagePlugin` is present.
- Spot-check that `Graph::merge_file_graph` appears as a `[method]` with parent `Graph` (line in `crates/codegraph-graph/src/graph.rs`).
- Phase 5.5 baseline (host-target, this repo, `crates/`): 43 files, 835 symbols, 7651 edges, 0 warnings — drift here is a sign the Rust parser changed behavior, not necessarily a regression. Update this number when the workspace itself changes.

The harness is exploratory by design — there is no automated assertion for the workspace-wide totals (the workspace evolves; the assertion would either be brittle or meaningless). The Phase 5.5 corpus test (`crates/codegraph-lang-rust/tests/corpus.rs`) is the deterministic regression gate; this dogfood pass is the "does it survive contact with reality" check.
