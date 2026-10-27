<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Orchestrator state — handoff to the Mac mini (2026-10-27)

The cloud orchestrator session ends here. A new session on the dedicated Mac mini continues with the
`orchestrate` skill. Read this file, then `PROGRESS.md`, then work the queue below. Goal: every M1–M4
task ticked so M5 can finish.

## First actions
1. `git fetch origin` and check every open PR below: CI state and `mergeable`. Fix conflicts oldest
   first; a green PR that conflicts does not auto-merge.
2. Fix the **panic bug** (queue item 1) before new feature work: it is show-safety.
3. Run `make test` and `make mac` locally once, so the Swift side is known-good on this machine.

## Open PRs (all have passed adversarial review unless noted)
| PR | Task | Waits on |
| --- | --- | --- |
| #33 | M4.2g device stall watchdog | CI |
| #44 | M2.26 nothing_to_fade, M4.1d, M4.13 | #60 (its fuzz job hit the WAV crash); then merge main into #44 |
| #48 | M2.28 second QLab batch | #44 merged, then move the `control_media` preview-Panic tests to #44's V4/V6 behaviour, then enable auto-merge |
| #49 | M4.7a/b/c engine load, unclean exits, live log | CI |
| #59 | M4.12g BatchSet in place | CI |
| #60 | Fuzz: non-WAVE RIFF start hid an overflowing WAV header | CI |
| #61 | M4.5c/M4.5d OSC passcode table, show-file v4 | built on #48: enable auto-merge only after #48 merges |
| (branch) `m2-25b-probe-starvation` | M2.25b stalled volumes | final Opus check was running at handoff (report: see below); open its PR when it passes |

Show-file versions: main is v2; #48 makes v3 (`devamp_mode`); #61 makes v4 (passcode table). M2.29d
(`media_hints`) must be v5 and M2.21 (audition output) after it — never two branches on one version.

## Queue (in order)
1. **Panic bug (new, pre-existing on main, core):** during a Panic fade, a stream failure
   (`ActionFailed { reason: StreamFailed }`) lets another cue start (`StateChanged … PreWaiting`).
   Found by `cue_engine_prop::engine_invariants`. Reproduce by putting this line in
   `engine/crates/core/tests/cue_engine_prop.proptest-regressions`:
   ```
   cc 8935f5a73b0bf2177db77da801f46ba020ec9b5e77bdebce26c66a56bc12c691 # shrinks to case = Case { specs: [Spec { kind: 2, pre: 0, post: 0, mode: 0, armed: true, dup: false, level: 0, group_mode: 0, target: 0, flag: false, second: 0, release: false }, Spec { kind: 0, pre: 0, post: 0, mode: 0, armed: false, dup: true, level: 0, group_mode: 0, target: 0, flag: false, second: 0, release: false }, Spec { kind: 27, pre: 0, post: 0, mode: 0, armed: false, dup: false, level: 0, group_mode: 0, target: 0, flag: false, second: 0, release: false }, Spec { kind: 0, pre: 0, post: 0, mode: 0, armed: false, dup: false, level: 0, group_mode: 0, target: 0, flag: false, second: 0, release: false }, Spec { kind: 27, pre: 0, post: 0, mode: 0, armed: false, dup: false, level: 0, group_mode: 0, target: 0, flag: false, second: 0, release: false }, Spec { kind: 22, pre: 0, post: 0, mode: 0, armed: false, dup: false, level: 0, group_mode: 4, target: 0, flag: false, second: 0, release: false }, Spec { kind: 27, pre: 0, post: 0, mode: 0, armed: false, dup: false, level: 1, group_mode: 0, target: 0, flag: false, second: 0, release: false }], steps: [Step { dt: 0, op: 2, a: 96, b: 0 }, Step { dt: 0, op: 1, a: 0, b: 0 }, Step { dt: 0, op: 20, a: 0, b: 0 }, Step { dt: 0, op: 31, a: 0, b: 9 }], lookahead: 0, min_go: 0, panic: 1 }
   ```
   Opus implementer and reviewer. Add a PROGRESS line for it.
2. **M4.15d → e → f → g → h → i → j** (multiple workspaces; design in `docs/ARCHITECTURE.md` §1, §3.1,
   §3.5 and the M4.15 PROGRESS lines). Sequential, all touch `control/mod.rs`: one at a time, Opus
   for d, e, f. M4.10a after M4.15f; M4.9 folds into M4.15g with M4.4d/e.
3. In parallel with M4.15 (different files): M4.6a (Network cue property commands), M4.6c, M4.2d,
   M4.2e, M4.4g, M4.12d/e/f, M1.17, M1.19, M1.20, M2.30.
4. After #48 and #61 merge: M2.29b, M2.29d (show-file v5), M2.21.
5. Flake: M4.4j (`stalled_tail` FIFO tests hang/fail under load; `a_start_held_by_its_probe_fails_after_five_seconds`).
6. QLab checks still open: M4.6b (TCP framing), M2.27 leftovers.

## Rules learned the hard way
- At most 5 agents, at most 2 on Opus. One build per worktree; never share a `CARGO_TARGET_DIR`
  between two source trees (stale artifacts give false compile errors). Delete `engine/target`
  (and `target/x86_64-pc-windows-gnu`) when an agent finishes.
- `PROGRESS.md`: tick boxes and add one-line tasks only; never append notes (every PR edits it).
- Every brief includes the adversarial checklist in `.claude/agents/implementer.md` (live-show
  impact, hostile input, Windows, slow 2014 Mac, undetectable failures).
- Two green PRs can still break main together (a new field + an old literal). After each merge,
  re-check the other open PRs; CI tests each PR's merge with main at the time it ran.
- Never let an agent reuse another agent's branch name in a scratch worktree (it moves that ref).
- Claude Code may not edit its own permission settings; the user owns `.claude/settings.json`.
- Commits: no Co-authored-by or AI trailers (CLAUDE.md).

## Decisions awaiting the user's review
All in `docs/handoff/decisions/`: `qlab-confirm.md`, `m2-28.md`, `m2-29.md`, `m2-25b.md`,
`m4-2g-watchdog.md`, `m4-4c.md`, `m4-5c.md`, `m4-6.md`, `m4-7b.md`, `m4-11b.md` (some arrive with
their PRs). The ones most worth a look: M4.11b post-wait rule, M4.6 send policy (IPv4 first, 1 s
drop, unresolved host = broken), M4.5c rights and lockouts, M2.25b which volumes may stall, M4.15b
resize under a playing cue.
