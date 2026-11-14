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
    path=${path%%@*} # `path@kind` entries (the wall-clock list) name a file and what is excused in it
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
# ("clock") and recv_timeout (a hang guard, still a real-time wait) are allowed in engine test code only where
# scripts/wall-clock-allowlist.txt lists them with a reason: `path` exempts the clock reads of that file,
# `path@recv_timeout` only its recv_timeout, so one never excuses the other. Test code is everything under a
# tests/ directory or in a tests.rs, plus any item under #[cfg(test)], #[cfg(all(test, ..))] or
# #[cfg(any(test, ..))] (test-support code): the item runs from its first line to the brace (or paren, or
# `;`/`,`) that closes it, so a multi-line signature, a nested attribute or a trailing // comment do not end
# or fake a region, and production code after a test helper is not test code.
wall_clock_scan() { # files: prints "<file> clock" / "<file> recv_timeout" for each kind a file reads in test code
  awk '
    function flag(kind) { if (!((FILENAME SUBSEP kind) in seen)) { seen[FILENAME SUBSEP kind] = 1; print FILENAME, kind } }
    FNR == 1 { t = 0; arm = 0; d = 0; pd = 0; opened = 0; whole = (FILENAME ~ /\/tests\// || FILENAME ~ /\/tests\.rs$/) }
    {
      code = $0
      gsub(/"([^"\\]|\\.)*"/, "\"\"", code)
      gsub(/'"'"'[{}()]'"'"'/, "'"'"''"'"'", code)
      sub(/\/\/.*/, "", code)
      match(code, /^[ \t]*/); body = substr(code, RLENGTH + 1)
      if (!t && body ~ /^#\[cfg\((all\(|any\()?test[,)]/) { arm = 1; next }
      if (arm && body != "" && body !~ /^#\[/) { arm = 0; t = 1; d = 0; pd = 0; opened = 0 }
      if (whole || t) {
        if (code ~ /Instant::now|\.elapsed\(\)|SystemTime/) flag("clock")
        if (code ~ /recv_timeout/) flag("recv_timeout")
      }
      if (t) {
        for (i = 1; i <= length(code); i++) {
          ch = substr(code, i, 1)
          if (ch == "{") { d++; opened = 1 } else if (ch == "}") d--
          else if (ch == "(" || ch == "[") pd++
          else if (ch == ")" || ch == "]") pd--
        }
        if (d <= 0 && pd <= 0 && (opened || code ~ /[;,][ \t]*$/)) t = 0
      }
    }
  ' "$@"
}
# wall_clock_excused <allowlist> <file> <kind>: a `path` entry excuses the clock reads of a file, a
# `path@recv_timeout` entry only its recv_timeout; neither excuses the other kind.
wall_clock_excused() {
  if [ "$3" = clock ]; then allowlisted "$1" "$2"; else allowlisted "$1" "$2@$3"; fi
}
wall_clock() {
  local hits f kind
  hits=$(wall_clock_scan $(git ls-files -co --exclude-standard 'engine/*.rs' | sort -u))
  allowlist_entries wall-clock "$CLOCK_ALLOW"
  while read -r f kind; do
    [ -n "$f" ] || continue
    wall_clock_excused "$CLOCK_ALLOW" "$f" "$kind" && continue
    fail wall-clock "$f reads the wall clock in test code ($kind; clock = Instant::now, .elapsed(), SystemTime); drive time from the virtual clock, or list the file in $CLOCK_ALLOW (a clock entry is 'path', a hang guard 'path@recv_timeout') with an open issue"
  done <<<"$hits"
  for f in $(allowlist_paths "$CLOCK_ALLOW"); do
    case "$f" in
      *@recv_timeout) printf '%s\n' "$hits" | grep -qxF "${f%@recv_timeout} recv_timeout" || fail wall-clock "$CLOCK_ALLOW lists $f, which no longer calls recv_timeout in test code; delete the entry" ;;
      *) printf '%s\n' "$hits" | grep -qxF "$f clock" || fail wall-clock "$CLOCK_ALLOW lists $f, which no longer reads the wall clock in test code; delete the entry" ;;
    esac
  done
}


