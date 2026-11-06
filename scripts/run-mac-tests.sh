#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# The app test runs that have a hang limit. On a hosted macOS VM the paravirtual GPU driver can stall
# with WindowServer inside it; every window call on the machine then blocks, so whichever test makes a
# window next hits the limit. What stalls the driver is not known; a later process on the same runner
# has been fine. When the spindump XCTest took for a timed-out test proves exactly that, and every failure of the run
# is such a timeout, those tests run once more; they must pass. Any other failure fails the step at once.
#
#   scripts/run-mac-tests.sh XCODEBUILD... -- [SELECTION...]    run `XCODEBUILD... SELECTION... test`
#   scripts/run-mac-tests.sh --classify SUMMARY ATTACHMENTS     print -only-testing arguments and exit 0
#                                                               if the run was only wedged, else exit 1
# SUMMARY: `xcresulttool get test-results summary`; ATTACHMENTS: `xcresulttool export attachments`.
set -uo pipefail

# A report whose every sample has WindowServer's main thread in the paravirtual GPU driver and the
# tested process's main thread waiting on WindowServer. A moment in the driver in passing is not
# enough, and nor is a background thread: those wait on a stalled WindowServer too, so one waiting
# says nothing about what stopped the main thread (it may be a deadlock of our own). The wait may be
# split over sample ranges (353, 1, 142), so the kernel frames (`*`) carrying the wait add up, one thread
# tree at a time: a report can hold a second tree of the same thread (a Swift task's), which must not
# count twice. The tested process is the one with the header's PID: another may share its name.
# The report may not name the wait: a message to WindowServer can show only as `mach_msg2_trap`, so that
# counts when, for all samples, the caller of `mach_msg` is SkyLight or QuartzCore itself. Our own code
# or another framework in between could be waiting on anything.
wedged() {
  awk '
    function end_thread() {
      if (waited > 0 && waited == samples) waiting = 1
      waited = 0
      caller = 0
    }
    /^PID:/ && pid == "" { pid = $2 }
    /^Process:/ { end_thread(); tested = index($0, "[" pid "]") > 0; process = $2; samples = "" }
    /^Num samples:/ { samples = $3 }
    /^  Thread 0x/ { end_thread(); thread = $0 }
    samples != "" {
      count = $1
      in_kernel = sub(/^\*/, "", count)
      if (count == samples && process == "WindowServer" && thread ~ /ws_main_thread/ && /AppleParavirt/) driver = 1
      if (in_kernel && tested && thread ~ /com\.apple\.main-thread/ && /blocked by turnstile waiting for WindowServer/) waited += count
      if (tested && thread ~ /com\.apple\.main-thread/ && count == samples) {
        if (/mach_msg2_trap/) { if (caller) waiting = 1 }
        else if (!/\(libsystem_kernel\.dylib \+/) caller = /\((SkyLight|QuartzCore) \+/
      }
    }
    END { end_thread(); exit !(driver && waiting) }
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

# The result bundles there now.
bundles() { find build/Logs/Test -maxdepth 1 -name '*.xcresult' 2>/dev/null | sort; }

# Left in the temporary folder: what it exports after a failure is worth reading.
work=$(mktemp -d)
bundles >"$work/before"
"${base[@]}" "$@" test
rc=$?
[ "$rc" = 0 ] && exit 0

# Only this run's bundle: an older one must not excuse a run that left none.
bundle=$(bundles | comm -13 "$work/before" - | tail -n 1)
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
bundles >"$work/before-retry"
# shellcheck disable=SC2086 # one argument per line, none with a space (test names)
"${base[@]}" $retry test
retry_rc=$?
[ "$retry_rc" = 0 ] || exit "$retry_rc"

# xcodebuild succeeds on a selection that runs no test: the retried tests must really have passed.
bundle=$(bundles | comm -13 "$work/before-retry" - | tail -n 1)
want=$(echo "$retry" | wc -l | tr -d ' ')
passed=
if [ -n "$bundle" ]; then
  passed=$(xcrun xcresulttool get test-results summary --path "$bundle" | jq -r '.passedTests')
fi
if ! [ "$passed" -ge "$want" ] 2>/dev/null; then
  echo "run-mac-tests: the retry passed ${passed:-no} tests of $want"
  exit "$rc"
fi
