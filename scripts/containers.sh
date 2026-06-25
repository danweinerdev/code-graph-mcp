#!/usr/bin/env bash
# scripts/containers.sh — shared library for the per-tool dev-container
# launcher(s) (currently claude-container.sh).
#
# This file is *sourced*, not executed. Each entrypoint sets a small per-tool
# contract and then calls `cg_container_main "$@"`; everything else — host
# path resolution, the bind-mount set, image (re)build, and the final
# `podman run` — lives here.
#
# Per-tool contract the entrypoint MUST set before calling cg_container_main:
#   CG_TOOL               short name, e.g. "claude" (usage text only)
#   CG_IMAGE              image tag to build/run, e.g. "code-graph-dev:latest"
#   CG_CONTAINERFILE_REL  Containerfile path relative to the repo root
#   CG_CONTAINER_SUFFIX   per-worktree container/hostname suffix, e.g. "dev"
#   CG_DEFAULT_CMD        bash array: the command run with no subcommand args
#   CG_ENV_VARS           bash array: extra env var NAMES forwarded with -e
#   cg_tool_prepare_state_mounts()
#                         a function that creates any host-side state dirs and
#                         assigns the STATE_MOUNTS array (tool config/auth
#                         persisted across runs). It may use $CONTAINER_HOME.
#
# Per-tool contract the entrypoint MAY set:
#   CG_BUILD_CONTEXT_REL  build context dir relative to the repo root (default:
#                         the Containerfile's own directory). The Claude image
#                         builds the code-graph-mcp server from the workspace, so
#                         its entrypoint sets this to "." (the repo root).
#
# Cache layout (host -> container):
#   ${CG_CACHE_DIR}/cargo   -> /cache/cargo   (CARGO_HOME: registry, git, bins)
#   ${CG_CACHE_DIR}/target  -> /cache/target  (CARGO_TARGET_DIR: build outputs)
# CG_CACHE_DIR defaults to ./.cache next to the repo root.

# Guard against running this library directly.
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    echo "scripts/containers.sh is a library; run ./claude-container.sh instead." >&2
    exit 1
fi

