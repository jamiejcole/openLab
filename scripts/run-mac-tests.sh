#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# The app test runs that have a hang limit. On a hosted macOS VM the paravirtual GPU driver can stall
# with WindowServer inside it; every window call on the machine then blocks, so whichever test makes a
# window next hits the limit. What stalls the driver is not known; a later process on the same runner
# has been fine, but the stall can outlast a restart. When the spindump XCTest took for a timed-out test proves exactly that, and every failure of the run
# is such a timeout, those tests run once more; they must pass. Any other failure fails the step at once.
# One more failure is part of a wedge: after a timeout xcodebuild starts the app again, and an app started
# into the same stalled WindowServer never connects ("The test runner hung before establishing
# connection"), which ends the run with the tests after the timeout unrun. With a spindump of its own that
# proves it the same way, the run is not retried (the stall has outlasted the restart, so another start
# would most likely hang to the step's limit): it fails, saying the runner stalled and the job needs a re-run.
#
#   scripts/run-mac-tests.sh XCODEBUILD... -- [SELECTION...]    run `XCODEBUILD... SELECTION... test`
#   scripts/run-mac-tests.sh --classify SUMMARY ATTACHMENTS     print -only-testing arguments and exit 0
#                                                               if the run was only wedged, else exit 1
#                                                               (`--cut-short` when the wedge also ended the run: no retry)
# SUMMARY: `xcresulttool get test-results summary`; ATTACHMENTS: `xcresulttool export attachments`.
#
# Exit codes: xcodebuild's own, except 75 (CUT_SHORT_RC): the run was cut short by a proven stall and the
# job needs a re-run. `make test-mac` starts no second process on seeing it (the runner is wedged).
# And 76 (HUNG_RC): a test hung (it passed its time allowance, the app never connected, or the run went
# silent) and no proven stall with a passing retry excuses it. Whatever the cause, the next process on
# this machine would most likely hang too, to the step's limit: `make test-mac` starts none, so a hang
# costs minutes, not the rest of the step.
#
# A run that prints nothing for SILENCE_LIMIT seconds once its tests have started is stopped: a test
# prints as it starts and ends and a hung one is ended by its own allowance, so that long a silence means
# the app is not coming up.
set -uo pipefail

CUT_SHORT_RC=75
HUNG_RC=76
SILENCE_LIMIT=${OPENLAB_TEST_SILENCE_LIMIT:-300}

