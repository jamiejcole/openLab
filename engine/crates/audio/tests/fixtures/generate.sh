#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Regenerates the compressed decode fixtures from a synthetic test tone (no third-party audio):
# 0.5 s, 44.1 kHz stereo, left 440 Hz at 0.5, right 1000 Hz at 0.25.
# Requires ffmpeg with libmp3lame. WAV and AIFF fixtures are written by the tests.
set -eu
cd "$(dirname "$0")"
tmp=$(mktemp -t tone)
src="$tmp.wav"
ffmpeg -v error -y \
    -f lavfi -i "sine=frequency=440:sample_rate=44100:duration=0.5" \
    -f lavfi -i "sine=frequency=1000:sample_rate=44100:duration=0.5" \
    -filter_complex "[0]volume=4[l];[1]volume=2[r];[l][r]join=inputs=2:channel_layout=stereo" \
    -c:a pcm_s16le "$src"
ffmpeg -v error -y -i "$src" -c:a flac tone.flac
ffmpeg -v error -y -i "$src" -c:a libmp3lame -b:a 128k tone.mp3
ffmpeg -v error -y -i "$src" -c:a aac -b:a 128k tone.m4a
ffmpeg -v error -y -i "$src" -c:a alac tone-alac.m4a
ffmpeg -v error -y -i "$src" -c:a vorbis -strict -2 -q:a 4 tone.ogg
# A longer Vorbis file: symphonia drops a damaged Ogg page and resynchronises only when the
# stream has more pages than its probe scans.
ffmpeg -v error -y -stream_loop 7 -i "$src" -c:a vorbis -strict -2 -q:a 4 tone-4s.ogg
rm -f "$tmp" "$src"
