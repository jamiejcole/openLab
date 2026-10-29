#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Checks a built openLab.app: app and engine are universal (arm64 + x86_64), every slice targets macOS 12.0,
# the bundle's signature is valid, and no fake engine is in it.
set -euo pipefail

app="${1:-build/Build/Products/Release/openLab.app}"

for bin in "$app/Contents/MacOS/openLab" "$app/Contents/MacOS/openlab-engine"; do
    for arch in arm64 x86_64; do
        # This lipo accepts one arch per -verify_arch.
        lipo "$bin" -verify_arch "$arch" || { echo "error: $bin has no $arch slice" >&2; exit 1; }
        minos=$(vtool -arch "$arch" -show-build "$bin" | awk '$1 == "minos" { print $2 }')
        if [ "$minos" != "12.0" ]; then
            echo "error: $bin ($arch) has minos '$minos', expected 12.0" >&2
            exit 1
        fi
    done
done
# The fake engine and its fault switches are Debug only (project.yml excludes them from Release).
if grep -q -a "FakeEngine" "$app/Contents/MacOS/openLab"; then
    echo "error: $app contains the fake engine" >&2
    exit 1
fi
if [ -e "$app/Contents/Resources/welcome.json" ]; then
    echo "error: $app bundles the fake engine's welcome.json" >&2
    exit 1
fi
codesign --verify --deep --strict "$app"
echo "verify-bundle: $app ok"
