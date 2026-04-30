# code-graph-mcp

An MCP server that builds an in-memory semantic code graph from C++ source files using [tree-sitter](https://tree-sitter.github.io/), exposing 15 query tools to AI agents over stdio. Instead of an agent burning tokens grepping files to understand how code is connected, it calls `analyze_codebase` once and then issues targeted queries like `get_callers`, `get_class_hierarchy`, or `generate_diagram` to navigate the codebase instantly.

## Installation

### Prebuilt binaries

> TODO: Prebuilt binaries are not yet published. Once releases are cut, download the appropriate archive from the [GitHub Releases](https://github.com/danweinerdev/code-graph-mcp/releases) page and extract the `code-graph-mcp` binary plus the bundled `.code-graph.toml.example` to a directory on your PATH.

### From source via `cargo install`

```bash
git clone https://github.com/danweinerdev/code-graph-mcp.git
cd code-graph-mcp
cargo install --path crates/code-graph-mcp
```

This installs `code-graph-mcp` to `~/.cargo/bin/` (which should already be on your PATH if you have a working Rust toolchain).

### From source via `cargo build`

```bash
git clone https://github.com/danweinerdev/code-graph-mcp.git
cd code-graph-mcp
make build                # cargo build --release -p code-graph-mcp
```

The binary lands at `target/release/code-graph-mcp`. Symlink or copy it into your PATH:

```bash
ln -s "$(pwd)/target/release/code-graph-mcp" ~/.local/bin/code-graph-mcp
```

No CGo or C toolchain required — the tree-sitter grammars link via their pure-Rust `cc`-built crates.

## MCP client configuration

Register the binary as an MCP server in your client. For Claude Desktop, edit `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) or the platform equivalent:

```json
{
  "mcpServers": {
    "code-graph": {
      "command": "/path/to/code-graph-mcp"
    }
  }
}
```

For Claude Code, add the equivalent block to your project's `.claude/settings.json`. Any MCP-compatible client should work — the server speaks [Model Context Protocol](https://modelcontextprotocol.io/) over stdio with no CLI flags.

## Configuration

Each indexed root may contain a `.code-graph.toml` file controlling discovery and parsing knobs. See [`.code-graph.toml.example`](.code-graph.toml.example) at the repo root for the documented schema.

Schema (all keys optional; defaults shown):

```toml
[discovery]
max_threads = 0           # 0 = auto (num CPUs); over-cap values are clamped
respect_gitignore = true  # honor .gitignore / .ignore / global ignore files
follow_symlinks = false   # follow symlinks during discovery
extra_ignore = []         # additional gitignore-style globs to exclude

[parsing]
max_threads = 0           # 0 = auto; same clamping rule as discovery
```

The file is read once per `analyze_codebase` call from `<root>/.code-graph.toml` and cached on the server for subsequent watch events. Missing file → defaults. Malformed TOML → `analyze_codebase` fails with a parse error (no silent fallback).

## Tools

The server exposes 15 tools. Descriptions are copied verbatim from the `#[tool(description = "...")]` attributes in `crates/codegraph-tools/src/server.rs` (the source of truth).

### Indexing

| Tool | Description |
|------|-------------|
| `analyze_codebase` | Index a codebase (C/C++, Rust, Go, Python) and build the code graph. Must be called before any query tools. |

The index is cached to `.code-graph-cache.json` in the indexed directory. On subsequent calls, files unchanged by mtime are loaded from cache; only modified files are re-parsed. Use `force=true` to re-index from scratch.

### Symbol queries

| Tool | Description |
|------|-------------|
| `get_file_symbols` | List all symbols (functions, classes, etc.) defined in a file |
| `search_symbols` | Search for symbols by name pattern across the indexed codebase. Returns paginated results. Default brief mode omits signatures for token efficiency. |
| `get_symbol_detail` | Get full details for a symbol by its ID |
| `get_symbol_summary` | Get symbol counts grouped by namespace and kind — useful for codebase orientation |

Symbol IDs use the format `file:name` for free functions and `file:Parent::name` for methods (e.g., `/path/engine.cpp:Engine::update`).

### Call graph

| Tool | Description |
|------|-------------|
| `get_callers` | Find functions that call the given symbol (upstream call chain) |
| `get_callees` | Find functions called by the given symbol (downstream call chain) |

### Dependencies

| Tool | Description |
|------|-------------|
| `get_dependencies` | List files included/imported by the given file |

### Structural analysis

| Tool | Description |
|------|-------------|
| `detect_cycles` | Detect circular include dependencies in the indexed codebase |
| `get_orphans` | Find symbols with no incoming call edges (uncalled functions/methods) |
| `get_class_hierarchy` | Get the inheritance tree for a class (base classes and derived classes) |
| `get_coupling` | Get cross-file dependency counts for a file |

### Visualization

| Tool | Description |
|------|-------------|
| `generate_diagram` | Generate a graph diagram: call graph (symbol), file dependencies (file), or inheritance tree (class). Returns edges as JSON by default, or Mermaid syntax when format=mermaid. |

### Watch mode

| Tool | Description |
|------|-------------|
| `watch_start` | Start watching the indexed directory for file changes and auto-reindex modified files |
| `watch_stop` | Stop watching for file changes |

Watch mode uses [notify-debouncer-full](https://docs.rs/notify-debouncer-full) with a 250ms debounce window. Re-indexing is index-lock-aware: if `analyze_codebase` is in flight, the event is dropped (the in-flight analyze will pick up the file's current state anyway).

## C++ Parser Limitations

Validated against tree-sitter-cpp v0.23.4.

### Supported C++ Patterns

- Free functions, qualified methods (`Class::method`), inline methods in class bodies
- Classes, structs, enums (including `enum class`), typedefs, `using` aliases
- Function pointer typedefs (`typedef void (*Callback)(int)`)
- Operator overloads (`operator+`, `operator==`, etc.) — both in-class and free
- Auto return types (trailing `-> T` and deduced)
- Nested classes/structs (Parent field set correctly)
- Lambda call edges (calls inside and to lambdas)
- All call patterns: free, method, arrow, qualified, template

### Known Limitations

1. **Macro-generated definitions** — Macros like `DEFINE_HANDLER(name)` that expand to function definitions are not visible to tree-sitter (it sees the macro call, not the expansion). Macro invocations that look like function calls ARE captured as call edges.

2. **Complex template metaprogramming** — Deeply nested template specializations may produce incomplete or error-containing AST nodes. The parser skips error nodes gracefully.

3. **Call resolution is heuristic** — Call edges are resolved via scope-aware heuristic matching (same file > same class > same namespace > global). This is syntactic, not semantic — overloaded functions may resolve to the wrong candidate.

4. **C++ cast expressions** — `static_cast`, `dynamic_cast`, `const_cast`, `reinterpret_cast` are filtered out (tree-sitter parses them as call expressions).

5. **Forward declarations excluded** — Only `function_definition` (with body) produces symbols. Forward declarations (`void foo();`) are intentionally excluded to avoid duplicates.

6. **Template method calls** — `obj.foo<T>()` via `template_method` node type is not matched in tree-sitter-cpp v0.23.4. These calls fall through to the regular `field_expression` pattern when possible.

## Building from source / Cross-compilation

The repo cross-compiles all 6 supported targets from a single Linux host using [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild), which routes the C compiler through `zig cc`.

**One-time setup (Fedora; adapt the package manager line for other distros):**

```bash
# 1. Rust targets
rustup target add \
  x86_64-unknown-linux-gnu \
  x86_64-unknown-linux-musl \
  aarch64-unknown-linux-musl \
  x86_64-apple-darwin \
  aarch64-apple-darwin \
  x86_64-pc-windows-gnu

# 2. cargo-zigbuild
cargo install cargo-zigbuild

# 3. zig itself
sudo dnf install zig          # Fedora
# brew install zig            # macOS
# sudo apt install zig        # Debian/Ubuntu (may need backports)
```

**Build:**

```bash
make release-all              # all 6 targets in sequence
make release-all -j6          # build them in parallel
make release-linux-x86_64-gnu # one specific target
make release-host-smoke       # host-only sanity build (used in dev)
make release-tar              # Linux x86_64-gnu binary + sample config + README + LICENSE
                              # → dist/code-graph-mcp-x86_64-linux-gnu.tar.gz
```

Output binaries land at `bin/<rust-triple>/code-graph-mcp(.exe)`. The release profile in the workspace `Cargo.toml` strips symbols and applies thin LTO with `codegen-units = 1`; the host (`x86_64-unknown-linux-gnu`) binary is well under the 30 MB ceiling.

**CI:** see [`.github/workflows/release.yml`](.github/workflows/release.yml) for a `workflow_dispatch` job that reproduces `make release-all` on a Linux runner and uploads the `bin/` tree as a workflow artifact.

## Smoke test

Watch-mode and incremental-cache behavior have automated coverage in `crates/codegraph-tools/tests/watch_race.rs` and `crates/codegraph-tools/tests/watch_dangling_edges.rs`. For end-to-end validation against an MCP client, see [`docs/SMOKE_TEST.md`](docs/SMOKE_TEST.md).

## License

See [LICENSE](LICENSE).
