#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Writes the media the local QLab fixture workspaces use (skill qlab-import): one-second sine
tones at 48 kHz, 16-bit (audio/tone-mono.wav at 440 Hz, audio/tone-stereo.wav at 660 Hz) and a
solid 64 x 36 PNG (video/solid.png), into the folder given. Standard library only.

    python3 scripts/qlab-fixture-media.py /tmp/qlfx/<workspace>
"""
import math
import os
import struct
import sys
import wave
import zlib


def tone(path, channels, freq, seconds=1.0, rate=48000, amp=0.25):
    frames = bytearray()
    for i in range(int(rate * seconds)):
        s = int(amp * 32767 * math.sin(2 * math.pi * freq * i / rate))
        for c in range(channels):
            frames += struct.pack('<h', s if c == 0 else s // 2)
    with wave.open(path, 'wb') as w:
        w.setnchannels(channels)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(bytes(frames))


def png(path, w=64, h=36, rgb=(0, 128, 255)):
    raw = b''.join(b'\x00' + bytes(rgb) * w for _ in range(h))

    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))

    with open(path, 'wb') as f:
        f.write(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0))
                + chunk(b'IDAT', zlib.compress(raw, 9)) + chunk(b'IEND', b''))


def main():
    if len(sys.argv) != 2:
        sys.exit('usage: qlab-fixture-media.py <workspace folder>')
    folder = sys.argv[1]
    os.makedirs(os.path.join(folder, 'audio'), exist_ok=True)
    os.makedirs(os.path.join(folder, 'video'), exist_ok=True)
    tone(os.path.join(folder, 'audio', 'tone-mono.wav'), 1, 440.0)
    tone(os.path.join(folder, 'audio', 'tone-stereo.wav'), 2, 660.0)
    png(os.path.join(folder, 'video', 'solid.png'))


if __name__ == '__main__':
    main()
