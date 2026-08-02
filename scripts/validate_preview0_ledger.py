#!/usr/bin/env python3
"""Validate the Preview-0 overlay and status ledger without external packages."""
from __future__ import annotations

import hashlib
import json
import pathlib
import re
import subprocess
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
OVERLAY = ROOT / "docs/roadmap/modernisation/PoseidonWGPU_Execution_Overlay_Preview0_v1.2.1.yaml"
LEDGER = ROOT / "docs/roadmap/status-ledger.yaml"
MANIFEST = ROOT / "docs/roadmap/evidence/preview0-manifest.json"
CHECKSUMS = ROOT / "docs/roadmap/modernisation/PoseidonWGPU_Roadmap_4.4.1_CHECKSUMS_Overlay_v1.2.1.txt"


def text(path: pathlib.Path) -> str:
    if not path.is_file():
        raise ValueError(f"missing: {path.relative_to(ROOT)}")
    return path.read_text(encoding="utf-8")


def ids(doc: str, key: str) -> list[str]:
    lines = doc.splitlines()
    try:
        start = next(i for i, line in enumerate(lines) if line == f"{key}:") + 1
    except StopIteration as exc:
        raise ValueError(f"missing overlay key: {key}") from exc
    found = []
    for line in lines[start:]:
        if line and not line.startswith(" "):
            break
        stripped = line.strip()
        if stripped.startswith("- id: "):
            found.append(stripped.removeprefix("- id: "))
        elif key in {"preview_blockers", "authorised_tickets"} and stripped.startswith("- "):
            found.append(stripped.removeprefix("- "))
    return found


def ticket_block(doc: str, ticket: str) -> str:
    marker = f"- id: {ticket}"
    try:
        return marker + doc.split(marker, 1)[1].split("\n  - id:", 1)[0]
    except IndexError as exc:
        raise ValueError(f"missing ledger record: {ticket}") from exc


def value(block: str, field: str) -> str:
    for line in block.splitlines():
        if line.strip().startswith(f"{field}:"):
            return line.split(":", 1)[1].strip()
    raise ValueError(f"missing {field}")


def git(*args: str) -> str:
    result = subprocess.run(("git", *args), cwd=ROOT, text=True, capture_output=True)
    if result.returncode:
        raise ValueError(result.stderr.strip() or f"git {' '.join(args)} failed")
    return result.stdout.strip()


def git_bytes(*args: str) -> bytes:
    result = subprocess.run(("git", *args), cwd=ROOT, capture_output=True)
    if result.returncode:
        message = result.stderr.decode("utf-8", errors="replace").strip()
        raise ValueError(message or f"git {' '.join(args)} failed")
    return result.stdout


def tracked_file(raw_path: str, context: str) -> pathlib.Path:
    path = ROOT / raw_path
    if not path.is_file():
        raise ValueError(f"{context} missing file: {raw_path}")
    git("ls-files", "--error-unmatch", "--", raw_path)
    return path


def evidence_paths(block: str) -> list[str]:
    paths: list[str] = []
    for line in block.splitlines():
        match = re.match(r"\s+-\s+((?:docs|engine|apps|scripts|\.github)/\S+)", line)
        if match:
            paths.append(match.group(1))
    return paths


def list_values(raw: str) -> list[str]:
    return [item for item in raw.strip("[]").replace(" ", "").split(",") if item]


def verify_production_call_sites(block: str, ticket: str) -> None:
    """Require stable file/symbol records instead of prose-only call sites."""
    lines = block.splitlines()
    start = next(i for i, line in enumerate(lines) if line.strip().startswith("production_call_sites:"))
    if lines[start].strip() == "production_call_sites: []":
        return
    entries: list[dict[str, str]] = []
    current: dict[str, str] | None = None
    for line in lines[start + 1:]:
        if line.startswith("    ") and not line.startswith("      "):
            break
        stripped = line.strip()
        if stripped.startswith("- file: "):
            if current is not None:
                entries.append(current)
            current = {"file": stripped.removeprefix("- file: ")}
        elif current is not None and stripped.startswith("symbol: "):
            current["symbol"] = stripped.removeprefix("symbol: ")
    if current is not None:
        entries.append(current)
    for entry in entries:
        if not entry.get("file") or not entry.get("symbol"):
            raise ValueError(f"{ticket} production call site needs file and symbol")
        tracked_file(entry["file"], f"{ticket} production call site")
    raw_lines = [line.strip() for line in lines[start + 1:] if line.startswith("      - ")]
    if raw_lines and not entries:
        raise ValueError(f"{ticket} production call sites must use file/symbol records")


