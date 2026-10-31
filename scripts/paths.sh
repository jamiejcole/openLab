#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Which paths need which checks. Sourced by scripts/prepush.sh and by the `changes` job in
# .github/workflows/ci.yml, so the local gate and CI choose their jobs from the same diff.
# ERE patterns, matched with `grep -E` against repo-relative paths.

# A CI, Makefile or gate-script change re-runs everything.
PATHS_ALL_RE='^(\.github/|Makefile$|scripts/(paths|rust-checks|soak-rt-ci)\.sh$|scripts/ui-smoke\.txt$)'
# The Rust workspace (Cargo files, toolchain pin and deny.toml all live under engine/).
PATHS_ENGINE_RE='^engine/'
# The macOS app job. On a pull request it runs only for what the app reads from the engine: the
# protocol and show file (Swift Codable types, fixtures), the engine's command line and engine.json
# (host/), and the bundled build. The app's tests use FakeEngine; once XCUITests drive a real
# engine (M5.16), widen this to every engine change. On main every engine change rebuilds the app.
PATHS_MAC_PR_RE='^(mac/|engine/fixtures/protocol/|engine/crates/core/src/(protocol|show_file)|engine/crates/engine/src/(main\.rs|host/)|engine/Cargo\.(toml|lock)$|engine/rust-toolchain\.toml$|docs/(protocol|file-format)\.md$)'
PATHS_MAC_MAIN_RE='^(mac|engine)/'
# The Release universal build and bundle check (`make mac`, `make verify-bundle`): on main always,
# on a pull request only when the build itself changes.
PATHS_BUNDLE_RE='^(mac/(project\.yml|scripts/)|engine/Cargo\.(toml|lock)$|engine/rust-toolchain\.toml$)'
# Perf-sensitive code also runs on the Intel runner (closest to the 2014 Mac mini target).
PATHS_INTEL_RE='^engine/(crates/audio/|crates/engine/tests/(perf|soak|soak_rt)\.rs$|crates/engine/tests/soak_common/|crates/engine/src/control/(watch|media|voices)/|crates/engine/src/control/watch_share\.rs$)'
# App code that XCUITests exercise (the engine client under Engine/ is covered by unit tests).
PATHS_UI_RE='^mac/openLab/.*\.swift$'
PATHS_UI_EXCLUDE_RE='^mac/openLab/Engine/'
