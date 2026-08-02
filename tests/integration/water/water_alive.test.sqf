// First water coverage in the suite. Water previously had no test observability
// at all: a frozen FFT, a collapsed CDLOD selection, or water that silently
// stopped drawing were invisible to every harness command, so none of the water
// work to date had a regression test behind it.
//
// triWaterStats reports the most recently rendered water frame. This fixture
// asserts the two things that distinguish live water from broken water without
// depending on any particular camera framing or coastline:
//
//   1. a water frame is being published at all;
//   2. it keeps advancing, so the water path is running per frame rather than
//      latched on one stale result.
//
// Deliberately no pixel or screenshot assertions here. Those need a known
// coastal viewpoint and stable tolerances, which belong to the fuller
// TEST-WTR-001 pack; this fixture is the part that can be asserted objectively
// today, and it fails loudly if water stops rendering.

triSimFrames 60

// GL33 never publishes (it has no CDLOD water path), so this fixture is
// meaningful only under WGPU. Under GL33 it reports the backend gap rather than
// silently passing.
_s1 = triWaterStats
triAssertNe [_s1, "FAIL:no_water_frame"]

// Let several frames pass, then confirm the reported frame moved on. Equal
// strings here mean water stopped being submitted -- exactly the silent
// regression this fixture exists to catch.
triSimFrames 120

_s2 = triWaterStats
triAssertNe [_s2, "FAIL:no_water_frame"]
triAssertNe [_s2, _s1]

triEndTest
