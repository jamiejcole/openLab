#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Xcode build phase (and usable by hand): fail when a Localizable.strings file defines a key twice.
# The later definition silently wins, so a stale copy can hide the text a screen expects.
set -euo pipefail

status=0
for file in "${@:-$SRCROOT/openLab/en.lproj/Localizable.strings}"; do
    dups="$(grep -oE '^[[:space:]]*"([^"\\]|\\.)+"[[:space:]]*=' "$file" | sed -E 's/^[[:space:]]*//; s/[[:space:]]*=$//' | sort | uniq -d)"
    if [ -n "$dups" ]; then
        while IFS= read -r key; do
            echo "$file: error: duplicate key $key"
        done <<<"$dups"
        status=1
    fi
done
exit $status
