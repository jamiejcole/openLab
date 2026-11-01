#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Which paths need which checks. Sourced by scripts/prepush.sh and by the `changes` job in
# .github/workflows/ci.yml, so the local gate and CI choose their jobs from the same diff.
# ERE patterns, matched with `grep -E` against repo-relative paths.

# A CI, Makefile or gate-script change re-runs everything.
PATHS_ALL_RE='^(\.github/|Makefile$|scripts/(paths|rust-checks|soak-rt-ci)\.sh$|scripts/ui-smoke\.txt$)'
# The Rust workspace (Cargo files, toolchain pin and deny.toml all live under engine/).
PATHS_ENGINE_RE='^engine/'
# The macOS app job. On a pull request it runs for app changes and for what the app reads from the
# engine: the protocol and show file (Swift Codable types, fixtures), the engine's command line and
# engine.json (host/), and the bundled build. On main every engine change rebuilds the app.
PATHS_MAC_PR_RE='^(mac/|engine/fixtures/protocol/|engine/crates/core/src/(protocol|show_file)|engine/crates/engine/src/(main\.rs|host/)|engine/Cargo\.(toml|lock)$|engine/rust-toolchain\.toml$|docs/(protocol|file-format)\.md$)'
PATHS_MAC_MAIN_RE='^(mac|engine)/'
# The app tests that start the real engine (its virtual device) and the UI smoke set: expensive, so on
# a pull request or push only when the engine, the protocol, the app's engine client, the app's tests
# (unit and UI: a new or changed real-engine test runs before merge), the test shows or the project
# (how the tests and shows are built and bundled) change; nightly and on demand always. Other app
# changes run the engine-free unit tests (`make test-mac-unit`; the real-engine tests report a skip).
PATHS_APP_ENGINE_RE='^(engine/|docs/protocol\.md$|mac/openLab/Engine/|mac/TestShows/|mac/scripts/build-engine\.sh$|mac/openLabTests/|mac/openLabUITests/|mac/project\.yml$)'
# The Release universal build and bundle check (`make mac`, `make verify-bundle`): on main always,
# on a pull request only when the build itself changes.
PATHS_BUNDLE_RE='^(mac/(project\.yml|scripts/)|engine/Cargo\.(toml|lock)$|engine/rust-toolchain\.toml$)'
# Perf-sensitive code also runs on the Intel runner (closest to the 2014 Mac mini target).
PATHS_INTEL_RE='^engine/(crates/audio/|crates/engine/tests/(perf|soak|soak_rt)\.rs$|crates/engine/tests/soak_common/|crates/engine/src/control/(watch|media|voices)/|crates/engine/src/control/watch_share\.rs$)'
