#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Xcode build phase: build openlab-engine with cargo and copy it to Contents/MacOS.
# Release builds a universal (arm64 + x86_64) binary; Debug builds the host arch only.
set -euo pipefail

# Xcode's PATH does not include rustup's proxies.
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
export MACOSX_DEPLOYMENT_TARGET=12.0

cd "$SRCROOT/../engine"
dest="$TARGET_BUILD_DIR/$EXECUTABLE_FOLDER_PATH"
mkdir -p "$dest"

if [ "$CONFIGURATION" = "Release" ]; then
    for target in aarch64-apple-darwin x86_64-apple-darwin; do
        cargo build --release --locked --bin openlab-engine --target "$target"
    done
    lipo -create -output "$dest/openlab-engine" \
        target/aarch64-apple-darwin/release/openlab-engine \
        target/x86_64-apple-darwin/release/openlab-engine
else
    cargo build --locked --bin openlab-engine
    cp target/debug/openlab-engine "$dest/openlab-engine"
fi

# Xcode signs the bundle but not this nested binary, and the linker only ad-hoc signs the arm64 slice.
codesign --force --sign "${EXPANDED_CODE_SIGN_IDENTITY:--}" --identifier dev.openlab.openlab-engine "$dest/openlab-engine"
