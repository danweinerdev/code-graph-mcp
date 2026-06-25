#!/usr/bin/env bash
# code-graph-session-reset.sh — SessionStart hook.
#
# Resets the per-session nudge throttle written by code-graph-nudge.sh so the
# code-graph guidance re-injects whenever the model's context is wiped or
# replaced. SessionStart fires with a `source` discriminator:
#   startup  - fresh session
#   resume   - --continue / --resume
#   clear    - the user ran /clear  (context wiped, model forgot the nudge)
#   compact  - context was compacted (nudge may have been summarized away)
# In every one of these cases the model no longer reliably holds the earlier
# nudge, so we drop THIS session's stamp files to let the next Grep/Glob nudge
# again. This is the fix for "the nudge stays silent after /clear even though
# the model lost the guidance."
#
# Fails open (always exit 0, no stdout) — must never disrupt session startup.

set -euo pipefail

input="$(cat 2>/dev/null || true)"

state_root="${CLAUDE_PROJECT_DIR:-${TMPDIR:-/tmp}}"
state_dir="${state_root}/.code-graph-plugin/nudge"
[[ -d "${state_dir}" ]] || exit 0

session=""
source_kind=""
if command -v jq >/dev/null 2>&1 && [[ -n "${input}" ]]; then
  session="$(printf '%s' "${input}" | jq -r '.session_id // .sessionId // empty' 2>/dev/null || true)"
  source_kind="$(printf '%s' "${input}" | jq -r '.source // empty' 2>/dev/null || true)"
fi

# Drop this session's stamps (both the .Grep and .Glob stamp for the session)
# so the throttle starts fresh. Match on the sanitized session prefix the nudge
# script uses; if we couldn't read a session id, fall back to clearing the
# whole dir on an explicit context-wipe source.
if [[ -n "${session}" ]]; then
  safe="${session//[^A-Za-z0-9_-]/_}"
  find "${state_dir}" -maxdepth 1 -type f -name "${safe}.*" -delete 2>/dev/null || true
elif [[ "${source_kind}" == "clear" || "${source_kind}" == "compact" ]]; then
  find "${state_dir}" -maxdepth 1 -type f -delete 2>/dev/null || true
fi

# Housekeeping: prune stamps from long-dead sessions regardless of source.
find "${state_dir}" -maxdepth 1 -type f -mtime +1 -delete 2>/dev/null || true

exit 0
