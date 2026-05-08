#!/bin/sh
# Snapshot-audit: assert that the snapshot delta vs HEAD touches ONLY
# the expected files. Catches accidental cross-tool effects when a
# wire-format change "improvement" silently regenerates an unrelated
# tool's snapshot.
#
# Pattern surfaced ~7 times across PaginationOverhaul and CppMacroStrip;
# every wire-format-touching phase ran the equivalent check by hand. This
# script automates it.
#
# Usage:
#   scripts/snapshot-audit.sh <expected-fragment> [<expected-fragment>...]
#
# Each <expected-fragment> is a substring matched against the modified-
# snapshot file paths. A snapshot file appears in the diff legitimately
# if it contains ANY of the listed fragments; otherwise it's flagged.
#
# Examples:
#   scripts/snapshot-audit.sh response_get_orphans tools_list_get_orphans
#     # Only get_orphans-related snapshots may have changed.
#
#   scripts/snapshot-audit.sh response_get_class_hierarchy_ue tools_list_get_class_hierarchy
#     # Only the new UE class-hierarchy snapshots + the class_hierarchy
#     # tools-list entry may have changed.
#
# Exits 0 if every changed/new snapshot matches at least one fragment,
# or if no snapshots changed.
# Exits 1 if any changed/new snapshot doesn't match any fragment.
# Exits 2 on usage error.

set -e

if [ $# -lt 1 ]; then
    cat >&2 <<EOF
Usage: $0 <expected-fragment> [<expected-fragment>...]

Each fragment is a substring matched against snapshot file paths.
A snapshot whose path doesn't contain ANY listed fragment fails the audit.
EOF
    exit 2
fi

cd "$(git rev-parse --show-toplevel)"

# Find every snapshot file that's modified or untracked under tests/snapshots/
# anywhere in the workspace. `git status --porcelain` covers both staged and
# unstaged + untracked, with --porcelain stable for parsing.
changed=$(git status --porcelain -- '**/tests/snapshots/*.snap' 2>/dev/null \
    | awk '{ print $NF }' \
    | grep -F 'tests/snapshots/' || true)

if [ -z "$changed" ]; then
    echo "✓ No snapshot files changed."
    exit 0
fi

unexpected=""
for snap in $changed; do
    matched=0
    for frag in "$@"; do
        case "$snap" in
            *"$frag"*) matched=1; break ;;
        esac
    done
    if [ "$matched" -eq 0 ]; then
        unexpected="$unexpected\n    $snap"
    fi
done

if [ -n "$unexpected" ]; then
    echo "✗ Unexpected snapshot churn — these files don't match any expected fragment:"
    printf "%b\n" "$unexpected"
    echo ""
    echo "Expected fragments:"
    for frag in "$@"; do
        echo "    $frag"
    done
    echo ""
    echo "If the change is intentional, add a matching fragment. If accidental,"
    echo "investigate before committing — a snapshot regeneration outside the"
    echo "expected set usually signals an unintended cross-tool effect."
    exit 1
fi

count=$(echo "$changed" | wc -l | tr -d ' ')
echo "✓ All $count changed snapshot(s) match the expected fragments."