wall_clock_cases() { # the scan must see every test region, stop at its end, and tell the kinds apart
  local d
  d=$(mktemp -d)
  mkdir "$d/tests"
  printf 'struct S;\nimpl S {\n    #[cfg(test)]\n    mod t {\n        fn f() { let _ = std::time::Instant::now(); }\n    }\n}\n' >"$d/indented.rs"
  printf '#[cfg(test)]\nfn helper() {\n    let _ = 1;\n}\n\nfn prod() { let _ = Instant::now(); }\n' >"$d/after_helper.rs"
  printf 'struct S {\n    #[cfg(test)]\n    flag: bool,\n}\nfn prod() { let _ = Instant::now(); }\n' >"$d/field.rs"
  printf '#[cfg(test)]\nmod tests {\n    fn f() { rx.recv_timeout(D); }\n}\n' >"$d/guard.rs"
  printf 'fn prod() { let _ = SystemTime::now(); }\n#[cfg(test)]\nmod tests {\n    fn f() {}\n}\n' >"$d/clean.rs"
  printf 'fn f() { rx.recv_timeout(D); }\n' >"$d/tests/dir.rs"
  printf '#[cfg(test)]\nfn helper(\n    a: u32,\n) -> u32 {\n    let _ = Instant::now();\n    a\n}\n' >"$d/signature.rs"
  printf 'mod m {\n    #[cfg(test)]\n    mod t {\n        #[cfg(test)]\n        fn a() {}\n        fn b() { let _ = Instant::now(); }\n    }\n}\n' >"$d/nested.rs"
  printf '#[cfg(all(test, unix))]\nmod t {\n    fn f() { let _ = Instant::now(); }\n}\n' >"$d/all.rs"
  printf '#[cfg(any(test, feature = "test-support"))]\nfn f() { let _ = Instant::now(); }\n' >"$d/any.rs"
  printf '#[cfg(test)]\nfn f() { // note\n    g();\n}\nfn prod() { let _ = Instant::now(); }\n' >"$d/comment_brace.rs"
  printf '#[cfg(test)]\nuse std::time::Instant; // for the tests\nfn prod() { let _ = Instant::now(); }\n' >"$d/comment_use.rs"
  printf '#[cfg(test)]\nfn f() {\n    g(); // Instant::now() would be wrong here\n    let _ = "}";\n}\nfn prod() { let _ = Instant::now(); }\n' >"$d/comment_token.rs"
  printf '#[cfg(test)]\nmod t {\n    fn f() { let _ = Instant::now(); rx.recv_timeout(D); }\n}\n' >"$d/both.rs"
  printf "#[cfg(test)]\\nfn f() {\\n    let _ = '{';\\n}\\nfn prod() { let _ = Instant::now(); }\\n" >"$d/char_brace.rs"
  printf '#[cfg(test)]\nfn f() {\n    let _ = "}";\n    let _ = Instant::now();\n}\n' >"$d/string_brace.rs"
  [ "$(wall_clock_scan "$d/indented.rs")" = "$d/indented.rs clock" ] || fail wall-clock-cases "an indented #[cfg(test)] block must count as test code"
  [ -z "$(wall_clock_scan "$d/after_helper.rs")" ] || fail wall-clock-cases "production code after a test helper must not count as test code"
  [ -z "$(wall_clock_scan "$d/field.rs")" ] || fail wall-clock-cases "a cfg(test) field must not make the rest of the file test code"
  [ "$(wall_clock_scan "$d/guard.rs")" = "$d/guard.rs recv_timeout" ] || fail wall-clock-cases "recv_timeout in test code must be flagged, as its own kind"
  [ -z "$(wall_clock_scan "$d/clean.rs")" ] || fail wall-clock-cases "a clock read before the test module is production code"
  [ "$(wall_clock_scan "$d/tests/dir.rs")" = "$d/tests/dir.rs recv_timeout" ] || fail wall-clock-cases "recv_timeout under a tests/ directory must be flagged"
  [ "$(wall_clock_scan "$d/signature.rs")" = "$d/signature.rs clock" ] || fail wall-clock-cases "a multi-line signature must not close the test region before its body"
  [ "$(wall_clock_scan "$d/nested.rs")" = "$d/nested.rs clock" ] || fail wall-clock-cases "a nested #[cfg(test)] must not end the enclosing test region"
  [ "$(wall_clock_scan "$d/all.rs")" = "$d/all.rs clock" ] || fail wall-clock-cases "cfg(all(test, ..)) must count as test code"
  [ "$(wall_clock_scan "$d/any.rs")" = "$d/any.rs clock" ] || fail wall-clock-cases "cfg(any(test, ..)) must count as test code"
  [ -z "$(wall_clock_scan "$d/comment_brace.rs")" ] || fail wall-clock-cases "a trailing // comment must not hide the opening brace of a test item"
  [ -z "$(wall_clock_scan "$d/comment_use.rs")" ] || fail wall-clock-cases "a trailing // comment must not hide the end of a one-line test item"
  [ -z "$(wall_clock_scan "$d/comment_token.rs")" ] || fail wall-clock-cases "a token in a trailing comment, and a brace in a string, must not count"
  [ "$(wall_clock_scan "$d/string_brace.rs")" = "$d/string_brace.rs clock" ] || fail wall-clock-cases "a brace inside a string must not end the test region"
  [ -z "$(wall_clock_scan "$d/char_brace.rs")" ] || fail wall-clock-cases "an open brace in a char literal must not stretch the test region over the production clock read after it"
  [ "$(wall_clock_scan "$d/both.rs" | wc -l | tr -d ' ')" = 2 ] || fail wall-clock-cases "clock reads and recv_timeout in one file must be reported as two kinds"
  printf 'a.rs | permanent: `x`\nb.rs@recv_timeout | permanent: `y`\n' >"$d/allow"
  wall_clock_excused "$d/allow" a.rs clock || fail wall-clock-cases "a path entry must excuse that file's clock reads"
  wall_clock_excused "$d/allow" b.rs recv_timeout || fail wall-clock-cases "a path@recv_timeout entry must excuse that file's recv_timeout"
  wall_clock_excused "$d/allow" b.rs clock && fail wall-clock-cases "a recv_timeout entry must not excuse a clock read in the same file"
  wall_clock_excused "$d/allow" a.rs recv_timeout && fail wall-clock-cases "a clock entry must not excuse recv_timeout"
  rm -rf "$d"
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

# 11b. The app-test wrapper retries a hang only when the spindump proves the runner's WindowServer is
# stuck in the GPU driver and every failure is such a hang. Canned reports and summaries.
wedged_report() { # samples in the driver; samples waiting on WindowServer: several ranges, `/` starts a second tree of the thread, `=N` is not a kernel frame, and a stack with no wait named: `qN` QuartzCore, `sN` SkyLight, `oN` our code, `fN` our code with a framework in its name, `mN` libsystem_malloc, `nN` mach_msg alone, `tN` mach_msg down to the trap, then kernel frames `bN` waiting on launchd, `pN` on this process, `uN` suspended, `wN` on WindowServer; waiting thread; waiting process; samples another thread waits
  local main='DispatchQueue "com.apple.main-thread"(1)' n
  local wait='  %s%s ??? (kernel) (blocked by turnstile waiting for WindowServer [2] after 2 hops) 1-9\n'
  printf 'Command: openLab\nPID: 1\nProcess: %s\nNum samples: 9 (1-9)\n  Thread 0x1 %s\n' "${4:-openLab [1]}" "${3:-$main}"
  for n in $2; do
    case "$n" in
      /) printf '  Thread 0x1 %s\n' "${3:-$main}" ;;
      =*) printf "$wait" '' "${n#=}" ;;
      q*) printf '  %s _CASCreateFencePort + 1 (QuartzCore + 1)\n' "${n#q}" ;;
      s*) printf '  %s SLSNewWindow + 1 (SkyLight + 1)\n' "${n#s}" ;;
      o*) printf '  %s ourCode + 1 (openLab.debug.dylib + 1)\n' "${n#o}" ;;
      f*) printf '  %s QuartzCoreHelper + 1 (openLab.debug.dylib + 1)\n' "${n#f}" ;;
      m*) printf '  %s malloc + 1 (libsystem_malloc.dylib + 1)\n' "${n#m}" ;;
      n*) printf '  %s mach_msg + 1 (libsystem_kernel.dylib + 1)\n' "${n#n}" ;;
      b*) printf '  *%s ??? (kernel + 1) (blocked by turnstile waiting for launchd [9] after 2 hops)\n' "${n#b}" ;;
      p*) printf '  *%s ??? (kernel + 1) (blocked by turnstile waiting for openLab [1] thread 0x4)\n' "${n#p}" ;;
      u*) printf '  *%s ??? (kernel + 1) (suspended)\n' "${n#u}" ;;
      w*) printf '  *%s ??? (kernel + 1) (blocked by turnstile with priority 31 waiting for WindowServer [2])\n' "${n#w}" ;;
      t*) printf '  %s mach_msg + 1 (libsystem_kernel.dylib + 1)\n  %s mach_msg2_trap + 1 (libsystem_kernel.dylib + 1)\n  *%s ??? (kernel + 1)\n' "${n#t}" "${n#t}" "${n#t}" ;;
      *) printf "$wait" '*' "$n" ;;
    esac
  done
  if [ -n "${5:-}" ]; then
    printf '  Thread 0x2 Thread name "com.apple.NSEventThread"\n'
    printf "$wait" '*' "$5"
  fi
  printf 'Process: WindowServer [2]\nNum samples: 9 (1-9)\n  Thread 0x3 Thread name "ws_main_thread"\n  *%s AppleParavirtDevice::createObjectInternal 1-9\n' "$1"
}
wedged_classifier() {
  local d out timeout other
  d=$(mktemp -d)
  timeout='{"failureText":"Test exceeded execution time allowance of 3 minutes","targetName":"openLabTests","testIdentifierString":"Flow/testHung()"}'
  other='{"failureText":"XCTAssertEqual failed","targetName":"openLabTests","testIdentifierString":"Flow/testBad()"}'
  printf '{"testFailures":[%s]}' "$timeout" >"$d/hung.json"
  printf '{"testFailures":[%s,%s]}' "$timeout" "$other" >"$d/mixed.json"
  printf '{"testFailures":[]}' >"$d/none.json"
  printf '[{"testIdentifier":"Flow/testHung()","attachments":[{"exportedFileName":"s.txt","isAssociatedWithFailure":true,"suggestedHumanReadableName":"x-Spindump_0.txt"}]}]' >"$d/manifest.json"
  wedged_report 9 9 >"$d/s.txt"
  out=$(scripts/run-mac-tests.sh --classify "$d/hung.json" "$d") || fail wedged-classifier "a proven hang must be retried"
  [ "$out" = "-only-testing:openLabTests/Flow/testHung" ] || fail wedged-classifier "wrong retry arguments: '$out'"
  scripts/run-mac-tests.sh --classify "$d/mixed.json" "$d" >/dev/null 2>&1 && fail wedged-classifier "a hang beside another failure must not be retried"
  scripts/run-mac-tests.sh --classify "$d/none.json" "$d" >/dev/null 2>&1 && fail wedged-classifier "a run with no test failure (a build error) must not be retried"
  # The app started again after a hang into the same stalled WindowServer never connects: a failure of
  # the run, not of a test, with a spindump of its own. The tests after the hang never ran, so the run fails
  # with a notice that the job needs a re-run.
  local started hung_entry restart_entry
  started='{"failureText":"The test runner hung before establishing connection.","targetName":"openLabTests","testIdentifierString":"openLab (5) encountered an error"}'
  hung_entry='{"testIdentifier":"Flow/testHung()","attachments":[{"exportedFileName":"s.txt","isAssociatedWithFailure":true,"suggestedHumanReadableName":"x-Spindump_0.txt"}]}'
  restart_entry='{"testIdentifier":"openLab (5) encountered an error","attachments":[{"exportedFileName":"r.txt","isAssociatedWithFailure":true,"suggestedHumanReadableName":"Hang (Spindump)_1.txt"}]}'
  printf '{"testFailures":[%s,%s]}' "$started" "$timeout" >"$d/cut-short.json"
  printf '{"testFailures":[%s,%s,%s]}' "$started" "$timeout" "$other" >"$d/cut-short-mixed.json"
  printf '[%s,%s]' "$hung_entry" "$restart_entry" >"$d/manifest.json"
  wedged_report 9 9 >"$d/r.txt"
  out=$(scripts/run-mac-tests.sh --classify "$d/cut-short.json" "$d") || fail wedged-classifier "a proven hang and a proven failed restart must be recognised"
  [ "$out" = "--cut-short" ] || fail wedged-classifier "a run cut short must be said so, not '$out'"
  scripts/run-mac-tests.sh --classify "$d/cut-short-mixed.json" "$d" >/dev/null 2>&1 && fail wedged-classifier "a failed restart beside another failure must not be retried"
  wedged_report 3 9 >"$d/r.txt"
  scripts/run-mac-tests.sh --classify "$d/cut-short.json" "$d" >/dev/null 2>&1 && fail wedged-classifier "a failed restart whose spindump proves no wedge must not be retried"
  printf '[%s]' "$hung_entry" >"$d/manifest.json"
  scripts/run-mac-tests.sh --classify "$d/cut-short.json" "$d" >/dev/null 2>&1 && fail wedged-classifier "a failed restart with no spindump must not be retried"
  wedged_report 3 9 >"$d/s.txt"
  scripts/run-mac-tests.sh --classify "$d/hung.json" "$d" >/dev/null 2>&1 && fail wedged-classifier "WindowServer only passing through the driver must not excuse a hang"
  wedged_report 9 3 >"$d/s.txt"
  scripts/run-mac-tests.sh --classify "$d/hung.json" "$d" >/dev/null 2>&1 && fail wedged-classifier "a process not waiting on WindowServer must not be excused"
  wedged_report 9 "5 1 3" >"$d/s.txt"
  scripts/run-mac-tests.sh --classify "$d/hung.json" "$d" >/dev/null 2>&1 || fail wedged-classifier "a wait split over sample ranges that add up to all samples is proven"
  wedged_report 9 "5 1 2" >"$d/s.txt"
  scripts/run-mac-tests.sh --classify "$d/hung.json" "$d" >/dev/null 2>&1 && fail wedged-classifier "split ranges that add up short must not excuse a hang"
  wedged_report 9 "5 5" >"$d/s.txt"
  scripts/run-mac-tests.sh --classify "$d/hung.json" "$d" >/dev/null 2>&1 && fail wedged-classifier "ranges that add up past the samples must not excuse a hang"
  wedged_report 9 5 '' '' 4 >"$d/s.txt"
  scripts/run-mac-tests.sh --classify "$d/hung.json" "$d" >/dev/null 2>&1 && fail wedged-classifier "a background thread's wait must not add to the main thread's"
  wedged_report 9 9 'Thread name "com.apple.NSEventThread"' >"$d/s.txt"
  scripts/run-mac-tests.sh --classify "$d/hung.json" "$d" >/dev/null 2>&1 && fail wedged-classifier "only a background thread waiting must not excuse a hung main thread"
  wedged_report 9 "5 / 4" >"$d/s.txt"
  scripts/run-mac-tests.sh --classify "$d/hung.json" "$d" >/dev/null 2>&1 && fail wedged-classifier "two trees of the main thread must not add up"
  wedged_report 9 "5 =4" >"$d/s.txt"
  scripts/run-mac-tests.sh --classify "$d/hung.json" "$d" >/dev/null 2>&1 && fail wedged-classifier "a wait line that is not a kernel frame must not add up"
  wedged_report 9 "9 / =9" >"$d/s.txt"
  scripts/run-mac-tests.sh --classify "$d/hung.json" "$d" >/dev/null 2>&1 || fail wedged-classifier "a whole wait followed by a Swift task's copy of the tree is proven"
  wedged_report 9 '' | sed '4s/: 9 /: 0 /' >"$d/s.txt"
  scripts/run-mac-tests.sh --classify "$d/hung.json" "$d" >/dev/null 2>&1 && fail wedged-classifier "a section with no samples and no wait must not excuse a hang"
  wedged_report 9 "q9 t9" >"$d/s.txt"
  scripts/run-mac-tests.sh --classify "$d/hung.json" "$d" >/dev/null 2>&1 || fail wedged-classifier "QuartzCore's message to WindowServer for all samples is proven"
  wedged_report 9 "s9 t9" >"$d/s.txt"
  scripts/run-mac-tests.sh --classify "$d/hung.json" "$d" >/dev/null 2>&1 || fail wedged-classifier "SkyLight's message to WindowServer for all samples is proven"
  local refused
  wedged_report 9 "q9 t9 w9" >"$d/s.txt"
  scripts/run-mac-tests.sh --classify "$d/hung.json" "$d" >/dev/null 2>&1 || fail wedged-classifier "a kernel frame under the trap that names WindowServer is proven"
  for refused in "t9" "o9 t9" "q9 o9 t9" "q5 t5" "q9 t5" "q5 t9" "q9 / t9" "f9 t9" "q9 m9 t9" "q9 t9 b9" "q9 t9 p9" "q9 t9 u9" "q9 t9 b9 / q9 t9" "q9 n9"; do
    wedged_report 9 "$refused" >"$d/s.txt"
    scripts/run-mac-tests.sh --classify "$d/hung.json" "$d" >/dev/null 2>&1 && fail wedged-classifier "a main thread in mach_msg ($refused) must not excuse a hang"
  done
  wedged_report 3 "q9 t9" >"$d/s.txt"
  scripts/run-mac-tests.sh --classify "$d/hung.json" "$d" >/dev/null 2>&1 && fail wedged-classifier "QuartzCore's message with WindowServer only passing through the driver must not excuse a hang"
  wedged_report 9 "q9 t9" 'Thread name "com.apple.NSEventThread"' >"$d/s.txt"
  scripts/run-mac-tests.sh --classify "$d/hung.json" "$d" >/dev/null 2>&1 && fail wedged-classifier "QuartzCore's message on a background thread must not excuse a hang"
  wedged_report 9 "q9 t9" '' 'openLab [7]' >"$d/s.txt"
  scripts/run-mac-tests.sh --classify "$d/hung.json" "$d" >/dev/null 2>&1 && fail wedged-classifier "QuartzCore's message in another process must not excuse a hang"
  wedged_report 9 9 '' 'other [7]' >"$d/s.txt"
  scripts/run-mac-tests.sh --classify "$d/hung.json" "$d" >/dev/null 2>&1 && fail wedged-classifier "another process waiting must not excuse the tested one"
  wedged_report 9 9 '' 'openLab [7]' >"$d/s.txt"
  scripts/run-mac-tests.sh --classify "$d/hung.json" "$d" >/dev/null 2>&1 && fail wedged-classifier "another process of the same name waiting must not excuse the tested one"
  wedged_report 9 9 >"$d/s.txt"
  printf '[]' >"$d/manifest.json"
  scripts/run-mac-tests.sh --classify "$d/hung.json" "$d" >/dev/null 2>&1 && fail wedged-classifier "a hang with no spindump must not be retried"
  (cd "$d" && rm -f hung.json mixed.json none.json cut-short.json cut-short-mixed.json manifest.json s.txt r.txt) && rmdir "$d"
}

