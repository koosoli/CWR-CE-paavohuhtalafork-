// PROBE, not an acceptance test. It exists to answer one question: why does a
// scripted camera that goes underwater kill the game process?
//
// The symptom on record is Trident reporting "broken pipe" after about 59 s.
// That message comes from the harness failing to WRITE a command
// (client/connection.rs:84), which means the game had already gone. So the
// interesting failure is on the game side, not the harness side, and the useful
// artefact is the game log rather than the Trident report.
//
// The sweep walks the camera down through the waterline in one-metre steps and
// calls triWaterStats at each one, so a run that dies identifies the height it
// died at instead of just timing out. Offshore position is the water_alive
// beach pushed 150 m along its own view heading (322.98 deg), which is the
// direction that fixture faces out to sea.

triSimFrames 30

_camera = "camera" camCreate [3221.9, 1692.6, 20.0]
_camera camSetDir 322.98
_camera cameraEffect ["internal", "back"]
_camera camCommit 0

triSimFrames 30

// Well clear of the water. If this already fails, the problem is the camera
// command path itself and has nothing to do with submersion.
_camera camSetPos [3221.9, 1692.6, 20.0]
_camera camCommit 0
triSimFrames 20
triAssertNe [(triWaterStats), "FAIL:no_water_frame"]

// KNOWN LIMITATION, and the reason this file is a probe rather than an
// acceptance test: nothing here proves the scripted camera is the one being
// RENDERED. triWaterStats keeps reporting live water whatever the camera does,
// so a camSetPos that never reached the render camera would still walk the
// whole sweep green. Treat a pass as "the process survives the sweep", not as
// "the camera went underwater".
//
// The obvious check is triCamPos, and it does not work here: it is viewer-mode
// only and returns FAIL:not_viewer_mode under PoseidonGame, asserted below so
// this stays true rather than becoming a stale comment. Closing the gap needs a
// harness command that reports submersion the way triWaterStats reports the
// water frame; until then the evidence is the "Water submersion:" line in the
// game log, which carries camY, the local surface and the resulting depth.
_low = triCamPos
triAssertEq [_low, "FAIL:not_viewer_mode"]

// Descend from 6 m over the datum to 6 m under, a metre at a time.
//
// The loop counts UP and derives the height, rather than counting a float down
// past zero. The classic evaluator rejects a negative literal in a `while`
// condition string — `while "_h > -6.5"` raises "Invalid number in expression"
// — and in test mode a script error takes the whole process down, which is a
// confusing way to discover a syntax problem.
// Reading water stats at every step is what makes a failure localisable: the
// last height that produced a line is the last height that survived.
_i = 0
while { _i < 13 } do { _h = 6 - _i; _camera camSetPos [3221.9, 1692.6, _h]; _camera camCommit 0; triSimFrames 15; triAssertNe [(triWaterStats), "FAIL:no_water_frame"]; _i = _i + 1 }

// Back up through the surface. If the descent is survivable but the ascent is
// not, the fault is in a transition rather than in the submerged state.
_i = 0
while { _i < 13 } do { _h = _i - 6; _camera camSetPos [3221.9, 1692.6, _h]; _camera camCommit 0; triSimFrames 15; _i = _i + 1 }

triAssertNe [(triWaterStats), "FAIL:no_water_frame"]

triEndTest