def verify_clean_manifest() -> None:
    """Verify the tracked Preview-0 evidence bundle was made from a clean tree."""
    tracked_file(str(MANIFEST.relative_to(ROOT)), "Preview-0 manifest")
    try:
        manifest = json.loads(text(MANIFEST))
    except json.JSONDecodeError as exc:
        raise ValueError(f"invalid Preview-0 manifest JSON: {exc}") from exc
    if manifest.get("git_dirty") is not False:
        raise ValueError("Preview-0 manifest must record git_dirty: false")
    commit = manifest.get("git_commit")
    if not isinstance(commit, str) or not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("Preview-0 manifest must record a full git commit")
    git("cat-file", "-e", f"{commit}^{{commit}}")

    for section, required_keys in (
        ("runtime_check", ("log", "sha256")),
        ("capture", ("path", "sha256")),
        ("metrics", ("path", "sha256")),
    ):
        entry = manifest.get(section)
        if not isinstance(entry, dict) or not all(key in entry for key in required_keys):
            raise ValueError(f"Preview-0 manifest missing {section} evidence")
        raw_path = entry.get("path", entry.get("log"))
        digest = entry["sha256"]
        if not isinstance(raw_path, str) or not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
            raise ValueError(f"Preview-0 manifest has invalid {section} evidence metadata")
        evidence_path = tracked_file(raw_path.replace("\\", "/"), f"Preview-0 manifest {section}")
        if hashlib.sha256(evidence_path.read_bytes()).hexdigest() != digest:
            raise ValueError(f"Preview-0 manifest {section} evidence hash mismatch")

    shaders = manifest.get("shaders")
    if not isinstance(shaders, list) or not shaders:
        raise ValueError("Preview-0 manifest must list shader hashes")
    for shader in shaders:
        if not isinstance(shader, dict):
            raise ValueError("Preview-0 manifest has malformed shader metadata")
        raw_path, digest = shader.get("path"), shader.get("sha256")
        if not isinstance(raw_path, str) or not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
            raise ValueError("Preview-0 manifest has invalid shader hash")
        shader_path = raw_path.replace("\\", "/")
        # A clean capture may intentionally describe an earlier, pinned commit;
        # validate against that commit rather than the mutable checkout HEAD.
        shader_bytes = git_bytes("show", f"{commit}:{shader_path}")
        if hashlib.sha256(shader_bytes).hexdigest() != digest:
            raise ValueError(f"Preview-0 manifest shader hash mismatch: {raw_path}")


def verify_canonical_checksums(roadmap: pathlib.Path, overlay: pathlib.Path) -> None:
    tracked_file(str(CHECKSUMS.relative_to(ROOT)), "Preview-0 checksum manifest")
    expected_lines = {
        f"{hashlib.sha256(roadmap.read_bytes()).hexdigest()}  {roadmap.name}",
        f"{hashlib.sha256(overlay.read_bytes()).hexdigest()}  {overlay.name}",
    }
    recorded_lines = set(text(CHECKSUMS).splitlines())
    missing = expected_lines - recorded_lines
    if missing:
        raise ValueError(f"Preview-0 checksum manifest mismatch: {sorted(missing)[0]}")


