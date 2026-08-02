// The Demo-island twin of water_alive, so water coverage can eventually run on
// hosted CI rather than only on a workstation with retail data.
//
// Asserts liveness only: that a water frame is published at all, and that it
// keeps advancing rather than latching on one stale result. Deliberately no
// node-count bound here, unlike water_alive -- that fixture sits at a measured
// viewpoint on Malden where 24 nodes are selected, whereas this mission's
// camera may see little or no ocean, and a node count of zero would then be
// correct rather than a failure.
//
// Broken state: triWaterStats returns FAIL:no_water_frame (water never
// rendered), or returns the same frame twice (water stopped being submitted).

triSimFrames 60

_s1 = triWaterStats
triAssertNe [_s1, "FAIL:no_water_frame"]

triSimFrames 120

_s2 = triWaterStats
triAssertNe [_s2, "FAIL:no_water_frame"]
triAssertNe [_s2, _s1]

triEndTest
