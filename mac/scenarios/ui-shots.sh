#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# `make ui-shots`: runs one scenario (openLabTests/ScenarioShotTests/testScenario) and leaves its
# shots and steps.log in OUT. Builds for testing only when the app, its tests, the test shows or
# the engine changed since the last build, so a re-run of an edited scenario starts at once.
#
#   mac/scenarios/ui-shots.sh SCENARIO.json OUT_DIR
set -uo pipefail

[ $# -eq 2 ] || { echo "usage: $0 SCENARIO.json OUT_DIR" >&2; exit 2; }
[ -f "$1" ] || { echo "ui-shots: no scenario file at $1" >&2; exit 2; }
scenario="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
mkdir -p "$2" || exit 2
out="$(cd "$2" && pwd)"
rm -f "$out"/*.png "$out/steps.log"

xcodebuild=(xcodebuild -project mac/openLab.xcodeproj -scheme openLab -derivedDataPath build)
destination="platform=macOS,arch=$(uname -m)"
testrun() { ls -t build/Build/Products/*.xctestrun 2>/dev/null | head -1; }

run="$(testrun)"
if [ -z "$run" ] || [ -n "$(find mac/openLab mac/openLabTests mac/TestShows mac/project.yml engine/crates \
  engine/Cargo.toml engine/Cargo.lock -newer "$run" -type f -print -quit)" ]; then
  echo "ui-shots: building for testing (sources changed since the last build)"
  (cd mac && xcodegen --quiet) || exit 1
  "${xcodebuild[@]}" -destination "$destination" -quiet build-for-testing || exit 1
  run="$(testrun)"
  # A build with nothing to do leaves the file's time as it was: mark the sources as built.
  touch "$run"
fi

TEST_RUNNER_OPENLAB_SCENARIO="$scenario" TEST_RUNNER_OPENLAB_SHOTS_OUT="$out" \
  xcodebuild test-without-building -xctestrun "$run" -destination "$destination" -quiet \
  -test-timeouts-enabled YES -default-test-execution-time-allowance 180 \
  -only-testing:openLabTests/ScenarioShotTests/testScenario
status=$?

if [ -f "$out/steps.log" ]; then
  echo "--- $out/steps.log"
  cat "$out/steps.log"
else
  echo "ui-shots: the run wrote no steps.log (it did not start; see the xcodebuild output above)"
  [ $status -ne 0 ] || status=1
fi
exit $status
