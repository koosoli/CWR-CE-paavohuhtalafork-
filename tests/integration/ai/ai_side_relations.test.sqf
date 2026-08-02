// Verifies the runtime side-relation initialisation from a6718e2 the same way
// Zeus reaches it: centres and groups created while the world is already
// running, never through the mission's ArcadeIntel.
//
// Broken state (uninitialised AICenter::_friends): same-side units read each
// other as hostile, so countFriendly is 0 / countEnemy is 1 for the two West
// riflemen, and they shoot each other during the combat window at the end.
//
// Two things this test deliberately works around:
//   - countEnemy / countFriendly read the centre's *known-target* list, not the
//     relationship matrix.  An undetected unit is neither friend nor enemy and
//     both counts read 0, so every unit is revealed first.
//   - A created unit settles onto the terrain and takes a fixed drop's worth of
//     damage on the way.  Damage is therefore measured as a delta across the
//     combat window, never as an absolute.

triSimFrames 20

// Runtime centres -- this is the AICenter(side, mode) constructor path.
createCenter east
createCenter west

_wg = createGroup west
_eg = createGroup east

// Two West riflemen a few metres apart, one East rifleman ~60m away in plain
// sight.  The coordinates are lifted from the retail 00training.abel unit
// placements: dry, level ground, and well clear of the mission's own player
// unit.  Two placement mistakes both read as friendly fire in the damage check
// below -- spawning over water drowns the pair, and spawning on top of another
// unit crushes it -- so neither is a free choice.
// NOTE the component order: an SQF position is [east, north, elevation], while
// mission.sqm writes position[]={east, elevation, north}.  Copying the .sqm
// order here puts the units at north=40 -- 70m under the sea, where they drown
// and read as friendly-fire casualties.
"SoldierWB" createUnit [[9782.3, 4024.0, 40.26], _wg, "", 1, "PRIVATE"]
"SoldierWB" createUnit [[9788.3, 4024.0, 40.26], _wg, "", 1, "PRIVATE"]
"SoldierEB" createUnit [[9758.0, 3969.0, 39.70], _eg, "", 1, "PRIVATE"]

triSimFrames 60

_w1 = (units _wg) select 0
_w2 = (units _wg) select 1
_e1 = (units _eg) select 0

// Side values are not comparable through the assert API (they stringify to
// empty), so the relationship is probed through the centre's own friend/enemy
// classification instead.
_w1 reveal _w2
_w1 reveal _e1
_e1 reveal _w1

triSimFrames 60

// 1. Same side must be friendly to its own centre, and never an enemy.
triAssertEq [(_w1 countFriendly [_w2]), 1]
triAssertEq [(_w1 countEnemy [_w2]), 0]

// 2. Opposing sides must be mutually hostile.
triAssertEq [(_w1 countEnemy [_e1]), 1]
triAssertEq [(_e1 countEnemy [_w1]), 1]
triAssertEq [(_w1 countFriendly [_e1]), 0]

// 3. Behavioural check: with weapons free at point-blank range, same-side units
//    must not damage each other.  The East rifleman is removed first -- it is a
//    legitimate threat to both West units, and leaving it in would attribute
//    its (correct) hits to friendly fire.
deleteVehicle _e1
triSimFrames 60

_w1 setBehaviour "COMBAT"
_w2 setBehaviour "COMBAT"
_w1 setCombatMode "RED"
_w2 setCombatMode "RED"

_d1 = damage _w1
_d2 = damage _w2

triSimFrames 600

triAssertEq [(alive _w1), true]
triAssertEq [(alive _w2), true]
triAssertLe [((damage _w1) - _d1), 0.01]
triAssertLe [((damage _w2) - _d2), 0.01]

triEndTest