# 11c. The wrapper's run path, with a fake xcodebuild and xcrun on PATH. Each case: exit code, number of
# xcodebuild calls. Settings for the fakes come from the caller's environment. The folder stays in the
# temporary folder, as the wrapper's own do.
wedged_case() { # name, wanted exit code, wanted calls, xcodebuild's exit codes, selection
  local c="$WEDGED/$1" rc n
  mkdir -p "$c/build/Logs/Test"
  printf '%s\n' $4 >"$c/rcs"
  [ -z "${OLD_BUNDLE:-}" ] || mkdir "$c/build/Logs/Test/Old.xcresult"
  # The summary is CI's real one: a fake wedge must not be reported there.
  (cd "$c" && GITHUB_STEP_SUMMARY= CI= RUNNER_ENVIRONMENT= TMPDIR="$c" PATH="$WEDGED/bin:$PATH" "$OLDPWD/scripts/run-mac-tests.sh" fakexb base -- ${5:-}) >"$c/out" 2>&1
  rc=$?
  n=$(wc -l <"$c/calls" | tr -d ' ')
  { [ "$rc" = "$2" ] && [ "$n" = "$3" ]; } || fail wedged-run "$1: want exit $2 and $3 call(s), got exit $rc and $n"
}
wedged_run() {
  local d
  d=$(mktemp -d)
  WEDGED=$d
  mkdir "$d/bin" "$d/att"
  cat >"$d/bin/fakexb" <<'FAKE'
#!/bin/sh
echo "$*" >>calls
n=$(wc -l <calls | tr -d ' ')
if [ -z "$NO_BUNDLE" ] && { [ "$n" = 1 ] || [ -z "$NO_RETRY_BUNDLE" ]; }; then mkdir "build/Logs/Test/Test-$n.xcresult"; fi
if [ -n "$SILENT" ]; then echo "Testing started"; sleep 60; fi
exit "$(sed -n "${n}p" rcs)"
FAKE
  cat >"$d/bin/xcrun" <<'FAKE'
#!/bin/sh
case "$*" in
  # An old bundle reads as a proven wedge, and passes enough tests: only the wrapper's choice of bundle can save it.
  *"test-results summary"*Test-1.xcresult* | *"test-results summary"*Old.xcresult*) [ -z "$BAD_SUMMARY" ] || exit 9; cat "$FIRST_SUMMARY" ;;
  *"test-results summary"*) printf '{"passedTests":%s}' "${PASSED:-1}" ;;
  *"export attachments"*) [ -z "$BAD_EXPORT" ] || exit 9; for a; do p=$a; done; cp -R "$WEDGED_ATT" "$p" ;;
