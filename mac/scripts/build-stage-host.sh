#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Xcode build phase: build openlab-stage (the stage host: video output windows) with cargo and wrap it
# in the helper app Contents/Helpers/openLab Stage.app, where the engine looks for it. A helper app of
# its own (LSUIElement: no Dock icon, no menu bar) keeps AppKit from taking it for openLab itself.
# Release builds a universal (arm64 + x86_64) binary without test support; Debug builds the host arch
# only, with `test-support` (virtual displays for the app's tests). verify-bundle.sh checks a Release
# stage host refuses `--virtual-displays`. The helper is signed here, inside-out, before Xcode signs
# the app.
set -euo pipefail

export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
export MACOSX_DEPLOYMENT_TARGET=12.0

cd "$SRCROOT/../engine"
helper="$TARGET_BUILD_DIR/$CONTENTS_FOLDER_PATH/Helpers/openLab Stage.app"
dest="$helper/Contents/MacOS"
mkdir -p "$dest"

if [ "$CONFIGURATION" = "Release" ]; then
    for target in aarch64-apple-darwin x86_64-apple-darwin; do
        cargo build --release --locked -p openlab-video --bin openlab-stage --target "$target"
    done
    lipo -create -output "$dest/openlab-stage" \
        target/aarch64-apple-darwin/release/openlab-stage \
        target/x86_64-apple-darwin/release/openlab-stage
else
    cargo build --locked -p openlab-video --bin openlab-stage --features test-support
    cp target/debug/openlab-stage "$dest/openlab-stage"
fi

plist="$helper/Contents/Info.plist"
rm -f "$plist"
/usr/libexec/PlistBuddy \
    -c "Add :CFBundleIdentifier string dev.openlab.openLab.stage" \
    -c "Add :CFBundleExecutable string openlab-stage" \
    -c "Add :CFBundleName string openLab Stage" \
    -c "Add :CFBundlePackageType string APPL" \
    -c "Add :CFBundleShortVersionString string ${MARKETING_VERSION}" \
    -c "Add :CFBundleVersion string ${CURRENT_PROJECT_VERSION}" \
    -c "Add :LSMinimumSystemVersion string 12.0" \
    -c "Add :LSUIElement bool true" \
    -c "Add :NSHighResolutionCapable bool true" \
    -c "Add :NSHumanReadableCopyright string Licensed under the GNU GPL v3 or later." \
    "$plist"

# Inside-out: the helper's executable and then the helper bundle, before the app's own signature.
codesign --force --sign "${EXPANDED_CODE_SIGN_IDENTITY:--}" --identifier dev.openlab.openLab.stage "$dest/openlab-stage"
codesign --force --sign "${EXPANDED_CODE_SIGN_IDENTITY:--}" --identifier dev.openlab.openLab.stage "$helper"
