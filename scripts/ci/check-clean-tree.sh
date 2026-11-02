#!/usr/bin/env bash
# Informational: report whether tracked files are modified (this is what makes
# the beacon build report `-dirty`; untracked files do not count).
#
# Usage: bash scripts/ci/check-clean-tree.sh "<when>"
#
# Emits a GitHub ::warning:: with `git diff --stat` when dirty, but never fails.
# TODO: make this fatal (exit 1) once a master run is clean.

set -uo pipefail

WHEN="${1:-now}"
STATUS="$(git status --porcelain --untracked-files=no)"

echo "git status --porcelain --untracked-files=no ($WHEN):"
echo "$STATUS"
echo ""
echo "git diff --stat ($WHEN):"
git diff --stat

if [ -n "$STATUS" ]; then
    # %0A is a newline in workflow commands
    STAT="$(git diff --stat | sed ':a;N;$!ba;s/\n/%0A/g')"
    echo "::warning::Working tree is dirty ${WHEN}; beacon would report -dirty.%0A${STAT}"
fi
exit 0
