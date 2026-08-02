#!/usr/bin/env python3
"""Write a reproducible Preview-0 build manifest."""
from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import platform
import re
import subprocess
from datetime import datetime, timezone


ROOT = pathlib.Path(__file__).resolve().parents[1]


def digest(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def canonical_digest(path: pathlib.Path) -> str:
    """Hash repository files as Git stores them, independent of checkout EOLs."""
    try:
        relative = path.resolve().relative_to(ROOT.resolve()).as_posix()
        filtered_blob = subprocess.check_output(
            ["git", "hash-object", "--path", relative, "--filters", "--stdin"],
            cwd=ROOT,
            input=path.read_bytes(),
        ).strip()
        canonical = subprocess.check_output(
            ["git", "cat-file", "blob", filtered_blob], cwd=ROOT
        )
        return hashlib.sha256(canonical).hexdigest()
    except (OSError, subprocess.CalledProcessError, ValueError):
        return digest(path)


def git(*args: str) -> str:
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True).strip()


def tool_version(*command: str) -> str | None:
    """Capture a tool identity without making manifest generation environment-fragile."""
    try:
        result = subprocess.run(command, cwd=ROOT, text=True, capture_output=True,
                                check=False, timeout=10)
    except (OSError, subprocess.TimeoutExpired):
        return None
    output = (result.stdout or result.stderr).strip().splitlines()
    return output[0] if result.returncode == 0 and output else None


def cmake_compiler(cache: pathlib.Path) -> str | None:
    if not cache.is_file():
        return None
    match = re.search(r"^CMAKE_CXX_COMPILER:FILEPATH=(.+)$",
                      cache.read_text(encoding="utf-8", errors="replace"), re.MULTILINE)
    return match.group(1).strip() if match else None


def shader_hashes(root: pathlib.Path, commit: str) -> list[dict[str, str]]:
    """Hash canonical Git blobs so the manifest is portable across line endings."""
    if not root.is_dir():
        raise SystemExit(f"missing shader root: {root}")
    return [
        {
            "path": str(path.relative_to(ROOT)),
            "sha256": hashlib.sha256(
                subprocess.check_output(
                    ["git", "show", f"{commit}:{path.relative_to(ROOT).as_posix()}"],
                    cwd=ROOT,
                )
            ).hexdigest(),
        }
        for path in sorted(root.rglob("*.wgsl"))
    ]


