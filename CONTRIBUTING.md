<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Contributing to openLab

openLab runs live shows, so a crash or a wrong cue can stop a performance. Correctness comes before speed, and
every rule that matters is enforced by a check rather than by review. The full rule set is
[`CLAUDE.md`](CLAUDE.md); it is written for the coding agents that do most of the work here, and it applies to
people too. This page is the short way in.

## Build and test

Machine setup is in [`docs/dev-setup.md`](docs/dev-setup.md); the commands are in the [README](README.md).
Before you push, run `make prepush`. It runs the lint, the Rust tests, the macOS unit tests and the UI smoke set,
and prints `PREPUSH PASS <sha>` when all of it passed for that commit.

## How the code is laid out

- `engine/`: a Rust workspace. `crates/core` is pure logic (the show model, the cue state machine, editing
  commands, the wire protocol types): no threads, clocks, files or sockets, and time is passed in.
  `crates/audio` has devices, decoding, the mixer and rendering. `crates/engine` is the headless engine binary:
  control thread, persistence, the WebSocket server and OSC.
- `mac/`: the Swift and AppKit app for macOS 12 and later. It is a client of the engine, so a fault in the UI
  never stops playback. The Xcode project is generated from `mac/project.yml`.
- `docs/`: the two contracts, [`protocol.md`](docs/protocol.md) (engine and app) and
  [`file-format.md`](docs/file-format.md) (the show file).
- `.claude/skills/`: one folder per area (mixer, cue engine, persistence, the mac app, and so on) holding that
  area's design, behaviour and invariants. Read the one for the area you touch before changing it, and update it
  in the same commit when you change behaviour.

## Rules that catch people out

- **The audio callback** never allocates, locks, logs, formats or touches the disk. It talks to the rest of the
  engine through preallocated lock-free queues and atomics only.
- **Engine code never panics** outside tests: no `unwrap`, `expect` or unchecked indexing. A failure becomes a
  broken cue, a warning or a log line, and the show keeps running.
- **Every input from outside the process is validated**: show files, media, OSC and protocol messages. Bad input
  is a warning, never a crash or a hang.
- **Behaviour follows QLab 5.** Where its behaviour is known, match it and cite the manual page in the pull
  request. Never copy its text, labels, icons or screenshots into this repository.
- **Contracts change together.** A protocol change updates the Rust types, the fixtures in
  `engine/fixtures/protocol/`, the Swift types and `docs/protocol.md` in one commit. A show-file change updates
  `docs/file-format.md` and the current fixture.
- **The app** is Swift 6 and AppKit only, with no third-party packages. User-facing text goes through
  `Localizable.strings`.
- **New dependencies** must be GPLv3-compatible and are listed, with the reason, in the architecture skill's
  dependency list.
- Every source file starts with `SPDX-License-Identifier: GPL-3.0-or-later`.

## Tests

- Every change ships with tests. A bug fix ships with one regression test that failed before the fix.
- Tests use the virtual clock or the offline render driver, never real-time sleeps.
- A behaviour the user sees is proven end to end on the real engine: the test does what the user does and checks
  what the user would see.
- A change to what the app draws is looked at before review. `make ui-shots SCENARIO=mac/scenarios/<name>.json`
  drives the real window and saves pictures of it.

## Pull requests

- Work on a branch; changes reach `main` only through a pull request with green CI.
- Commit subjects are an area prefix and an imperative, for example `core: add continue-mode state transitions`.
- Fill in the pull request template: the issues closed, the `PREPUSH PASS` line, and which skills and docs changed.
- Findings are rated High, Medium or Low (the definitions are in `CLAUDE.md`). High and Medium block a merge.

Open work is in GitHub issues and milestones. The licence is GPLv3 or later.
