<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Developer machine setup

Facts about building and testing openLab on a Mac that are not obvious from the code.

- **Commit identity.** Agent commits are authored by the repo-local identity `openlab-dev <349245076+openlab-dev@users.noreply.github.com>`. Set it per clone or worktree with `git config user.name` / `git config user.email`; do not rely on the global config.
- **XCTest deploy target.** The test targets deploy at macOS 14.0 because XCTest requires it. The app itself stays at 12.0 (`make verify-bundle` checks that).
- **XCUITest and Automation Mode.** UI tests need macOS Automation Mode. The first run on a machine may prompt for authorisation. They take over the screen, so run one XCUITest session at a time: `make prepush` takes a `lockf` lock (`/tmp/openlab-xcuitest.lock`) and waits for another run to finish. Do not start `make test-mac` by hand while a prepush is running.
- **Engine signing.** The engine build phase (`mac/scripts/build-engine.sh`) ad-hoc signs the engine, because Xcode does not sign nested binaries in `Contents/MacOS`.
- **Shared build cache.** Parallel agents share one machine and sccache (`~/.cargo/config.toml` wraps rustc; incremental compilation is off). Use one build directory per worktree and never share a `CARGO_TARGET_DIR` between two source trees. Wait only on your own command: never poll machine-wide process lists (for example `pgrep cargo`) to decide whether to start a build.
- **Before a push.** `make prepush` (see `CLAUDE.md` "Commands"). It prints `PREPUSH PASS` or `PREPUSH FAIL: <step>` and the path of its log.
