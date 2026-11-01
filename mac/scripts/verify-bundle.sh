#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Checks a built openLab.app: app and engine are universal (arm64 + x86_64), every slice targets macOS 12.0,
# the bundle's signature is valid, the app cannot start the test engine, and the engine has no virtual audio device.
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
# The app's test-engine launch (`-OpenLabEngine test`) and its test-only requests (`advance`,
# `lose_device`, `restore_device`) are Debug only (`#if DEBUG`, mac/openLab/Engine/EngineLaunch.swift
# and ClientProtocol.swift).
for marker in "-OpenLabEngine" "-OpenLabVirtualDevice" "--virtual-device" "lose_device" "restore_device"; do
    if grep -q -a -- "$marker" "$app/Contents/MacOS/openLab"; then
        echo "error: $app contains test-engine support ('$marker')" >&2
        exit 1
    fi
done
# The engine's virtual audio device and its test-only requests are built only with the test-support
# feature (docs/control.md "Virtual device"): the bundled engine must not contain it and must refuse
# `--virtual-device` as a usage error (exit 2) without starting. `--watch-stdin` with no stdin ends an
# engine that wrongly started at once.
engine="$app/Contents/MacOS/openlab-engine"
if grep -q -a "openLab Virtual Output" "$engine"; then
    echo "error: $engine contains the virtual audio device" >&2
    exit 1
fi
support=$(mktemp -d)
set +e
"$engine" --virtual-device stepped --app-support "$support" --watch-stdin </dev/null >/dev/null 2>&1
code=$?
set -e
rm -rf "$support"
if [ "$code" != 2 ]; then
    echo "error: $engine did not refuse --virtual-device (exit $code, expected 2)" >&2
    exit 1
fi
codesign --verify --deep --strict "$app"
echo "verify-bundle: $app ok"
