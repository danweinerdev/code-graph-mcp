#!/usr/bin/env bash
#
# Fan the canonical Claude plugin content out to the other harness trees.
#
#   canonical            mirrors
#   ---------            -------
#   plugin/skills/       .codex-plugin/skills/    opencode-plugin/skills/
#   plugin/commands/     .codex-plugin/commands/  opencode-plugin/commands/
#   plugin/scripts/      .codex-plugin/hooks/     (hook scripts only)
#
# plugin/ is the source of truth because the dev container and the marketplace
# manifest already load it directly (--plugin-dir). Everything else is generated.
# Edit plugin/, then run this. Never edit a mirror by hand -- the next run
# overwrites it.
#
# The skill and command lists are discovered by scanning plugin/ at run time, not
# hardcoded below. A hardcoded list is a second place to update, and the failure
# is silent: the new skill lands in plugin/ only, and the harnesses that never
# received it go on advertising a tool surface they cannot drive.
#
# Usage:
#   scripts/sync-plugin-skills.sh            regenerate the mirrors
#   scripts/sync-plugin-skills.sh --check    fail (non-zero) if a mirror is stale,
#                                            without writing anything

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

CANON="plugin"

CHECK_ONLY=0
case "${1:-}" in
--check)
	CHECK_ONLY=1
	;;
"") ;;
*)
	printf 'sync-plugin-skills: unknown argument: %s\n' "$1" >&2
	exit 2
	;;
esac

SKILLS=()
while IFS= read -r dir; do
	SKILLS+=("$(basename "$dir")")
done < <(find "$CANON/skills" -mindepth 1 -maxdepth 1 -type d | sort)

COMMANDS=()
while IFS= read -r file; do
	COMMANDS+=("$(basename "$file" .md)")
done < <(find "$CANON/commands" -mindepth 1 -maxdepth 1 -name '*.md' | sort)

[[ ${#SKILLS[@]} -gt 0 && ${#COMMANDS[@]} -gt 0 ]] || {
	printf 'sync-plugin-skills: no canonical skills or commands found under %s/ -- wrong cwd?\n' "$CANON" >&2
	exit 1
}

for skill in "${SKILLS[@]}"; do
	[[ -f "$CANON/skills/$skill/SKILL.md" ]] || {
		printf 'sync-plugin-skills: missing canonical skill: %s/skills/%s/SKILL.md\n' "$CANON" "$skill" >&2
		exit 1
	}
done

# Hook scripts shared with harnesses that consume the SDK-standard SessionStart
# contract. The Claude-specific .sh hooks (nudge + session-reset) are NOT mirrored:
# they implement a PreToolUse Grep/Glob interception that only Claude Code exposes.
HOOK_SCRIPTS=(
	"session-start"
	"run-hook.cmd"
)

TREES=(
	".codex-plugin"
	"opencode-plugin"
)
HOOK_TREES=(
	".codex-plugin"
)

for script in "${HOOK_SCRIPTS[@]}"; do
	[[ -f "$CANON/scripts/$script" ]] || {
		printf 'sync-plugin-skills: missing canonical hook script: %s/scripts/%s\n' "$CANON" "$script" >&2
		exit 1
	}
done

tree_wants_hooks() {
	local candidate="$1" tree
	for tree in "${HOOK_TREES[@]}"; do
		[[ "$tree" == "$candidate" ]] && return 0
	done
	return 1
}

# Write the full generated content for one tree into $dest. Used both to update a
# mirror in place and, under --check, to build a throwaway copy to diff against.
materialize() {
	local tree="$1" dest="$2" skill cmd script

	rm -rf "$dest/skills" "$dest/commands"
	mkdir -p "$dest/skills" "$dest/commands"
	for skill in "${SKILLS[@]}"; do
		mkdir -p "$dest/skills/$skill"
		cp "$CANON/skills/$skill/SKILL.md" "$dest/skills/$skill/SKILL.md"
	done
	for cmd in "${COMMANDS[@]}"; do
		cp "$CANON/commands/$cmd.md" "$dest/commands/$cmd.md"
	done

	if tree_wants_hooks "$tree"; then
		rm -rf "$dest/hooks-generated"
		mkdir -p "$dest/hooks-generated"
		for script in "${HOOK_SCRIPTS[@]}"; do
			cp -p "$CANON/scripts/$script" "$dest/hooks-generated/$script"
		done
	fi
}

# Move the generated hook scripts into place. Kept separate from materialize()
# because hooks/ also holds a hand-maintained hooks.json that is NOT generated --
# clobbering the whole directory would delete it.
install_hooks() {
	local dest="$1" script
	[[ -d "$dest/hooks-generated" ]] || return 0
	mkdir -p "$dest/hooks"
	for script in "${HOOK_SCRIPTS[@]}"; do
		cp -p "$dest/hooks-generated/$script" "$dest/hooks/$script"
	done
	rm -rf "$dest/hooks-generated"
}

STALE=0

for tree in "${TREES[@]}"; do
	if [[ $CHECK_ONLY -eq 1 ]]; then
		staging="$(mktemp -d)"
		materialize "$tree" "$staging"
		install_hooks "$staging"
		for sub in skills commands $(tree_wants_hooks "$tree" && echo hooks); do
			# hooks/ carries a non-generated hooks.json; compare only the files we own.
			if [[ "$sub" == "hooks" ]]; then
				for script in "${HOOK_SCRIPTS[@]}"; do
					if ! diff -q "$staging/hooks/$script" "$tree/hooks/$script" >/dev/null 2>&1; then
						printf 'sync-plugin-skills: STALE %s/hooks/%s\n' "$tree" "$script" >&2
						STALE=1
					fi
				done
				continue
			fi
			if ! diff -r -q "$staging/$sub" "$tree/$sub" >/dev/null 2>&1; then
				printf 'sync-plugin-skills: STALE %s/%s\n' "$tree" "$sub" >&2
				diff -r -q "$staging/$sub" "$tree/$sub" 2>&1 | sed 's/^/    /' >&2 || true
				STALE=1
			fi
		done
		rm -rf "$staging"
	else
		materialize "$tree" "$tree"
		install_hooks "$tree"
		printf 'sync-plugin-skills: %s <- %d skills + %d commands' "$tree" "${#SKILLS[@]}" "${#COMMANDS[@]}"
		if tree_wants_hooks "$tree"; then
			printf ' + %d hook scripts' "${#HOOK_SCRIPTS[@]}"
		fi
		printf '\n'
	fi
done

if [[ $CHECK_ONLY -eq 1 ]]; then
	if [[ $STALE -ne 0 ]]; then
		printf '\n✗ plugin mirrors are stale. Run: scripts/sync-plugin-skills.sh\n' >&2
		exit 1
	fi
	printf '✓ plugin mirrors are in sync with %s/\n' "$CANON"
fi