def png_dimensions(path: pathlib.Path) -> tuple[int, int]:
    """Read PNG dimensions without adding an image-library dependency."""
    header = path.read_bytes()[:24]
    if len(header) != 24 or header[:8] != b"\x89PNG\r\n\x1a\n" or header[12:16] != b"IHDR":
        raise SystemExit(f"capture is not a valid PNG: {path}")
    return int.from_bytes(header[16:20], "big"), int.from_bytes(header[20:24], "big")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", type=pathlib.Path, required=True)
    parser.add_argument("--dll", type=pathlib.Path, required=True)
    parser.add_argument("--backend", default="wgpu")
    parser.add_argument("--adapter", default="unknown")
    parser.add_argument("--driver", default="unknown",
                        help="graphics-driver version paired with the captured adapter")
    parser.add_argument("--runtime-log", type=pathlib.Path,
                        help="optional --check log used as backend-proof evidence")
    parser.add_argument("--capture", type=pathlib.Path,
                        help="optional WGPU screenshot capture to fingerprint")
    parser.add_argument("--metrics", type=pathlib.Path,
                        help="optional JSON timing sidecar produced by --capture-metrics")
    parser.add_argument("--comparison", type=pathlib.Path,
                        help="optional JSON image-comparison result bundle")
    parser.add_argument("--cmake-cache", type=pathlib.Path,
                        help="optional CMakeCache.txt used to identify the C++ compiler")
    parser.add_argument("--shader-root", type=pathlib.Path,
                        default=ROOT / "engine/WgpuRenderer/rust/src",
                        help="WGSL source tree to fingerprint (default: renderer Rust source tree)")
    parser.add_argument("--out", type=pathlib.Path, required=True)
    args = parser.parse_args()
    for path in (args.exe, args.dll):
        if not path.is_file():
            raise SystemExit(f"missing artifact: {path}")
    runtime = None
    if args.runtime_log:
        if not args.runtime_log.is_file():
            raise SystemExit(f"missing runtime log: {args.runtime_log}")
        log_text = args.runtime_log.read_text(encoding="utf-8", errors="replace")
        adapter_match = re.search(r"wgpu adapter: (.+)", log_text)
        capabilities_match = re.search(r"wgpu capabilities: (.+)", log_text)
        runtime = {
            "log": str(args.runtime_log),
            "sha256": canonical_digest(args.runtime_log),
            "wgpu_selected": "Wgpu: creating renderer" in log_text and "wgpu renderer created" in log_text,
            "initialization_check_completed": "Initialization check complete - exiting" in log_text,
            "adapter": adapter_match.group(1).strip() if adapter_match else None,
            "gpu_timestamps_enabled": "GPU timestamp instrumentation enabled" in log_text,
            "capabilities": capabilities_match.group(1).strip() if capabilities_match else None,
            "enabled_features": {
                "msaa": "wgpu MSAA enabled" in log_text,
                "hdr": "wgpu HDR path enabled" in log_text,
                "gpu_indirect": "GPU-driven indirect draws enabled" in log_text,
                "gpu_driven": "GPU-driven rendering enabled" in log_text,
                "gpu_timestamps": "GPU timestamp instrumentation enabled" in log_text,
            },
        }
        if not runtime["wgpu_selected"] or not runtime["initialization_check_completed"]:
            raise SystemExit("runtime log does not prove a successful WGPU --check")
    capture = None
    if args.capture:
        if not args.capture.is_file():
            raise SystemExit(f"missing capture: {args.capture}")
        width, height = png_dimensions(args.capture)
        capture = {
            "path": str(args.capture),
            "sha256": canonical_digest(args.capture),
            "bytes": args.capture.stat().st_size,
            "format": "png",
            "width": width,
            "height": height,
        }
    metrics = None
    if args.metrics:
        if not args.metrics.is_file():
            raise SystemExit(f"missing metrics sidecar: {args.metrics}")
        try:
            metric_data = json.loads(args.metrics.read_text(encoding="utf-8"))
        except json.JSONDecodeError as exc:
            raise SystemExit(f"invalid metrics JSON: {exc}") from exc
        timings = metric_data.get("gpu_timings_ms")
        if metric_data.get("schema_version") != 1 or not metric_data.get("gpu_timestamps_available") or not isinstance(timings, list):
            raise SystemExit("metrics sidecar does not prove available GPU timestamps")
        metrics = {
            "path": str(args.metrics),
            "sha256": canonical_digest(args.metrics),
            "bytes": args.metrics.stat().st_size,
            "renderer": metric_data.get("renderer"),
            "runtime": metric_data.get("runtime"),
            "timed_regions": len(timings),
            "measured_regions": sum(1 for region in timings if region.get("milliseconds", -1) >= 0),
        }
        grass = metric_data.get("grass")
        if isinstance(grass, dict):
            metrics["grass_instances"] = {
                band: grass.get(f"{band}_instances")
                for band in ("near", "mid", "far")
            }
    comparison = None
    if args.comparison:
        if not args.comparison.is_file():
            raise SystemExit(f"missing comparison bundle: {args.comparison}")
        try:
            comparison_data = json.loads(args.comparison.read_text(encoding="utf-8"))
        except json.JSONDecodeError as exc:
            raise SystemExit(f"invalid comparison JSON: {exc}") from exc
        if comparison_data.get("schema_version") != 1 or not isinstance(comparison_data.get("changed_pixel_ratio"), (int, float)):
            raise SystemExit("comparison bundle has an unsupported schema")
        comparison = {
            "path": str(args.comparison),
            "sha256": canonical_digest(args.comparison),
            "bytes": args.comparison.stat().st_size,
            "changed_pixel_ratio": comparison_data["changed_pixel_ratio"],
            "mean_absolute_rgb_delta": comparison_data.get("mean_absolute_rgb_delta"),
            "mask": comparison_data.get("mask"),
        }
    commit = git("rev-parse", "HEAD")
    # Build directories and test artefacts are intentionally untracked.  They
    # must not turn a clean source build into a falsely "dirty" manifest;
    # only tracked/index changes can alter the compiled source provenance.
    dirty = bool(git("status", "--porcelain", "--untracked-files=no"))
    if dirty:
        raise SystemExit("refusing to write Preview-0 manifest with tracked source changes")
    manifest = {
        "schema_version": 3,
        "created_utc": datetime.now(timezone.utc).isoformat(),
        "git_commit": commit,
        "git_dirty": False,
        "platform": platform.platform(),
        "backend_requested": args.backend,
        "adapter": args.adapter,
        "driver": args.driver,
        "toolchain": {
            "cmake": tool_version("cmake", "--version"),
            "rustc": tool_version("rustc", "--version"),
            "cargo": tool_version("cargo", "--version"),
            "cxx_compiler": cmake_compiler(args.cmake_cache) if args.cmake_cache else None,
        },
        "runtime_check": runtime,
        "capture": capture,
        "metrics": metrics,
        "comparison": comparison,
        "shaders": shader_hashes(args.shader_root, commit),
        "artifacts": [{"path": str(path), "sha256": digest(path), "bytes": path.stat().st_size}
                      for path in (args.exe, args.dll)],
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(args.out)


if __name__ == "__main__":
    main()
