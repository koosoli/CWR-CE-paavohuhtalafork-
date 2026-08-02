// WTR-001 lifecycle: water must come up correctly on a cold boot, and again on
// a second boot that inherits the first one's user directory.
//
// `.seq` phases are separate game launches sharing one POSEIDON_USER_DIR, so
// phase 2 starts against whatever state phase 1 left behind. That is the cheap
// half of WTR-260's question: not in-process invalidation, but whether water
// resources initialise cleanly when the process is new and the profile is not.
//
// Phase 2 is the same assertions against a warm profile. A failure here that
// phase 1 passed means water depends on first-boot state, or that something the
// first launch wrote leaves the second unable to build its resources.

triSimFrames 60

_s1 = triWaterStats
triAssertNe [_s1, "FAIL:no_water_frame"]
triAssertGt [(triWaterNodeCount), 0]

triSimFrames 120

triAssertNe [(triWaterStats), _s1]
triAssertGt [(triWaterNodeCount), 0]

triEndTest
