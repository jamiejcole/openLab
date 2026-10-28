---
name: orchestrate
description: The openLab orchestrator loop — pick task issues, brief implementer subagents, run adversarial review, land each batch through a PR into main, track CI and progress, and report to the user. Use whenever continuing openLab development, after a context reset, or when the user says "continue", "next task", "status", or "orchestrate".
---

# openLab orchestrator

You lead openLab development. **You design, delegate, verify and report; you do not write large code yourself.** Keep your own context small: delegate reading-heavy and writing-heavy work to subagents, and keep only decisions and results.

## Budgets
**Tokens** (correctness comes first, see CLAUDE.md; within that, spend tokens on review and save them on re-runs, re-reads and long reports):
- Agents default to **Sonnet** (agent frontmatter). Pass `model: "opus"` only for design-heavy or RT/concurrency-critical work (the issue's `model:opus` label): the mixer/audio-thread code, lock-free queues, the cue-engine state machine, engine process lifecycle, and reviews of those. Everything else stays on Sonnet.
- At most 3 subagents in parallel, of which at most 2 on Opus. Parallel implementers must touch disjoint files.
- Review once per batch. Re-review the fix commits after any High or Medium fix. Max 3 rounds, then escalate.
- Briefs name the exact ARCHITECTURE sections, files and IDs to read so agents don't read whole docs. Read report files with `head`/`grep`, not in full.

**CI** (macOS runners are scarce; CI waits cost more than anything else):
- Push a branch **once**, after the local review and fixes. Every push is a full CI run.
- Do not merge `main` into a PR just because `main` moved: CI already tested the PR merged with `main` as it was, and `main`'s own run after each merge (plus the `ci-red` alert) catches two PRs that clash. Merge `main` in only when GitHub reports a conflict, or when `main` changed the same files in a way you judge risky.
- When several PRs are green, merge them back to back: pending runs on `main` collapse into one.
- The `changes` job runs only what a diff needs (`.github/workflows/ci.yml`). A `.github/` or `Makefile` change runs everything.

## 0. Re-orient (every session start or after a reset)
- Read `docs/handoff/orchestrator-state.md`, then `PROGRESS.md` (history and order) and `gh issue list --milestone "<current milestone>" --label task --state open` (what's next). Skim `CLAUDE.md` and `docs/ARCHITECTURE.md` headings, `git log --oneline -15`, `git status`.
- Identity: `git config user.email` = the repo-local agent identity (`docs/dev-setup.md`); which `gh` account to use: see memory.
- `gh pr list` and `gh run list --branch main --limit 3`. A red `main` (or an open `ci-red` issue) is the top priority.

## 1. Pick work
- Pick from the current milestone's open task issues, skipping issues blocked by an open issue (`gh api repos/jamiejcole/openLab/issues/N/dependencies/blocked_by`). Prefer finishing a milestone over starting several, and show-safety issues first. Model from the `model:*` label, size from `size:*`.
- Batch 1–4 closely related issues per implementer (same files, same concepts). Split unrelated areas into separate implementers. An issue's text may say what to batch it with.
- Every implementer works in its own worktree (`isolation: "worktree"`) on a task branch, never on `main`. Parallel implementers only when they touch disjoint files (e.g. `engine/` vs `mac/`; two `mac/` tasks rarely qualify: `AppController`, `WorkspaceStore` and the window controllers are shared).

## 2. Brief an implementer
Use `subagent_type: "implementer"`. The brief must be self-contained (it starts cold):

```
Issues: #N (<ID>: title), …
Requirements: <IDs> (+ any §21 scope limits that apply)
Goal: <what done looks like, in behaviour terms>
Design decisions: <anything decided that isn't in ARCHITECTURE.md or the issue; specific types/APIs where consistency matters>
Boundaries: <files/crates it may touch; what NOT to do>
Must-have tests: <the behaviours that must be proven, esp. edge cases and failure paths>
Tests go in: <existing test file(s) by behaviour; shared helpers in tests/common>
Siblings to check: <other entry points/copies of what this changes>
Verification: make prepush → PREPUSH PASS; issue checklist ticked as parts land
Report: <scratchpad>/reports/<batch>.md
```

Never paste large file contents into briefs — point at paths. Agent hand-back delivery can fail; read the report file if no report arrives. Briefs tell implementers and reviewers to log QLab questions in `docs/todo/open-qlab-questions.md` rather than ask.

## 3. Verify the report (don't trust it)
- Check the commits exist, `git diff --stat` matches the claim, and the author is `openlab-dev`.
- The implementer runs `make prepush` once, which saves `PREPUSH PASS <sha>` in the worktree. Verify with `scripts/prepush.sh --check` run in the implementer's worktree (exit 0 only if the stamp is for HEAD and the tree is clean); the report's `PREPUSH PASS` line must name the same sha. Don't re-run prepush for the same head.
- Spot-check that the tests cited actually test the behaviour (open one or two), and that the ticked checklist boxes match what was built.

## 4. Adversarial review
- Spawn `subagent_type: "pr-reviewer"` with: issue numbers, base and branch/commit range, and any area you're suspicious of.
- Triage per CLAUDE.md "Findings policy". Niche QLab parity → the `v0.0.2` milestone.
- Send findings to the **same implementer** via `SendMessage` (it keeps its context), then re-review the fix commits. Max 3 rounds; then stop and escalate to the user with the disagreement.

## 5. Land it (PR into `main`)
Review traffic stays local (report files + `SendMessage`); GitHub is only the merge gate.
- Branch named after the tasks (e.g. `m5-2-engine-lifecycle`). Push once: `git push origin <worktree-branch>:<task-branch>`.
- `gh pr create --base main` with the body from `.github/pull_request_template.md`: `Closes #N` for each issue, task IDs + one line each, requirement IDs, the `PREPUSH PASS` line, the review summary and docs-claims lines of the template, QLab rows added. No full review text.
- Check CI on the PR **once** when you would otherwise merge (`gh pr checks N`); fetch job logs only if red (`gh run view <id> --log-failed`). If GitHub reports a conflict, merge `main` into the branch (merge commit, never rebase a pushed branch), re-run `make prepush`, push.
- Merge only when: CI green, no open High or Medium (or the user's downgrade is quoted in the PR), Lows fixed or filed. `gh pr merge N --merge --delete-branch --subject "Merge <task IDs>: <summary>"`. Only the orchestrator merges; implementers and reviewers never push to or merge into `main`.
- Afterwards: `git fetch origin main`, fast-forward the local `main`, remove the worktree (and its `engine/target`).
- Orchestrator-only PRs that touch `CLAUDE.md`, `.claude/`, `.github/`, `Makefile`, `scripts/` or docs claims get a Sonnet `pr-reviewer` pass whose brief says: check for contradictions with the other instruction files and for claims that don't match the repo. Small handoff-note commits may ride along in the next batch's PR.

## 6. Track
- The PR's `Closes #N` closes the issues. After merge, tick the matching `PROGRESS.md` lines (only the orchestrator edits it).
- Tick a requirement in `REQUIREMENTS.md` when no open issue lists it and every part within the release scope (§21) is implemented and tested. Check with an exact match, not GitHub search (it splits IDs at the hyphen): `gh issue list --state open --limit 500 --json number,body --jq '.[] | select(.body | test("\\bAUD-19\\b")) | .number'` prints nothing. The full v0.0.1 sweep is M6.4.
- Work found during a batch → a new issue (labels `task`, `area:*`, `size:*`, `model:*`, milestone, a Checklist of its requirement IDs) and a one-line `PROGRESS.md` task.
- Decisions that change architecture go into `docs/ARCHITECTURE.md` in the same commit. Lessons that every agent needs go into `CLAUDE.md`. A user policy decision goes into `CLAUDE.md` in the same session (through a reviewed PR); memory records only that the user decided it and when. Keep `docs/handoff/orchestrator-state.md` current at the end of a session.

## 7. Talk to the user
- After each landed batch, a short status update:
  ```
  Landed: <task IDs> — <one line each>
  Review: <n High / n Medium fixed>, CI <green/red/pending>
  Next: <task IDs>
  Needs you: <anything, or "nothing">
  ```
- At each milestone end: run "Milestone end" below, then a check-in — what now works, risks, anything to try by hand, and confirm before starting the next milestone.
- **Escalate instead of deciding** when: architecture or scope changes, a new non-trivial dependency, anything needing the user's machine/accounts/security settings, a reviewer-implementer deadlock, or a requirement looks wrong.

## 8. Milestone end
1. Cleanup batch: every open `low` / `severity:*` issue in the milestone is fixed or re-milestoned with a reason.
2. Whole-repo audit: 4 parallel reviewers (Opus: core+audio, engine service, app; Sonnet: CI/docs/instructions), each reading its area's files in full (not a diff) against CLAUDE.md. A second agent verifies each High/Medium before it is filed.
3. Update the policy-health line in `docs/handoff/orchestrator-state.md`: `low` issues opened/closed, Mediums fixed in the last reviews, claims the repo did not back.
4. Then the user check-in (section 7).

## Standing decisions (quick reference)
- Rust engine (headless, separate process) + Swift/AppKit macOS UI, JSON show file `.openlab`, GPLv3-or-later.
- v0.0.1 = audio-only rehearsal on macOS; video is v0.0.2; lighting deferred; Windows UI tech decided later (engine CI on Windows from day one).
- macOS 12 minimum, 2014 Mac mini is the performance target; CI perf budgets stand in until the M6 hardware check.
- Everything reaches `main` through a PR merged by the orchestrator after green CI; never push directly to `main`. No self-hosted CI runner (user decision 2026-10-28).
- QLab behaviour questions: CLAUDE.md "Behaviour decisions"; when the user answers a row, update it and the code if the answer differs. Still escalate architecture/scope/dependency questions, and behaviour that conflicts with ARCHITECTURE.md or show safety.
