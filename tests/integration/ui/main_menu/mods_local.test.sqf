// THE WIRING: the MODS screen lists REAL local mods on disk, and Apply mounts the
// ticked set via an in-process re-mount.
//
// DisplayMods::OnCreateCtrl scans the mods root (here --mods-dir
// ../../tests/fixtures/mods, via the .toml sidecar) for @<mod> folders. The
// isolated test mod @fixturemod is the only one there, so exactly one Local row
// shows (Downloaded/unticked since nothing is mounted at launch). Ticking it + Apply
// re-mounts with tests/fixtures/mods/@fixturemod; afterwards ModSystem::GetModList
// carries it (triAssertActiveMod) — the full scan -> tick -> Apply -> re-mount ->
// mounted loop. Workshop/catalog rows stay stubbed; only local mods are wired.

triSetLanguage "English"
triSimUntil { triGameMode == 2 }
triAssertEq [(triDisplay), 0]

triClick 119                 // IDC_MAIN_MODS
triAssertEq [(triDisplay), 72]          // IDD_MODS

triWaitFrames 10
_n = triModsVisibleCount
if (_n != 1) exitWith { format ["FAIL:visible=%1 (want exactly 1 local mod)", _n] }
triScreenshot "01_one_local_mod"

// Tick the mod -> active set
triModsRowClick [0, 0.03]
triAssertEq [(triGetModsActiveSet), "fixturemod"]
triScreenshot "02_ticked_local"

// Apply -> in-process re-mount with the ticked local mod
triClick 115                 // IDC_MODS_APPLY
triSimFrames 90
triSimUntil { triGameMode == 2 && triLoadedShapeCount > 0 }
triAssertEq [(triDisplay), 0]           // landed back on a fresh main menu
triAssertIncludes [(triGetActiveMods), "fixturemod"]   // GRAIL: the mod is now in the active mod path
triEndTest
