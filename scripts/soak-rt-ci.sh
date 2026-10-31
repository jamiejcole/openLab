#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# CI's real-time soak step. The tests run with OPENLAB_SOAK_RT_REQUIRE=1, so a host that cannot keep
# real time makes them FAIL rather than return early as a pass. Hosted macOS VMs are such hosts (see
# soak_rt.rs), so this wrapper turns exactly that refusal, and nothing else, into a visible SKIPPED:
# a GitHub warning annotation and a job-summary line. Any other failure still fails the step.
#
#   scripts/soak-rt-ci.sh                run the tests (needs a release-capable toolchain)
#   scripts/soak-rt-ci.sh --classify LOG RC   print and exit by what the log of a run with exit code RC says
set -uo pipefail

MARK="OPENLAB_SOAK_RT_REQUIRE is set"

# classify <log> <rc>: 0 = the run passed or only the host refused; 1 = a real failure.
classify() {
  local log=$1 rc=$2 failed refused
  if [ "$rc" = 0 ]; then
    echo "soak_rt: ran and passed"
    return 0
  fi
  failed=$(sed -n 's/^test result: FAILED\. .* \([0-9][0-9]*\) failed;.*/\1/p' "$log" | paste -sd+ - | bc 2>/dev/null); failed=${failed:-0}
  refused=$(grep -c "$MARK" "$log")
  if [ "$failed" -gt 0 ] && [ "$failed" = "$refused" ]; then
    echo "::warning title=soak_rt SKIPPED::this runner cannot keep real time, so the real-time soak did not run (the host measurement is in the log)"
    if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
      echo "### Real-time soak: SKIPPED, not passed" >>"$GITHUB_STEP_SUMMARY"
      echo "The runner's timers are too coarse for the stand-in device; $failed test(s) refused to run. Run it on a machine that keeps real time." >>"$GITHUB_STEP_SUMMARY"
    fi
    echo "soak_rt: SKIPPED ($failed test(s) refused: host cannot keep real time)"
    return 0
  fi
  echo "soak_rt: FAILED (exit code $rc; $failed failed test(s), $refused of them host refusals)"
  return 1
}

if [ "${1:-}" = "--classify" ]; then
  classify "$2" "$3"
  exit $?
fi

cd "$(dirname "${BASH_SOURCE[0]}")/../engine" || exit 2
log=$(mktemp)
OPENLAB_SOAK_RT_REQUIRE=1 OPENLAB_SOAK_RT_SECS="${OPENLAB_SOAK_RT_SECS:-180}" \
  cargo test --release --locked -p openlab-engine --test soak_rt -- --ignored --nocapture 2>&1 | tee "$log"
rc=${PIPESTATUS[0]}
classify "$log" "$rc"
