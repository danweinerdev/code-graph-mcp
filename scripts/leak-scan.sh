#!/bin/sh
# Leak-scan: enforce the standing "no plan/task/phase pointers in source"
# rule. A plan-artifact pointer is any comment / doc-comment / string
# that tells a future reader *which task, phase, plan decision, or design
# doc added this code* — these ROT (no plan context survives in the
# repo) and must be rewritten to describe the BEHAVIOR/mechanism instead.
#
# Why this script exists: the one-time remediation sweep (ResponseShapePolish
# Task 6.7) had to widen its discovery grep THREE times — `Task N.N|Phase N`
# missed `in 7.x` / `wired in` / `live in`, then `plan Decision` / `design
# brief` / `per the task brief`. Each miss cost a corrective cycle. The
# lesson: the *discovery* pattern must be maximally inclusive from the
# first pass; the bucket-1-vs-bucket-2 *judgment* stays a human/agent step.
# This script bakes the broad pattern in once so the narrow-grep churn
# cannot recur, and serves as the mechanical enforcement of the standing
# prevention rule for all NEW code.
#
# Bucket (1) — plan-artifact leak: rot, MUST be rewritten. Flagged here.
# Bucket (2) — canonical-origin preamble: a `//!` module header recording
#   *when/how a whole subsystem first came to exist under the original
#   RustRewrite plan* (the test-corpus / watch-mode / integration origin
#   headers and the two server/handlers scaffold preambles). These are
#   legitimate historical documentation, NOT rot — allowlisted below by
#   CONTENT signature (durable across line-number drift), not file:line.
#
# Usage:
#   scripts/leak-scan.sh [<pathspec>...]
#     # default pathspecs: crates/*/src/* crates/*/tests/*
#
# Exit status:
#   0  — only allowlisted bucket-2 origin preambles survive (clean)
#   1  — one or more bucket-1 leaks found (printed, grouped by file)
#   2  — usage / environment error
#
# Run standalone, via `make leak-scan`, or wire into CI / a pre-commit
# hook to keep the rule enforced.

set -eu

cd "$(git rev-parse --show-toplevel 2>/dev/null)" || {
	echo "✗ leak-scan: not inside a git work tree" >&2
	exit 2
}

# Default scan surface: all first-party Rust source and test files.
if [ "$#" -eq 0 ]; then
	set -- 'crates/*/src/*' 'crates/*/tests/*'
fi

# --- Broad detection pattern (union of every form the sweeps converged on)
# Case-insensitive `phase` (lowercase `phase 7.4` slipped a case-sensitive
# pass). Prose forms (`wired in 7.3`, `live in 3.2`, `documented in 7.2`)
# are the ones that historically evaded narrower patterns.
DETECT='(Task [0-9]+\.[0-9]+|[Pp]hase [0-9]+|plan (Decision|task)|per the task brief|the task brief|per the (plan|spec|design)|plan doc|design (doc|brief)|spec [0-9]+\.|Plans/Active|ResponseShapePolish|\.plans/|\b(wired|live|lands?|documented|covered|defined) in [0-9]+\.[0-9])'

# --- Bucket-2 allowlist: canonical-origin preambles, matched by content.
# Each alternative is a stable signature of a legitimately-preserved
# origin header. Keep this list in sync when a NEW subsystem ships with a
# deliberate `//!` origin preamble (rare — most code must be behavioral).
BUCKET2='(//! Phase [0-9.]+ (corpus|watch-mode reindex) regression test|//! This is the comprehensive Phase [0-9.]+ corpus|Concurrent reader/writer integration test .*\(Phase [0-9.]+\)|The Phase [0-9]+ design keeps|the lock at the .ServerInner. level in Phase [0-9]+|before Phase [0-9]+ builds on top of it|Phase 3\.1 shipped the scaffold|short\. Phase 3\.4 filled in|P0 handlers; Phase 3\.5 filled in P1\+P2 plus watch stubs; Phase 4\.1)'

# git grep over the requested pathspecs; drop allowlisted bucket-2 lines.
# `git grep` exit 1 == "no matches" == clean, which is the success case.
hits=$(git grep -nE "$DETECT" -- "$@" 2>/dev/null | grep -vE "$BUCKET2" || true)

if [ -z "$hits" ]; then
	echo "✓ leak-scan: no plan-artifact pointers in source (only allowlisted origin preambles survive)."
	exit 0
fi

count=$(printf '%s\n' "$hits" | grep -c . || true)
echo "✗ leak-scan: $count bucket-1 plan-artifact leak(s) — rewrite each to describe BEHAVIOR, not which task/phase/plan added it:"
echo ""
printf '%s\n' "$hits" | sed 's/^/    /'
echo ""
echo "  A surviving hit is legitimate ONLY if it is a canonical-origin"
echo "  preamble; if so, add its content signature to BUCKET2 in"
echo "  scripts/leak-scan.sh (by content, never file:line). Otherwise it"
echo "  is rot — describe the mechanism and drop the pointer."
exit 1
