#!/usr/bin/env python3
"""Fail when two integration tests would write the same screenshot file.

Trident polls for `{seq:03}_{label}.png` in one shared output directory, where
`seq` restarts at 0 for every test. A screenshot's filename is therefore unique
only if no other test uses the same label at the same position.

Serially a clash merely overwrote a file. Under `-j` it is a false pass: a test
waits for its screenshot to appear, another test writes that exact name first,
and the waiter reports success without its own capture ever being taken.

Run from the repository root:

    python scripts/check_screenshot_labels.py
"""
from __future__ import annotations

import pathlib
import re
import sys
from collections import defaultdict

ROOT = pathlib.Path(__file__).resolve().parents[1]
TESTS = ROOT / "tests" / "integration"
SHOT = re.compile(r'^\s*triScreenshot\s+"([^"]+)"', re.MULTILINE)


def main() -> int:
    if not TESTS.is_dir():
        print(f"no integration tests at {TESTS}", file=sys.stderr)
        return 1

    # stem -> [test files producing it]
    stems: dict[str, list[str]] = defaultdict(list)
    for path in sorted(TESTS.rglob("*.test.sqf")):
        text = path.read_text(encoding="utf-8", errors="replace")
        for seq, label in enumerate(SHOT.findall(text)):
            stems[f"{seq:03}_{label}"].append(str(path.relative_to(ROOT)).replace("\\", "/"))

    clashes = {stem: files for stem, files in stems.items() if len(files) > 1}
    if not clashes:
        print(f"screenshot labels OK: {len(stems)} unique capture stems")
        return 0

    # Plain ASCII: this runs on a Windows console too, where a stray em-dash
    # comes out as a replacement character in the middle of a failure message.
    print("Screenshot filename clash - these tests would write the same file:", file=sys.stderr)
    for stem in sorted(clashes):
        print(f"  {stem}.png", file=sys.stderr)
        for f in clashes[stem]:
            print(f"      {f}", file=sys.stderr)
    print(
        "\nRename one label so each capture is unique. Under parallel runs a clash "
        "is a false pass, not just an overwritten file.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
