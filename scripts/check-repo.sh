#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Repo hygiene checks that need no toolchain; run by `make lint` and the `checks` CI job.
# Runs every check and lists every failure, so one run shows all of them.
#
# Known failures go in scripts/check-repo-known.txt (`check | item | reason`; the reason cites
# an open issue or starts `permanent:`), never into a weaker check; delete the entry in the commit that fixes the finding.
# Behaviour-changing uses of debug_assertions are allowed only for files listed in
# scripts/debug-assertions-allowlist.txt, wall-clock use in engine test code only for files listed in
# scripts/wall-clock-allowlist.txt (both `path | reason`; allowlist_entries says what a reason needs).
set -u

cd "$(git rev-parse --show-toplevel)" || exit 2
KNOWN=scripts/check-repo-known.txt
DEBUG_ALLOW=scripts/debug-assertions-allowlist.txt
CLOCK_ALLOW=scripts/wall-clock-allowlist.txt
failed=0

fail() { echo "FAIL [$1] $2"; failed=1; }

# issue_open <n>: true when GitHub says the issue is open. Needs `gh`; where it cannot answer, locally the
# open-issue sub-check is skipped (one SKIPPED line), in CI it fails. (Plain variables: macOS has bash 3.2.)
GH_SKIPPED=0
ISSUE_CACHE=""
# On a pull request only the issues on allowlist lines the PR adds or changes are asked about (BASE set):
# closing an issue elsewhere must not turn unrelated PRs red. The full sweep (BASE empty) runs on main
# and nightly, where a red run raises the ci-red alert. CHECK_REPO_BASE=<ref> forces the PR mode locally.
BASE="${CHECK_REPO_BASE:-}"
if [ -z "$BASE" ] && [ "${GITHUB_EVENT_NAME:-}" = pull_request ]; then BASE="origin/${GITHUB_BASE_REF:-main}"; fi
CHANGED_ISSUES=""
if [ -n "$BASE" ]; then
  CHANGED_ISSUES=$(git diff "$BASE...HEAD" -- 'scripts/*allowlist*.txt' scripts/check-repo-known.txt |
    grep -E '^\+[^+]' | grep -oE '#[0-9]+' | tr -d '#' | sort -u)
fi
issue_open() {
  local n=$1 state
  if [ -n "$BASE" ] && ! printf '%s\n' "$CHANGED_ISSUES" | grep -qx "$n"; then return 0; fi
  state=$(printf '%s' "$ISSUE_CACHE" | sed -n "s/^$n=//p")
  if [ -z "$state" ]; then
    # Retry once: a transient API error must not read as an answer.
    state=$(gh issue view "$n" --json state --jq .state 2>/dev/null || gh issue view "$n" --json state --jq .state 2>/dev/null) || state=UNKNOWN
    [ -n "$state" ] || state=UNKNOWN
    ISSUE_CACHE="$ISSUE_CACHE
$n=$state"
  fi
  case "$state" in
    OPEN) return 0 ;;
    UNKNOWN)
      if [ -n "${CI:-}${GITHUB_ACTIONS:-}" ]; then
        fail allowlist "cannot read issue #$n with gh in CI (is GH_TOKEN set?)"
      elif [ "$GH_SKIPPED" = 0 ]; then
        GH_SKIPPED=1
        echo "SKIPPED [allowlist] open-issue check: gh cannot read issues here (CI runs it)"
      fi
      return 0
      ;;
    *) return 1 ;;
  esac
}