cg_container_main() {
    # The repo root is this library's parent directory (scripts/ lives directly
    # under it), independent of which entrypoint sourced us or the caller's CWD.
    local REPO_DIR
    REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
    local IMAGE="${CG_IMAGE}"
    local CONTAINERFILE="${REPO_DIR}/${CG_CONTAINERFILE_REL}"
    local CACHE_DIR="${CG_CACHE_DIR:-${REPO_DIR}/.cache}"

    # Mirror the host username + home path inside the container so absolute
    # paths in things like ~/.ssh/config (IdentityFile /home/daniel/...) keep
    # working.
    local HOST_USER HOST_HOME CONTAINER_HOME
    HOST_USER="$(id -un)"
    HOST_HOME="${HOME}"
    CONTAINER_HOME="/home/${HOST_USER}"

    # Per-worktree container identity so concurrent containers (and the in-shell
    # prompt) are distinguishable across worktrees and tools:
    # cg-<tree>-<suffix>, where <tree> is this repo's directory name sanitised
    # to the charset hostnames/podman names accept — lowercased, every run of
    # non-alphanumerics collapsed to a single '-', and leading/trailing '-'
    # trimmed. Falls back to a bare "cg-<suffix>" if that leaves nothing. Only
    # labels the running container; the image tag is unchanged.
    local TREE_SLUG CONTAINER_BASE
    TREE_SLUG="$(basename "${REPO_DIR}")"
    TREE_SLUG="${TREE_SLUG,,}"
    TREE_SLUG="${TREE_SLUG//[^a-z0-9]/-}"
    while [[ "${TREE_SLUG}" == *--* ]]; do TREE_SLUG="${TREE_SLUG//--/-}"; done
    TREE_SLUG="${TREE_SLUG#-}"
    TREE_SLUG="${TREE_SLUG%-}"
    if [[ -n "${TREE_SLUG}" ]]; then
        CONTAINER_BASE="cg-${TREE_SLUG}-${CG_CONTAINER_SUFFIX}"
    else
        CONTAINER_BASE="cg-${CG_CONTAINER_SUFFIX}"
    fi

    # Persisted cargo state lives on the host so it survives image rebuilds and
    # is visible to host tools. Pre-create the leaf dirs so podman doesn't make
    # them root-owned.
    mkdir -p "${CACHE_DIR}/cargo" "${CACHE_DIR}/target"

    # Tool-specific persisted state (e.g. ~/.claude). The entrypoint creates the
    # host paths and fills STATE_MOUNTS; declared local here so the hook
    # (dynamically scoped) writes into our arrays.
    local STATE_MOUNTS=()
    local TOOL_ENV_FLAGS=()
    cg_tool_prepare_state_mounts

    # Mount the host ~/.gitconfig (read-only) when it exists so git inside the
    # container picks up the user's identity and aliases.
    local GITCONFIG_MOUNT=()
    if [[ -f "${HOME}/.gitconfig" ]]; then
        GITCONFIG_MOUNT+=(-v "${HOME}/.gitconfig:${CONTAINER_HOME}/.gitconfig:ro")
    fi

    # Forward the host SSH agent so git operations (push, clone via git@…)
    # inside the container can authenticate without copying keys in. Skipped if
    # the host isn't running an agent.
    local SSH_AGENT_MOUNT=()
    if [[ -n "${SSH_AUTH_SOCK:-}" && -S "${SSH_AUTH_SOCK}" ]]; then
        SSH_AGENT_MOUNT+=(
            -v "${SSH_AUTH_SOCK}:/run/host-ssh-agent.sock"
            -e "SSH_AUTH_SOCK=/run/host-ssh-agent.sock"
        )
    fi
    # Mount ~/.ssh/config and ~/.ssh/known_hosts read-only (not the whole
    # ~/.ssh) so Host aliases resolve and SSH can verify remote host keys for
    # git push/fetch, while private keys stay out of the container. Client auth
    # still goes through the forwarded agent above. Both are read-only so the
    # container can't mutate the host's SSH state (e.g. append a new host key to
    # known_hosts).
    local SSH_DIR_MOUNT=()
    if [[ -f "${HOME}/.ssh/config" ]]; then
        SSH_DIR_MOUNT+=(-v "${HOME}/.ssh/config:${CONTAINER_HOME}/.ssh/config:ro")
    fi
    if [[ -f "${HOME}/.ssh/known_hosts" ]]; then
        SSH_DIR_MOUNT+=(-v "${HOME}/.ssh/known_hosts:${CONTAINER_HOME}/.ssh/known_hosts:ro")
    fi

    # If this repo is checked out as a linked git worktree, its top-level `.git`
    # is a *file* pointing at a gitdir under a shared common dir, and that common
    # dir holds the objects/refs every git operation needs. Only ${REPO_DIR} is
    # mounted at /src, so for git to work inside the container we must also
    # expose the common dir — at the *same absolute host path*, since the `.git`
    # file's `gitdir:` line and the gitdir's `commondir` back-reference are
    # recorded with that path. For an ordinary clone the common dir is
    # ${REPO_DIR}/.git (already under /src), so no extra mount is added. Mounted
    # read-write because commit/fetch/branch all write objects + refs there.
    #
    # Caveat: the *other* worktrees recorded in the shared common dir are not
    # mounted here, so git inside the container sees them (and this one) as
    # "prunable". Don't run `git worktree prune` in the container — it would
    # delete those worktrees' admin data and break them on the host (recoverable
    # on the host with `git worktree repair`).
    local GIT_COMMON_MOUNT=() GIT_COMMON_DIR
    if GIT_COMMON_DIR="$(git -C "${REPO_DIR}" rev-parse --git-common-dir 2>/dev/null)"; then
        # rev-parse returns a relative ".git" for normal repos, an absolute path
        # for linked worktrees; normalise to an absolute path either way.
        case "${GIT_COMMON_DIR}" in
            /*) : ;;
            *)  GIT_COMMON_DIR="${REPO_DIR}/${GIT_COMMON_DIR}" ;;
        esac
        GIT_COMMON_DIR="$(cd "${GIT_COMMON_DIR}" && pwd)"
        # Only mount when the common dir lives outside the /src tree.
        if [[ "${GIT_COMMON_DIR}/" != "${REPO_DIR}/"* ]]; then
            echo ">>> Linked worktree detected; mounting git common dir ${GIT_COMMON_DIR}"
            GIT_COMMON_MOUNT+=(-v "${GIT_COMMON_DIR}:${GIT_COMMON_DIR}")
        fi
    fi

    local REBUILD=0
    if [[ "${1:-}" == "--rebuild" ]]; then
        REBUILD=1
        shift
    fi

    if [[ ${REBUILD} -eq 1 ]] || ! podman image exists "${IMAGE}"; then
        # Build context: the Containerfile's own directory by default, or the
        # repo-relative dir the entrypoint requested via CG_BUILD_CONTEXT_REL.
        # The Claude image builds the code-graph-mcp server from the workspace,
        # so it sets the context to the repo root; .containerignore keeps the
        # context lean.
        local BUILD_CONTEXT
        if [[ -n "${CG_BUILD_CONTEXT_REL:-}" ]]; then
            BUILD_CONTEXT="$(cd "${REPO_DIR}/${CG_BUILD_CONTEXT_REL}" && pwd)"
        else
            BUILD_CONTEXT="$(dirname "${CONTAINERFILE}")"
        fi
        echo ">>> Building ${IMAGE} from ${CONTAINERFILE} (context ${BUILD_CONTEXT})"
        # --squash-all collapses the final image into a single layer. The Rust
        # toolchains are several GB and later steps (the chmod -R over /opt/rust,
        # the COPY --from=builder of the cargo tools / MCP server) rewrite large
        # subtrees into fresh layers, so a normal layered build carries a lot of
        # inter-layer duplication. Squashing removes that. The expensive work
        # still caches at the *builder stage* across rebuilds (the multi-stage
        # split), so squashing only the final image is a good trade for a dev
        # sandbox.
        podman build \
            --squash-all \
            --build-arg "USER_UID=$(id -u)" \
            --build-arg "USER_GID=$(id -g)" \
            --build-arg "USERNAME=${HOST_USER}" \
            -t "${IMAGE}" \
            -f "${CONTAINERFILE}" \
            "${BUILD_CONTEXT}"
    fi

    # Default command: the tool's entrypoint command in the repo.
    # Subcommands: shell -> bash;  -- <cmd...> -> run that command.
    local CMD=("${CG_DEFAULT_CMD[@]}")
    if [[ $# -gt 0 ]]; then
        case "$1" in
            shell)
                CMD=(bash -l)
                ;;
            --)
                shift
                CMD=("$@")
                ;;
            *)
                CMD=("$@")
                ;;
        esac
    fi

    # TTY/interactive flags only when stdin is a terminal — keeps the script
    # usable from CI/scripts (e.g. `./claude-container.sh -- cargo build`).
    local TTY_FLAGS=()
    if [[ -t 0 && -t 1 ]]; then
        TTY_FLAGS+=(-it)
    fi

    # Forward the shared terminal vars plus the tool's auth/config env names.
    local ENV_FLAGS=(-e TERM -e COLORTERM)
    local _var
    for _var in "${CG_ENV_VARS[@]}"; do
        ENV_FLAGS+=(-e "${_var}")
    done

    # Raise the container's open-file limit. High-load Rust test runs (the
    # workspace links dozens of test binaries at once) can exhaust the default
    # soft limit and fail with "Too many open files". Request a large value, but
    # clamp to the host's hard limit — rootless podman cannot raise the
    # container's hard limit above the host user's, so an over-large request
    # would make `podman run` fail. This also bumps the *soft* limit up to that
    # ceiling, which is the usual fix.
    local FD_LIMIT=1048576
    local HOST_FD_HARD
    HOST_FD_HARD="$(ulimit -Hn 2>/dev/null || true)"
    if [[ "${HOST_FD_HARD}" =~ ^[0-9]+$ && "${HOST_FD_HARD}" -lt "${FD_LIMIT}" ]]; then
        FD_LIMIT="${HOST_FD_HARD}"
    fi

    # --security-opt seccomp=unconfined + --cap-add=SYS_PTRACE: needed for the
    # sanitizer suites. podman's default seccomp profile restricts ptrace, which
    # LeakSanitizer (on by default under the ASan runtime) uses for its
    # stop-the-world leak scan at process exit — it freezes sibling threads via
    # an in-process ptrace(PTRACE_ATTACH) and reads their stacks/registers. That
    # self-attach has two gates: the seccomp profile must allow `ptrace`
    # (covered by seccomp=unconfined) AND the host's Yama policy must permit it.
    # The Yama knob /proc/sys/kernel/yama/ptrace_scope is a host kernel sysctl,
    # NOT namespaced, so the container inherits the host value; on a hardened
    # host (scope 1/2) the self-attach is denied and LSan dies with "does not
    # work under ptrace". Holding CAP_SYS_PTRACE lifts the Yama restriction for
    # scopes 1/2. label=disable keeps SELinux from blocking the bind mounts.
    # All three relaxations are acceptable here: this is a single-user dev
    # sandbox already running the caller's own code. (Rootless caveat:
    # --cap-add only grants what the host user's bounding set allows; if that
    # leaves CapEff at 0, set kernel.yama.ptrace_scope=0 on the host instead.)
    exec podman run --rm \
        "${TTY_FLAGS[@]}" \
        --userns=keep-id \
        --ulimit "nofile=${FD_LIMIT}:${FD_LIMIT}" \
        --cap-add=SYS_PTRACE \
        --security-opt label=disable \
        --security-opt seccomp=unconfined \
        --hostname "${CONTAINER_BASE}" \
        --name "${CONTAINER_BASE}-$$" \
        -v "${REPO_DIR}:/src" \
        "${GIT_COMMON_MOUNT[@]}" \
        "${STATE_MOUNTS[@]}" \
        "${GITCONFIG_MOUNT[@]}" \
        "${SSH_AGENT_MOUNT[@]}" \
        "${SSH_DIR_MOUNT[@]}" \
        -v "${CACHE_DIR}/cargo:/cache/cargo" \
        -v "${CACHE_DIR}/target:/cache/target" \
        -w /src \
        "${ENV_FLAGS[@]}" \
        "${TOOL_ENV_FLAGS[@]}" \
        "${IMAGE}" \
        "${CMD[@]}"
}
