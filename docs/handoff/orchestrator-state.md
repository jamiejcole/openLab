<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Orchestrator state (2026-10-28)

M1–M4 are complete: every engine-service issue is closed and ticked in `PROGRESS.md`. Work now is M5
(macOS app), then M6 (release v0.0.1). Open work lives in GitHub issues by milestone
(`gh issue list --milestone "M5 macOS app" --label task --state open`); each `PROGRESS.md` line names its
issue. Every task issue carries a Checklist of its requirement IDs, ticked by the implementer as parts land.

## Start of a session
1. `git fetch origin`; check open PRs and the last CI run on `main`.
2. Confirm `git config user.email` is the `openlab-dev` noreply address and `gh` is logged in as the
   account CLAUDE.md "Known environment facts" names (see "Flagged account" below).
3. The app's engine-side needs are issues now: #233 (protocol 2 switch), #110 (lifecycle contract),
   #236 (media duration), #237 (durations), #238 (undo guard), #239 (media paths, paste), #121 (hotkeys),
   #124 (Show Mode).

## CI (changed 2026-10-28)
Before: about 4.9 CI runs per PR, mostly from merging `main` into green PRs, and macOS queues of 10-20 min.
Now: push once, merge `main` into a PR only on a conflict, only `main` saves caches, and a PR runs the
macOS app job only when the app can be affected (see the `changes` job). Intel still runs on every merge
to `main` (QA-07).

## History rewrite (2026-10-28)
All commits except the first three were re-authored to `openlab-dev` and `main` was force-pushed.
Branches merged by then were deleted. Clones or worktrees made before that date are stale: re-clone.

## Flagged account (2026-10-28)
GitHub flagged the new `openlab-dev` account within an hour of use: its profile, PR #232 and issues
#216–#231 are hidden (404) from everyone else, and Actions do not run for it. The user appealed. Until it
is lifted, `gh` runs as `jamiejcole`; #233–#248 re-create #216–#231 (same order, number + 17). When
`https://api.github.com/users/openlab-dev` returns 200 again: close #216–#231 and #232 as duplicates, then
switch `gh` back (`gh auth switch`).

## Rules learned the hard way
- At most 3 agents at once, at most 2 on Opus (orchestrate skill).
- Pushing a branch whose workflow file is old runs that old workflow (old branches ran CI on every
  push). Delete stale branches rather than pushing them.
- One build per worktree; never share
  a `CARGO_TARGET_DIR` between two source trees. Delete a worktree's `engine/target` when its agent finishes.
- `PROGRESS.md`: tick boxes and add one-line tasks only.
- Every brief includes the adversarial checklist in `.claude/agents/implementer.md`.
- Two green PRs can still break `main` together; `main`'s run after each merge catches it, and a red
  `main` is fixed (through a PR) before new work.
- Never let an agent reuse another agent's branch name in a scratch worktree (it moves that ref).
- Claude Code may not edit its own permission settings; the user owns `.claude/settings.json`.
- Pushing from a scratch clone: `osxkeychain` fails without a terminal; use
  `git -c credential.helper= -c 'credential.helper=!gh auth git-credential' push ...`.

## Waiting on the user (answered in batches, never blocking)
- Decision notes in `decisions/` (most worth a look: `m4-11b.md`, `m4-6.md`, `m4-5c.md`, `m2-25b.md`).
- Rows marked `assumed` in `docs/todo/open-qlab-questions.md`.
- Issues labelled `low`.