# allowlist_entries <check> <file>: validates every `path | reason` line of an allowlist.
# The path must be a tracked file. The reason must cite an open issue (`#N`: a debt being fixed) or start
# `permanent:` (a standing exception). Every `quoted fragment` in backticks must still occur in the path,
# and a permanent entry needs at least one, so an entry about code that was deleted or rewritten fails.
allowlist_entries() {
  local check=$1 file=$2 line path reason frag n frags ok
  while IFS= read -r line; do
    case "$line" in '' | '#'*) continue ;; esac
    path=${line%% | *}
    reason=${line#* | }
    if [ "$reason" = "$line" ]; then
      fail "$check" "$file: '$line' is not 'path | reason'"
      continue
    fi
    if ! git ls-files --error-unmatch "$path" >/dev/null 2>&1; then
      fail "$check" "$file: $path is not a tracked file; delete the entry"
      continue
    fi
    ok=0
    n=$(printf '%s' "$reason" | grep -oE '#[0-9]+' | head -n 1 | tr -d '#')
    if [ -n "$n" ]; then
      ok=1
      issue_open "$n" || fail "$check" "$file: $path cites #$n, which is not open; fix it and delete the entry, or cite the issue that tracks it"
    fi
    frags=$(printf '%s' "$reason" | grep -oE '`[^`]+`' | tr -d '`')
    case "$reason" in
      permanent:*)
        ok=1
        [ -n "$frags" ] || fail "$check" "$file: permanent entry for $path quotes no \`code fragment\`"
        ;;
    esac
    [ "$ok" = 1 ] || fail "$check" "$file: entry for $path cites no open issue (#N) and is not 'permanent:'; track it or fix it"
    while IFS= read -r frag; do
      [ -n "$frag" ] || continue
      grep -qF -- "$frag" "$path" || fail "$check" "$file: the code quoted for $path no longer occurs in it: $frag"
    done <<<"$frags"
  done <"$file"
}

# allowlisted <file> <path>: true when the allowlist has an entry for the path.
allowlisted() {
  awk -F' *\\| *' -v p="$2" '$1 == p && $2 != "" { found = 1 } END { exit !found }' "$1" 2>/dev/null
}

# allowlist_paths <file>: the paths the allowlist names.
allowlist_paths() { awk -F' *\\| *' '/^[^#]/ && $1 != "" { print $1 }' "$1"; }

# known <check> <item>: true when the item is listed, with a reason, as a known failure.
known() {
  awk -F' *\\| *' -v c="$1" -v i="$2" '$1 == c && $2 == i && $3 != "" { found = 1 } END { exit !found }' "$KNOWN" 2>/dev/null
}

# known_entries: every known-failure reason cites an open issue (`#N`) or starts `permanent:`.
known_entries() {
  local check item reason n
  while IFS='|' read -r check item reason || [ -n "$check" ]; do
    case "$check" in '' | '#'*) continue ;; esac
    reason=${reason# }
    case "$reason" in
      permanent:*[![:space:]]*) continue ;;
      permanent:*) fail known "$KNOWN: permanent entry '${check% } |${item% }' does not say why"; continue ;;
    esac
    n=$(printf '%s' "$reason" | grep -oE '#[0-9]+' | head -n 1 | tr -d '#')
    if [ -z "$n" ]; then
      fail known "$KNOWN: entry '${check% } |${item% }' cites no open issue (#N) and is not 'permanent:'"
    elif ! issue_open "$n"; then
      fail known "$KNOWN: entry '${check% } |${item% }' cites #$n, which is not open"
    fi
  done <"$KNOWN"
}

# 1. SPDX header on every source file (CLAUDE.md, "Contracts (same commit)").
spdx() {
  local missing
  missing=$(git ls-files -co --exclude-standard '*.rs' '*.swift' '*.sh' '*.yml' '*.strings' 'scripts/hooks/*' Makefile |
    sort -u | xargs grep -L "SPDX-License-Identifier: GPL-3.0-or-later")
  if [ -n "$missing" ]; then
    fail spdx "missing 'SPDX-License-Identifier: GPL-3.0-or-later' in: $(echo $missing)"
  fi
}

# 2. No duplicate keys in the string table: the later one silently wins, so a message is lost.
strings_keys() {
  local f=mac/openLab/en.lproj/Localizable.strings dup key
  dup=$(grep -oE '^"([^"\\]|\\.)*"[[:space:]]*=' "$f" | sed -E 's/[[:space:]]*=$//' | sort | uniq -d)
  [ -z "$dup" ] && return 0
  while IFS= read -r key; do
    if known strings-key "$key"; then continue; fi
    fail strings-key "duplicate key $key in $f (later definition silently wins); remove one"
  done <<<"$dup"
}

