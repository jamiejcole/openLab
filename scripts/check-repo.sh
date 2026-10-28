#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Repo hygiene checks that need no toolchain; run by `make lint` and the `checks` CI job.
# Runs every check and lists every failure, so one run shows all of them.
#
# Known failures that an open branch fixes go in scripts/check-repo-known.txt (`check | item | reason`),
# never into a weaker check; delete the entry in the commit that fixes the finding.
# Behaviour-changing uses of debug_assertions are allowed only for files listed in
# scripts/debug-assertions-allowlist.txt (`path | reason`).
set -u

cd "$(git rev-parse --show-toplevel)" || exit 2
KNOWN=scripts/check-repo-known.txt
DEBUG_ALLOW=scripts/debug-assertions-allowlist.txt
failed=0

fail() { echo "FAIL [$1] $2"; failed=1; }

# known <check> <item>: true when the item is listed, with a reason, as a known failure.
known() {
  awk -F' *\\| *' -v c="$1" -v i="$2" '$1 == c && $2 == i && $3 != "" { found = 1 } END { exit !found }' "$KNOWN" 2>/dev/null
}

# 1. SPDX header on every source file (CLAUDE.md, "Every source file").
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
  [ -z "$hits" ] && return 0
  while IFS= read -r f; do
    awk -F' *\\| *' -v p="$f" '$1 == p && $2 != "" { found = 1 } END { exit !found }' "$DEBUG_ALLOW" 2>/dev/null && continue
    fail debug-assertions "$f uses debug_assertions and is not in $DEBUG_ALLOW; make debug and release behave the same, or list the file there with a reason (only detectors that log or count qualify)"
  done <<<"$hits"
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

# 5. UI tests run against FakeEngine; the docs must not claim otherwise until a real-engine UI job exists.
headless() {
  grep -q 'real-engine-ui:' .github/workflows/ci.yml && return 0
  local f
  for f in $(git ls-files -co --exclude-standard '*.md' | sort -u | xargs grep -li 'real headless engine'); do
    if known headless-claim "$f"; then continue; fi
    fail headless-claim "$f says 'real headless engine', but no real-engine UI test job exists (job id real-engine-ui in ci.yml); say 'FakeEngine'"
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

spdx
strings_keys
debug_assertions
versions
headless
one_home

if [ "$failed" = 0 ]; then
  echo "check-repo: ok"
else
  echo "check-repo: FAILED"
  exit 1
fi
