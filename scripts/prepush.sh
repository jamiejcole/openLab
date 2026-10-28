#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Pre-push gate: runs the checks the diff against origin/main needs, stops at the first failure.
# PREPUSH_UI=1 forces the mac UI tests, PREPUSH_UI=0 skips them.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
LOG="${TMPDIR:-/tmp}/openlab-prepush-$(date +%Y%m%d-%H%M%S)-$$.log"
: >"$LOG"

changed="$( { git diff --name-only origin/main...HEAD; git diff --name-only HEAD; git ls-files --others --exclude-standard; } | sort -u)"

want_mac_unit=0
want_ui=0
if printf '%s\n' "$changed" | grep -Eq '^(mac/|engine/fixtures/protocol/|engine/crates/core/src/protocol/|engine/crates/core/src/show_file\.rs$|docs/file-format\.md$)'; then
  want_mac_unit=1
fi
if printf '%s\n' "$changed" | grep -E '^mac/openLab/.*\.swift$' | grep -vq '^mac/openLab/Engine/'; then
  want_ui=1
fi
case "${PREPUSH_UI:-}" in
  1) want_ui=1 ;;
  0) want_ui=0 ;;
esac

current="(start)"
trap 'rc=$?; if [ $rc -ne 0 ]; then echo "PREPUSH FAIL: $current (log: $LOG)"; fi' EXIT

step() {
  current="$1"
  shift
  printf '%s ... ' "$current"
  echo "=== $current: $*" >>"$LOG"
  if "$@" >>"$LOG" 2>&1; then
    echo ok
  else
    echo FAILED
    exit 1
  fi
}

clippy_windows() { (cd engine && cargo clippy --target x86_64-pc-windows-gnu --workspace --all-targets -- -D warnings); }
mac_unit() {
  make project
  xcodebuild -project mac/openLab.xcodeproj -scheme openLab -derivedDataPath build \
    -destination "platform=macOS,arch=$(uname -m)" -skip-testing:openLabUITests test
}

step "lint" make lint
step "clippy-windows" clippy_windows
step "test-rust" make test-rust
step "mac-build" make mac
if [ "$want_ui" = 1 ]; then
  step "test-mac-ui" make test-mac
elif [ "$want_mac_unit" = 1 ]; then
  step "test-mac-unit" mac_unit
fi

trap - EXIT
echo "PREPUSH PASS (log: $LOG)"
