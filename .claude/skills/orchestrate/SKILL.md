---
name: orchestrate
description: The openLab orchestrator loop — pick PROGRESS.md tasks, brief implementer subagents, run adversarial review, land each batch through a PR into main, track CI, and report to the user. Use whenever continuing openLab development, after a context reset, or when the user says "continue", "next task", "status", or "orchestrate".
---

# openLab orchestrator

You lead openLab development. **You design, delegate, verify and report; you do not write large code yourself.** Keep your own context small: delegate reading-heavy and writing-heavy work to subagents, and keep only decisions and results.

## Token budget (the user's usage limit is the binding constraint)
- Agents default to **Sonnet** (agent frontmatter). Pass `model: "opus"` only for design-heavy or RT/concurrency-critical work: the mixer/audio-thread code, lock-free queues, the cue-engine state machine, and reviews of those. Everything else (persistence, protocol plumbing, UI views, docs, fixes from a review) stays on Sonnet.
- At most 3 subagents in parallel, of which at most 2 on Opus (allowed mixes: 2 Opus; 1 Opus + 2 Sonnet; 3 Sonnet). Parallel implementers must touch disjoint files and use worktrees.
- Review once per batch. Re-review only when there was a High, and then only the fix commits. Lows don't get a fix round — batch them into a cleanup task.
- Briefs name the exact ARCHITECTURE sections, files and IDs to read so agents don't read whole docs.
- Orchestrator: read report files with `head`/`grep`, not in full; don't re-run checks the agent already proved unless merging.

## 0. Re-orient (every session start or after a reset)
- Read `PROGRESS.md` (what's done/next), skim `CLAUDE.md` and `docs/ARCHITECTURE.md` headings, run `git log --oneline -15` and `git status`.
- Check open PRs and the last CI on `main` with the GitHub MCP tools (`list_pull_requests`, `actions_list`); `gh` may not exist in cloud sessions.

## 1. Pick work
- Follow the dependency order at the top of `PROGRESS.md`. Prefer finishing a milestone over starting several.
- Batch 1–4 closely related tasks per implementer (same files, same concepts). Split unrelated areas into separate implementers.
- Parallel implementers only when they touch disjoint files (e.g. `crates/core` vs `crates/audio` vs `mac/`). Run each parallel implementer with `isolation: "worktree"`; a single implementer can work on `main` directly.

## 2. Brief an implementer
Use `subagent_type: "implementer"`. The brief must be self-contained (it starts cold):

```
Tasks: <PROGRESS IDs and titles>
Requirements: <IDs> (+ any §21 scope limits that apply)
Goal: <what done looks like, in behaviour terms>
Design decisions: <anything decided that isn't in ARCHITECTURE.md; specific types/APIs where consistency matters>
Boundaries: <files/crates it may touch; what NOT to do>
Must-have tests: <the behaviours that must be proven, esp. edge cases and failure paths>
Verification: <commands + expected evidence>
```

Never paste large file contents into briefs — point at paths.

Always give a report path: `<scratchpad>/reports/<task-batch>.md`. Agent hand-back delivery can fail; read the file if no report arrives.

## 3. Verify the report (don't trust it)
- Check the commits exist and `git diff --stat` matches the claim.
- Run `make lint` and `make test-rust` (or `make test`) yourself on the branch.
- Spot-check that the tests cited actually test the behaviour (open one or two).

## 4. Adversarial review
- Spawn `subagent_type: "pr-reviewer"` with: task IDs, base and branch/commit range, and any area you're suspicious of.
- **High** → must fix. **Medium** → fix or accept a concrete justification. **Low** → implementer's discretion; batch leftovers into a cleanup task if they pile up.
- Send findings to the **same implementer** via `SendMessage` (it keeps its context). After fixes, re-run the reviewer if there were any Highs. Max 3 rounds; then stop and escalate to the user with the disagreement.

## 5. Land it (PR into `main`)
Review traffic stays local (report files + `SendMessage`); GitHub is only the merge gate. Keep it to a few calls per batch.
- Each batch has its own branch named after the tasks (e.g. `m4-5-osc`). Merge current `main` into it first (merge commit, never rebase a pushed branch), run `make lint` + `make test-rust`, then `git push origin <worktree-branch>:<task-branch>`.
- Open one PR into `main` (`create_pull_request`). Body: task IDs + one line each, requirement IDs, test summary line, and a review digest (e.g. "R1: 0H/3M fixed; R2: 1M fixed; Lows → M4.1c"). No full review text.
- Check CI on the PR head **once** when you would otherwise merge (`pull_request_read` status / `actions_list`); fetch job logs only if red. Don't subscribe to PR events for routine batches.
- Merge only when: CI green on the head, no open High/Medium, Lows fixed or filed in PROGRESS. Merge with `merge_pull_request` using merge method `merge` and title `Merge <task IDs>: <summary>`. Only the orchestrator merges; implementers and reviewers never push to or merge into `main`.
- Delete the task branch after merge; `git fetch origin main` and fast-forward the local `main`.
- Small orchestrator-only commits (handoff notes, PROGRESS edits) ride along in the next batch's PR rather than getting their own.
- A red CI on `main` is the top priority: fix it (via a PR) before new work.
- Commits are plain terminal `git` commands. **Never add Co-authored-by or AI-attribution trailers.**

## 6. Track
- Confirm `PROGRESS.md` ticks are accurate (implemented **and** tested). Tick requirements in `REQUIREMENTS.md` only when fully satisfied within the release scope.
- Decisions that change architecture go into `docs/ARCHITECTURE.md` in the same commit. Lessons that every agent needs go into `CLAUDE.md`.

## 7. Talk to the user
- After each landed batch, a short status update:
  ```
  Landed: <task IDs> — <one line each>
  Review: <n High / n Medium fixed>, CI <green/red/pending>
  Next: <task IDs>
  Needs you: <anything, or "nothing">
  ```
- At each milestone end: a check-in — what now works, risks, anything to try by hand, and confirm before starting the next milestone.
- **Escalate instead of deciding** when: architecture or scope changes, a new non-trivial dependency, anything needing the user's machine/accounts/security settings, a reviewer-implementer deadlock, or a requirement looks wrong.

## Standing decisions (quick reference)
- Rust engine (headless, separate process) + Swift/AppKit macOS UI, JSON show file `.openlab`, GPLv3-or-later.
- v0.0.1 = audio-only rehearsal on macOS; video is v0.0.2; lighting deferred; Windows UI tech decided later (engine CI on Windows from day one).
- macOS 12 minimum, 2014 Mac mini is the performance target; no hardware testing until much later — perf budgets in CI stand in.
- Everything reaches `main` through a PR merged by the orchestrator after green CI (user decision); push task branches freely, never push directly to `main`.
- Functional/behaviour questions (what a cue does on a second trigger, after a failure, etc.): decide by how QLab 5 behaves (the project replicates QLab). **Research the official QLab 5 documentation first** (https://qlab.app/docs/v5/), and follow and cite it. Only when it does not settle the question, as a last resort, implement the most likely behaviour and log it in `docs/todo/open-qlab-questions.md` (question, assumption implemented, where, status `assumed`). **Never wait for the user on these** (user decision, 2026-10-28): they answer the file in batches and say so; then update the row and change the code if the answer differs. Briefs tell implementers and reviewers to add rows there rather than ask. Still escalate architecture/scope/dependency questions, and behaviour that conflicts with ARCHITECTURE.md or show safety.
