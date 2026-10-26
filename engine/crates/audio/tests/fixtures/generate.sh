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
# AAC with a known onset, for the encoder-priming tests: silence for 1000 frames, then the tone
# (left 440 Hz at 0.5, right 1000 Hz at 0.25, phase 0 at frame 1000), 0.5 s in all. ffmpeg's AAC
# encoder delays by 1024 frames; the first file says so in its edit list, the second has none
# (the tests add an iTunSMPB tag to a copy of it).
onset="aevalsrc='if(lt(n,1000),0,0.5*sin(2*PI*440*(n-1000)/44100))|if(lt(n,1000),0,0.25*sin(2*PI*1000*(n-1000)/44100))':s=44100:d=0.5"
ffmpeg -v error -y -f lavfi -i "$onset" -c:a aac -b:a 128k tone-onset.m4a
ffmpeg -v error -y -f lavfi -i "$onset" -c:a aac -b:a 128k -use_editlist 0 tone-onset-noedit.m4a
# Digital silence for 3 s, then 1 s of tone, as FLAC (lossless, so the silence is exact zeros):
# the seek index's checkpoints in the silence verify at any alignment (L-R7).
silence="aevalsrc='if(lt(n,132300),0,0.5*sin(2*PI*440*(n-132300)/44100))|if(lt(n,132300),0,0.25*sin(2*PI*1000*(n-132300)/44100))':s=44100:d=4"
ffmpeg -v error -y -f lavfi -i "$silence" -c:a flac silence-lead.flac
rm -f "$tmp" "$src"
