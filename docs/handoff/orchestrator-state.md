<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Orchestrator state (2026-10-28)

M1–M4 are complete: every engine-service issue is closed and ticked in `PROGRESS.md`. Work now is M5
(macOS app), then M6 (release v0.0.1). Open work lives in GitHub issues by milestone
(`gh issue list --milestone "M5 macOS app" --label task`); the ID-to-issue table is `issue-map.md`.

## Start of a session
1. `git fetch origin`; check open PRs and the last CI run on `main`.
2. Confirm `gh auth status` shows `openlab-dev` and `git config user.email` is its noreply address
   (CLAUDE.md, "Known environment facts"). Agent work never runs under the owner's account.
3. Read `m5-protocol-requests.md`: what the app still needs from the engine, and what the engine
   already does that the app has not switched to yet.

## History rewrite (2026-10-28)
All commits except the first three were re-authored to `openlab-dev` and `main` was force-pushed.
Branches merged by then were deleted. Clones or worktrees made before that date are stale: re-clone.

## Rules learned the hard way
- At most 3 agents at once, at most 2 on Opus (orchestrate skill). One build per worktree; never share
  a `CARGO_TARGET_DIR` between two source trees. Delete a worktree's `engine/target` when its agent finishes.
- `PROGRESS.md`: tick boxes and add one-line tasks only.
- Every brief includes the adversarial checklist in `.claude/agents/implementer.md`.
- Two green PRs can still break `main` together. After each merge, re-check the other open PRs.
- Never let an agent reuse another agent's branch name in a scratch worktree (it moves that ref).
- Claude Code may not edit its own permission settings; the user owns `.claude/settings.json`.
- Pushing from a scratch clone: `osxkeychain` fails without a terminal; use
  `git -c credential.helper= -c 'credential.helper=!gh auth git-credential' push ...`.

## Waiting on the user (answered in batches, never blocking)
- Decision notes in `decisions/` (most worth a look: `m4-11b.md`, `m4-6.md`, `m4-5c.md`, `m2-25b.md`).
- Rows marked `assumed` in `docs/todo/open-qlab-questions.md`.
- Issues labelled `low`.
