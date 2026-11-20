#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Writes or checks THIRD-PARTY-NOTICES.txt: the licence notices for every third-party Rust crate
linked into the shipped binaries (openlab-engine and openlab-stage, both Apple targets).

  third-party-notices.py generate   rewrite THIRD-PARTY-NOTICES.txt from `cargo metadata`
  third-party-notices.py check [FILE]
        FILE (default: the repo's copy) must list every shipped crate at its locked version. With the
        crate sources available this uses the real dependency graph; without them (a CI job that
        never fetched crates) it only checks the file against Cargo.lock and says so.
"""
import hashlib
import json
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ENGINE = os.path.join(ROOT, "engine")
OUT = os.path.join(ROOT, "THIRD-PARTY-NOTICES.txt")
SHIPPED = ["openlab-engine", "openlab-stage"]
TARGETS = ["aarch64-apple-darwin", "x86_64-apple-darwin"]
LICENCE_NAME = re.compile(r"^(LICENSE|LICENCE|COPYING|NOTICE|UNLICENSE)([-_.].*)?$", re.I)
MARK = "== Crates =="


def metadata(target):
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked", "--offline", "--filter-platform", target],
        cwd=ENGINE, capture_output=True, text=True,
    )
    if out.returncode != 0:
        raise RuntimeError(out.stderr.strip() or "cargo metadata failed")
    return json.loads(out.stdout)


def shipped_crates():
    """{(name, version): package} of third-party crates the shipped binaries link (normal deps only)."""
    found = {}
    for target in TARGETS:
        md = metadata(target)
        pkgs = {p["id"]: p for p in md["packages"]}
        nodes = {n["id"]: n for n in md["resolve"]["nodes"]}
        roots = [p["id"] for p in md["packages"] if p["name"] in SHIPPED and p["source"] is None]
        seen, stack = set(), list(roots)
        while stack:
            pid = stack.pop()
            if pid in seen:
                continue
            seen.add(pid)
            for dep in nodes[pid]["deps"]:
                if any(k["kind"] is None for k in dep["dep_kinds"]):
                    stack.append(dep["pkg"])
        for pid in seen:
            p = pkgs[pid]
            if p["source"] is None:
                continue
            if any("proc-macro" in t["kind"] for t in p["targets"]):
                continue  # runs at build time, is not in the binary
            found[(p["name"], p["version"])] = p
    return found


def licence_files(pkg):
    d = os.path.dirname(pkg["manifest_path"])
    names = []
    for root, dirs, files in os.walk(d):
        if os.path.relpath(root, d).count(os.sep) >= 1:
            dirs[:] = []
        for f in files:
            if LICENCE_NAME.match(f) and os.path.getsize(os.path.join(root, f)) > 0:
                names.append(os.path.join(root, f))
    return sorted(names)


def generate():
    crates = shipped_crates()
    keys = sorted(crates)
    texts = {}  # sha -> (text, [crate labels])
    lines = []
    for name, ver in keys:
        p = crates[(name, ver)]
        lic = p.get("license") or p.get("license_file") or "see licence text below"
        label = f"{name} {ver}"
        lines.append(f"{label} - {lic}")
        for f in licence_files(p):
            data = open(f, "rb").read().decode("utf-8", "replace").replace("\r\n", "\n").strip()
            sha = hashlib.sha256(data.encode()).hexdigest()
            texts.setdefault(sha, (data, []))[1].append(label)
    rule = "=" * 78
    out = [
        "openLab third-party notices",
        "",
        "openLab is free software under the GNU General Public License, version 3 or (at your option)",
        "any later version; the full text is in the file LICENSE next to this one. It includes the",
        "third-party Rust crates listed below, each under the licence named beside it. The licence",
        "texts and copyright notices those licences require follow the list.",
        "",
        "Source code for the MPL-2.0 components (the symphonia crates and any other crate listed",
        "below under MPL-2.0) is available unmodified at https://crates.io/crates/<name>/<version>",
        "(for example https://crates.io/crates/symphonia/0.6.1) and from the repository named on",
        "that page; openLab's own source, with the locked version of every crate (engine/Cargo.lock),",
        "is at the project's release page.",
        "",
        MARK,
        "",
    ]
    out += lines
    out += ["", "== Licence texts and copyright notices ==", ""]
    for sha in sorted(texts, key=lambda s: texts[s][1][0]):
        data, who = texts[sha]
        out += [rule, "Applies to: " + ", ".join(who), rule, "", data, ""]
    missing = [f"{n} {v}" for (n, v) in keys if not any((n + " " + v) in w for _, w in texts.values())]
    if missing:
        out += [rule, "Crates whose package has no licence file; each is under the licence named in the list above, whose standard text appears in this file:",
                ", ".join(missing), ""]
    with open(OUT, "w", encoding="utf-8") as fh:
        fh.write("\n".join(out))
    print(f"third-party-notices: wrote {len(keys)} crates to {OUT}")


def listed(path):
    with open(path, encoding="utf-8") as fh:
        body = fh.read()
    if MARK not in body:
        raise RuntimeError(f"{path}: no crate list")
    got = set()
    for line in body.split(MARK, 1)[1].split("== Licence texts", 1)[0].splitlines():
        m = re.match(r"^(\S+) (\d\S*) - ", line)
        if m:
            got.add((m.group(1), m.group(2)))
    return got


def locked():
    got = set()
    text = open(os.path.join(ENGINE, "Cargo.lock"), encoding="utf-8").read()
    for blk in text.split("[[package]]")[1:]:
        n = re.search(r'^name = "(.*)"', blk, re.M)
        v = re.search(r'^version = "(.*)"', blk, re.M)
        if n and v and "source = " in blk:
            got.add((n.group(1), v.group(1)))
    return got


def check(path):
    try:
        have = listed(path)
    except (OSError, RuntimeError) as e:
        print(f"third-party-notices: {e}", file=sys.stderr)
        return 1
    try:
        want = set(shipped_crates())
        mode = "dependency graph"
    except (RuntimeError, OSError, KeyError):
        # Crate sources not fetched here: the file may not name anything the lock does not have.
        want = None
        mode = "Cargo.lock only"
    if want is not None:
        missing = sorted(want - have)
        stale = sorted(have - want)
    else:
        missing = []
        stale = sorted(have - locked())
    if not have:
        missing = ["(the list is empty)"]
    if missing or stale:
        if missing:
            print("third-party-notices: shipped crates missing from " + path + ": "
                  + ", ".join(m if isinstance(m, str) else f"{m[0]} {m[1]}" for m in missing), file=sys.stderr)
        if stale:
            print("third-party-notices: listed but not shipped at that version: "
                  + ", ".join(f"{n} {v}" for n, v in stale), file=sys.stderr)
        print("third-party-notices: run scripts/third-party-notices.py generate", file=sys.stderr)
        return 1
    print(f"third-party-notices: {path} ok ({len(have)} crates, checked against {mode})")
    return 0


def main(argv):
    if len(argv) >= 2 and argv[1] == "generate":
        generate()
        return 0
    if len(argv) >= 2 and argv[1] == "check":
        return check(argv[2] if len(argv) > 2 else OUT)
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
