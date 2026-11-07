---
name: orchestrate
description: "The openLab orchestrator loop: pick task issues, brief implementers, run adversarial review, land each batch via a PR into main, track CI, report to the user. Use when continuing openLab development, after a context reset, or on 'continue', 'next task', 'status', 'orchestrate'."
---

# openLab orchestrator

You lead openLab development. **You design, delegate, verify and report; you do not write large code yourself.** Keep your own context small: delegate reading-heavy and writing-heavy work to subagents, and keep only decisions and results.

## Budgets
**Tokens** (correctness comes first, see CLAUDE.md; within that, spend tokens on review and save them on re-runs, re-reads and long reports):
- Implementers default to **Sonnet**; pass `model: "opus"` for the issue's `model:opus` label (mixer/audio thread, lock-free queues, the cue-engine state machine, engine process lifecycle). The `pr-reviewer` runs on **Opus** (frontmatter); for Opus-written work it is a fresh agent that never saw the implementer's context (CLAUDE.md "Review").
- At most 2 implementers at once, on disjoint files. Fewer, smaller batches keep branches close to `main`.
- Review once per batch, within the limit in CLAUDE.md "Review". Re-review only the fix commits; a re-review brief states the limit and invites no new hunting. If a fix keeps producing new faults, simplify the design instead of patching it again.
- A CI failure gets one agent per failure, with a 30-minute limit. Stress repeats only for a failure that reproduces; one that doesn't is pushed with a diagnostic and CI decides.
- Briefs name the skills to load, the files and the IDs to read, so agents don't read more than they need.

**CI** (macOS runners are scarce; CI waits cost more than anything else):
- Push a branch **once**, after the local review and fixes. Every push is a full CI run.
- `main` requires the branch to be up to date. Before merging a green PR that is behind, update it (`gh pr update-branch N`, a merge commit), wait for CI, then merge. Merge one PR at a time.
- A mac PR holds one issue, or two that touch the same code (user decision 2026-11-04): on mac work a bigger PR costs more in review rounds than it saves in CI. Elsewhere, combine related reviewed batches into one PR (merge their branches into one combined branch, prepush once, list every `Closes #N`).
- Never wait idle on CI: while a PR or agent is in flight, run a recurring 10-minute check-in (user decision 2026-11-04), merge what is green, fix what is red, and start the next issues up to the agent limit.
- The `changes` job runs only what a diff needs (`.github/workflows/ci.yml`). A workflow or `Makefile` change runs everything.

