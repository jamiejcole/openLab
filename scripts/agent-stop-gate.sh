#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# SubagentStop hook: an agent that has committed on a task branch may not stop until
# `scripts/prepush.sh --check` passes for its HEAD (clean tree, stamp for HEAD).
# Read-only agents (no commits ahead of main) and the main checkout pass through.
input=$(cat)
field() { printf '%s' "$input" | python3 -I -c "import json,sys; print(json.load(sys.stdin).get('$1') or '')"; }
# Already blocked once: let it stop so a broken gate can't loop; the orchestrator re-checks.
[ "$(field stop_hook_active)" = "True" ] && exit 0
cwd=$(field cwd)
[ -n "$cwd" ] && cd "$cwd" 2>/dev/null || exit 0
branch=$(git branch --show-current 2>/dev/null)
case "$branch" in ''|main) exit 0 ;; esac
ahead=$(git rev-list --count main..HEAD 2>/dev/null || echo 0)
[ "$ahead" -gt 0 ] || exit 0
if ! scripts/prepush.sh --check >/dev/null 2>&1; then
  echo "Branch $branch has commits but no PREPUSH PASS stamp for HEAD with a clean tree. Run make prepush (commit first) and report its last line." >&2
  exit 2
fi
exit 0
