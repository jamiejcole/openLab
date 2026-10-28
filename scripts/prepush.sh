#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Pre-push gate. Lint (including scripts/check-repo.sh) always runs; the rest is chosen from the diff
# against origin/main (committed, staged, unstaged and untracked files) with the same path sets CI uses
# (scripts/paths.sh):
#   engine/ changed or CI/Makefile changed -> Windows clippy and the Rust tests
#   app code changed                       -> the app's UI tests (else its unit tests when the
#                                             protocol or show file changed)
#   what ci.yml's `bundle` output covers   -> the Release `make mac` universal build
# It stops at the first failure and, on a clean tree, saves `PREPUSH PASS <HEAD sha>` to
# `$(git rev-parse --git-dir)/openlab-prepush`.
#
#   scripts/prepush.sh                run the gate
#   scripts/prepush.sh --check [sha]  exit 0 only if the saved stamp is for `sha` (default HEAD) and,
#                                     for HEAD, the tree is clean (the pre-push hook uses this)
# PREPUSH_UI=1 forces the app UI tests, PREPUSH_UI=0 skips them.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
STAMP="$(git rev-parse --git-dir)/openlab-prepush"
# shellcheck source=scripts/paths.sh
. scripts/paths.sh

tree_clean() { [ -z "$(git status --porcelain)" ]; }

if [ "${1:-}" = "--check" ]; then
  head="$(git rev-parse HEAD)"
  want="${2:-$head}"
  saved="$(sed -n 's/^PREPUSH PASS //p' "$STAMP" 2>/dev/null | head -n 1)"
  if [ "$saved" != "$want" ]; then
    echo "prepush: no PREPUSH PASS for ${want} (saved: ${saved:-none}). Run scripts/prepush.sh (make prepush)."
    exit 1
  fi
  if [ "$want" = "$head" ] && ! tree_clean; then
    echo "prepush: PREPUSH PASS is for HEAD, but the working tree has changes since. Commit them and run scripts/prepush.sh again."
    exit 1
  fi
  echo "prepush: PREPUSH PASS ${want}"
  exit 0
fi

LOG="${TMPDIR:-/tmp}/openlab-prepush-$(date +%Y%m%d-%H%M%S)-$$.log"
: >"$LOG"
start_head="$(git rev-parse HEAD)"
if tree_clean; then start_clean=1; else start_clean=0; fi

if git rev-parse --verify -q origin/main >/dev/null; then
  changed="$( { git diff --name-only origin/main...HEAD; git diff --name-only HEAD; git ls-files --others --exclude-standard; } | sort -u)"
  known_base=1
else
  changed=""
  known_base=0
fi
touched() { [ "$known_base" = 0 ] || printf '%s\n' "$changed" | grep -Eq "$1"; }

# Without a base to diff against, run everything (as CI does).
want_rust=0
want_bundle=0
want_mac_unit=0
want_ui=0
if touched "$PATHS_ALL_RE" || touched "$PATHS_ENGINE_RE"; then want_rust=1; fi
if touched "$PATHS_ALL_RE" || touched "$PATHS_BUNDLE_RE"; then want_bundle=1; fi
if touched "$PATHS_ALL_RE" || touched "$PATHS_MAC_PR_RE"; then want_mac_unit=1; fi
if [ "$known_base" = 0 ] || printf '%s\n' "$changed" | grep -E "$PATHS_UI_RE" | grep -vq "$PATHS_UI_EXCLUDE_RE"; then
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
if [ "$want_rust" = 1 ]; then
  step "clippy-windows" clippy_windows
  step "test-rust" make test-rust
fi
if [ "$want_bundle" = 1 ]; then
  step "mac-build" make mac
fi
if [ "$want_ui" = 1 ]; then
  # XCUITests drive the real screen: two at once (parallel agents) find each other's windows and fail.
  # lockf waits for the other run to finish.
  step "test-mac-ui" lockf -k /tmp/openlab-xcuitest.lock make test-mac
elif [ "$want_mac_unit" = 1 ]; then
  step "test-mac-unit" mac_unit
fi

trap - EXIT
# The stamp vouches for a commit, so write it only if the tree was clean throughout and HEAD did not move.
if [ "$start_clean" = 1 ] && [ "$(git rev-parse HEAD)" = "$start_head" ] && tree_clean; then
  echo "PREPUSH PASS $start_head" | tee "$STAMP"
  echo "(log: $LOG)"
else
  echo "PREPUSH PASS (not stamped: the tree was not clean or HEAD moved; commit and run again) (log: $LOG)"
fi