# 3. debug_assertions must not change behaviour: a debug-only path means tests exercise code the
# release build never runs. Detectors that only log or count are allowed, per file, with a reason.
debug_assertions() {
  local hits f
  # Any code line naming the cfg (comments excluded): cfg!(), #[cfg()], cfg(not()), cfg_attr, all()/any().
  hits=$(git ls-files -co --exclude-standard 'engine/*.rs' | sort -u |
    xargs grep -lE '^[[:space:]]*[^[:space:]/].*debug_assertions' 2>/dev/null)
  allowlist_entries debug-assertions "$DEBUG_ALLOW"
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    allowlisted "$DEBUG_ALLOW" "$f" && continue
    fail debug-assertions "$f uses debug_assertions and is not in $DEBUG_ALLOW; make debug and release behave the same, or list the file there with a reason (only detectors that log or count qualify)"
  done <<<"$hits"
  for f in $(allowlist_paths "$DEBUG_ALLOW"); do
    printf '%s\n' "$hits" | grep -qxF "$f" || fail debug-assertions "$DEBUG_ALLOW lists $f, which no longer uses debug_assertions; delete the entry"
  done
}

# 4. A contract doc's stated current version equals the code constant.
versions() {
  local code doc n
  code=$(sed -n 's/^pub const FORMAT_VERSION: u32 = \([0-9][0-9]*\);.*/\1/p' engine/crates/core/src/show_file.rs)
  [ -n "$code" ] || fail versions "cannot read FORMAT_VERSION from engine/crates/core/src/show_file.rs"
  doc=$(sed -n 's/^Status: format version \*\*\([0-9][0-9]*\)\*\*.*/\1/p' docs/file-format.md)
  if [ "$doc" != "$code" ]; then
    fail versions "docs/file-format.md says format version '${doc:-?}' on its Status line; FORMAT_VERSION is $code"
  fi

  code=$(sed -n 's/^pub const PROTOCOL_VERSION: u32 = \([0-9][0-9]*\);.*/\1/p' engine/crates/core/src/protocol/mod.rs)
  [ -n "$code" ] || fail versions "cannot read PROTOCOL_VERSION from engine/crates/core/src/protocol/mod.rs"
  # "`PROTOCOL_VERSION` is **N**", "(now **N**" and "`PROTOCOL_VERSION` stays N" all state the current one.
  for n in $(grep -oE 'PROTOCOL_VERSION`? (is|stays) \*{0,2}[0-9]+|PROTOCOL_VERSION`[^.*]*\(now \*\*[0-9]+' docs/protocol.md | grep -oE '[0-9]+$'); do
    if [ "$n" != "$code" ]; then
      fail versions "docs/protocol.md states PROTOCOL_VERSION $n; the constant is $code (past versions: say 'stayed N', not 'stays N')"
    fi
  done
  if ! grep -qE "PROTOCOL_VERSION\`? is \*\*$code\*\*" docs/protocol.md; then
    fail versions "docs/protocol.md has no line saying 'PROTOCOL_VERSION is **$code**'"
  fi
}

# 5. The app's tests run on the real engine (#132): no source, build or CI file refers to the deleted
# in-process fake engine, the UI tests launch the test engine, and no doc says a test runs on a fake one.
real_engine_tests() {
  local f
  for f in $(git ls-files -co --exclude-standard mac scripts Makefile .github | sort -u |
    grep -v -e '^scripts/check-repo' | xargs grep -l -E 'FakeEngine|FakeShows' 2>/dev/null); do
    fail fake-engine "$f refers to FakeEngine, which is deleted: app tests use the real engine (mac/openLabTests/EngineTestSupport.swift)"
  done
  grep -rq -e '"-OpenLabEngine", "test"' mac/openLabUITests ||
    fail fake-engine "mac/openLabUITests does not launch the test engine ('-OpenLabEngine test')"
  for f in $(git ls-files -co --exclude-standard 'docs/*.md' | sort -u | xargs grep -l -i -E 'FakeEngine|fake engine|real headless engine' 2>/dev/null); do
    if known fake-engine "$f"; then continue; fi
    fail fake-engine "$f mentions a fake engine; the app's tests run on the real engine on its virtual device"
  done
}

