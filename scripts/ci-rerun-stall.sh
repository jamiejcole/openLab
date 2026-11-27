#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Re-runs a CI run's failed jobs once when every failure is a proven runner stall, so nobody waits to
# re-run it by hand. A proven stall is a macOS app job that scripts/run-mac-tests.sh ended with exit 75
# (a sample showed WindowServer stuck in the hosted VM's GPU driver, the job told to be re-run), whose
# test step is the only step that failed and whose only error is the step's exit code. Any other failed
# job, a step that ran out of time, a hang not proven to be the runner (exit 76) or a test that failed
# keeps the run red with nothing re-run. The workflow calls it only for a run's first attempt, so a stall
# that comes back on the re-run stays red and is investigated.
#
#   scripts/ci-rerun-stall.sh RUN_ID                          decide from the run's first attempt and
#                                                             re-run its failed jobs (GH_TOKEN, GH_REPO)
#   scripts/ci-rerun-stall.sh --classify JOBS ANNOTATIONS     exit 0 if the run is only stalled
# JOBS: a JSON array of the attempt's jobs (`.jobs[]` of the jobs API, with their steps);
# ANNOTATIONS/<job id>.json: a JSON array of that job's check-run annotations.
set -euo pipefail

STALL_TITLE='WindowServer wedged on the runner'
STALL_TEXT='re-run the job'

# Prints why the run is not only stalled and exits 1, or exits 0.
classify() {
  local jobs=$1 ann=$2 failed id name why
  # "CI Result" fails because another job did; it is re-run with them.
  failed=$(jq -r '.[] | select(.name != "CI Result")
    | select((.conclusion // "unfinished") as $c | ["success", "skipped", "neutral"] | index($c) | not)
    | .id' "$jobs")
  if [ -z "$failed" ]; then
    echo "no failed job but the result"
    return 1
  fi
  for id in $failed; do
    name=$(jq -r --argjson id "$id" '.[] | select(.id == $id) | .name' "$jobs")
    why=$(jq -r --argjson id "$id" '.[] | select(.id == $id)
      | if .conclusion != "failure" then "ended \(.conclusion)"
        elif (.name | startswith("macOS app") | not) then "not an app test job"
        elif ([.steps[]? | select(.conclusion == "failure")] | length) != 1 then "more or fewer than one failed step"
        elif ([.steps[]? | select(.conclusion == "failure") | .name | startswith("Run make test-mac")] | all | not)
          then "the failed step is not the app tests"
        else empty end' "$jobs")
    if [ -z "$why" ]; then
      if [ ! -f "$ann/$id.json" ]; then
        why="no annotations"
      else
        why=$(jq -r --arg title "$STALL_TITLE" --arg text "$STALL_TEXT" '
          if ([.[] | select(.title == $title and (.message | contains($text)))] | length) == 0
            then "no proven stall"
          elif ([.[] | select(.annotation_level == "failure"
              and (.message | startswith("Process completed with exit code") | not))] | length) > 0
            then "another error: \([.[] | select(.annotation_level == "failure")][0].message | .[0:120])"
          else empty end' "$ann/$id.json")
      fi
    fi
    if [ -n "$why" ]; then
      echo "$name: $why"
      return 1
    fi
  done
  return 0
}

if [ "${1:-}" = --classify ]; then
  classify "$2" "$3"
  exit
fi

run=${1:?usage: scripts/ci-rerun-stall.sh RUN_ID | --classify JOBS ANNOTATIONS}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/ann"
gh api --paginate "repos/$GH_REPO/actions/runs/$run/attempts/1/jobs?per_page=100" --jq '.jobs[]' |
  jq -s . >"$work/jobs.json"
for id in $(jq -r '.[] | select(.conclusion == "failure") | .id' "$work/jobs.json"); do
  gh api --paginate "repos/$GH_REPO/check-runs/$id/annotations?per_page=100" --jq '.[]' |
    jq -s . >"$work/ann/$id.json"
done
if why=$(classify "$work/jobs.json" "$work/ann"); then
  gh run rerun "$run" --failed
  echo "Every failure of run $run is a proven runner stall (exit 75): its failed jobs were re-run once." |
    tee -a "${GITHUB_STEP_SUMMARY:-/dev/null}"
else
  echo "Run $run is not re-run: $why" | tee -a "${GITHUB_STEP_SUMMARY:-/dev/null}"
fi
