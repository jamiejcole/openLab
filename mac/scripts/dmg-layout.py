#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Writes the Finder window layout (.DS_Store) of the release disk image, without a Finder session.

usage: dmg-layout.py <out .DS_Store> <app name>
       dmg-layout.py --check <.DS_Store>

The file holds one leaf of the .DS_Store b-tree: the window's bounds, icon view and size for "." and the
position of each item. Finder reads it when the image is opened, so it works on a CI runner where scripting
Finder does not. `--check` reads a file back and prints the window bounds and the placed items (used by check-dmg.sh).
"""
import plistlib
import struct
import sys

WINDOW = (200, 120, 560, 400)  # x, y, width, height
ICON_SIZE = 96.0
# Icon centres inside the window: the app and the Applications link side by side, the licence texts below.
ITEMS = {
    "app": (150, 125),
    "Applications": (410, 125),
    "LICENSE.txt": (150, 285),
    "THIRD-PARTY-NOTICES.txt": (410, 285),
}


def ustr(text):
    return struct.pack(">I", len(text)) + text.encode("utf-16-be")


def record(name, code, kind, value):
    head = ustr(name) + code.encode() + kind.encode()
    if kind == "blob":
        return head + struct.pack(">I", len(value)) + value
    if kind == "long":
        return head + struct.pack(">I", value)
    if kind == "type":
        return head + value.encode()
    raise ValueError(kind)


def records(app_name):
    bwsp = plistlib.dumps(
        {
            "WindowBounds": "{{%d, %d}, {%d, %d}}" % WINDOW,
            "ShowSidebar": False,
            "ShowToolbar": False,
            "ShowStatusBar": False,
            "ShowPathbar": False,
            "ShowTabView": False,
            "ContainerShowSidebar": False,
            "SidebarWidth": 0,
        },
        fmt=plistlib.FMT_BINARY,
    )
    icvp = plistlib.dumps(
        {
            "viewOptionsVersion": 1,
            "backgroundType": 0,
            "backgroundColorRed": 1.0,
            "backgroundColorGreen": 1.0,
            "backgroundColorBlue": 1.0,
            "gridOffsetX": 0.0,
            "gridOffsetY": 0.0,
            "gridSpacing": 100.0,
            "arrangeBy": "none",
            "showIconPreview": True,
            "showItemInfo": False,
            "labelOnBottom": True,
            "iconSize": ICON_SIZE,
            "textSize": 12.0,
            "scrollPositionX": 0.0,
            "scrollPositionY": 0.0,
        },
        fmt=plistlib.FMT_BINARY,
    )
    out = [
        (".", "bwsp", "blob", bwsp),
        (".", "icvp", "blob", icvp),
        (".", "vSrn", "long", 1),
        (".", "vstl", "type", "icnv"),
    ]
    for key, (x, y) in ITEMS.items():
        name = app_name if key == "app" else key
        out.append((name, "Iloc", "blob", struct.pack(">IIII", x, y, 0xFFFFFFFF, 0xFFFF0000)))
    out.sort(key=lambda r: (r[0].lower(), r[1].encode()))
    return out


def pow2_at_least(n, floor=32):
    size = floor
    while size < n:
        size *= 2
    return size


def build(app_name):
    recs = records(app_name)
    leaf = struct.pack(">II", 0, len(recs)) + b"".join(record(*r) for r in recs)
    tree = struct.pack(">IIIII", 2, 0, len(recs), 1, 0x1000)

    root_size = 2048
    leaf_size = pow2_at_least(len(leaf))
    # Blocks sit at offsets (from byte 4) that are multiples of their size; the first 32 bytes are the header.
    tree_off, root_off = 32, root_size
    leaf_off = max(0x1000, leaf_size)
    blocks = [(root_off, root_size), (tree_off, 32), (leaf_off, leaf_size)]

    def addr(off, size):
        return off | (size.bit_length() - 1)

    offsets = [addr(o, s) for o, s in blocks]
    table = struct.pack(">II", len(offsets), 0)
    table += b"".join(struct.pack(">I", a) for a in offsets)
    table += b"\0" * (4 * (256 - len(offsets)))
    table += struct.pack(">I", 1) + bytes([4]) + b"DSDB" + struct.pack(">I", 1)
    table += b"\0\0\0\0" * 32  # empty free lists
    assert len(table) <= root_size

    data = bytearray(4 + leaf_off + leaf_size)
    data[0:4] = struct.pack(">I", 1)
    data[4:8] = b"Bud1"
    struct.pack_into(">III", data, 8, root_off, root_size, root_off)
    data[4 + root_off : 4 + root_off + len(table)] = table
    data[4 + tree_off : 4 + tree_off + len(tree)] = tree
    data[4 + leaf_off : 4 + leaf_off + len(leaf)] = leaf
    return bytes(data)


def read(path):
    """Returns [(name, code, value)] for the leaf of a file written by build(), or raises ValueError."""
    data = open(path, "rb").read()
    if data[4:8] != b"Bud1":
        raise ValueError("not a .DS_Store")
    root_off, root_size, _ = struct.unpack_from(">III", data, 8)
    count = struct.unpack_from(">I", data, 4 + root_off)[0]
    offs = struct.unpack_from(">%dI" % count, data, 4 + root_off + 8)
    node_addr = offs[2]
    node = 4 + (node_addr & ~0x1F)
    pointer, n = struct.unpack_from(">II", data, node)
    if pointer != 0:
        raise ValueError("not a single-leaf file")
    pos = node + 8
    out = []
    for _ in range(n):
        (length,) = struct.unpack_from(">I", data, pos)
        name = data[pos + 4 : pos + 4 + 2 * length].decode("utf-16-be")
        pos += 4 + 2 * length
        code, kind = data[pos : pos + 4].decode(), data[pos + 4 : pos + 8].decode()
        pos += 8
        if kind == "blob":
            (blen,) = struct.unpack_from(">I", data, pos)
            value = data[pos + 4 : pos + 4 + blen]
            pos += 4 + blen
        elif kind in ("long", "type"):
            value = data[pos : pos + 4]
            pos += 4
        else:
            raise ValueError("unknown type " + kind)
        out.append((name, code, value))
    return out


def main(argv):
    if len(argv) == 3 and argv[1] == "--check":
        recs = read(argv[2])
        names = {r[0] for r in recs if r[1] == "Iloc"}
        bounds = [plistlib.loads(r[2])["WindowBounds"] for r in recs if r[1] == "bwsp"]
        print("bounds %s items %s" % (bounds, sorted(names)))
        return 0
    if len(argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    with open(argv[1], "wb") as f:
        f.write(build(argv[2]))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