# 6. One home per rule: each phrase appears in exactly one instruction file.
one_home() {
  local phrase files count
  for phrase in 'Co-authored-by' 'pgrep' 'fuzzer'; do
    files=$(git ls-files -co --exclude-standard CLAUDE.md '.claude/*.md' docs/dev-setup.md | sort -u |
      xargs grep -li -- "$phrase" 2>/dev/null)
    count=$(printf '%s' "$files" | grep -c .)
    [ "$count" = 1 ] && continue
    known one-home "$phrase" && continue
    fail one-home "'$phrase' appears in $count of CLAUDE.md, .claude/**/*.md, docs/dev-setup.md (want exactly 1): $(echo $files); keep the rule in one place and link to it"
  done
}

# 7. Every CI path filter in scripts/paths.sh names something that exists, and every source file is
# covered by a job. A pattern for a file that was split or renamed would silently stop matching, and
# the job it gates would not run for changes to it.
path_filters() {
  local var re item f files
  files=$(git ls-files -co --exclude-standard | sort -u)
  # shellcheck source=scripts/paths.sh
  . scripts/paths.sh
  for var in $(set | sed -n 's/^\(PATHS_[A-Z_]*_RE\)=.*/\1/p' | sort -u); do
    re=${!var}
    # Expand every (a|b) group into plain alternatives; each must match a tracked file.
    while IFS= read -r item; do
      [ -n "$item" ] || continue
      printf '%s\n' "$files" | grep -qE "$item" || fail path-filters "$var: '$item' matches no file in the repo (renamed, split or deleted?); fix scripts/paths.sh"
    done < <(perl -e 'sub ex { my ($s) = @_; if ($s =~ /^(.*?)\(([^()]*)\)(.*)$/s) { my ($p, $a, $q) = ($1, $2, $3); return map { ex($p . $_ . $q) } split(/\|/, $a, -1); } return $s; } print "$_\n" for ex("(" . $ARGV[0] . ")");' "$re" | sort -u)
  done
  for f in $(printf '%s\n' "$files" | grep -E '^engine/'); do
    printf '%s\n' "$f" | grep -qE "$PATHS_ENGINE_RE" || fail path-filters "$f is covered by no engine job (PATHS_ENGINE_RE)"
  done
  for f in $(printf '%s\n' "$files" | grep -E '^mac/'); do
    printf '%s\n' "$f" | grep -qE "$PATHS_MAC_MAIN_RE" || fail path-filters "$f is covered by no app job (PATHS_MAC_MAIN_RE)"
  done
}

