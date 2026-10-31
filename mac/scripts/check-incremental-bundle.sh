#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Regression check: an engine-only change followed by an incremental `make mac` must still leave a validly
# signed bundle. The "Build openlab-engine" phase rewrites Contents/MacOS/openlab-engine; unless Xcode is
# told that file is an output, it skips the app's CodeSign step and the sealed hash no longer matches
# (on Apple silicon the engine is then killed at launch). Slow: two Release builds on top of the first.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
src=engine/crates/engine/src/main.rs
backup="$(mktemp)"
cp "$src" "$backup"
trap 'cp "$backup" "$src"; rm -f "$backup"' EXIT

make mac
make verify-bundle
# A different binary, not just a newer mtime: identical bytes would keep the old seal valid.
printf '\n#[used]\nstatic REBUILD_PROBE: [u8; 8] = *b"probe-%02d";\n' "$((RANDOM % 100))" >>"$src"
make mac
make verify-bundle
echo "check-incremental-bundle: ok"