esac
FAKE
  chmod +x "$d/bin/fakexb" "$d/bin/xcrun"
  printf '{"passedTests":5,"testFailures":[{"failureText":"Test exceeded execution time allowance of 3 minutes","targetName":"openLabTests","testIdentifierString":"Flow/testHung()"}]}' >"$d/hung.json"
  printf '{"testFailures":[{"failureText":"Test exceeded execution time allowance of 3 minutes","targetName":"openLabTests","testIdentifierString":"Flow/testHung()"},{"failureText":"XCTAssertEqual failed","targetName":"openLabTests","testIdentifierString":"Flow/testBad()"}]}' >"$d/mixed.json"
  printf '[{"testIdentifier":"Flow/testHung()","attachments":[{"exportedFileName":"s.txt","isAssociatedWithFailure":true,"suggestedHumanReadableName":"x-Spindump_0.txt"}]}]' >"$d/att/manifest.json"
  wedged_report 9 9 >"$d/att/s.txt"
  printf '{"testFailures":[{"failureText":"XCTAssertEqual failed","targetName":"openLabTests","testIdentifierString":"Flow/testWrong()"}]}' >"$d/plain.json"
  export WEDGED_ATT="$d/att" FIRST_SUMMARY="$d/hung.json"

  # The app that could not start again after the hang: no second start (the stall has outlasted the
  # restart, so it would hang to the step's limit), the failure kept, and the job told to be re-run.
  local att_cut="$d/att-cut" att_bad="$d/att-bad" started
  started='{"failureText":"The test runner hung before establishing connection.","targetName":"openLabTests","testIdentifierString":"openLab (5) encountered an error"}'
  mkdir "$att_cut" "$att_bad"
  printf '[%s,%s]' \
    '{"testIdentifier":"Flow/testHung()","attachments":[{"exportedFileName":"s.txt","isAssociatedWithFailure":true,"suggestedHumanReadableName":"x-Spindump_0.txt"}]}' \
    '{"testIdentifier":"openLab (5) encountered an error","attachments":[{"exportedFileName":"r.txt","isAssociatedWithFailure":true,"suggestedHumanReadableName":"Hang (Spindump)_1.txt"}]}' \
    >"$att_cut/manifest.json"
  cp "$att_cut/manifest.json" "$att_bad/manifest.json"
  wedged_report 9 9 >"$att_cut/s.txt"
  cp "$att_cut/s.txt" "$att_bad/s.txt"
  wedged_report 9 9 >"$att_cut/r.txt"
  wedged_report 3 9 >"$att_bad/r.txt"
  printf '{"testFailures":[%s,{"failureText":"Test exceeded execution time allowance of 3 minutes","targetName":"openLabTests","testIdentifierString":"Flow/testHung()"}]}' "$started" >"$d/cut-short.json"
  WEDGED_ATT="$att_cut" FIRST_SUMMARY="$d/cut-short.json" wedged_case cut-short 75 1 "65" -only-testing:everything
  grep -q '::warning.*re-run the job' "$d/cut-short/out" || fail wedged-run "a run cut short must say the job needs a re-run: $(cat "$d/cut-short/out")"
  WEDGED_ATT="$att_bad" FIRST_SUMMARY="$d/cut-short.json" wedged_case cut-short-unproven 76 1 "65"
  ! grep -q 're-run the job' "$d/cut-short-unproven/out" || fail wedged-run "a restart not proven wedged must not be called a wedge"

  wedged_case proven 0 2 "65 0" -only-testing:everything
  grep -q '::warning' "$d/proven/out" || fail wedged-run "a retry must warn"
  [ "$(sed -n 2p "$d/proven/calls")" = "base -only-testing:openLabTests/Flow/testHung test" ] \
    || fail wedged-run "the retry must select the one test: $(sed -n 2p "$d/proven/calls")"
  wedged_case retry-fails 76 2 "65 65"
  wedged_case retry-other-code 76 2 "65 70"
  NO_BUNDLE=1 wedged_case no-bundle 70 1 "70"
  NO_BUNDLE=1 OLD_BUNDLE=1 wedged_case old-bundle-only 70 1 "70"
  BAD_SUMMARY=1 wedged_case bad-summary 65 1 "65"
  BAD_EXPORT=1 wedged_case bad-export 76 1 "65"
  FIRST_SUMMARY="$d/mixed.json" wedged_case mixed 76 1 "65"
  wedged_case passes 0 1 "0"
  [ "$(cat "$d/passes/calls")" = "base test" ] || fail wedged-run "an empty selection: $(cat "$d/passes/calls")"
  PASSED=0 wedged_case retry-ran-nothing 76 2 "65 0"
  NO_RETRY_BUNDLE=1 wedged_case retry-left-no-bundle 76 2 "65 0"
  # A failure that is no hang keeps xcodebuild's own code.
  FIRST_SUMMARY="$d/plain.json" wedged_case plain-failure 65 1 "65"
  # A run that goes silent once its tests have started is stopped and counts as a hang.
  SILENT=1 OPENLAB_TEST_SILENCE_LIMIT=2 wedged_case silent 76 1 "0"
  grep -q 'no output for 2s' "$d/silent/out" || fail wedged-run "a silenced run must say so: $(cat "$d/silent/out")"
  unset WEDGED_ATT FIRST_SUMMARY
  make_test_mac_cases "$d"
}
# `make test-mac` with a stub in place of the test wrapper: the stub exits with the given codes, one per
# call; wanted is make's exit code and the number of processes it started.
make_test_mac_case() { # dir, name, wanted exit code, wanted calls, stub exit codes
  local c="$1/make-$2" rc n want
  mkdir -p "$c"
  printf '%s\n' $5 >"$c/rcs"
  : >"$c/calls"
  cat >"$c/stub" <<'STUB'
#!/bin/sh
d=$(dirname "$0")
echo "$*" >>"$d/calls"
exit "$(sed -n "$(wc -l <"$d/calls" | tr -d ' ')p" "$d/rcs")"
STUB
  chmod +x "$c/stub"
  make -s -o project test-mac MAC_TESTS_WITH_LIMIT="$c/stub" >"$c/out" 2>&1
  rc=$?
  n=$(wc -l <"$c/calls" | tr -d ' ')
  # make reports a failed recipe as exit 2 and "Error <recipe's code>".
  if [ "$3" = 0 ]; then want=0; else want=2; fi
  { [ "$rc" = "$want" ] && [ "$n" = "$4" ] && { [ "$3" = 0 ] || grep -q "Error $3\$" "$c/out"; }; } \
    || fail make-test-mac "$2: want exit $3 and $4 process(es), got make exit $rc and $n: $(cat "$c/out")"
}
make_test_mac_cases() {
  make_test_mac_case "$1" cut-short 75 1 "75 0"
  grep -q 'not starting the second' "$1/make-cut-short/out" || fail make-test-mac "a skipped second process must say why"
  make_test_mac_case "$1" hung 76 1 "76 0"
  grep -q 'not starting the second' "$1/make-hung/out" || fail make-test-mac "a hang must stop the second process and say why"
  make_test_mac_case "$1" pass 0 2 "0 0"
  make_test_mac_case "$1" first-fails 65 2 "65 0"
  make_test_mac_case "$1" second-fails 70 2 "0 70"
  make_test_mac_case "$1" both-fail 70 2 "65 70"
}

