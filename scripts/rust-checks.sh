#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# The Rust gate's command lines, in one place: `make lint`, `make test-rust`, scripts/prepush.sh and the
# `rust` job in .github/workflows/ci.yml all call this, so the local gate and CI cannot drift apart.
# scripts/check-repo.sh fails if one of them spells these commands out itself.
#
#   fmt              rustfmt, check only
#   clippy           lints the test build (--all-targets), then the shipped build (--lib --bins): without
#                    --all-targets the test-only features (test-support) are not unified in, so a warning
#                    only the shipped build shows is caught
#                    and checks no test-only feature (test-support, fault-injection) is in the shipped
#                    engine's dependency graph, so test seams never compile into a release build
#   clippy-windows   the same lints for the Windows target (needs `rustup target add x86_64-pc-windows-gnu`)
#   test             the workspace tests, every failure listed, then every test skipped (a test that
#                    cannot run on this machine records why in $OPENLAB_TEST_SKIPS; without that
#                    variable it fails instead) and what the machine reported, passing or not
#   test-release     the same, optimised (debug-only checks off), in two runs: everything but the video
#                    tests that crash or hang a stage host's decoder (video_cues' stage_host_faults
#                    module), then those alone. On the hosted arm64 VM, in release, every movie probe and
#                    open after them waited about 30 s, which pushed the suite past its time limit (#751)
#   fuzz-lint        fmt and clippy for engine/fuzz, which is outside the workspace
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../engine"

lints() { cargo clippy --workspace "$@" --locked -- -D warnings; }

# The engine and stage host binaries as shipped (`cargo build --release --bin openlab-engine` and
# `--bin openlab-stage`: no dev-dependencies) must not enable a test-only feature on any crate, for any
# target. The tree is inverted on each workspace crate (-i): a plain tree does not list the root package's
# own features, so a `default = ["test-support"]` in a package's Cargo.toml would slip through.
shipped_features() {
  local tree hits
  tree=$(cargo tree --locked --target all -p openlab-engine -e normal,build,features --prefix none \
    -i openlab-engine -i openlab-audio -i openlab-core)
  tree+=$'\n'$(cargo tree --locked --target all -p openlab-video -e normal,build,features --prefix none \
    -i openlab-video -i openlab-core)
  hits=$(printf '%s\n' "$tree" | grep -E 'feature "(test-support|fault-injection)"' | sort -u || true)
  if [ -n "$hits" ]; then
    echo "test-only features in the shipped engine or stage host build:" >&2
    echo "$hits" >&2
    return 1
  fi
}

# Runs the workspace tests with skips recorded, then prints them (and the decoders the video tests
# found) as one block, also when the tests pass, and exits with the tests' status.
# With `split` first, the video tests that crash or hang a stage host's decoder run after the rest, picked
# by their module's path (video_cues.rs refuses those switches in a test outside that module).
FAULTS='stage_host_faults::'
tests() {
  local skips status=0 split=0
  if [ "${1:-}" = split ]; then
    split=1
    shift
  fi
  mkdir -p target/tmp
  skips="$PWD/target/tmp/openlab-test-skips.txt"
  : > "$skips"
  if [ "$split" = 1 ]; then
    OPENLAB_TEST_SKIPS="$skips" cargo test "$@" --workspace --locked --no-fail-fast -- --skip "$FAULTS" ||
      status=$?
    # --workspace again, so the first run's build is reused (one crate alone unifies other features
    # and rebuilds); only video_cues has tests on that path.
    echo "== the video tests that crash or hang a stage host's decoder:"
    OPENLAB_TEST_SKIPS="$skips" cargo test "$@" --workspace --locked --no-fail-fast -- "$FAULTS" ||
      status=$?
  else
    OPENLAB_TEST_SKIPS="$skips" cargo test "$@" --workspace --locked --no-fail-fast || status=$?
  fi
  echo "== machine, as the tests found it:"
  # Each test process of a split run reports the decoders it found: list each once.
  grep '^decoder ' "$skips" | sort -u | sed 's/^/   /' || echo "   (nothing reported)"
  echo "== SKIPPED $(grep -c '^skip ' "$skips" || true) test(s):"
  grep '^skip ' "$skips" | sed 's/^skip /   /' || true
  return "$status"
}

case "${1:-}" in
  fmt) cargo fmt --all --check ;;
  clippy)
    lints --all-targets
    lints --lib --bins
    shipped_features
    ;;
  clippy-windows)
    lints --target x86_64-pc-windows-gnu --all-targets
    lints --target x86_64-pc-windows-gnu --lib --bins
    ;;
  test) tests ;;
  fuzz-lint)
    # The fuzz crate is outside the workspace, so the modes above never see it.
    cd fuzz
    cargo fmt --check
    cargo clippy --locked -- -D warnings
    ;;
  test-release) tests split --release ;;
  *)
    echo "usage: scripts/rust-checks.sh fmt|clippy|clippy-windows|fuzz-lint|test|test-release" >&2
    exit 2
    ;;
esac
