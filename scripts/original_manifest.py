#!/usr/bin/env python3
"""Read-only content manifest of the original Strobe project (BRIEF-U0).

    original_manifest.py snapshot <out.json>   hash every file (Library/Temp/Logs excluded)
    original_manifest.py compare <a.json> <b.json>   exit 0 only if identical

The original is only ever read here; this is the evidence that the spike
never modified it.
"""
import hashlib
import json
import os
import subprocess
import sys

ORIGINAL = os.path.expanduser("~/strobe-trackb")
EXCLUDED_TOP = {"Library", "Temp", "Logs"}


def snapshot(out_path):
    files = {}
    for root, dirs, names in os.walk(ORIGINAL):
        rel_root = os.path.relpath(root, ORIGINAL)
        if rel_root == ".":
            dirs[:] = sorted(d for d in dirs if d not in EXCLUDED_TOP)
        else:
            dirs.sort()
        for name in sorted(names):
            path = os.path.join(root, name)
            if os.path.islink(path):
                files[os.path.relpath(path, ORIGINAL)] = "symlink:" + os.readlink(path)
                continue
            digest = hashlib.sha256()
            with open(path, "rb") as handle:
                for block in iter(lambda: handle.read(1 << 20), b""):
                    digest.update(block)
            files[os.path.relpath(path, ORIGINAL)] = digest.hexdigest()
    git = lambda *args: subprocess.run(
        ["git", "-C", ORIGINAL, *args], capture_output=True, text=True, check=True
    ).stdout
    record = {
        "head": git("rev-parse", "HEAD").strip(),
        "status": git("status", "--porcelain=v1", "-uall"),
        "files": files,
    }
    with open(out_path, "w") as handle:
        json.dump(record, handle, indent=1, sort_keys=True)
    print(f"files hashed: {len(files)}; head {record['head'][:12]}; "
          f"dirty lines: {len(record['status'].splitlines())}")


def compare(a_path, b_path):
    with open(a_path) as a, open(b_path) as b:
        before, after = json.load(a), json.load(b)
    same = before == after
    print(f"files before {len(before['files'])}, after {len(after['files'])}; "
          f"head equal {before['head'] == after['head']}; "
          f"status equal {before['status'] == after['status']}; identical {same}")
    return 0 if same else 1


if __name__ == "__main__":
    if sys.argv[1:2] == ["snapshot"] and len(sys.argv) == 3:
        snapshot(sys.argv[2])
    elif sys.argv[1:2] == ["compare"] and len(sys.argv) == 4:
        sys.exit(compare(sys.argv[2], sys.argv[3]))
    else:
        sys.exit(__doc__)
