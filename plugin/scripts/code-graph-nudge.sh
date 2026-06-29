#!/usr/bin/env bash
# code-graph-nudge.sh — PreToolUse hook for Grep/Glob.
#
# Goal: when the model reaches for a raw text/path search that a structural
# query would answer better, inject a one-time, NON-BLOCKING nudge toward the
# code-graph MCP tools. We never block: Grep/Glob are legitimately the right
# tool for free text, comments, config files, and anything outside the indexed
# language set. The nudge is advisory context only.
#
# Contract (Claude Code PreToolUse hook):
#   - stdin: a JSON object with at least { tool_name, tool_input, session_id }.
#     Field spellings vary across versions, so we read defensively.
#   - stdout (exit 0): a JSON object whose
#       .hookSpecificOutput.additionalContext
#     is injected into the model's context for its next turn. We emit NO
#     permission decision, so the tool runs exactly as it would have.
#   - Any failure path exits 0 with no stdout (fail-open): a broken hook must
#     never wedge a search.
#
# Throttle: at most one nudge per (session, tool-kind) per
# NUDGE_COOLDOWN_SECONDS, tracked by a stamp file under the session state dir,
# so a search-heavy turn doesn't get spammed.

set -euo pipefail

# Fail-open helper: emit nothing, let the tool proceed untouched.
pass_through() { exit 0; }

# Resolve the per-user, per-project state dir holding the nudge throttle stamps.
#
# Rooted at a user-level XDG state dir ($XDG_STATE_HOME, else ~/.local/state),
# falling back to $TMPDIR/tmp when no usable HOME is set — so we NEVER write into
# the project tree. Earlier versions rooted directly at $CLAUDE_PROJECT_DIR,
# which littered the working tree with a `.code-graph-plugin/` dir (and, when the
# project was a bind-mounted container workdir, leaked those files onto the
# host). $CLAUDE_PROJECT_DIR is still honoured, but ONLY to namespace the state
# per project (basename + a hash of the full path), preserving the original
# per-project throttling without the pollution.
#
# Keep this function byte-identical to the copy in code-graph-session-reset.sh so
# both hooks resolve the same directory.
code_graph_state_dir() {
  local base proj ns
  if [[ -n "${XDG_STATE_HOME:-}" ]]; then
    base="${XDG_STATE_HOME}/code-graph-plugin"
  elif [[ -n "${HOME:-}" ]]; then
    base="${HOME}/.local/state/code-graph-plugin"
  else
    base="${TMPDIR:-/tmp}/code-graph-plugin"
  fi
  proj="${CLAUDE_PROJECT_DIR:-}"
  if [[ -n "${proj}" ]]; then
    # basename for readability + a cksum of the full path for collision safety.
    local h
    h="$(printf '%s' "${proj}" | cksum 2>/dev/null | cut -d' ' -f1)"
    ns="$(basename "${proj}")-${h:-0}"
  else
    ns="no-project"
  fi
  # Sanitise the namespace to a safe single path component.
  printf '%s/nudge/%s' "${base}" "${ns//[^A-Za-z0-9_.-]/_}"
}

# jq is required to parse the event and build valid JSON output. If it's
# missing, silently pass through rather than risk malformed stdout.
command -v jq >/dev/null 2>&1 || pass_through

input="$(cat || true)"
[[ -n "${input}" ]] || pass_through

# Tolerate field-name drift: tool_name|tool, and the session id under a few keys.
tool="$(printf '%s' "${input}" | jq -r '.tool_name // .tool // empty' 2>/dev/null || true)"
session="$(printf '%s' "${input}" | jq -r '.session_id // .sessionId // .session // "default"' 2>/dev/null || true)"
[[ -n "${tool}" ]] || pass_through

case "${tool}" in
  Grep) pattern="$(printf '%s' "${input}" | jq -r '.tool_input.pattern // empty' 2>/dev/null || true)" ;;
  Glob) pattern="$(printf '%s' "${input}" | jq -r '.tool_input.pattern // .tool_input.glob // empty' 2>/dev/null || true)" ;;
  *)    pass_through ;;
esac

