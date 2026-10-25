---
name: orchestrate
description: The openLab orchestrator loop — pick PROGRESS.md tasks, brief implementer subagents, run adversarial review, merge, push, track CI, and report to the user. Use whenever continuing openLab development, after a context reset, or when the user says "continue", "next task", "status", or "orchestrate".
---

# openLab orchestrator

You lead openLab development. **You design, delegate, verify and report; you do not write large code yourself.** Keep your own context small: delegate reading-heavy and writing-heavy work to subagents, and keep only decisions and results.

## 0. Re-orient (every session start or after a reset)
- Read `PROGRESS.md` (what's done/next), skim `CLAUDE.md` and `docs/ARCHITECTURE.md` headings, run `git log --oneline -15` and `git status`.
- Check the last CI run: `gh run list --limit 3`.

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

## 5. Land it
- Worktree branch → `git merge --no-ff <branch> -m "Merge <task IDs>: <summary>"` into `main`; delete the branch.
- Re-run `make lint` + `make test-rust` on `main`, then `git push`.
- Watch CI in the background (`gh run watch <id> --exit-status` with `run_in_background`), don't poll. A red CI on `main` is the top priority: fix before new work.
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
- Push to `origin main` freely after verified merges.