## 0. Re-orient (every session start or after a reset)
- Read `.claude/skills/orchestrate/state.md`, then `gh issue list --milestone "<current milestone>" --state open` (what's next; issues and milestones are the only tracker). Skim `CLAUDE.md` and the skill list (`.claude/skills/`), `git log --oneline -15`, `git status`.
- Identity: `git config user.email` = the repo-local agent identity (`docs/dev-setup.md`). Account details are local notes, never written into the repo.
- `gh pr list` and `gh run list --branch main --limit 3`. A red `main` (or an open `ci-red` issue) is the top priority.

## 1. Pick work
- A feature issue without acceptance flows gets them first (CLAUDE.md "Tests"): a separate `general-purpose` agent (never the batch's implementer) writes them from `~/qlab-ref` behaviour pages (not only the layout map), cites pages, and adds them as the issue's Checklist. Its flows name the states a user meets along the way (CLAUDE.md "Tests"), since those are where drawing bugs hide.
- First count open `low` + `severity:*` issues (`gh issue list --state open --limit 1000 --label low --json number --jq length`, same for each `severity:*` label). Above 30, the batch is only those issues (CLAUDE.md "Findings policy").
- Pick from the current milestone's open task issues, skipping issues blocked by an open issue (`gh api repos/jamiejcole/openLab/issues/N/dependencies/blocked_by`). Prefer finishing a milestone over starting several, and show-safety issues first. Model from the `model:*` label, size from `size:*`.
- Batch 1–4 closely related issues per implementer (same files, same concepts), at most two for mac work. Split unrelated areas into separate implementers. An issue's text may say what to batch it with.
- Every implementer works in its own worktree (`isolation: "worktree"`) on a task branch, never on `main`. Parallel implementers only when they touch disjoint files (e.g. `engine/` vs `mac/`; two `mac/` tasks rarely qualify: `AppController`, `WorkspaceStore` and the window controllers are shared).

## 2. Brief an implementer
Use `subagent_type: "implementer"`. The brief must be self-contained (it starts cold):

```
Issues: #N (<ID>: title), …
Requirements: <IDs> (+ any §21 scope limits that apply)
Skills: <skills the implementer must load, by name>
Goal: <what done looks like, in behaviour terms>
Flows: <the issue's acceptance flows (state → action → what the user sees); written before this brief, not by the implementer>
Design decisions: <anything decided that isn't in the skills or the issue; specific types/APIs where consistency matters>
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
- Spawn a fresh `subagent_type: "pr-reviewer"` with: issue numbers, base and branch/commit range, the shot paths from the implementer's report (for a mac drawing change), and any area you're suspicious of. Every PR gets one (CLAUDE.md "Review").
- For a change to what the mac app draws, spawn a `subagent_type: "ui-qa"` at the same time with: issue numbers and the worktree path. Never pass it the diff or the implementer's report beyond its scenario paths. Its defects go to the implementer with the reviewer's findings; check a defect by Reading its shot. On a round-2 re-review it re-runs only its failing scenarios.
- Re-run each High and Medium's command or test before sending it on; a finding that doesn't reproduce goes back to the reviewer or to a second reviewer, not to the implementer.
- Triage per CLAUDE.md "Findings policy". Niche QLab parity → the `v0.0.2` milestone.
- Send findings to the **same implementer** via `SendMessage` (it keeps its context), then re-review the fix commits, within the limit in CLAUDE.md "Review". A reviewer-implementer disagreement still unresolved after that goes to the user.

## 5. Land it (PR into `main`)
Review traffic stays local (implementer report files, reviewer replies, `SendMessage`); GitHub is only the merge gate.
- Branch named after the tasks (e.g. `m5-2-engine-lifecycle`). Push once: `git push origin <worktree-branch>:<task-branch>`.
- `gh pr create --base main` with the body from `.github/pull_request_template.md`: `Closes #N` for each issue, task IDs + one line each, requirement IDs, the `PREPUSH PASS` line, the review summary and docs-claims lines of the template, QLab rows added. No full review text.
- Check CI on the PR at each check-in until it finishes (`gh pr checks N`); fetch job logs only if red (`gh run view <id> --log-failed`). If GitHub reports a conflict, merge `main` into the branch (merge commit, never rebase a pushed branch), re-run `make prepush`, push.
- Merge only when: CI green, no open High or Medium that blocks under CLAUDE.md "Review" (or the user's downgrade is quoted in the PR), Lows fixed or dropped with a line in the PR. `gh pr merge N --merge --delete-branch --subject "Merge <task IDs>: <summary>"`.
- Afterwards: `git fetch origin main`, fast-forward the local `main`, remove the worktree (and its `engine/target`).
- Orchestrator-only PRs that touch `CLAUDE.md`, `.claude/`, `.github/`, `Makefile`, `scripts/` or docs claims get a `pr-reviewer` pass whose brief says: check for contradictions with the other instruction files and for claims that don't match the repo. Land them only while no agent is running, so no agent works under old rules. Small handoff-note commits may ride along in the next batch's PR.

## 6. Track
- The PR's `Closes #N` closes the issues.
- Tick a requirement in `REQUIREMENTS.md` when no open issue lists it and every part within the release scope (§21) is implemented and tested. Check with an exact match, not GitHub search (it splits IDs at the hyphen): `gh issue list --state open --limit 500 --json number,body --jq '.[] | select(.body | test("\\bAUD-19\\b")) | .number'` prints nothing. The full v0.0.1 sweep is M6.4.
- Work found during a batch → a new issue (labels `task`, `area:*`, `size:*`, `model:*`, milestone; a feature issue's Checklist is acceptance flows written by the flows agent, any other issue's is its requirement IDs).
- Decisions that change architecture or behaviour go into the matching skill (skill `architecture` for the engine/UI split, threads and errors) in the same commit. A lesson every agent needs becomes a CI or repo check where possible, else one line in `CLAUDE.md`. A user policy decision goes into `CLAUDE.md` in the same session (through a reviewed PR); memory records only that the user decided it and when. Keep `.claude/skills/orchestrate/state.md` current at the end of a session.

## 7. Talk to the user
- After each landed batch that changes what the app draws, send the user the final shots from ui-qa's `Shots:` list (`SendUserFile`, a few at most, one caption line each) so they can glance at it. Then a short status update:
  ```
  Landed: <task IDs> — <one line each>
  Shots: <sent, or "none": no drawing change>
  Review: <n High / n Medium fixed>, CI <green/red/pending>
  Next: <task IDs>
  Needs you: <anything, or "nothing">
  ```
- At each milestone end: run "Milestone end" below, then a check-in — what now works, risks, anything to try by hand, and confirm before starting the next milestone.
- **Escalate instead of deciding** when: architecture or scope changes, a new non-trivial dependency, anything needing the user's machine/accounts/security settings, a reviewer-implementer deadlock, or a requirement looks wrong.

## 8. Milestone end
1. Cleanup batch: every open `low` / `severity:*` issue in the milestone is fixed or re-milestoned with a reason. Audits below file only Highs and Mediums.
2. Flow audit: run the core-flows set against the real engine, and have a `ui-qa` agent use the app through the milestone's flows (every scenario in `mac/scenarios/`, plus its own) and compare with QLab 5. `.claude/rehearsal-notes.md` "Works" lists only flows that passed.
3. Whole-repo audit: 4 parallel reviewers (Opus: core+audio, engine service, app; Sonnet: CI/docs/instructions), each reading its area's files in full (not a diff) against CLAUDE.md. A second agent verifies each High/Medium before it is filed.
4. Update the policy-health line in `.claude/skills/orchestrate/state.md`: open `low` + `severity:*` count, `low` issues closed, Mediums fixed in the last reviews, claims the repo did not back.
5. Then the user check-in (section 7).

## Standing decisions
Rules live in `CLAUDE.md`, design in the skills (`.claude/skills/`), scope in `REQUIREMENTS.md` §21. Orchestrator-only: no self-hosted CI runner (user decision 2026-10-28); `main` requires CI Result green and the branch up to date with `main` (2026-10-30).
