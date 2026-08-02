// Two halves, both against runtime-created centres (the Zeus path):
//
//   1. Opposing sides in contact must actually fight -- acquire each other and
//      inflict damage.
//   2. A runtime-spawned soldier's death must not crash the simulation.  The
//      MFDead branch of Man::ProcessMoveFunction builds a blood decal from an
//      optional preloaded shape; before 44f9c9d a world without that shape
//      dereferenced null there.  A crash kills the harness connection, so
//      simply reaching triEndTest is the assertion for that half.

triSimFrames 20

createCenter east
createCenter west

_wg = createGroup west
_eg = createGroup east

// Close enough to see and engage each other in the open (~60m).  The
// coordinates are lifted from the retail 00training.abel unit placements: dry,
// level ground, and well clear of the mission's own player unit.  Spawning over
// water or on top of another unit would satisfy the damage check below without
// a shot being fired.
// NOTE the component order: an SQF position is [east, north, elevation], while
// mission.sqm writes position[]={east, elevation, north}.  Copying the .sqm
// order here puts the units at north=40 -- 70m under the sea, where they drown
// and satisfy the damage check below without a shot being fired.
"SoldierWB" createUnit [[9782.3, 4024.0, 40.26], _wg, "", 1, "PRIVATE"]
"SoldierEB" createUnit [[9758.0, 3969.0, 39.70], _eg, "", 1, "PRIVATE"]

triSimFrames 60

_w1 = (units _wg) select 0
_e1 = (units _eg) select 0

_w1 setBehaviour "COMBAT"
_e1 setBehaviour "COMBAT"
_w1 setCombatMode "RED"
_e1 setCombatMode "RED"
_w1 reveal _e1
_e1 reveal _w1

triSimFrames 60

// Mutual hostility, now that both centres hold target info for the other.
triAssertEq [(_w1 countEnemy [_e1]), 1]
triAssertEq [(_e1 countEnemy [_w1]), 1]

// Detection must survive into the AI's own knowledge model.
triAssertGt [((_w1 knowsAbout _e1) + (_e1 knowsAbout _w1)), 0]

// Lethality: opposing riflemen at ~40m with weapons free must draw blood.
// Measured as a delta -- a created unit settles onto the terrain and takes a
// fixed drop's worth of damage first, which must not be mistaken for a hit.
_d0 = (damage _w1) + (damage _e1)

triSimFrames 1200

triAssertGt [(((damage _w1) + (damage _e1)) - _d0), 0]

// Death handling: force it deterministically rather than depending on AI
// marksmanship, then keep simulating across the death animation and decal.
_e1 setDamage 1
triSimFrames 300

triAssertEq [(alive _e1), false]

// A second death while the first corpse is still in the world.
_w1 setDamage 1
triSimFrames 300

triAssertEq [(alive _w1), false]

triEndTest
