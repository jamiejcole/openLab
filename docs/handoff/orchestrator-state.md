<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Orchestrator state (2026-10-30)

M1–M4 are complete. Work now is M5 (macOS app), then M6 (release v0.0.1). GitHub issues and milestones are the only
tracker (`gh issue list --milestone "M5 macOS app" --state open`); every task issue carries a Checklist of its
requirement IDs, ticked by the implementer as parts land.

## 2026-10-30 session
- Two external reviews (code, process) found Highs the loop missed. Response: rules become CI checks (#400), the
  instruction files were made consistent and CLAUDE.md shortened (#399), PROGRESS.md deleted (the archive stays: issues link it), reviewers run on Opus
  and re-run what they cite, `main` requires up-to-date branches. Code Highs: #401, #402, #403.
- User decisions: no human reads code or PRs; everything matches QLab 5, extras included (output delay goes, #404).

## 2026-10-29 session
- Closed the post-review groundwork: about 55 issues fixed or closed (PRs #375, #380, #382, #390 and the QLab-parity PR). Remaining Lows were filed during reviews.
- Every QLab answer is in `docs/todo/open-qlab-questions.md`. Search it before asking the user anything.
- A local map of QLab 5's UI is in `~/qlab-ref/ui` (see CLAUDE.md). It is never copied into the repo or issues.
- Scope now matches QLab 5 (CLAUDE.md, REQUIREMENTS edits). The open M5 issues carry a layout section and the new scope. #391-#395 list gaps in the UI already built.
- Next: start M5 feature work.

## Start of a session
1. `git fetch origin`; check open PRs and the last CI run on `main`.
2. Confirm `git config user.email` is the repo-local agent identity (`docs/dev-setup.md`).
3. The app's engine-side needs are issues now: #233 (protocol 2 switch), #110 (lifecycle contract),
   #236 (media duration), #237 (durations), #238 (undo guard), #239 (media paths, paste), #121 (hotkeys),
   #124 (Show Mode).

## CI (changed 2026-10-28)
Before: about 4.9 CI runs per PR, mostly from merging `main` into green PRs, and macOS queues of 10-20 min.
Now: push once, merge `main` into a PR only on a conflict, only `main` saves caches, and a PR runs the
macOS app job only when the app can be affected (see the `changes` job). Intel still runs on every merge
to `main` (QA-07).

## Policy health (updated at each milestone end; SKILL.md "Milestone end")
- 2026-10-28 baseline: `low` issues 94 open / 6 closed; Mediums fixed in the last 5 reviews: 0; claims the repo did not back: 5.

## Rules learned the hard way
- Pushing a branch whose workflow file is old runs that old workflow (old branches ran CI on every
  push). Delete stale branches rather than pushing them.
- Delete a worktree (and its `engine/target`) once its branch is pushed: agent worktrees filled the disk on 2026-10-28.
- Every brief includes the adversarial checklist in `.claude/agents/implementer.md`.
- Two green PRs can still break `main` together; `main`'s run after each merge catches it, and a red
  `main` is fixed (through a PR) before new work.
- Never let an agent reuse another agent's branch name in a scratch worktree (it moves that ref).
- Claude Code may not edit its own permission settings; the user owns `.claude/settings.json`.

## Waiting on the user (answered in batches, never blocking)
- Decision notes in `decisions/` (most worth a look: `m4-11b.md`, `m4-6.md`, `m4-5c.md`, `m2-25b.md`).
- Rows marked `assumed` in `docs/todo/open-qlab-questions.md`.
