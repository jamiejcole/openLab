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
#   test             the workspace tests, every failure listed
#   test-release     the same, optimised (debug-only checks off)
#   fuzz-lint        fmt and clippy for engine/fuzz, which is outside the workspace
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../engine"

lints() { cargo clippy --workspace "$@" --locked -- -D warnings; }

# The engine binary as shipped (`cargo build --release --bin openlab-engine`: no dev-dependencies) must not
# enable a test-only feature on any crate, for any target.
shipped_features() {
  local tree hits
  tree=$(cargo tree --locked --target all -p openlab-engine -e normal,build,features --prefix none)
  hits=$(printf '%s\n' "$tree" | grep -E 'feature "(test-support|fault-injection)"' | sort -u || true)
  if [ -n "$hits" ]; then
    echo "test-only features in the shipped engine build:" >&2
    echo "$hits" >&2
    return 1
  fi
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
  test) cargo test --workspace --locked --no-fail-fast ;;
  fuzz-lint)
    # The fuzz crate is outside the workspace, so the modes above never see it.
    cd fuzz
    cargo fmt --check
    cargo clippy --locked -- -D warnings
    ;;
  test-release) cargo test --release --workspace --locked --no-fail-fast ;;
  *)
    echo "usage: scripts/rust-checks.sh fmt|clippy|clippy-windows|fuzz-lint|test|test-release" >&2
    exit 2
    ;;
esac
