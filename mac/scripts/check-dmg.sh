#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Checks a release disk image: it mounts, and holds "openLab <version>.app" (version = the image's file name),
# an Applications link to /Applications, the licence texts, and a Finder window layout (.DS_Store) that places
# the app and the link.
set -euo pipefail

dmg="${1:?usage: check-dmg.sh <openLab-version.dmg>}"

name=$(basename "$dmg" .dmg)
version=${name#openLab-}
if [ "$name" = "$version" ] || [ -z "$version" ]; then
  echo "error: $dmg is not named openLab-<version>.dmg" >&2
  exit 1
fi

mnt=$(mktemp -d)
cleanup() {
  hdiutil detach "$mnt" >/dev/null 2>&1 || hdiutil detach -force "$mnt" >/dev/null 2>&1 || true
  rmdir "$mnt" 2>/dev/null || true
}
trap cleanup EXIT
hdiutil attach "$dmg" -readonly -nobrowse -noautoopen -mountpoint "$mnt" >/dev/null

fail() { echo "error: $dmg: $1" >&2; exit 1; }
[ -d "$mnt/openLab $version.app" ] || fail "no 'openLab $version.app'"
[ -x "$mnt/openLab $version.app/Contents/MacOS/openLab" ] || fail "the app has no executable"
{ [ -L "$mnt/Applications" ] && [ "$(readlink "$mnt/Applications")" = "/Applications" ]; } || fail "no Applications link to /Applications"
[ -s "$mnt/LICENSE.txt" ] || fail "no LICENSE.txt"
[ -s "$mnt/THIRD-PARTY-NOTICES.txt" ] || fail "no THIRD-PARTY-NOTICES.txt"
[ -s "$mnt/.DS_Store" ] || fail "no Finder window layout (.DS_Store)"
layout=$(python3 -I "$(dirname "$0")/dmg-layout.py" --check "$mnt/.DS_Store") || fail "the Finder window layout is unreadable"
case "$layout" in
  *"openLab $version.app"*Applications* | *Applications*"openLab $version.app"*) ;;
  *) fail "the layout does not place the app and the Applications link ($layout)" ;;
esac
echo "check-dmg: $dmg ok"
