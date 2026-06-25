#!/usr/bin/env bash
# claude-container.sh — spawn a podman dev container for this repo and launch Claude Code.
#
# Usage:
#   ./claude-container.sh                # build (if needed) + launch claude in /src
#   ./claude-container.sh shell          # drop into bash instead of claude
#   ./claude-container.sh -- cargo test  # run an arbitrary command in the container
#   ./claude-container.sh --rebuild      # force-rebuild the image
#
# All path resolution, mounts, image (re)build, and `podman run` wiring live in
# scripts/containers.sh. This entrypoint only declares the Claude-specific bits:
# image tag, Containerfile, build context, persisted state (~/.claude +
# ~/.claude.json), and the auth env vars.
#
# Cache layout (host -> container):
#   ./.cache/cargo   -> /cache/cargo   (CARGO_HOME: registry, git, installed bins)
#   ./.cache/target  -> /cache/target  (CARGO_TARGET_DIR: build outputs)
#
# Overridable via $CG_CACHE_DIR (defaults to ./.cache next to this script) and
# $CG_DEV_IMAGE (defaults to code-graph-claude-dev:latest).

set -euo pipefail

CG_TOOL="claude"
CG_IMAGE="${CG_DEV_IMAGE:-code-graph-claude-dev:latest}"
CG_CONTAINERFILE_REL="tools/claude/Containerfile"
# The image builds the code-graph-mcp server from the workspace, so the build
# context is the repo root (not the Containerfile's dir). .containerignore keeps
# the context lean (no target/, .git, caches, or external/ corpora).
CG_BUILD_CONTEXT_REL="."
CG_CONTAINER_SUFFIX="dev"
# Default launch activates the baked-in code-graph plugin (hooks + skills) via
# --plugin-dir against the image-resident plugin path. This loads the plugin for
# the session without any install step or writes under the bind-mounted host
# ~/.claude. `./claude-container.sh shell` and `-- <cmd>` bypass this (they run
# bash / the given command), which is intended — the plugin is a Claude concern.
CG_DEFAULT_CMD=(claude --plugin-dir /opt/code-graph-plugin/plugin)
# Forward Claude's auth env vars so an API key / OAuth token on the host carries
# into the container.
CG_ENV_VARS=(ANTHROPIC_API_KEY CLAUDE_CODE_OAUTH_TOKEN)

# Mirror the host ~/.claude (directory: memory, projects, settings) and the
# top-level ~/.claude.json (sessions, auth, project index) so Claude Code state
# persists across runs. Touch the json file if it doesn't exist yet so podman
# bind-mounts a file, not a root-owned dir.
cg_tool_prepare_state_mounts() {
    local host_dir="${HOME}/.claude"
    local host_json="${HOME}/.claude.json"
    mkdir -p "${host_dir}"
    [[ -e "${host_json}" ]] || : > "${host_json}"
    STATE_MOUNTS=(
        -v "${host_dir}:${CONTAINER_HOME}/.claude"
        -v "${host_json}:${CONTAINER_HOME}/.claude.json"
    )
    TOOL_ENV_FLAGS=()
}

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/scripts/containers.sh"
cg_container_main "$@"
