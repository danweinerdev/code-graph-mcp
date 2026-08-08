#!/usr/bin/env bash
#
# code-graph SessionStart hook (Codex / Gemini / other harnesses that consume the
# SDK-standard {"additionalContext": ...} SessionStart contract).
#
# Injects a short reminder that the code-graph MCP server is available and, when it
# can cheaply tell, whether this repo already has an index on disk. Claude Code does
# NOT use this script -- it uses plugin/scripts/code-graph-session-reset.sh, which
# resets the Grep/Glob nudge throttle instead.
#
# Fails open: every error path exits 0 with no output, so a broken hook can never
# wedge session start.

set -u

emit() {
	printf '%s' "$1"
	exit 0
}

silent_exit() { exit 0; }

trap silent_exit ERR

# Resolve the project root the same way the server does: nearest ancestor holding a
# .code-graph.toml, else the git toplevel, else $PWD. Purely advisory here -- we only
# use it to look for a cache file.
find_project_root() {
	local dir
	dir="${CODE_GRAPH_PROJECT_DIR:-${CLAUDE_PROJECT_DIR:-$PWD}}"
	[ -d "$dir" ] || dir="$PWD"

	local probe="$dir"
	while [ -n "$probe" ] && [ "$probe" != "/" ]; do
		if [ -f "$probe/.code-graph.toml" ]; then
			printf '%s' "$probe"
			return 0
		fi
		probe="$(dirname "$probe")"
	done

	if command -v git >/dev/null 2>&1; then
		local top
		top="$(git -C "$dir" rev-parse --show-toplevel 2>/dev/null || true)"
		if [ -n "$top" ]; then
			printf '%s' "$top"
			return 0
		fi
	fi

	printf '%s' "$dir"
}

ROOT="$(find_project_root 2>/dev/null || printf '%s' "$PWD")"

INDEX_STATE="unknown"
if [ -f "$ROOT/.code-graph-cache.db" ]; then
	INDEX_STATE="present"
else
	INDEX_STATE="absent"
fi

CONFIG_NOTE=""
if [ ! -f "$ROOT/.code-graph.toml" ]; then
	CONFIG_NOTE=" No .code-graph.toml was found at the project root, so built-in defaults apply; for C/C++ trees that use API macros (class CORE_API Foo), those declarations will not extract until [cpp].macro_strip is configured."
fi

case "$INDEX_STATE" in
present)
	STATE_NOTE="A code-graph cache exists at $ROOT/.code-graph-cache.db, so analyze_codebase will load it and incrementally re-index changed files rather than rebuilding."
	;;
absent)
	STATE_NOTE="No code-graph cache exists at $ROOT yet. The first structural query will need analyze_codebase (or analyze_codebase_async on a large tree) before it can answer."
	;;
*)
	STATE_NOTE="Index state for $ROOT is unknown; call get_status to check before trusting a query."
	;;
esac

CONTEXT="The code-graph MCP server is available in this session. It holds a tree-sitter symbol graph of this repository with resolved call, include, and inheritance edges across C++, Rust, Go, Python, C#, and Java.

Prefer it over grep/glob/file-reading for structural questions -- it returns paths, line numbers, and signatures rather than file bodies:
- where a symbol is defined -> search_symbols, get_symbol_detail, find_class_candidates
- what a file contains -> get_file_symbols, get_symbol_summary
- what calls or is called by a symbol -> get_callers, get_callees, find_overrides
- class relationships -> get_class_hierarchy
- file dependencies, coupling, cycles -> get_dependencies, get_coupling, detect_cycles
- unused symbols and diagrams -> get_orphans, generate_diagram
- server and index state -> get_status

${STATE_NOTE}${CONFIG_NOTE}

Paginated tools return {results, total, offset, limit, truncated, next_offset}; when truncated is true, re-call with offset = next_offset. Fall back to grep only for free-text or regex searches that are not about code structure."

if command -v python3 >/dev/null 2>&1; then
	python3 -c 'import json,sys; print(json.dumps({"additionalContext": sys.stdin.read()}))' <<<"$CONTEXT" 2>/dev/null || silent_exit
else
	silent_exit
fi

exit 0