# A report whose every sample has WindowServer's main thread in the paravirtual GPU driver and the
# tested process's main thread waiting on WindowServer. A moment in the driver in passing is not
# enough, and nor is a background thread: those wait on a stalled WindowServer too, so one waiting
# says nothing about what stopped the main thread (it may be a deadlock of our own). The wait may be
# split over sample ranges (353, 1, 142), so the kernel frames (`*`) carrying the wait add up, one thread
# tree at a time: a report can hold a second tree of the same thread (a Swift task's), which must not
# count twice. The tested process is the one with the header's PID: another may share its name.
# The report may not name the wait: a message to WindowServer can show only as `mach_msg2_trap`, so that
# counts when, for all samples, the caller of `mach_msg` is SkyLight or QuartzCore itself. Our own code
# or another framework in between could be waiting on anything, and a kernel frame under the trap that
# names another holder, or a suspended thread, says what it is really waiting on; a second tree of the
# thread does not undo that.
wedged() {
  awk '
    function end_thread() {
      if (waited > 0 && waited == samples) waiting = 1
      if (trapped && !other_holder) waiting = 1
      waited = 0
      caller = 0
      trapped = 0
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
      if (trapped && in_kernel && /\(blocked by|\(suspended\)/ && !/waiting for WindowServer \[/) other_holder = 1
      if (tested && thread ~ /com\.apple\.main-thread/ && count == samples) {
        if (/mach_msg2_trap/) { if (caller) trapped = 1 }
        else if (!/\(libsystem_kernel\.dylib \+/) caller = /\((SkyLight|QuartzCore) \+/
      }
    }
    END { end_thread(); exit !(driver && waiting) }
  ' "$1"
}

classify() {
  local summary=$1 dir=$2 failure target test report proved args=() cut_short=
  [ "$(jq -r '.testFailures | length' "$summary")" -gt 0 ] || return 1
  while IFS=$'\t' read -r failure target test; do
    case "$failure" in
      *"exceeded execution time allowance"*) ;;
      *"test runner hung before establishing connection"*) cut_short=1 ;;
      *) return 1 ;;
    esac
    proved=
    while IFS= read -r report; do
      if [ -n "$report" ] && wedged "$dir/$report"; then proved=1; fi
    done < <(jq -r --arg t "$test" '.[] | select(.testIdentifier == $t) | .attachments[]
      | select(.isAssociatedWithFailure) | select(.suggestedHumanReadableName | test("Spindump"))
      | .exportedFileName' "$dir/manifest.json")
    [ -n "$proved" ] || return 1
    args+=("-only-testing:$target/${test%()}")
  done < <(jq -r '.testFailures[] | [.failureText, .targetName, .testIdentifierString] | @tsv' "$summary")
  if [ -n "$cut_short" ]; then
    echo "--cut-short"
    return 0
  fi
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

# Runs the command with its output also kept in a file, and stops it after SILENCE_LIMIT seconds without
# output once the tests have started (the build before them can be quiet for longer).
run_watched() {
  local log="$work/out.$1" pid dog rc
  shift
  : >"$log"
  "$@" > >(tee -a "$log") 2>&1 &
  pid=$!
  (
    while kill -0 "$pid" 2>/dev/null; do
      sleep 5
      grep -q -e 'Testing started' -e '^Test Suite' "$log" 2>/dev/null || continue
      # BSD stat, then GNU stat (the repo checks run this on Linux).
      changed=$(stat -f %m "$log" 2>/dev/null) || changed=$(stat -c %Y "$log" 2>/dev/null) || continue
      if [ $(($(date +%s) - changed)) -ge "$SILENCE_LIMIT" ]; then
        echo "run-mac-tests: no output for ${SILENCE_LIMIT}s after the tests started: stopping the run" | tee -a "$log"
        : >"$work/silenced"
        # Interrupt first: xcodebuild then closes its result bundle, which holds the hang reports of
        # the tests that timed out before the silence. Killed outright, the bundle is left unreadable.
        kill -INT "$pid" 2>/dev/null
        for _ in $(seq 1 45); do
          kill -0 "$pid" 2>/dev/null || break
          sleep 1
        done
        kill "$pid" 2>/dev/null
        break
      fi
    done
  ) &
  dog=$!
  wait "$pid"
  rc=$?
  kill "$dog" 2>/dev/null
  wait "$dog" 2>/dev/null
  return "$rc"
}

# Whether the window server is stuck in the hosted Mac's virtual GPU driver right now: a sample of it
# (needs password-less sudo, which the hosted runners have) whose main thread sits in the driver. That
# is the runner's fault, not a test's: nothing drawn by any process gets through until the VM is gone.
# The sample's main thread is printed, so the job's log holds the evidence.
windowserver_stalled() {
  local f="$work/windowserver-sample.txt"
  sudo -n sample WindowServer 2 -file "$f" >/dev/null 2>&1 || return 1
  awk '
    /^ +[0-9]+ Thread_/ { main = /ws_main_thread|com\.apple\.main-thread/ }
    main { if (lines++ < 60) print > "/dev/stderr" }
    main && /AppleParavirt/ { found = 1 }
    END { exit !found }
  ' "$f"
}

# What the machine's display and power state is, for the job's log: it is not known what stalls the
# GPU driver on a hosted Mac, and each stall so far has left no record of whether the display had gone
# to sleep, a screen saver had started or the machine had sat without input. Printed before the tests
# and again when a run goes silent, so the two can be compared. Never fails the run.
machine_state() {
  {
    echo "run-mac-tests: machine state ($1):"
    echo "  uptime: $(uptime 2>/dev/null)"
    echo "  seconds since last input: $(ioreg -c IOHIDSystem 2>/dev/null | awk '/HIDIdleTime/ { print int($NF / 1000000000); exit }')"
    echo "  screen saver starts after: $(defaults -currentHost read com.apple.screensaver idleTime 2>/dev/null || echo unset) s"
    pmset -g 2>/dev/null | grep -E 'displaysleep|^ sleep|powernap|hibernatemode' | sed 's/^/  pmset:/'
    pmset -g assertions 2>/dev/null | grep -E 'PreventUserIdleDisplaySleep|PreventUserIdleSystemSleep|UserIsActive' | head -6 | sed 's/^/  assertion:/'
    pmset -g log 2>/dev/null | grep -E 'Display is turned|Entering Sleep|Wake from|DarkWake' | tail -6 | sed 's/^/  power log: /'
  } >&2 || true
}

# Ends a failed run: HUNG_RC when a test of it hung, else the given code.
hung=
give_up() {
  [ -z "$hung" ] || exit "$HUNG_RC"
  exit "$1"
}

# On a hosted runner nobody touches the machine, so its display may sleep or a screen saver may start
# part-way through the tests. Whether that is what stalls the driver is not known: holding the display
# awake for the length of the run rules it in or out (the state printed below says if it took).
if [ -n "${CI:-}" ] && command -v caffeinate >/dev/null 2>&1; then
  caffeinate -dimsu -w $$ &
fi
machine_state "before the tests"

bundles >"$work/before"
run_watched first "${base[@]}" "$@" test
rc=$?
[ "$rc" = 0 ] && exit 0
machine_state "after a failed run"
if [ -e "$work/silenced" ]; then
  if windowserver_stalled; then
    echo "::warning title=WindowServer wedged on the runner::the run went silent and a sample shows the window server stuck in the GPU driver (above); re-run the job"
    exit "$CUT_SHORT_RC"
  fi
  echo "::warning title=App tests went silent::the run printed nothing for ${SILENCE_LIMIT}s and was stopped, and the window server is not shown stuck in the GPU driver: find out why before re-running"
  exit "$HUNG_RC"
fi

# Only this run's bundle: an older one must not excuse a run that left none.
bundle=$(bundles | comm -13 "$work/before" - | tail -n 1)
[ -n "$bundle" ] || exit "$rc"
xcrun xcresulttool get test-results summary --path "$bundle" >"$work/summary.json" || exit "$rc"
if jq -e '[.testFailures[]?.failureText
    | select(test("exceeded execution time allowance|hung before establishing connection"))] | length > 0' \
  "$work/summary.json" >/dev/null 2>&1; then
  hung=1
fi
xcrun xcresulttool export attachments --only-failures --path "$bundle" --output-path "$work/att" >/dev/null || give_up "$rc"
retry=$(classify "$work/summary.json" "$work/att") || give_up "$rc"

if [ "$retry" = "--cut-short" ]; then
  echo "::warning title=WindowServer wedged on the runner::the GPU driver stalled and the app could not start again, so the tests after the hang never ran; re-run the job"
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    {
      echo "### App tests: WindowServer wedged on the runner, run cut short"
      echo "The app could not start again after a test hung, so the tests after it never ran. Re-run the job."
    } >>"$GITHUB_STEP_SUMMARY"
  fi
  exit "$CUT_SHORT_RC"
fi
echo "::warning title=WindowServer wedged on the runner::the GPU driver stalled, so these tests hit their hang limit and run again: $(echo "$retry" | tr '\n' ' ')"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "### App tests: WindowServer wedged on the runner, retried"
    echo "$retry"
  } >>"$GITHUB_STEP_SUMMARY"
fi
bundles >"$work/before-retry"
# shellcheck disable=SC2086 # one argument per line, none with a space (test names)
run_watched retry "${base[@]}" $retry test
retry_rc=$?
[ ! -e "$work/silenced" ] || exit "$HUNG_RC"
[ "$retry_rc" = 0 ] || give_up "$retry_rc"

# xcodebuild succeeds on a selection that runs no test: the retried tests must really have passed.
bundle=$(bundles | comm -13 "$work/before-retry" - | tail -n 1)
want=$(echo "$retry" | wc -l | tr -d ' ')
passed=
if [ -n "$bundle" ]; then
  passed=$(xcrun xcresulttool get test-results summary --path "$bundle" | jq -r '.passedTests')
fi
if ! [ "$passed" -ge "$want" ] 2>/dev/null; then
  echo "run-mac-tests: the retry passed ${passed:-no} tests of $want"
  give_up "$rc"
fi