def main() -> int:
    try:
        overlay = text(OVERLAY)
        ledger = text(LEDGER)
        if value(overlay, "template") not in {"true", "false"}:
            raise ValueError("overlay template must be true or false")
        overlay_template = value(overlay, "template") == "true"
        overlay_activation = value(overlay, "activation_state")
        activation = value(ledger, "activation_state")
        if activation not in {"PREPARED", "ACTIVE"}:
            raise ValueError(f"invalid activation state {activation}")
        if overlay_activation != activation:
            raise ValueError("overlay and ledger activation states differ")
        if overlay_template and activation != "PREPARED":
            raise ValueError("template overlay must remain PREPARED")
        if not overlay_template and activation != "ACTIVE":
            raise ValueError("resolved overlay must be ACTIVE")
        path_line = next(line for line in overlay.splitlines() if line.startswith("  path: "))
        roadmap = ROOT / path_line.split(": ", 1)[1]
        roadmap_text = text(roadmap)
        tracked_file(path_line.split(": ", 1)[1], "canonical roadmap")
        tracked_file(str(OVERLAY.relative_to(ROOT)), "canonical overlay")
        expected_hash = next(line for line in overlay.splitlines() if line.startswith("  sha256: ")).split(": ", 1)[1]
        actual_hash = hashlib.sha256(roadmap.read_bytes()).hexdigest()
        if actual_hash != expected_hash:
            raise ValueError("canonical roadmap hash mismatch")
        verify_canonical_checksums(roadmap, OVERLAY)
        verify_clean_manifest()
        authorised = ids(overlay, "authorised_tickets")
        blockers = ids(overlay, "preview_blockers")
        queue = ids(overlay, "execution_queue")
        ledger_ids = [line.strip().removeprefix("- id: ") for line in ledger.splitlines() if line.strip().startswith("- id: ")]
        if len(queue) != len(set(queue)) or len(ledger_ids) != len(set(ledger_ids)):
            raise ValueError("duplicate ticket ID")
        if set(authorised) != set(queue) or set(authorised) != set(ledger_ids):
            raise ValueError("authorised, queue, and ledger ticket sets differ")
        if not set(blockers).issubset(authorised):
            raise ValueError("preview blocker is not authorised")
        holds = ids(overlay, "named_holds")
        candidates = ids(overlay, "next_candidates")
        if set(holds) & set(authorised):
            raise ValueError("named hold appears in authorised tickets")
        for ticket in authorised + holds + candidates:
            if ticket not in roadmap_text:
                raise ValueError(f"canonical roadmap does not define {ticket}")
        active = []
        for ticket in ledger_ids:
            block = ticket_block(ledger, ticket)
            required_fields = (
                "id:", "is_example:", "title:", "class:", "obligation:",
                "execution_mode:", "scheduling_state:", "status:", "outcome:",
                "owner:", "branch:", "baseline_commit:", "verification_commit:",
                "milestone:", "milestone_role:", "depends_on:", "blocked_by:",
                "blocks:", "implementation_commits:", "production_call_sites:",
                "test_ids:", "benchmark_artifacts:", "evidence_hash:",
                "decision_record:", "known_failures:", "fallback:", "reviewer:",
                "supersedes:", "evidence:",
            )
            for field in required_fields:
                if field not in block:
                    raise ValueError(f"{ticket} missing {field}")
            verify_production_call_sites(block, ticket)
            scheduling = value(block, "scheduling_state")
            lifecycle = value(block, "status")
            if scheduling not in {"OPEN", "ACTIVE", "BLOCKED", "DONE", "HOLD"}:
                raise ValueError(f"{ticket} has invalid scheduling state {scheduling}")
            if lifecycle not in {"PLANNED", "RESEARCHED", "PROTOTYPED", "INTEGRATED", "VALIDATED", "SHIPPABLE", "DEFERRED", "SUPERSEDED"}:
                raise ValueError(f"{ticket} has invalid lifecycle status {lifecycle}")
            if value(block, "outcome") not in {"ADOPTED", "ADAPTED", "REJECTED", "DEFERRED", "NOT_APPLICABLE"}:
                raise ValueError(f"{ticket} has invalid outcome")
            evidence = evidence_paths(block)
            for evidence_path in evidence:
                tracked_file(evidence_path, ticket)
            decision = value(block, "decision_record")
            if decision != "null":
                tracked_file(decision, ticket)
            completed = scheduling == "DONE" or lifecycle in {"VALIDATED", "SHIPPABLE"}
            if completed:
                verification = value(block, "verification_commit")
                if not re.fullmatch(r"[0-9a-f]{7,64}", verification):
                    raise ValueError(f"completed {ticket} needs a verification commit")
                git("cat-file", "-e", f"{verification}^{{commit}}")
                evidence_hash = value(block, "evidence_hash")
                if not re.fullmatch(r"[0-9a-f]{64}", evidence_hash):
                    raise ValueError(f"completed {ticket} needs a SHA-256 evidence hash")
                reviewer = value(block, "reviewer")
                owner = value(block, "owner")
                integration_owner = value(ledger, "integration_owner")
                if reviewer in {"", "null", owner, integration_owner}:
                    raise ValueError(f"completed {ticket} needs an independent reviewer")
                if not evidence:
                    raise ValueError(f"completed {ticket} needs tracked file evidence")
                if not list_values(value(block, "implementation_commits")):
                    raise ValueError(f"completed {ticket} needs implementation commits")
                for implementation_commit in list_values(value(block, "implementation_commits")):
                    if not re.fullmatch(r"[0-9a-f]{7,64}", implementation_commit):
                        raise ValueError(f"completed {ticket} has an invalid implementation commit")
                    git("cat-file", "-e", f"{implementation_commit}^{{commit}}")
            if scheduling == "ACTIVE":
                for field in ("owner", "branch", "baseline_commit"):
                    if value(block, field) in {"", "null"}:
                        raise ValueError(f"ACTIVE {ticket} has no {field}")
                active.append(ticket)
            for dependency in list_values(value(block, "depends_on")):
                if dependency and dependency not in ledger_ids:
                    raise ValueError(f"{ticket} depends on unknown ticket {dependency}")
                if scheduling == "DONE" and dependency and value(ticket_block(ledger, dependency), "scheduling_state") != "DONE":
                    raise ValueError(f"completed {ticket} depends on unfinished {dependency}")
        if len(active) > 3:
            raise ValueError("active ticket count exceeds Preview-0 WIP maximum")
        print(f"Preview-0 ledger valid: {len(authorised)} tickets, roadmap SHA-256 {actual_hash}")
    except ValueError as exc:
        print(f"Preview-0 ledger invalid: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
