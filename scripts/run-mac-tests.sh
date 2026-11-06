#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# The app test runs that have a hang limit. On a hosted macOS VM the paravirtual GPU driver can stall
# with WindowServer inside it; every window call on the machine then blocks, so whichever test makes a
# window next hits the limit, with no fault of its own (a later process on the same runner is fine).
# When the spindump XCTest took for a timed-out test proves exactly that, and every failure of the run
# is such a timeout, those tests run once more; they must pass. Any other failure fails the step at once.
#
#   scripts/run-mac-tests.sh XCODEBUILD... -- [SELECTION...]    run `XCODEBUILD... SELECTION... test`
#   scripts/run-mac-tests.sh --classify SUMMARY ATTACHMENTS     print -only-testing arguments and exit 0
#                                                               if the run was only wedged, else exit 1
# SUMMARY: `xcresulttool get test-results summary`; ATTACHMENTS: `xcresulttool export attachments`.
set -uo pipefail

# A report whose every sample has WindowServer's main thread in the paravirtual GPU driver and the
# tested process waiting on WindowServer. A moment in the driver in passing is not enough.
wedged() {
  awk '
    /^Command:/ && target == "" { target = $2 }
    /^Process:/ { process = $2; samples = "" }
    /^Num samples:/ { samples = $3 }
    /^  Thread 0x/ { thread = $0 }
    samples != "" {
      count = $1
      sub(/^\*/, "", count)
      if (count == samples) {
        if (process == "WindowServer" && thread ~ /ws_main_thread/ && /AppleParavirt/) driver = 1
        if (process == target && /blocked by turnstile waiting for WindowServer/) waiting = 1
      }
    }
    END { exit !(driver && waiting) }
  ' "$1"
}

classify() {
  local summary=$1 dir=$2 failure target test report proved args=()
  [ "$(jq -r '.testFailures | length' "$summary")" -gt 0 ] || return 1
  while IFS=$'\t' read -r failure target test; do
    case "$failure" in *"exceeded execution time allowance"*) ;; *) return 1 ;; esac
    proved=
    while IFS= read -r report; do
      if [ -n "$report" ] && wedged "$dir/$report"; then proved=1; fi
    done < <(jq -r --arg t "$test" '.[] | select(.testIdentifier == $t) | .attachments[]
      | select(.isAssociatedWithFailure) | select(.suggestedHumanReadableName | test("Spindump"))
      | .exportedFileName' "$dir/manifest.json")
    [ -n "$proved" ] || return 1
    args+=("-only-testing:$target/${test%()}")
  done < <(jq -r '.testFailures[] | [.failureText, .targetName, .testIdentifierString] | @tsv' "$summary")
  printf '%s\n' "${args[@]}" | sort -u
}

if [ "${1:-}" = "--classify" ]; then
  classify "$2" "$3"
  exit $?
fi

base=()
while [ $# -gt 0 ] && [ "$1" != "--" ]; do
  base+=("$1")
  shift
done
[ $# -gt 0 ] && shift

# Left in the temporary folder: what it exports after a failure is worth reading.
work=$(mktemp -d)
touch "$work/start"
"${base[@]}" "$@" test
rc=$?
[ "$rc" = 0 ] && exit 0

# Only this run's bundle: an older one must not excuse a run that left none.
bundle=$(find build/Logs/Test -maxdepth 1 -name '*.xcresult' -newer "$work/start" 2>/dev/null | sort | tail -n 1)
[ -n "$bundle" ] || exit "$rc"
xcrun xcresulttool get test-results summary --path "$bundle" >"$work/summary.json" || exit "$rc"
xcrun xcresulttool export attachments --only-failures --path "$bundle" --output-path "$work/att" >/dev/null || exit "$rc"
retry=$(classify "$work/summary.json" "$work/att") || exit "$rc"

echo "::warning title=WindowServer wedged on the runner::the GPU driver stalled, so these tests hit their hang limit and run again: $(echo "$retry" | tr '\n' ' ')"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "### App tests: WindowServer wedged on the runner, retried"
    echo "$retry"
  } >>"$GITHUB_STEP_SUMMARY"
fi
# shellcheck disable=SC2086 # one argument per line, none with a space (test names)
"${base[@]}" $retry test