# 8. Tests do not depend on the wall clock (CLAUDE.md, "Tests"): Instant::now, .elapsed() and SystemTime
# are allowed in engine test code only in files listed, with a reason, in scripts/wall-clock-allowlist.txt.
# Test code is everything under a tests/ directory or in a tests.rs, and a source file's part after its
# first #[cfg(test)].
wall_clock() {
  local hits f
  hits=$(git ls-files -co --exclude-standard 'engine/*.rs' | sort -u | xargs awk '
    FNR == 1 { t = (FILENAME ~ /\/tests\// || FILENAME ~ /\/tests\.rs$/) }
    /^#\[cfg\(test\)\]/ { t = 1 }
    t && $0 !~ /^[[:space:]]*\/\// && /Instant::now|\.elapsed\(\)|SystemTime/ { print FILENAME; nextfile }
  ')
  allowlist_entries wall-clock "$CLOCK_ALLOW"
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    allowlisted "$CLOCK_ALLOW" "$f" && continue
    fail wall-clock "$f reads the wall clock (Instant::now, .elapsed(), SystemTime) in test code; drive time from the virtual clock, or list the file in $CLOCK_ALLOW with an open issue"
  done <<<"$hits"
  for f in $(allowlist_paths "$CLOCK_ALLOW"); do
    printf '%s\n' "$hits" | grep -qxF "$f" || fail wall-clock "$CLOCK_ALLOW lists $f, which no longer reads the wall clock in test code; delete the entry"
  done
}

# 9. The Rust fmt/clippy/test command lines live only in scripts/rust-checks.sh, which CI, `make` and
# prepush all call; a copy elsewhere is how the local gate and CI drift apart.
one_gate() {
  local f hits
  for f in .github/workflows/ci.yml Makefile scripts/prepush.sh; do
    # Any cargo fmt/clippy/test, except the ignored real-time gates (their own jobs and flags).
    hits=$(grep -nE '^[^#]*cargo (clippy|test|fmt)( |$)' "$f" | grep -v -- --ignored)
    [ -z "$hits" ] || fail one-gate "$f spells out a Rust gate command (call scripts/rust-checks.sh instead): $(echo "$hits" | head -n 3 | tr '\n' ' ')"
  done
}

# 10. Regression (#408): `prepush.sh --check` with no stamp file must say why it refuses.
prepush_no_stamp() {
  local out
  out=$(PREPUSH_STAMP=/nonexistent/openlab-prepush scripts/prepush.sh --check 2>&1) && fail prepush-stamp "--check passed with no stamp file"
  case "$out" in
    *"no PREPUSH PASS"*) ;;
    *) fail prepush-stamp "scripts/prepush.sh --check with no stamp file printed no explanation (got: '$out')" ;;
  esac
}

# 11. The soak_rt CI wrapper turns a host refusal, and only that, into a SKIPPED. Canned cargo logs.
soak_classifier() {
  local d out rc
  d=$(mktemp -d)
  printf 'host: slow (OPENLAB_SOAK_RT_REQUIRE is set)\nhost: slow (OPENLAB_SOAK_RT_REQUIRE is set)\ntest result: FAILED. 0 passed; 2 failed; 0 ignored\n' >"$d/refused"
  printf 'host: slow (OPENLAB_SOAK_RT_REQUIRE is set)\nrun failed on xruns 5\ntest result: FAILED. 0 passed; 2 failed; 0 ignored\n' >"$d/mixed"
  printf 'error[E0432]: unresolved import\n' >"$d/nosummary"
  out=$(GITHUB_STEP_SUMMARY=/dev/null scripts/soak-rt-ci.sh --classify "$d/refused" 101 2>&1) && rc=0 || rc=$?
  { [ "$rc" = 0 ] && printf '%s' "$out" | grep -q 'SKIPPED'; } || fail soak-classifier "all-refused log must exit 0 with SKIPPED (rc $rc: $out)"
  scripts/soak-rt-ci.sh --classify "$d/mixed" 101 >/dev/null 2>&1 && fail soak-classifier "a refusal plus another failure must exit 1"
  scripts/soak-rt-ci.sh --classify "$d/nosummary" 101 >/dev/null 2>&1 && fail soak-classifier "a log with no test summary and rc 101 must exit 1"
  rm -rf "$d"
}

# 12. Every UI smoke identifier (scripts/ui-smoke.txt) names an existing test, so a rename cannot
# silently drop it from the per-PR set.
ui_smoke() {
  local line cls name n=0
  while IFS= read -r line; do
    case "$line" in ''|'#'*) continue ;; esac
    n=$((n + 1))
    case "$line" in
      openLabUITests/*/*) cls=$(printf '%s' "$line" | cut -d/ -f2); name=$(printf '%s' "$line" | cut -d/ -f3) ;;
      *) fail ui-smoke "'$line' is not openLabUITests/<Class>/<test>"; continue ;;
    esac
    # The class and the test must be in the same file.
    f=$(grep -lE "^(final )?class $cls\\b" mac/openLabUITests/*.swift 2>/dev/null | head -n 1)
    if [ -z "$f" ] || ! grep -qE "func $name\\(\\)" "$f"; then
      fail ui-smoke "'$line' names no test in mac/openLabUITests"
    fi
  done <scripts/ui-smoke.txt
  [ "$n" -ge 3 ] || fail ui-smoke "scripts/ui-smoke.txt lists $n tests; the smoke set needs launch, GO and panic"
}

known_entries
spdx
strings_keys
debug_assertions
versions
real_engine_tests
one_home
path_filters
wall_clock
one_gate
prepush_no_stamp
soak_classifier
ui_smoke

if [ "$failed" = 0 ]; then
  echo "check-repo: ok"
else
  echo "check-repo: FAILED"
  exit 1
fi
