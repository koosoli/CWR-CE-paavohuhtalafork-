#!/usr/bin/env python3
"""Validate the shipped-renderer-systems register.

The register (docs/roadmap/renderer-systems-ledger.yaml) exists because RND-030
found that the renderer systems which actually ship had no ticket, owner or
evidence record. A hand-maintained table of that kind drifts silently, which is
how the reconciliation report came to cite `MAX_CASCADES = 4` as proof the
cascaded-shadow-map plan had landed -- 4 being precisely the value that plan
exists to change.

So the useful check is not that the file parses. It is that every recorded
production call site still exists in the branch: the file is tracked, and it
contains the symbol. That turns "this system ships" from an assertion into
something the build can refuse.

Deliberately parsed line-wise rather than with PyYAML, matching
validate_preview0_ledger.py, so this runs on a bare checkout with no
third-party packages.

    python scripts/validate_renderer_ledger.py
"""

from __future__ import annotations

import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
LEDGER = ROOT / "docs/roadmap/renderer-systems-ledger.yaml"

STATUSES = {"SHIPPED", "PARTIAL", "SHIPPED_DISABLED"}
REQUIRED = ("title:", "plan:", "status:", "owner:", "audited:", "completeness:")


def tracked(rel_path: str) -> bool:
    """True when git tracks the path. Untracked evidence is not evidence."""
    result = subprocess.run(
        ["git", "ls-files", "--error-unmatch", rel_path],
        cwd=ROOT,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    return result.returncode == 0


def entries(text: str) -> list[tuple[str, str]]:
    """(id, block) for each system, split on the two-space list marker."""
    parts = text.split("\n  - id: ")
    out = []
    for part in parts[1:]:
        ident = part.split("\n", 1)[0].strip()
        out.append((ident, part))
    return out


def call_sites(block: str) -> list[tuple[str, str]]:
    """(file, symbol) pairs, in order, from a system's production_call_sites."""
    section = block.split("production_call_sites:", 1)
    if len(section) == 1:
        return []
    pairs = []
    pending_file: str | None = None
    for line in section[1].splitlines():
        stripped = line.strip()
        if stripped.startswith("- file:"):
            pending_file = stripped.removeprefix("- file:").strip()
        elif stripped.startswith("symbol:") and pending_file is not None:
            pairs.append((pending_file, stripped.removeprefix("symbol:").strip()))
            pending_file = None
        elif stripped.startswith("- id:") or (stripped and not line.startswith(" " * 6)):
            break
    return pairs


def main() -> int:
    if not LEDGER.exists():
        print(f"renderer ledger missing: {LEDGER}", file=sys.stderr)
        return 1
    text = LEDGER.read_text(encoding="utf-8")
    systems = entries(text)
    if not systems:
        print("renderer ledger defines no systems", file=sys.stderr)
        return 1

    problems: list[str] = []
    seen: set[str] = set()
    checked_sites = 0

    for ident, block in systems:
        if ident in seen:
            problems.append(f"{ident}: duplicate id")
        seen.add(ident)
        if not re.fullmatch(r"RSYS-[A-Z0-9-]+", ident):
            problems.append(f"{ident}: id must look like RSYS-SOMETHING")

        for field in REQUIRED:
            if f"    {field}" not in block:
                problems.append(f"{ident}: missing {field.rstrip(':')}")

        status_match = re.search(r"^    status: (\S+)", block, re.MULTILINE)
        if status_match and status_match.group(1) not in STATUSES:
            problems.append(f"{ident}: unknown status {status_match.group(1)}")

        plan_match = re.search(r"^    plan: (\S+)", block, re.MULTILINE)
        if plan_match and not tracked(plan_match.group(1)):
            problems.append(f"{ident}: plan not tracked: {plan_match.group(1)}")

        sites = call_sites(block)
        if not sites:
            problems.append(f"{ident}: no production_call_sites")
        for rel_path, symbol in sites:
            checked_sites += 1
            if not tracked(rel_path):
                problems.append(f"{ident}: call site not tracked: {rel_path}")
                continue
            # The whole point: a system claimed to ship must still be findable.
            content = (ROOT / rel_path).read_text(encoding="utf-8", errors="replace")
            if symbol not in content:
                problems.append(
                    f"{ident}: {rel_path} no longer contains '{symbol}' — "
                    f"the system moved, was renamed, or never shipped"
                )

    if problems:
        print("renderer ledger invalid:", file=sys.stderr)
        for problem in problems:
            print(f"  - {problem}", file=sys.stderr)
        return 1

    print(
        f"renderer ledger valid: {len(systems)} systems, "
        f"{checked_sites} production call sites verified against the branch"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