# 12. Every identifier in scripts/ui-smoke.txt and scripts/core-flows.txt names an existing test, so a
# rename cannot silently drop it from the per-PR set or the core-flows set.
# test_in_class <class> <test> <files...>: true when the test function is declared inside that class.
test_in_class() {
  local cls=$1 name=$2
  shift 2
  # A class line may carry attributes (@MainActor final class B); a func line is anchored so a commented-out
  # `// func testGone()` does not count.
  awk -v cls="$cls" -v fn="^[ \t]*(@[A-Za-z0-9_]+[ \t]+)*((override|public|private|fileprivate|internal)[ \t]+)*func[ \t]+$name[ \t]*\\\\(" '
    FNR == 1 { cur = "" }
    /^(@[A-Za-z0-9_]+[ \t]+)*((public|internal|open)[ \t]+)?(final[ \t]+)?class[ \t]+[A-Za-z0-9_]+/ {
      c = $0; sub(/^(@[A-Za-z0-9_]+[ \t]+)*((public|internal|open)[ \t]+)?(final[ \t]+)?class[ \t]+/, "", c); sub(/[^A-Za-z0-9_].*/, "", c); cur = c
    }
    $0 ~ fn && cur == cls { found = 1 }
    END { exit !found }' "$@"
}
test_list() {
  # Sets LIST_N (not echoed: fail must run in this shell to keep its flag).
  local list=$1 check=$2 prefix=$3 line target cls name n=0
  while IFS= read -r line; do
    case "$line" in ''|'#'*) continue ;; esac
    n=$((n + 1))
    case "$line" in
      */*/*/*) fail "$check" "'$line' has more than <target>/<Class>/<test>"; continue ;;
      openLabUITests/*/*|"$prefix"/*/*) ;;
      *) fail "$check" "'$line' is not <target>/<Class>/<test>"; continue ;;
    esac
    target=$(printf '%s' "$line" | cut -d/ -f1); cls=$(printf '%s' "$line" | cut -d/ -f2); name=$(printf '%s' "$line" | cut -d/ -f3)
    # The test must be declared in the class named, not merely in the same file: xcodebuild runs
    # nothing, silently, for an identifier whose class does not hold the test.
    if ! test_in_class "$cls" "$name" mac/"$target"/*.swift; then
      fail "$check" "'$line' names no test in mac/$target"
    fi
  done <"$list"
  LIST_N=$n
}
test_list_cases() { # a class and a test of another class in the same file must not pair
  local d
  d=$(mktemp -d)
  printf 'final class A: XCTestCase {\n    func testOne() {}\n}\nfinal class B: XCTestCase {\n    func testTwo() {}\n}\n' >"$d/two.swift"
  printf 'func testOrphan() {}\n' >"$d/orphan.swift"
  printf '@MainActor\nfinal class C: XCTestCase {\n    // func testGone() {}\n    @MainActor func testKept() {}\n}\n@MainActor final class D: XCTestCase {\n    func testDee() {}\n}\n' >"$d/attr.swift"
  test_in_class A testOne "$d/two.swift" || fail ui-smoke-cases "a test must pair with its own class"
  test_in_class B testTwo "$d/two.swift" || fail ui-smoke-cases "the second class's test must pair with the second class"
  test_in_class A testTwo "$d/two.swift" && fail ui-smoke-cases "A/testTwo must not pass: testTwo is in class B"
  test_in_class B testOne "$d/two.swift" && fail ui-smoke-cases "B/testOne must not pass: testOne is in class A"
  test_in_class B testOrphan "$d/two.swift" "$d/orphan.swift" && fail ui-smoke-cases "a function before any class in the next file must not pair with the last class of the previous file"
  test_in_class C testGone "$d/attr.swift" && fail ui-smoke-cases "a commented-out func must not count as a test"
  test_in_class C testKept "$d/attr.swift" || fail ui-smoke-cases "an attributed func must pair with its class"
  test_in_class D testDee "$d/attr.swift" || fail ui-smoke-cases "a class declared with a leading attribute on the same line must be recognised"
  test_in_class C testDee "$d/attr.swift" && fail ui-smoke-cases "a test of an attributed class must not pair with the class before it"
  rm -rf "$d"
}
ui_smoke() {
  test_list scripts/ui-smoke.txt ui-smoke openLabUITests
  [ "$LIST_N" -ge 3 ] || fail ui-smoke "scripts/ui-smoke.txt lists $LIST_N tests; the smoke set needs launch, GO and panic"
  test_list scripts/core-flows.txt core-flows openLabTests
  [ "$LIST_N" -ge 10 ] || fail core-flows "scripts/core-flows.txt lists $LIST_N tests; the core-flows set is the everyday flows"
}

# `make test-mac` splits the unit tests over two processes: what the first skips, the second must run.
test_mac_split() {
  local plan skipped only
  plan=$(make -n test-mac | tr ' ' '\n')
  skipped=$(printf '%s\n' "$plan" | sed -n 's/^-skip-testing:openLabTests\///p' | sort)
  only=$(printf '%s\n' "$plan" | sed -n 's/^-only-testing:openLabTests\/\([^/]*\)$/\1/p' | sort)
  [ -n "$skipped" ] || fail test-mac-split "make test-mac skips no classes in its first process"
  [ "$skipped" = "$only" ] || fail test-mac-split "make test-mac's second process must run exactly what its first skips"
  printf '%s\n' "$plan" | grep -qx -- '-only-testing:openLabTests' || fail test-mac-split "make test-mac's first process must select the whole unit target"
}

# 13. Milestone and task IDs (M5.4, M4.12c; as identifiers m5_6b) stay out of code, comments and test names
# (CLAUDE.md, "Work and git"); requirement IDs (AUD-07) are fine. They go stale, and the issue or PR holds
# the history. The scan covers Rust and Swift sources and the skills except orchestrate (which
# briefs on milestones); other files are kept clean by hand.
TASK_ID_RE='(^|[^A-Za-z0-9_.])M[0-9]+\.[0-9]+[a-z]?([^A-Za-z0-9_]|$)|(^|[^A-Za-z0-9])[Mm][0-9]+_[0-9]+[a-z]?([^A-Za-z0-9]|$)'
task_ids_scan() { grep -nE "$TASK_ID_RE" "$@" 2>/dev/null || true; }
task_ids() {
  local hits d
  d=$(mktemp -d)
  printf '// fine: AUD-07, v1.2, Model3.5, an M4 Mac\nlet x = 1 // M5.4d\n' >"$d/a.swift"
  printf '/// done in M4.12c\n' >"$d/b.rs"
  printf '// (QLab check D6, M2.24)\n' >"$d/c.rs"
  printf '// AUD-07 and v1.2 only, Apple M4\n' >"$d/d.rs"
  printf 'fn test_m5_6b_duration() {}\n' >"$d/e.rs"
  printf 'fn rate_m4_of_3() { let om5_6 = 1; }\n' >"$d/f.rs"
  [ "$(task_ids_scan "$d/a.swift" | wc -l | tr -d ' ')" = 1 ] || fail task-ids-cases "a task ID after code must be found, and requirement IDs, versions and chip names must not"
  [ -n "$(task_ids_scan "$d/b.rs")" ] || fail task-ids-cases "a task ID with a letter suffix must be found"
  [ -n "$(task_ids_scan "$d/c.rs")" ] || fail task-ids-cases "a task ID after a comma must be found"
  [ -z "$(task_ids_scan "$d/d.rs")" ] || fail task-ids-cases "requirement IDs, versions and chip names must not be found"
  [ -n "$(task_ids_scan "$d/e.rs")" ] || fail task-ids-cases "a task ID in an identifier (m5_6b) must be found"
  [ -z "$(task_ids_scan "$d/f.rs")" ] || fail task-ids-cases "an identifier that only contains m<digit>_ must not be found"
  rm -rf "$d"
  hits=$(git ls-files -co --exclude-standard 'engine/*.rs' 'mac/*.swift' '.claude/skills/*' | grep -v '^\.claude/skills/orchestrate/' | sort -u | xargs grep -nE "$TASK_ID_RE" 2>/dev/null | head -n 20 || true)
  [ -z "$hits" ] || fail task-ids "milestone/task IDs in code, comments or test names (describe the behaviour instead):
$hits"
}

# 14. Skills (.claude/skills/*) are found by their description and loaded whole, so their shape is checked:
# the frontmatter name is the folder, the description is short enough to sit in every agent's list, the
# body is small, every reference is named in SKILL.md (so it is read on purpose), and no line is long
# enough to flood a grep. Code comments never cite skills, so a description is the only way in.
# skill_description_shape <file> <raw value>: the frontmatter must be strict YAML on one line. A block scalar
# (> or |) or an empty value hides the text from the one-line list agents search; an unquoted ": " is a YAML
# error; a quoted value must not hold a bare double quote.
skill_description_shape() {
  local f=$1 raw=$2 inner
  case "$raw" in
    '') fail skills "$f: description is missing or empty" ;;
    '>'* | '|'*) fail skills "$f: description is a block scalar (starts with ${raw%"${raw#?}"}); write it on one line, in quotes" ;;
    \"*\") inner=${raw#\"}; inner=${inner%\"}
      case "$inner" in *\"*) fail skills "$f: description holds a double quote inside its quotes; rephrase" ;; esac ;;
    *": "*) fail skills "$f: description has an unquoted ': ' (invalid YAML); put the whole value in double quotes" ;;
  esac
}
skill_shape_cases() {
  local out
  out=$(skill_description_shape f '' ; skill_description_shape f '> folded'; skill_description_shape f '| lit'; skill_description_shape f 'a: b'; skill_description_shape f '"a "b" c"')
  [ "$(printf '%s\n' "$out" | grep -c '^FAIL')" = 5 ] || fail skills-cases "skill_description_shape must reject empty, > and |, an unquoted ': ' and an inner quote (got: $out)"
  [ -z "$(skill_description_shape f '"fine: quoted"'; skill_description_shape f 'plain words')" ] || fail skills-cases "a quoted value with ': ' and a plain value must pass"
}
skills() {
  local dir name want desc ref n f
  for dir in .claude/skills/*/; do
    dir=${dir%/}
    want=$(basename "$dir")
    f=$dir/SKILL.md
    [ -f "$f" ] || { fail skills "$dir has no SKILL.md"; continue; }
    name=$(sed -n '2,/^---$/s/^name: *//p' "$f" | head -n 1)
    [ "$name" = "$want" ] || fail skills "$f: frontmatter name '$name' must equal the folder '$want'"
    desc=$(sed -n '2,/^---$/s/^description: *//p' "$f" | head -n 1)
    skill_description_shape "$f" "$desc"
    case "$desc" in \"*\") desc=${desc#\"}; desc=${desc%\"} ;; esac
    if [ "$(printf '%s' "$desc" | wc -m | tr -d ' ')" -gt 300 ]; then
      known skills "$f" || fail skills "$f: description is $(printf '%s' "$desc" | wc -m | tr -d ' ') characters (max 300)"
    fi
    [ "$(wc -l <"$f" | tr -d ' ')" -le 500 ] || fail skills "$f has more than 500 lines"
    for ref in $(grep -oE 'references/[A-Za-z0-9_.-]+\.md' "$f" | sort -u); do
      [ -f "$dir/$ref" ] || fail skills "$f names $ref, which does not exist"
    done
    for n in "$dir"/references/*.md; do
      [ -f "$n" ] || continue
      grep -q "references/$(basename "$n")" "$f" || fail skills "$n is not named in $f (an unlisted reference is never read)"
    done
    for n in $(find "$dir" -name '*.md' | sort); do
      if [ -n "$(awk 'length > 500 { print FNR; exit }' "$n")" ]; then
        known skills "$n" || fail skills "$n has a line over 500 characters (line $(awk 'length > 500 { print FNR; exit }' "$n")); split it"
      fi
    done
  done
}

# 15. The one-doc-per-subsystem files are gone; skills replaced them. A path to one in any file on disk
# (tracked or not yet added; skills included) is a dead link. Only docs/protocol.md and docs/file-format.md
# remain as contract docs. Code comments (engine/ and mac/ lines holding `//`) may cite no skill and
# no doc except docs/protocol.md, docs/file-format.md, REQUIREMENTS.md and CLAUDE.md: that also catches a
# bare control.md or a `see "Section"` pointer into a removed doc.
comment_citations_scan() { grep -nE '//.*(ARCHITECTURE|\.claude/skills|[Ss]kills? `[a-z]|[Ss]kill [a-z]+-[a-z]+|\(see "[A-Z]|[A-Za-z0-9_./-]+\.md)' "$@" 2>/dev/null |
  grep -vE '//.*(docs/protocol\.md|docs/file-format\.md|REQUIREMENTS\.md|CLAUDE\.md)' || true; }
dead_docs() {
  local hits d
  d=$(mktemp -d)
  printf '// see control.md\n' >"$d/a.rs"
  printf '// load skill `audio-mixer`\n' >"$d/b.rs"
  printf '/// (see "Chains and the chain bound")\n' >"$d/c.rs"
  printf '// ARCHITECTURE says\n' >"$d/d.rs"
  printf '// docs/protocol.md and REQUIREMENTS.md, CLAUDE.md; a skill is a file\n' >"$d/e.rs"
  printf '// (`protocol.md`)\n' >"$d/f.rs"
  for n in a b c d f; do
    [ -n "$(comment_citations_scan "$d/$n.rs")" ] || fail dead-docs-cases "a comment citation in $n.rs must be found"
  done
  [ -z "$(comment_citations_scan "$d/e.rs")" ] || fail dead-docs-cases "allowed citations and the word skill must not be found"
  rm -rf "$d"
  hits=$(git ls-files -co --exclude-standard | sort -u | grep -v -e '^scripts/check-repo\.sh$' |
    xargs grep -nE 'docs/(ARCHITECTURE|control|cue-engine|audio|osc|network|commands|persistence|logging)\.md|docs/handoff/|docs/todo/|ARCHITECTURE §' 2>/dev/null | head -n 20 || true)
  [ -z "$hits" ] || fail dead-docs "reference to a deleted doc (name the skill, or drop the citation; code comments cite only docs/protocol.md and docs/file-format.md):
$hits"
  hits=$(comment_citations_scan $(git ls-files -co --exclude-standard 'engine/*.rs' 'mac/*.swift' | sort -u) | head -n 20)
  [ -z "$hits" ] || fail dead-docs "comment cites a skill or a doc that is not docs/protocol.md, docs/file-format.md, REQUIREMENTS.md or CLAUDE.md (comments never cite skills; say the rule in the comment):
$hits"
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
wall_clock_cases
one_gate
prepush_no_stamp
soak_classifier
wedged_classifier
wedged_run
test_list_cases
ui_smoke
test_mac_split
task_ids
skill_shape_cases
skills
dead_docs

if [ "$failed" = 0 ]; then
  echo "check-repo: ok"
else
  echo "check-repo: FAILED"
  exit 1
fi