# Heuristic: only nudge when the query looks like it's hunting for a code
# symbol (a bare identifier, a Type::method / pkg.Func / mod::path token, or a
# C++/Rust-ish qualified name). Free-text searches, regex with anchors/classes,
# and obvious prose ("TODO", "error:", quoted sentences) are left alone — Grep
# is the right tool there and a nudge would be noise.
looks_like_symbol() {
  local p="$1"
  # Empty or very short -> not worth nudging.
  [[ -n "${p}" && "${#p}" -ge 3 ]] || return 1
  # Contains whitespace -> probably free text.
  [[ "${p}" == *" "* ]] && return 1
  # Contains regex metacharacters that signal a text/pattern search, not a
  # plain symbol name. ('::' and '.' are allowed — they appear in qualified
  # symbol names; everything else here is a regex tell.)
  case "${p}" in
    *'['*|*']'*|*'('*|*')'*|*'|'*|*'\'*|*'^'*|*'$'*|*'*'*|*'+'*|*'?'*|*'{'*|*'}'*) return 1 ;;
  esac
  # Identifier-ish: letters/digits/underscore plus the qualified-name joiners
  # '.' and ':'. If it matches, treat as a symbol query.
  [[ "${p}" =~ ^[A-Za-z_][A-Za-z0-9_.:]*$ ]]
}

# Glob patterns are path globs, not symbol names — nudge whenever the model is
# globbing source files, since get_file_symbols / search_symbols(subtree=…) is
# usually the better entry point. For Grep, gate on the symbol heuristic.
if [[ "${tool}" == "Grep" ]]; then
  looks_like_symbol "${pattern}" || pass_through
fi

# --- Throttle: one nudge per (session, tool) per cooldown window. -----------
NUDGE_COOLDOWN_SECONDS="${CODE_GRAPH_NUDGE_COOLDOWN:-900}"
# State dir for the throttle stamps — per-user, per-project, never written into
# the project tree (see code_graph_state_dir above). Keep the resolver IDENTICAL
# to the one in code-graph-session-reset.sh.
state_dir="$(code_graph_state_dir)"
mkdir -p "${state_dir}" 2>/dev/null || state_dir="${TMPDIR:-/tmp}"
stamp="${state_dir}/${session//[^A-Za-z0-9_-]/_}.${tool}"

now="$(date +%s 2>/dev/null || echo 0)"
if [[ -f "${stamp}" ]]; then
  last="$(cat "${stamp}" 2>/dev/null || echo 0)"
  [[ "${last}" =~ ^[0-9]+$ ]] || last=0
  if [[ "${now}" -gt 0 && $((now - last)) -lt "${NUDGE_COOLDOWN_SECONDS}" ]]; then
    pass_through
  fi
fi
printf '%s' "${now}" > "${stamp}" 2>/dev/null || true

# --- The nudge. Tailored per tool. -----------------------------------------
if [[ "${tool}" == "Grep" ]]; then
  message="code-graph MCP is available and indexes this codebase structurally (C/C++, Rust, Go, Python, C#, Java). For a symbol-shaped query like '${pattern}', a structural tool is usually faster and more precise than grep:
- Finding a definition / where something is declared → mcp__code-graph__search_symbols (name pattern, optional kind/namespace/subtree filters) or mcp__code-graph__get_file_symbols.
- Who calls X / what does X call → mcp__code-graph__get_callers / mcp__code-graph__get_callees (resolved call edges, not text matches).
- Type/class relationships → mcp__code-graph__get_class_hierarchy, mcp__code-graph__find_overrides.
Grep still wins for free text, comments, strings, config, or anything outside the indexed languages — proceed with grep if that's what you need. (Requires analyze_codebase to have run; see the code-graph-indexing skill.)"
else
  message="code-graph MCP is available and indexes this codebase structurally. If you're globbing source files to find symbols or map a subtree, these are usually a better entry point than a path glob:
- All symbols defined in a file → mcp__code-graph__get_file_symbols.
- Symbols matching a name under a subtree → mcp__code-graph__search_symbols with the 'subtree' filter (O(subtree), not O(repo)).
- A namespace/kind census of an area → mcp__code-graph__get_symbol_summary.
Glob still wins for locating files by path/extension or non-source assets — proceed if that's the goal. (Requires analyze_codebase to have run; see the code-graph-indexing skill.)"
fi

jq -n --arg ctx "${message}" '{
  hookSpecificOutput: {
    hookEventName: "PreToolUse",
    additionalContext: $ctx
  }
}'
exit 0
