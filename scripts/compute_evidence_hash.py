#!/usr/bin/env python3
"""Compute a Preview-0 ticket's evidence hash.

The ledger requires `evidence_hash` on any ticket that reaches VALIDATED or
SHIPPABLE. Until now nothing produced that value and the validator only checked
it was 64 hex characters — so any invented string satisfied it, and the field
attested to nothing.

This derives it from the ticket's own tracked evidence files, so the hash
changes when the evidence changes and a stale or fabricated value is detectable.
`validate_preview0_ledger.py` recomputes it the same way and rejects a mismatch.

    python scripts/compute_evidence_hash.py CORE-NEG-001
    python scripts/compute_evidence_hash.py --all

Content is read through git's filters (as `write_preview0_manifest.py` does), so
the hash is stable across checkouts with different line-ending settings rather
than changing between Windows and Linux.
"""
from __future__ import annotations

import hashlib
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
LEDGER = ROOT / "docs" / "roadmap" / "status-ledger.yaml"
EVIDENCE_LINE = re.compile(r"\s+-\s+((?:docs|engine|apps|scripts|\.github)/\S+)")


def canonical_bytes(rel_path: str) -> bytes:
    """File content as git stores it, independent of checkout line endings."""
    data = (ROOT / rel_path).read_bytes()
    try:
        blob = subprocess.check_output(
            ["git", "hash-object", "--path", rel_path, "--filters", "--stdin"],
            cwd=ROOT,
            input=data,
        ).strip()
        return subprocess.check_output(["git", "cat-file", "blob", blob], cwd=ROOT)
    except (OSError, subprocess.CalledProcessError):
        return data


def ticket_blocks(text: str) -> dict[str, str]:
    blocks: dict[str, str] = {}
    current_id, buf = None, []
    for line in text.splitlines():
        if line.startswith("  - id:"):
            if current_id:
                blocks[current_id] = "\n".join(buf)
            current_id, buf = line.split(":", 1)[1].strip(), [line]
        elif current_id is not None:
            buf.append(line)
    if current_id:
        blocks[current_id] = "\n".join(buf)
    return blocks


def evidence_hash(block: str) -> tuple[str, list[str]]:
    """SHA-256 over each evidence file's path and canonical content.

    Sorted so the value does not depend on the order entries happen to appear
    in, and length-prefixed so no concatenation of two files can collide with a
    different pair.
    """
    paths = sorted({m.group(1) for line in block.splitlines() if (m := EVIDENCE_LINE.match(line))})
    digest = hashlib.sha256()
    for rel in paths:
        if not (ROOT / rel).is_file():
            raise SystemExit(f"evidence file missing: {rel}")
        body = canonical_bytes(rel)
        digest.update(f"{rel}\0{len(body)}\0".encode("utf-8"))
        digest.update(body)
    return digest.hexdigest(), paths


def main(argv: list[str]) -> int:
    if not argv or argv[0] in {"-h", "--help"}:
        print(__doc__)
        return 0
    blocks = ticket_blocks(LEDGER.read_text(encoding="utf-8"))
    wanted = sorted(blocks) if argv[0] == "--all" else [argv[0]]
    for ticket in wanted:
        if ticket not in blocks:
            print(f"unknown ticket: {ticket}", file=sys.stderr)
            return 1
        value, paths = evidence_hash(blocks[ticket])
        if not paths:
            print(f"{ticket}: no tracked evidence files — nothing to hash", file=sys.stderr)
            return 1
        print(f"{ticket}  {value}  ({len(paths)} file(s))")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
