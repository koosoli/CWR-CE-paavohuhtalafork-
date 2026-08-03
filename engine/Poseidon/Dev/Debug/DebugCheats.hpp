#pragma once

// Cheat / dev actions invoked from the ImGui Cheats tab buttons, the
// dev-console, and per-action tri verbs.  Each Cmd_X exposes:
//   - Available()   — predicate: gates UI disabled state + console errors
//   - Invoke(args)  — the actual work; called by UI/tri directly with
//                     typed args, and by the console after string parse
//
// Each is also self-registered with DebugCommands so the console can
// dispatch by name.

#include <string>
#include <string_view>
#include <vector>

namespace Poseidon::Dev
{
namespace DebugCheats
{

// God mode — sticky toggle.  When active, damage applied to the real
// player by Object::SetDammage is silently dropped.  Has no effect on
// AI / non-player units.  Reset every time the toggle is set (no
// implicit "remember across missions").
namespace Cmd_God
{
bool Available();                                     // mission running
bool IsActive();                                      // current toggle state
void SetActive(bool active);                          // typed setter for UI / tri
void Invoke(std::string_view args, std::string& out); // console form
} // namespace Cmd_God

// Infinite ammo — sticky toggle.  When active, the real player's
// EntityAI::FireWeapon path refunds the burst it just consumed, so the
// magazine never depletes (and never auto-removes itself).  AI weapons
// behave normally.  Magazine swap behaviour (TakeMagazine) is unchanged.
namespace Cmd_InfiniteAmmo
{
bool Available();
bool IsActive();
void SetActive(bool active);
void Invoke(std::string_view args, std::string& out);
} // namespace Cmd_InfiniteAmmo

// Infinite fuel — sticky toggle.  When active, fuel consumption on the
// vehicle the real player is INSIDE (GetVehicleIn()) is refunded, so the
// tank/jeep/helicopter never runs dry.  AI vehicles are unaffected.
// Refuel (negative consume) is allowed through normally.
namespace Cmd_InfiniteFuel
{
bool Available();
bool IsActive();
void SetActive(bool active);
void Invoke(std::string_view args, std::string& out);
} // namespace Cmd_InfiniteFuel

// Show all units on the map — sticky toggle.  When active, the map
// display draws every unit of every AICenter regardless of side /
// fog-of-war.  Hook lives in CStaticMapMain::DrawExt; the normal
// player-visible draw still happens, the cheat just adds an
// unconditional pass on top.
namespace Cmd_ShowAllUnits
{
bool Available();
bool IsActive();
void SetActive(bool active);
void Invoke(std::string_view args, std::string& out);
} // namespace Cmd_ShowAllUnits

// Click-to-teleport — sticky toggle.  When active, a left-click on the
// in-mission map teleports the player's vehicle to the clicked world
// position instead of issuing the normal move/watch order.  Hook lives
// in CStaticMapMain::OnLButtonClick.  Reverts to normal click handling
// when the toggle is off.
namespace Cmd_MapTeleport
{
bool Available();
bool IsActive();
void SetActive(bool active);
void Invoke(std::string_view args, std::string& out);
} // namespace Cmd_MapTeleport

// Infinite armor — sticky toggle.  Identical hook site to god mode
// (Object::SetDammage) but the target predicate is "this is the vehicle
// the real player is in" instead of "this is the real player's body".
// Lets the player ride a tank / fly a chopper without taking damage to
// the vehicle.  Has no effect on the foot-soldier body (that's god
// mode); also no effect on AI-crewed vehicles.
namespace Cmd_InfiniteArmor
{
bool Available();
bool IsActive();
void SetActive(bool active);
void Invoke(std::string_view args, std::string& out);
} // namespace Cmd_InfiniteArmor

// Advance / rewind in-game time.  hours can be negative.  Action, not
// toggle.  Mirrors the original OFP Ctrl+T (fast-forward) / Ctrl+Y / Ctrl+G
// (skip one day) / Ctrl+H scancode cheats — same end effect via
// World::SkipTime(hours * OneHour).
namespace Cmd_SkipTime
{
bool Available();
void InvokeHours(float hours, std::string& out);      // typed entry point
void Invoke(std::string_view args, std::string& out); // console form: "<hours>"
} // namespace Cmd_SkipTime

// Set weather.  Calls World::SetWeather(overcast, fog, transition).
//
// NOTE on InvokeOvercast: the comment here used to claim the fog value was
// "left at the engine's current to avoid surprising the user". It never was —
// the implementation passes fog = 0, so setting overcast has always cleared fog
// too. Behaviour is left as-is because the console command and triCheatWeather
// both depend on it; InvokeWeather below is the entry point that controls both
// axes honestly.
namespace Cmd_SetWeather
{
bool Available();
void InvokeOvercast(float overcast, std::string& out); // typed setter; fog := 0
// Both axes, plus a transition length in seconds. 0 is immediate; a non-zero
// value lets the engine interpolate, which is what makes this usable as a live
// Zeus control rather than a snap.
void InvokeWeather(float overcast, float fog, float transitionSeconds, std::string& out);
void Invoke(std::string_view args, std::string& out); // console: "<overcast>"
} // namespace Cmd_SetWeather

// Set the clock to an absolute hour in [0, 24).
//
// The engine only offers a RELATIVE skip (World::SkipTime), so this computes the
// shortest forward delta to the requested hour and skips by that. Forward-only
// is deliberate: skipping backwards is not something the day/night and lighting
// state is known to handle, and wrapping forward past midnight reaches every
// hour anyway.
namespace Cmd_SetTimeOfDay
{
bool Available();
void InvokeHour(float hour, std::string& out);
} // namespace Cmd_SetTimeOfDay

// Time multiplier — sets World::_acceleratedTime to a selected
// constant.  UI exposes a fixed list (0.5x / 1x / 2x / 4x) — the
// same original OFP PAGE UP / PAGE DOWN scaling, just presented as a row
// of preset buttons.  Saturated at kTimeAccMin..kTimeAccMax by the
// World setter, so out-of-range values are silently clamped.
namespace Cmd_TimeMultiplier
{
bool Available();
float Get();                                          // current GWorld value (1.0 fallback)
void SetValue(float mult, std::string& out);          // typed setter
void Invoke(std::string_view args, std::string& out); // console form
} // namespace Cmd_TimeMultiplier

// Unlock all missions of the currently-displayed campaign.  Only
// usable while DisplayCampaignLoad is the active display (the original
// OFP UNLOCKCAMPAIGN word cheat is gated the same way — caught by
// that display's OnSimulate).  Action, not toggle.
namespace Cmd_UnlockCampaign
{
bool Available();
void Invoke(std::string_view args, std::string& out);
} // namespace Cmd_UnlockCampaign

// Save the current mission to <SaveDirectory>/save.fps.  Action, not
// toggle — single Invoke fires the save.  Available whenever a mission
// is running (no other gating; matches the "save no matter if possible"
// spirit of the original ENDMISSION-era word cheat).
namespace Cmd_SaveGame
{
bool Available();
void Invoke(std::string_view args, std::string& out);
} // namespace Cmd_SaveGame

// Store the camera + player position to the log and the system
// clipboard, formatted as a copy-pasteable Trident / mission.sqm
// init block.  Replaces the former INSERT dev hotkey.  Available
// whenever a world is loaded (even in the menu's demo loop the
// camera dump is useful).
namespace Cmd_StorePosition
{
bool Available();
void Invoke(std::string_view args, std::string& out);
} // namespace Cmd_StorePosition

// Load the save.fps file written by Cmd_SaveGame back into the live
// world.  Action, not toggle.  Available when a mission is running AND
// the save file exists on disk.  Calls World::LoadBin which rehydrates
// the world state in place (player, vehicles, ammo, damage, time).
namespace Cmd_LoadGame
{
bool Available();
void Invoke(std::string_view args, std::string& out);
} // namespace Cmd_LoadGame

// Mission ending — wins or loses the active mission.  outcome is one of:
//   "win"        → EMEnd1
//   "lose"       → EMLoser
//   "killed"     → EMKilled
//   "end1".."end6" → EMEnd1..EMEnd6 (pick a specific scripted ending)
namespace Cmd_EndMission
{
bool Available();
void Invoke(std::string_view outcome, std::string& out);

// Returns the outcomes that make sense for the currently-loaded
// mission.  Always begins with "win"/"lose"/"killed" (universally
// applicable); then appends "end1".."end6" for each ASTEnd* sensor
// actually present in the mission's sensorsMap.  An empty list means
// the cheat is unavailable (no active mission).  Re-queried per
// frame from the UI so it reacts live to mission start / end.
std::vector<std::string> AvailableOutcomes();
} // namespace Cmd_EndMission

} // namespace DebugCheats
} // namespace Poseidon::Dev
