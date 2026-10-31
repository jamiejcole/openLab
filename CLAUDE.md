# openLab — Agent Instructions

openLab is an open-source (GPLv3-or-later) show-playback system for theatre, a QLab 5 alternative. It runs live shows: **a crash or wrong behaviour can stop a performance.** Correctness beats speed. No human reads the code or reviews PRs: CI checks and an independent review agent are the only gates, so a rule that matters is a check, not a sentence here.

This file is the only copy of every rule below; other files link here instead of restating it. Read the sections of [`REQUIREMENTS.md`](REQUIREMENTS.md) (IDs like `AUD-07`, scope in §21) and [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) that your task names (grep; don't read them whole). Open work is GitHub issues and milestones; there is no other tracker.

## When rules or facts conflict
1. **Show safety at risk, or your brief asks for something ARCHITECTURE.md rules out:** stop that task and report to the orchestrator. It decides, fixes ARCHITECTURE.md in the same PR, or escalates scope, dependency and security questions to the user.
2. **A doc is stale** (ARCHITECTURE.md, REQUIREMENTS.md or a decision note disagrees with the code or with known QLab 5 behaviour): follow the code or QLab, carry on, and report the stale line.
3. **Unknown QLab behaviour:** never block; see "Behaviour". On a show-safety path, rule 1 applies instead.
4. **Style and everything else:** proceed and say what you chose in your report.

## Layout and commands
`engine/` Rust workspace (`crates/core` pure logic, `crates/audio` devices/decode/mixer/render, `crates/engine` binary). `mac/` Swift + AppKit app (XcodeGen from `mac/project.yml`; never commit `.xcodeproj`). `docs/` contracts. Machine setup: `docs/dev-setup.md`.
`make lint`, `make test`, `make test-rust`, `make test-mac` (unit tests + the UI smoke set, `scripts/ui-smoke.txt`), `make test-mac-ui-full` (every UI test; nightly in CI), `make mac`, `make verify-bundle`, `make prepush` (the pre-push gate; prints `PREPUSH PASS <sha>` or `PREPUSH FAIL: <step>`).

## Real-time audio thread (anything the audio callback calls)
- No allocation/deallocation, locks, syscalls, logging, formatting or disk I/O. Preallocate; hand objects to the control thread to free.
- Only `rtrb` SPSC queues and atomics. `assert_no_alloc` stays enabled in debug/test.

## Engine Rust code
- No `unwrap`, `expect`, `panic!` or unchecked indexing outside tests; return `Result`. Failures become broken-cue state, warnings or log entries.
- `openlab-core` is pure: no threads, clocks, files, sockets. Time is passed in. `unsafe` only outside core, with `// SAFETY:` and a test.
- Validate every external input (show files, media, OSC, protocol). Bad input → warning, never crash or hang. A limit or validation applies on every path that creates or changes that data, each path tested.
- Debug and release behave the same: `cfg(debug_assertions)` may add checks that log or count, never change behaviour.
- A caught fault is confined to the smallest unit (voice, cue, peer, document), which is dropped or marked broken and reported; the show keeps running.

## Tests
- Every change ships with tests; every bug fix with one regression test that failed before the fix. No variant matrices.
- No real-time sleeps or wall-clock in tests: use the virtual clock or offline render driver.
- New behaviour is proven by unit tests. Add an XCUITest only for a flow unit tests cannot reach (focus, window ordering) and say in the PR whether it joins the per-PR smoke set (`scripts/ui-smoke.txt`, 3 tests: launch, GO, panic) or only the nightly full suite (the default).
- Extend the existing test file and `tests/common`; no new harness types. `tests/common/mod.rs` starts with `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]`.
- A test that cannot run here fails or reports a skip; it never passes by returning early.
- Test-only code (fakes, fault switches, fixtures) never compiles into a Release product.
- Before reporting done: `make prepush`. Windows paths: expect `\` or use `to_portable`; APFS refuses non-UTF-8 names.

## Contracts (same commit)
- Protocol change: Rust types in core, fixtures in `engine/fixtures/protocol/`, Swift `Codable` types, `docs/protocol.md`.
- Show-file change: bump `format_version`, add a migration step and an old-version fixture, update `docs/file-format.md`.
- New dependency: GPLv3-compatible, justified, listed in ARCHITECTURE §12. Prefer stdlib or an existing one.
- Docs describe what exists; plans are issues. A claim about CI, lints or tests is checked against `ci.yml` / `Cargo.toml` when written.
- Every source file starts with `SPDX-License-Identifier: GPL-3.0-or-later`.

## macOS app
- Swift 6, AppKit only, macOS 12.0 target: no SwiftUI, no Observation, no newer API without an `#available` fallback. No third-party Swift packages. Assets are original or SF Symbols.
- View controllers talk to `WorkspaceStore`, never the socket. User-facing strings go through `Localizable.strings`.

## Behaviour: match QLab 5
- openLab builds what QLab 5 has and drops what it doesn't, including openLab extras already built (user decision 2026-10-29/30).
- Where QLab's behaviour is known, copy it and say so in your report. Look it up: `~/qlab-ref/qlab-ref search "<words>"` then `show <page>` if it exists, then https://qlab.app/docs/v5/. Paraphrase and cite pages. UI layout: `~/qlab-ref/ui` (`README.md`, `surfaces.json`, `menus.json`, `cue-types.json`, `shots/`); cite its surface ids.
- Where it is unknown: search [`docs/todo/open-qlab-questions.md`](docs/todo/open-qlab-questions.md) (every user answer is there; never ask twice), then implement the likeliest behaviour and add an `assumed` row (assumption, what you searched).
- Public repo: never copy QLab text, labels, tooltips, icons, artwork or screenshots into the repo, issues or PRs. Describe in our own words; give our controls our own similar names. Never put personal account or machine details in the repo, issues or PRs.

## Findings policy
- **High:** a show can stop, sound wrong, fire or stop the wrong cue, lose data, or the process can crash, panic or hang, on any input (fuzzers and property tests included; any input from outside the process is reachable).
- **Medium:** wrong behaviour; a false or stale claim in a contract doc, ARCHITECTURE.md or this file; a missing test on a changed path; debug/release difference; duplication, dead code or a parallel harness the change adds.
- **Low:** everything else.
- High and Medium block merge; only the user downgrades a Medium (quote it in the PR). Lows are fixed in the same PR or dropped with one line in the PR; they are never filed as issues.
- Review covers the diff. A pre-existing problem in a touched file is reported only if High (fixed in this PR, or the PR waits for its fix) or Medium (a `severity:medium` issue in the current milestone; blocks the milestone, not the PR, unless the PR makes it worse). Milestone audits file only Highs and Mediums.
- More than about 30 open finding issues (`severity:*` and `low`): the next batch is clean-up only. Existing `low` issues are worked down, not added to.
- Every known exception (an allowlist or known-failure entry, an accepted finding) cites an open issue, or says why it is permanent.
- A generator, fuzzer or oracle finding that is not a crash, panic or hang and needs input the loader already rejects: fix the generator or oracle.

## Review
- Every change is reviewed by a `pr-reviewer` agent on a different model or a fresh context from its implementer (Sonnet work: Opus review; Opus work: a fresh Opus review). The orchestrator never reviews its own change.
- A High or Medium finding comes with the test or command the reviewer ran and its output. The orchestrator re-runs it before acting, and checks reports by running the named tests, not by reading summaries.

## Work and git
- Work only on assigned issues. Tick an issue's Checklist box when that part is implemented **and tested**; say what is missing for any left open. Report extra work found; don't expand scope. Niche QLab parity not needed for v0.0.1 → `v0.0.2` milestone.
- Milestone and task IDs (`M5.4`) may appear in issues, PR titles and docs, never in code, comments or test names (requirement IDs are fine there).
- Only the orchestrator ticks `REQUIREMENTS.md`, after merge. Notes and decisions go in the issue, the PR or the docs.
- Commit with plain `git add` / `git commit`; author is the repo-local identity (`docs/dev-setup.md`); no `Co-authored-by` or AI trailer. Subject: area prefix + imperative, e.g. `core: add continue-mode state transitions`.
- Never push to `main`, force-push or rebase shared history. Changes reach `main` only through a PR the orchestrator merges after green CI. Implementers and reviewers never push or merge.
- Comments explain why. Boring over clever; small modules; no speculative abstractions or scaffolding for future features. Match surrounding code.

The orchestrator follows `.claude/skills/orchestrate/SKILL.md`; agent roles are in `.claude/agents/`.
