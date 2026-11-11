#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Makes the release disk image from a built, verified openLab.app: <out>/openLab-<version>.dmg holding the app
# and a link to /Applications. The app inside is named "openLab <version>.app" so several versions can sit side
# by side in /Applications and a show can stay on a known-good one.
set -euo pipefail

app="${1:?usage: make-dmg.sh <openLab.app> <out dir>}"
out="${2:?usage: make-dmg.sh <openLab.app> <out dir>}"

"$(dirname "$0")/verify-bundle.sh" "$app"
version=$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$app/Contents/Info.plist")
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
# ditto keeps the signature's extended attributes and symlinks.
ditto "$app" "$stage/openLab $version.app"
ln -s /Applications "$stage/Applications"
codesign --verify --deep --strict "$stage/openLab $version.app"
dmg="$out/openLab-$version.dmg"
rm -f "$dmg"
hdiutil create -quiet -volname "openLab $version" -srcfolder "$stage" -fs HFS+ -format UDZO "$dmg"
hdiutil verify -quiet "$dmg"
echo "make-dmg: $dmg"
