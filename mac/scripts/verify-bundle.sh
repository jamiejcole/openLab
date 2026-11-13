#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Checks a built openLab.app: app, engine and stage host are universal (arm64 + x86_64), every slice targets
# macOS 12.0, the bundle's and the stage host helper's signatures are valid, the app cannot start the test
# engine, the engine has no virtual audio device and links no GUI framework, and neither engine nor stage
# host takes test-only displays.
set -euo pipefail

app="${1:-build/Build/Products/Release/openLab.app}"

helper="$app/Contents/Helpers/openLab Stage.app"
stage="$helper/Contents/MacOS/openlab-stage"
if [ ! -x "$stage" ]; then
    echo "error: $stage is missing (the engine looks for the stage host there)" >&2
    exit 1
fi
for bin in "$app/Contents/MacOS/openLab" "$app/Contents/MacOS/openlab-engine" "$stage"; do
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
for marker in "-OpenLabEngine" "-OpenLabVirtualDevice" "-OpenLabVirtualDisplays" "--virtual-device" \
    "--virtual-displays" "lose_device" "restore_device" "stage_probe"; do
    if grep -q -a -- "$marker" "$app/Contents/MacOS/openLab"; then
        echo "error: $app contains test-engine support ('$marker')" >&2
        exit 1
    fi
done
# The engine's virtual audio device and its test-only requests are built only with the test-support
# feature: the bundled engine must not contain it and must refuse
# `--virtual-device` as a usage error (exit 2) without starting. `--watch-stdin` with no stdin ends an
# engine that wrongly started at once.
engine="$app/Contents/MacOS/openlab-engine"
if grep -q -a "openLab Virtual Output" "$engine"; then
    echo "error: $engine contains the virtual audio device" >&2
    exit 1
fi
# Finder opens a show file in the app only if the bundle declares the type.
ext=$(/usr/libexec/PlistBuddy -c "Print :UTExportedTypeDeclarations:0:UTTypeTagSpecification:public.filename-extension:0" "$app/Contents/Info.plist" 2>/dev/null || true)
doc=$(/usr/libexec/PlistBuddy -c "Print :CFBundleDocumentTypes:0:LSItemContentTypes:0" "$app/Contents/Info.plist" 2>/dev/null || true)
if [ "$ext" != "openlab" ] || [ "$doc" != "dev.openlab.show" ]; then
    echo "error: $app does not declare .openlab show files as its documents" >&2
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
# One version for the release: the app's and the bundled engine's agree (mac/project.yml MARKETING_VERSION
# and engine/Cargo.toml), and the app has its icon.
app_version=$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$app/Contents/Info.plist")
engine_version=$("$engine" --version | awk '{ print $2 }')
if [ -z "$app_version" ] || [ "$app_version" != "$engine_version" ]; then
    echo "error: app version '$app_version' and engine version '$engine_version' differ" >&2
    exit 1
fi
icon=$(/usr/libexec/PlistBuddy -c "Print :CFBundleIconName" "$app/Contents/Info.plist" 2>/dev/null || true)
if [ "$icon" != "AppIcon" ] || [ ! -f "$app/Contents/Resources/AppIcon.icns" ]; then
    echo "error: $app has no app icon" >&2
    exit 1
fi
# The stage host's helper app: no Dock icon or menu bar, its own bundle id, signed on its own.
ui_element=$(/usr/libexec/PlistBuddy -c "Print :LSUIElement" "$helper/Contents/Info.plist" 2>/dev/null || true)
helper_id=$(/usr/libexec/PlistBuddy -c "Print :CFBundleIdentifier" "$helper/Contents/Info.plist" 2>/dev/null || true)
if [ "$ui_element" != "true" ] || [ "$helper_id" != "dev.openlab.openLab.stage" ]; then
    echo "error: $helper is not an LSUIElement helper with bundle id dev.openlab.openLab.stage" >&2
    exit 1
fi
codesign --verify --strict "$helper"
# The engine stays free of GUI frameworks: windows are the stage host's.
if otool -L "$engine" | grep -E -q '/(AppKit|Metal|QuartzCore)\.framework/'; then
    echo "error: $engine links AppKit, Metal or QuartzCore" >&2
    exit 1
fi
# Test-only virtual displays: a release engine and stage host refuse the option (exit 2) without starting.
for bin in "$engine" "$stage"; do
    set +e
    if [ "$bin" = "$engine" ]; then
        support=$(mktemp -d)
        "$bin" --virtual-displays V1:8x8 --app-support "$support" --watch-stdin </dev/null >/dev/null 2>&1
        code=$?
        rm -rf "$support"
    else
        "$bin" --virtual-displays V1:8x8 </dev/null >/dev/null 2>&1
        code=$?
    fi
    set -e
    if [ "$code" != 2 ]; then
        echo "error: $bin did not refuse --virtual-displays (exit $code, expected 2)" >&2
        exit 1
    fi
done
codesign --verify --deep --strict "$app"
echo "verify-bundle: $app ok"
