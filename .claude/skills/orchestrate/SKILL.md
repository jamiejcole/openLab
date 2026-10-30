---
name: orchestrate
description: The openLab orchestrator loop — pick task issues, brief implementer subagents, run adversarial review, land each batch through a PR into main, track CI and progress, and report to the user. Use whenever continuing openLab development, after a context reset, or when the user says "continue", "next task", "status", or "orchestrate".
---

# openLab orchestrator

You lead openLab development. **You design, delegate, verify and report; you do not write large code yourself.** Keep your own context small: delegate reading-heavy and writing-heavy work to subagents, and keep only decisions and results.

## Budgets
**Tokens** (correctness comes first, see CLAUDE.md; within that, spend tokens on review and save them on re-runs, re-reads and long reports):
- Implementers default to **Sonnet**; pass `model: "opus"` for the issue's `model:opus` label (mixer/audio thread, lock-free queues, the cue-engine state machine, engine process lifecycle). The `pr-reviewer` runs on **Opus** (frontmatter); for Opus-written work it is a fresh agent that never saw the implementer's context (CLAUDE.md "Review").
- At most 2 implementers at once, on disjoint files. Fewer, smaller batches keep branches close to `main`.
- Review once per batch. Re-review the fix commits after any High or Medium fix. Max 3 rounds, then escalate.
- Briefs name the exact ARCHITECTURE sections, files and IDs to read so agents don't read whole docs.

**CI** (macOS runners are scarce; CI waits cost more than anything else):
- Push a branch **once**, after the local review and fixes. Every push is a full CI run.
- `main` requires the branch to be up to date. Before merging a green PR that is behind, update it (`gh pr update-branch N`, a merge commit), wait for CI, then merge. Merge one PR at a time; that is why batches are few and combined.
- Combine related reviewed batches into one PR (merge their branches into one combined branch, prepush once, list every `Closes #N`) instead of one PR per batch.
- The `changes` job runs only what a diff needs (`.github/workflows/ci.yml`). A `.github/` or `Makefile` change runs everything.

## 0. Re-orient (every session start or after a reset)
- Read `docs/handoff/orchestrator-state.md`, then `gh issue list --milestone "<current milestone>" --state open` (what's next; issues and milestones are the only tracker). Skim `CLAUDE.md` and `docs/ARCHITECTURE.md` headings, `git log --oneline -15`, `git status`.
- Identity: `git config user.email` = the repo-local agent identity (`docs/dev-setup.md`). Account details are local notes, never written into the repo.
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

Never paste large file contents into briefs — point at paths. Agent hand-back delivery can fail; read an implementer's report file if no report arrives (reviewers reply only).

## 3. Verify the report (don't trust it)
- Check the commits exist, `git diff --stat` matches the claim, and the author is `openlab-dev`.
- The implementer runs `make prepush` once, which saves `PREPUSH PASS <sha>` in the worktree. Verify with `scripts/prepush.sh --check` run in the implementer's worktree (exit 0 only if the stamp is for HEAD and the tree is clean); the report's `PREPUSH PASS` line must name the same sha. Don't re-run prepush for the same head.
- Verify per CLAUDE.md "Review": open the cited tests and run them. Ticked checklist boxes must match what was built.

## 4. Adversarial review
- Spawn a fresh `subagent_type: "pr-reviewer"` with: issue numbers, base and branch/commit range, and any area you're suspicious of. Every PR gets one (CLAUDE.md "Review").
- Re-run each High and Medium's command or test before sending it on; a finding that doesn't reproduce goes back to the reviewer or to a second reviewer, not to the implementer.
- Triage per CLAUDE.md "Findings policy". Niche QLab parity → the `v0.0.2` milestone.
- Send findings to the **same implementer** via `SendMessage` (it keeps its context), then re-review the fix commits. Max 3 rounds; then stop and escalate to the user with the disagreement.

## 5. Land it (PR into `main`)
Review traffic stays local (implementer report files, reviewer replies, `SendMessage`); GitHub is only the merge gate.
- Branch named after the tasks (e.g. `m5-2-engine-lifecycle`). Push once: `git push origin <worktree-branch>:<task-branch>`.
- `gh pr create --base main` with the body from `.github/pull_request_template.md`: `Closes #N` for each issue, task IDs + one line each, requirement IDs, the `PREPUSH PASS` line, the review summary and docs-claims lines of the template, QLab rows added. No full review text.
- Check CI on the PR **once** when you would otherwise merge (`gh pr checks N`); fetch job logs only if red (`gh run view <id> --log-failed`). If GitHub reports a conflict, merge `main` into the branch (merge commit, never rebase a pushed branch), re-run `make prepush`, push.
- Merge only when: CI green, no open High or Medium (or the user's downgrade is quoted in the PR), Lows fixed or filed. `gh pr merge N --merge --delete-branch --subject "Merge <task IDs>: <summary>"`.
- Afterwards: `git fetch origin main`, fast-forward the local `main`, remove the worktree (and its `engine/target`).
- Orchestrator-only PRs that touch `CLAUDE.md`, `.claude/`, `.github/`, `Makefile`, `scripts/` or docs claims get a `pr-reviewer` pass whose brief says: check for contradictions with the other instruction files and for claims that don't match the repo. Land them only while no agent is running, so no agent works under old rules. Small handoff-note commits may ride along in the next batch's PR.

## 6. Track
- The PR's `Closes #N` closes the issues.
- Tick a requirement in `REQUIREMENTS.md` when no open issue lists it and every part within the release scope (§21) is implemented and tested. Check with an exact match, not GitHub search (it splits IDs at the hyphen): `gh issue list --state open --limit 500 --json number,body --jq '.[] | select(.body | test("\\bAUD-19\\b")) | .number'` prints nothing. The full v0.0.1 sweep is M6.4.
- Work found during a batch → a new issue (labels `task`, `area:*`, `size:*`, `model:*`, milestone, a Checklist of its requirement IDs).
- Decisions that change architecture go into `docs/ARCHITECTURE.md` in the same commit. A lesson every agent needs becomes a CI or repo check where possible, else one line in `CLAUDE.md`. A user policy decision goes into `CLAUDE.md` in the same session (through a reviewed PR); memory records only that the user decided it and when. Keep `docs/handoff/orchestrator-state.md` current at the end of a session.

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

## Standing decisions
Rules live in `CLAUDE.md`, design in `docs/ARCHITECTURE.md`, scope in `REQUIREMENTS.md` §21. Orchestrator-only: no self-hosted CI runner (user decision 2026-10-28); `main` requires CI Result green and the branch up to date with `main` (2026-10-30).
