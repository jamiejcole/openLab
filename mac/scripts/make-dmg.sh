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
# Not -quiet, so a failure shows its reason. On CI runners hdiutil fails now and then with
# "Resource busy" while the system still holds the image or the folder (runs 44175952127 and
# 44368937589); only that failure is tried again, up to three times. Any other failure stops here.
attempt=1
while :; do
  rm -f "$dmg"
  if result=$(hdiutil create -volname "openLab $version" -srcfolder "$stage" -fs HFS+ -format UDZO "$dmg" 2>&1); then
    printf '%s\n' "$result"
    break
  fi
  printf '%s\n' "$result" >&2
  if [ "$attempt" -ge 3 ] || ! printf '%s' "$result" | grep -q "Resource busy"; then
    exit 1
  fi
  attempt=$((attempt + 1))
  echo "make-dmg: hdiutil was busy; trying again ($attempt of 3)" >&2
  sleep 5
done
hdiutil verify "$dmg"
echo "make-dmg: $dmg"
