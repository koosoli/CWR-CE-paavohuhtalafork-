# Cutting a Preview 0 package

The order below is not arbitrary. Each step produces something the next step
verifies, and the packaging script refuses rather than warns, so doing them out
of order fails loudly instead of shipping something subtly wrong.

## 1. Decide the candidate commit

Pick it deliberately. `HEAD` is not automatically the candidate, and
`package_preview0.ps1` has no default for `-Commit` precisely so this decision
cannot be made by accident.

Make every change you intend to release *first*. After this point, committing
anything invalidates the evidence you are about to generate — see step 5.

## 2. Build

```powershell
.\scripts\Build.ps1 -Target PoseidonGame
```

The packaging script re-verifies that what is staged in `dist/` matches the build
tree **by content**. Timestamps prove nothing here: this project has already shipped
a day-old binary while every check reported success, because a stale renderer DLL
still resolves its FFI exports and simply behaves like an older build.

## 3. Prove the backend, and capture

```powershell
.\scripts\run_preview0_wgpu_check.ps1 -BuildDir "build\win-x64-clang-rwdi" -SkipBuild `
    -LogPath "docs\roadmap\evidence\<name>.log"

.\scripts\run_preview0_wgpu_check.ps1 -BuildDir "build\win-x64-clang-rwdi" -SkipBuild `
    -MissionCaptureSmoke -LogPath "docs\roadmap\evidence\<name>-mission.log"
```

The first proves WGPU was actually selected — a silent fall back to GL33 is a
failure, not a graceful degradation. The second produces the reference capture and
the timing sidecar.

**Read the timings rather than filing them.** Both cost figures this project
carried into August 2026 were wrong, in opposite directions, and both were found by
running exactly this capture: an interior-bake load stall overstated roughly 30x,
and an ambient-occlusion cost understated about 4x because the original number came
from an 800x600 window.

## 4. Update the ledgers if the capture changed the story

If the numbers move a claim, fix the claim before packaging. The capability matrix
is generated from the ledgers and CI diffs it, so a stale ledger becomes a stale
public document:

```powershell
python scripts/generate_capability_matrix.py
python scripts/compute_evidence_hash.py --all --write
python scripts/validate_preview0_ledger.py
```

## 5. Generate the manifest — after the last commit

```powershell
python scripts/write_preview0_manifest.py `
    --exe dist/x64-win-rwdi/PoseidonGame.exe `
    --dll dist/x64-win-rwdi/wgpu_renderer.dll `
    --adapter "<from the runtime log>" --driver "<driver version>" `
    --runtime-log docs/roadmap/evidence/<name>.log `
    --capture  docs/roadmap/evidence/<name>-mission-...-capture.png `
    --metrics  docs/roadmap/evidence/<name>-mission-...-capture.json `
    --cmake-cache build/win-x64-clang-rwdi/CMakeCache.txt `
    --out docs/roadmap/evidence/preview0-manifest-<candidate>.json
```

The manifest records the commit that was `HEAD` when it ran, and the packaging
script refuses a manifest describing any other commit. So generate it **after**
your final commit and **leave it untracked** while packaging. Committing it moves
`HEAD` past the commit it describes, and the gate will — correctly — refuse.

Do not regenerate `docs/roadmap/evidence/preview0-manifest.json` in place. That
file is `TEST-002`'s evidence and is hashed as such; overwriting it silently
invalidates another ticket's record. Pass your candidate file with
`-ManifestPath` instead.

## 6. Package

```powershell
.\scripts\package_preview0.ps1 -Commit <sha> -SkipBuild `
    -ManifestPath docs/roadmap/evidence/preview0-manifest-<candidate>.json
```

It will refuse if the tree is dirty, if `-Commit` is not `HEAD`, if any ledger or
the capability matrix fails, if either binary is missing, if `dist/` disagrees with
the build tree, or if the manifest describes a different commit.

Output is `dist/packages/cwr-<version>-<short sha>/` plus a `.zip`, with
`package-fingerprint.json` listing a SHA-256 for every shipped file.

## 7. What the script cannot do for you

`REL-000` also requires a demonstration video of the released build, and a review.
Neither can be automated, and neither should be quietly skipped: the release notes
list them as missing, and that list is the honest part of the document.
