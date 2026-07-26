#include <SDL3/SDL_keycode.h>
#include <SDL3/SDL_rect.h>
#include <SDL3/SDL_scancode.h>
#include <stdint.h>
#include <algorithm>
#include <map>
#include <string_view>
#include <system_error>
#include <utility>
#include <Poseidon/Foundation/Framework/DebugLog.hpp>
#include <Poseidon/Foundation/Framework/Log.hpp>
#include <Poseidon/Foundation/Strings/RString.hpp>
#include <Poseidon/Foundation/platform.hpp>

// The PCH pulls in Logging.hpp which #defines DebugLog() as a logging macro.
// That collides with the method ImGui::DebugLog().  Undef before including
// ImGui headers — none of our code in this TU uses the DebugLog macro.
#ifdef DebugLog
#undef DebugLog
#endif

#include <imgui.h>
#include <imgui_impl_sdl3.h>
#include <imgui_impl_opengl3.h>
#include <SDL3/SDL.h>
#include <glad/gl.h>

#include <Poseidon/Dev/Debug/DebugOverlay.hpp>
#include <Poseidon/Dev/Debug/DebugCheats.hpp>
#include <Poseidon/Dev/Debug/DebugCommands.hpp>
#include <Poseidon/Dev/Debug/WtrTestHarness.hpp>
#include <Poseidon/Foundation/Logging/Logging.hpp>
#include <Poseidon/Core/Application.hpp>
#include <Poseidon/Core/Config/EngineConfig.hpp>
#include <Poseidon/Input/ControlsCategory.hpp>
#include <Poseidon/Input/InputSubsystem.hpp>
#include <Poseidon/Input/UserAction.hpp>
#include <Poseidon/Foundation/Platform/AppConfig.hpp>
#include <Poseidon/AI/AI.hpp>
#include <Poseidon/AI/LicensePlateTextTuning.hpp>
#include <Poseidon/Graphics/Rendering/Draw/FontMapping.hpp>
#include <Poseidon/UI/Locale/MissionLanguageDetector.hpp>
#include <Poseidon/UI/Locale/Stringtable/Stringtable.hpp>
#include <Poseidon/Dev/Diag/FrameProfiler.hpp>
#include <Poseidon/UI/Settings/GameSettingsConfig.hpp>
#include <Poseidon/UI/Settings/AspectRatio.hpp>
#include <Poseidon/Graphics/Core/Engine.hpp>
#include <Poseidon/Graphics/Rendering/WaterInteractionBridge.hpp>
#include <Poseidon/Core/Global.hpp>
#include <Poseidon/IO/ParamFileExt.hpp>
#include <Poseidon/Foundation/Memory/CheckMem.hpp>
#include <Poseidon/Foundation/Memory/MemFreeReq.hpp>
#include <Poseidon/World/World.hpp>
#include <Poseidon/World/WorldInputContext.hpp>
#include <Poseidon/World/Entities/Infantry/Person.hpp>
#include <Poseidon/World/Entities/Vehicles/Transport.hpp>
#include <Poseidon/World/Scene/Object.hpp>
#include <Poseidon/Foundation/Common/GamePaths.hpp>
#include <Evaluator/express.hpp>
#include <filesystem>

// Voice-language + visibility helpers — extern decls so we don't
// have to pull the locale/audio/world internals into this TU.  All
// three are linked in from Poseidon.lib at the unified-build stage.
extern const std::string& GetSelectedVoiceLanguage();
extern void SetSelectedVoiceLanguage(const std::string&);
extern void SetVisibility(float distance);

#include <string>
#include <cstring>
#include <cstdio>
#include <functional>
#include <vector>

namespace Poseidon::Dev
{
namespace DebugOverlay
{

namespace
{
// Which renderer backend composites ImGui: imgui_impl_opengl3 (GL33) or the
// Engine overlay virtuals (wgpu). The SDL3 platform backend is shared.
enum class RenderBackend
{
    OpenGL3,
    Engine
};
RenderBackend s_backend = RenderBackend::OpenGL3;

bool s_initialized = false;
bool s_visible = false;
bool s_selectShadowsTab = false; // one-shot: force-select the Shadows tab next draw
bool s_selectMemoryTab = false;  // one-shot: force-select the Memory tab next draw
SDL_Window* s_window = nullptr;
// Saved mouse-grab state while the dev panel holds the cursor released.
bool s_mouseReleasedByPanel = false;
bool s_savedMouseGrab = false;

// Deferred actions — populated by UI button click handlers, drained
// AFTER ImGui::Render() returns each frame.  Why deferred:
//
// Some cheat invocations call deep into the engine and run code that
// mutates state ImGui still needs in the current frame.  The worst
// offender is Cmd_SaveGame, whose World::SaveBin path ends with
// `MemoryCleanUp()` (engine/poseidon/Memory/JimboAllocator.cpp:252)
// — that shrinks the engine's memory pool back to the OS and can
// invalidate buffers some ImGui widget still references later in
// the same DrawCheatsTab pass.  The result was a confirmed crash
// stack:
//   ImGui::ButtonEx+0x38
//   DrawCheatsTab+0x275
//   DrawMainWindow+0x6d
//   EngineGL33::BackToFront+0xd
//
// Deferring keeps the click handlers tiny (just enqueue a closure)
// and runs the actual cheat after ImGui::Render() — by which point
// no ImGui internal data is still in flight, so any engine
// reallocation is safe.
std::vector<std::function<void()>> s_pendingActions;

void Defer(std::function<void()> action)
{
    s_pendingActions.push_back(std::move(action));
}

// One mutable copy per (slot, role) shown by the tuner.  Pulled from the
// active mapping on first Render() and pushed back to font.cpp via
// SetFontMappingTuning on every slider change.
struct RoleEditState
{
    const char* prefix;
    const char* alias; // legacy alias prefix kept in sync (or nullptr)
    int renderPx;
    float widthScale;
    float baselineOffset;
    float syntheticBold;
    float letterSpacing;
};

struct TuningState
{
    bool loaded = false;
    RoleEditState roles[5]; // title, body, mono, serif, hand
};

TuningState s_tuning;
int s_currentRole = 0; // which face's tuning the panel is editing

static const char* const kRoleNames[5] = {"Title", "Body", "Mono", "Serif", "Hand"};
static const char* const kRolePrefixes[5] = {"cwrtitle", "cwrbody", "cwrmono", "cwrserif", "cwrhand"};
static const char* const kRoleAliases[5] = {"steelfishb", "tahomab", "couriernewb", "garamond", "audreyshand"};

void LoadTuningIfNeeded()
{
    if (s_tuning.loaded)
        return;
    // Parse the dump to fill renderPx / widthScale for each role.
    // Format per line: '  {"prefix", "ttfPath", maxH, renderPx, widthScalef, oblique},'
    const char* dump = DumpFontTable();
    for (int r = 0; r < 5; r++)
    {
        s_tuning.roles[r].prefix = kRolePrefixes[r];
        s_tuning.roles[r].alias = kRoleAliases[r];
        s_tuning.roles[r].renderPx = 24;
        s_tuning.roles[r].widthScale = 1.0f;
        s_tuning.roles[r].baselineOffset = 0.0f;
        s_tuning.roles[r].syntheticBold = 0.0f;
        s_tuning.roles[r].letterSpacing = 0.0f;
        char needle[64];
        snprintf(needle, sizeof(needle), "{\"%s\",", kRolePrefixes[r]);
        const char* p = strstr(dump, needle);
        if (!p)
            continue;
        // Skip past prefix + ttfPath + maxH by counting commas.  DumpFontTable
        // emits:  prefix, ttfPath, maxH, renderPx, widthScalef, obliqueBool,
        //         baselineOffsetf, syntheticBoldf, letterSpacingf
        int commas = 0;
        const char* q = p;
        while (*q && commas < 3)
        {
            if (*q == ',')
                commas++;
            q++;
        }
        // q now points just after the 3rd comma — next is space + renderPx
        int rpx = 0;
        float ws = 1.0f;
        char oblique[16] = "false";
        float baseline = 0.0f, bold = 0.0f, spacing = 0.0f;
        if (sscanf(q, " %d, %ff, %15[^,], %ff, %ff, %ff", &rpx, &ws, oblique, &baseline, &bold, &spacing) >= 2)
        {
            s_tuning.roles[r].renderPx = rpx;
            s_tuning.roles[r].widthScale = ws;
            s_tuning.roles[r].baselineOffset = baseline;
            s_tuning.roles[r].syntheticBold = bold;
            s_tuning.roles[r].letterSpacing = spacing;
        }
    }
    s_tuning.loaded = true;
}

void DrawFontTab()
{
    // ── Face picker ────────────────────────────────────────────────
    // A single shipping font set; this picks which face the
    // size/stretch/spacing sliders below tune.
    ImGui::Text("Face:");
    for (int r = 0; r < 5; r++)
    {
        if (r > 0)
            ImGui::SameLine();
        bool active = (s_currentRole == r);
        if (active)
            ImGui::PushStyleColor(ImGuiCol_Button, ImVec4(0.3f, 0.6f, 0.9f, 1.0f));
        if (ImGui::Button(kRoleNames[r]))
            s_currentRole = r;
        if (active)
            ImGui::PopStyleColor();
    }
    ImGui::Separator();

    // ── Sliders for the selected face ─────────────────────────────
    LoadTuningIfNeeded();
    auto& role = s_tuning.roles[s_currentRole];

    ImGui::Text("%s - %s (alias %s)", kRoleNames[s_currentRole], role.prefix, role.alias ? role.alias : "(none)");

    bool changed = false;
    changed |= ImGui::SliderInt("renderPx", &role.renderPx, 8, 128);
    changed |= ImGui::SliderFloat("widthScale", &role.widthScale, 0.3f, 1.6f, "%.3f");
    changed |= ImGui::SliderFloat("baseline", &role.baselineOffset, -16.0f, 16.0f, "%.1f px");
    changed |= ImGui::SliderFloat("bold", &role.syntheticBold, -2.0f, 4.0f, "%.1f px");
    changed |= ImGui::SliderFloat("spacing", &role.letterSpacing, -4.0f, 8.0f, "%.1f px");
    if (changed)
    {
        SetFontMappingTuning(role.prefix, role.renderPx, role.widthScale, role.baselineOffset, role.syntheticBold,
                             role.letterSpacing, nullptr);
        if (role.alias)
            SetFontMappingTuning(role.alias, role.renderPx, role.widthScale, role.baselineOffset, role.syntheticBold,
                                 role.letterSpacing, nullptr);
    }
    ImGui::Separator();

    if (ImGui::Button("Dump font table to log"))
    {
        LOG_INFO(Graphics, "\n{}", DumpFontTable());
    }

    ImGui::Separator();
    ImGui::TextUnformatted("License plates");
    LicensePlateTextTuning plate = GetLicensePlateTextTuning();
    bool plateChanged = false;
    plateChanged |= ImGui::SliderFloat("plate width", &plate.widthScale, 0.30f, 1.20f, "%.3f");
    plateChanged |= ImGui::SliderFloat("plate x offset", &plate.horizontalOffset, -5.00f, 1.00f, "%.2f em");
    plateChanged |= ImGui::SliderFloat("plate y offset", &plate.verticalOffset, -1.00f, 2.00f, "%.2f em");
    plateChanged |= ImGui::SliderFloat("plate surface offset", &plate.surfaceOffset, 0.000f, 0.050f, "%.3f m");
    plateChanged |= ImGui::SliderFloat("plate softness", &plate.softness, 0.000f, 0.050f, "%.3f em");
    if (plateChanged)
        SetLicensePlateTextTuning(plate);
    ImGui::SameLine();
    if (ImGui::Button("Reset plate"))
        ResetLicensePlateTextTuning();
    ImGui::TextDisabled("  session-only override; defaults come from CfgLicensePlateText");
}

// Last command output for the Cheats tab — shown under the buttons so
// the user gets a visible confirmation that a click did something.
std::string s_cheatsStatus;

template <typename ClickFn>
void CheatButton(const char* label, bool enabled, const char* tooltip, ClickFn&& click)
{
    ImGui::BeginDisabled(!enabled);
    if (ImGui::Button(label))
        click();
    ImGui::EndDisabled();
    if (tooltip && ImGui::IsItemHovered(ImGuiHoveredFlags_AllowWhenDisabled))
        ImGui::SetTooltip("%s", tooltip);
}

void DrawCheatsTab()
{
    ImGui::TextUnformatted("Mission");
    ImGui::Separator();

    // One-button End Mission, matching the original OFP ENDMISSION word
    // cheat shape — no outcome picker; the cheat just wins the mission.
    // Power users who want a specific outcome use the `triEndMission
    // "lose"` / `endmission end3` / etc. paths through tri or the
    // console; Cmd_EndMission::Invoke keeps all the outcome strings.
    //
    // Re-queried every frame so the button greys instantly when there's
    // no mission running or the mission has already entered teardown.
    const bool canEnd = DebugCheats::Cmd_EndMission::Available();
    CheatButton("End Mission (win)", canEnd,
                canEnd ? "Force the active mission to win (sets EndMode=EMEnd1).\n"
                         "Matches the 1999 ENDMISSION word-cheat semantics.\n"
                         "Closes the dev panel afterwards — the engine starts\n"
                         "tearing down the world over the next several frames\n"
                         "and keeping any in-mission UI alive during that\n"
                         "transition risks crashes (textures get evicted while\n"
                         "we'd still be querying them).\n"
                         "Other outcomes still available via `triEndMission` /\n"
                         "the dev console (lose, killed, end1..end6)."
                       : "Requires an active mission that has not already ended.",
                []
                {
                    // Hide first (cheap, just sets a bool) — the deferred
                    // Invoke can then run after ImGui::Render with no
                    // panel-render side effects to worry about.
                    SetVisible(false);
                    Defer([] { DebugCheats::Cmd_EndMission::Invoke("win", s_cheatsStatus); });
                });

    ImGui::Spacing();
    ImGui::TextUnformatted("System");
    ImGui::Separator();

    // Full in-process reload — re-mounts all banks/addons/config and rebuilds
    // the world on the same window (the mod "Apply" path).  Gated to outside a
    // mission: re-mounting mid-mission would evict assets the simulation still
    // references.  Hide the panel first, then run after ImGui::Render — the
    // reload tears down the very world/UI we'd otherwise be drawing this frame.
    const bool canReload = Poseidon::GApp != nullptr && Poseidon::GApp->m_canRender && GWorld != nullptr &&
                           GWorld->GetMode() == GModeIntro;
    CheatButton("Reload game", canReload,
                canReload ? "Reload all game content (mods + config) in place.\n"
                            "Keeps the window, shows the loading screen, and lands\n"
                            "back on a fresh main menu."
                          : "Available from the main menu (not during a mission).",
                []
                {
                    SetVisible(false);
                    // Queue the reload for the next AppIdle (before simulate/draw) rather than
                    // running it inside the swap — see RequestRemount / RequestDeferredReload.
                    if (Poseidon::GApp != nullptr)
                        Poseidon::GApp->RequestRemount();
                });

    CheatButton("Exit game", true, "Exits game.",
                []
                {
                    SetVisible(false);
                    // Queue the close
                    if (Poseidon::GApp != nullptr)
                        Poseidon::GApp->m_closeRequest = 1;
                });

    ImGui::Spacing();
    ImGui::TextUnformatted("Player");
    ImGui::Separator();

    // God mode — sticky toggle.  Disabled state mirrors EndMission's
    // gating: needs a mission with a real player.  The toggle itself
    // is persisted by DebugCheats; we just round-trip the bool here.
    const bool canGod = DebugCheats::Cmd_God::Available();
    bool god = DebugCheats::Cmd_God::IsActive();
    ImGui::BeginDisabled(!canGod);
    if (ImGui::Checkbox("God mode", &god))
        DebugCheats::Cmd_God::SetActive(god);
    ImGui::EndDisabled();
    if (ImGui::IsItemHovered(ImGuiHoveredFlags_AllowWhenDisabled))
        ImGui::SetTooltip("%s", canGod ? "Silently drop all damage applied to the real player.\n"
                                         "Hooked in Object::SetDammage so every weapon / explosion /\n"
                                         "fall is covered, not just script-driven damage."
                                       : "Requires an active mission.");

    // Infinite ammo — same gating shape as god mode.
    const bool canAmmo = DebugCheats::Cmd_InfiniteAmmo::Available();
    bool infammo = DebugCheats::Cmd_InfiniteAmmo::IsActive();
    ImGui::BeginDisabled(!canAmmo);
    if (ImGui::Checkbox("Infinite ammo", &infammo))
        DebugCheats::Cmd_InfiniteAmmo::SetActive(infammo);
    ImGui::EndDisabled();
    if (ImGui::IsItemHovered(ImGuiHoveredFlags_AllowWhenDisabled))
        ImGui::SetTooltip("%s", canAmmo ? "Refund the burst the real player just fired so the current\n"
                                          "magazine never depletes.  Hooked in EntityAI::FireWeapon.\n"
                                          "AI weapons fire normally; magazine swaps are unaffected."
                                        : "Requires an active mission.");

    ImGui::Spacing();
    ImGui::TextUnformatted("Vehicle (player's current)");
    ImGui::Separator();

    // Infinite fuel — hooked in Transport::ConsumeFuel.  Only meaningful
    // when the player is inside a vehicle; the gating logic itself
    // still allows toggling on foot (the cheat just has no effect
    // until the player mounts something).
    const bool canFuel = DebugCheats::Cmd_InfiniteFuel::Available();
    bool inffuel = DebugCheats::Cmd_InfiniteFuel::IsActive();
    ImGui::BeginDisabled(!canFuel);
    if (ImGui::Checkbox("Infinite fuel", &inffuel))
        DebugCheats::Cmd_InfiniteFuel::SetActive(inffuel);
    ImGui::EndDisabled();
    if (ImGui::IsItemHovered(ImGuiHoveredFlags_AllowWhenDisabled))
        ImGui::SetTooltip("%s", canFuel ? "Refund fuel consumption on the vehicle the player is in.\n"
                                          "Hook lives in Transport::ConsumeFuel.  Refuel still works.\n"
                                          "No effect when on foot or in an AI-driven vehicle."
                                        : "Requires an active mission.");

    // Infinite armor — second SetDammage gate, targeting the player's
    // vehicle rather than the player's own body.
    const bool canArmor = DebugCheats::Cmd_InfiniteArmor::Available();
    bool infarmor = DebugCheats::Cmd_InfiniteArmor::IsActive();
    ImGui::BeginDisabled(!canArmor);
    if (ImGui::Checkbox("Infinite armor", &infarmor))
        DebugCheats::Cmd_InfiniteArmor::SetActive(infarmor);
    ImGui::EndDisabled();
    if (ImGui::IsItemHovered(ImGuiHoveredFlags_AllowWhenDisabled))
        ImGui::SetTooltip("%s", canArmor ? "Drop damage to the vehicle the player is currently in.\n"
                                           "Hooked alongside god mode in Object::SetDammage.\n"
                                           "Covers tanks, jeeps, planes, helicopters."
                                         : "Requires an active mission.");

    ImGui::Spacing();
    ImGui::TextUnformatted("Actions");
    ImGui::Separator();

    // Store position — log + clipboard.  The original OFP INSERT dev
    // hotkey.  Always available when a scene is up (works in the menu
    // demo loop too — camera dump is useful even without a mission
    // player).
    const bool canStore = DebugCheats::Cmd_StorePosition::Available();
    CheatButton("Store position (log + clipboard)", canStore,
                canStore ? "Dump camera + player position to the log AND copy a\n"
                           "ready-to-paste block (triSetView for the exact render\n"
                           "view + this setPos for the player) to the clipboard.\n"
                           "Replaces the old INSERT hotkey."
                         : "Requires a scene with an active camera.",
                [] { Defer([] { DebugCheats::Cmd_StorePosition::Invoke("", s_cheatsStatus); }); });

    // Save game — one-shot action.  Always usable when a mission is
    // running, including missions that normally disallow save.
    const bool canSave = DebugCheats::Cmd_SaveGame::Available();
    CheatButton("Save game now", canSave,
                canSave ? "Force-save the current world state to <SaveDir>/save.fps.\n"
                          "No 'save allowed' gating — bypasses mission-script restrictions."
                        : "Requires an active mission.",
                [] { Defer([] { DebugCheats::Cmd_SaveGame::Invoke("", s_cheatsStatus); }); });

    // Load game — inverse.  Grey out when the save file isn't on disk.
    const bool canLoad = DebugCheats::Cmd_LoadGame::Available();
    ImGui::SameLine();
    CheatButton("Load game now", canLoad,
                canLoad ? "Restore from <SaveDir>/save.fps via World::LoadBin.\n"
                          "Same engine path the normal 'Load Game' menu uses;\n"
                          "rehydrates the world in place (player, vehicles,\n"
                          "ammo, damage, time).  Deferred to run after ImGui\n"
                          "finishes the frame, same reason as Save."
                        : "Requires an active mission.  If <SaveDir>/save.fps doesn't\n"
                          "exist the click reports it in the status line; no crash.",
                [] { Defer([] { DebugCheats::Cmd_LoadGame::Invoke("", s_cheatsStatus); }); });

    // Skip time — four buttons.  The original OFP SCANCODE_T/Y/G/H
    // cheats are +1h / -1h continuous and +24h / -24h one-shot;
    // discrete buttons match the dev panel's click-driven UI better.
    const bool canTime = DebugCheats::Cmd_SkipTime::Available();
    ImGui::BeginDisabled(!canTime);
    if (ImGui::Button("Time -1h"))
        DebugCheats::Cmd_SkipTime::InvokeHours(-1.0f, s_cheatsStatus);
    ImGui::SameLine();
    if (ImGui::Button("Time +1h"))
        DebugCheats::Cmd_SkipTime::InvokeHours(+1.0f, s_cheatsStatus);
    ImGui::SameLine();
    if (ImGui::Button("Time -24h"))
        DebugCheats::Cmd_SkipTime::InvokeHours(-24.0f, s_cheatsStatus);
    ImGui::SameLine();
    if (ImGui::Button("Time +24h"))
        DebugCheats::Cmd_SkipTime::InvokeHours(+24.0f, s_cheatsStatus);
    ImGui::EndDisabled();
    if (ImGui::IsItemHovered(ImGuiHoveredFlags_AllowWhenDisabled) && !canTime)
        ImGui::SetTooltip("Requires an active mission.");

    // Precise time-of-day seek.  The slider re-reads the clock every frame, so it
    // tracks the advancing time when idle and, when dragged, seeks by skipping the
    // delta to the target hour — reusing SkipTime so _timeInYear (sun angle/season)
    // stays consistent.  Ctrl+click the slider to type an exact hour (e.g. 18.50).
    ImGui::BeginDisabled(!canTime);
    float todHours = Glob.clock.GetTimeOfDay() * 24.0f;
    ImGui::SetNextItemWidth(220.0f);
    if (ImGui::SliderFloat("Time of day", &todHours, 0.0f, 24.0f, "%05.2f h"))
    {
        const float cur = Glob.clock.GetTimeOfDay() * 24.0f;
        DebugCheats::Cmd_SkipTime::InvokeHours(todHours - cur, s_cheatsStatus);
    }
    ImGui::SetItemTooltip("Seek the time of day. Ctrl+click to type an exact hour.\n"
                          "Sun angle & season stay consistent (skips real time).");
    ImGui::EndDisabled();

    // Skipping time advances the overcast/fog forecast (World::SimulateLandscape), which
    // rolls new weather and shrinks the view range as you scrub. Freeze it while tuning.
    if (GWorld)
    {
        bool freeze = GWorld->IsWeatherFrozen();
        if (ImGui::Checkbox("Freeze weather while scrubbing time", &freeze))
            GWorld->SetFreezeWeather(freeze);
        ImGui::SetItemTooltip("Stops the overcast/fog forecast from advancing when you skip time,\n"
                              "so scrubbing changes only the sun/sky — not rain or view distance.");
    }

    // Weather presets — instant overcast change.  No active-value
    // highlight: there's no public World::GetOvercast() to read back,
    // so we can't reliably show which preset is in effect.
    const bool canWeather = DebugCheats::Cmd_SetWeather::Available();
    ImGui::TextUnformatted("Weather:");
    ImGui::SameLine();
    ImGui::BeginDisabled(!canWeather);
    struct WeatherPreset
    {
        const char* label;
        float overcast;
    };
    static const WeatherPreset kWeather[] = {
        {"Clear", 0.0f},
        {"Cloudy", 0.3f},
        {"Overcast", 0.7f},
        {"Storm", 1.0f},
    };
    for (int i = 0; i < (int)(sizeof(kWeather) / sizeof(kWeather[0])); i++)
    {
        if (i > 0)
            ImGui::SameLine();
        if (ImGui::Button(kWeather[i].label))
            DebugCheats::Cmd_SetWeather::InvokeOvercast(kWeather[i].overcast, s_cheatsStatus);
    }
    ImGui::EndDisabled();
    if (ImGui::IsItemHovered(ImGuiHoveredFlags_AllowWhenDisabled) && !canWeather)
        ImGui::SetTooltip("Requires an active mission.");

    // Time multiplier — preset list.  Highlights the active value so
    // the user sees what's currently selected.  Engine saturates to
    // [kTimeAccMin, kTimeAccMax]; reading back via Get() reports the
    // clamped value.
    const bool canMult = DebugCheats::Cmd_TimeMultiplier::Available();
    const float currentMult = canMult ? DebugCheats::Cmd_TimeMultiplier::Get() : 1.0f;
    ImGui::TextUnformatted("Time multiplier:");
    ImGui::SameLine();
    ImGui::BeginDisabled(!canMult);
    static const float kPresets[] = {0.5f, 1.0f, 2.0f, 4.0f};
    for (int i = 0; i < (int)(sizeof(kPresets) / sizeof(kPresets[0])); i++)
    {
        const float v = kPresets[i];
        // Highlight the active preset (within 0.01 — float compare).
        const bool isActive = canMult && (currentMult > v - 0.01f) && (currentMult < v + 0.01f);
        if (isActive)
            ImGui::PushStyleColor(ImGuiCol_Button, ImVec4(0.3f, 0.6f, 0.9f, 1.0f));
        char label[16];
        snprintf(label, sizeof(label), "%.1fx", v);
        if (i > 0)
            ImGui::SameLine();
        if (ImGui::Button(label))
            DebugCheats::Cmd_TimeMultiplier::SetValue(v, s_cheatsStatus);
        if (isActive)
            ImGui::PopStyleColor();
    }
    ImGui::EndDisabled();

    // Unlock campaign — writes <TmpSaveDir>/<campaign>.sqc files
    // directly so the unlock survives reopening the campaign load
    // screen.  Works from any display.
    if (ImGui::Button("Unlock all campaigns"))
        Defer([] { DebugCheats::Cmd_UnlockCampaign::Invoke("", s_cheatsStatus); });
    if (ImGui::IsItemHovered())
        ImGui::SetTooltip("%s", "Mark every mission of every installed campaign as available.\n"
                                "Writes the unlock to <TmpSaveDir>/<campaign>.sqc so the\n"
                                "next Campaign Load open sees it.  Refreshes the live list\n"
                                "if the Campaign Load screen happens to be open right now.");

    ImGui::Spacing();
    ImGui::TextUnformatted("Map");
    ImGui::Separator();

    // Show all units — independent of the _showUnits flag (which is
    // _ENABLE_CHEATS-gated).  Adds an unconditional DrawUnits pass in
    // CStaticMapMain::DrawExt across every AICenter.
    const bool canShowAll = DebugCheats::Cmd_ShowAllUnits::Available();
    bool showAll = DebugCheats::Cmd_ShowAllUnits::IsActive();
    ImGui::BeginDisabled(!canShowAll);
    if (ImGui::Checkbox("Show all units on map", &showAll))
        DebugCheats::Cmd_ShowAllUnits::SetActive(showAll);
    ImGui::EndDisabled();
    if (ImGui::IsItemHovered(ImGuiHoveredFlags_AllowWhenDisabled))
        ImGui::SetTooltip("%s", canShowAll ? "Draw every unit of every side on the in-mission map,\n"
                                             "bypassing fog-of-war and side filtering.  Hooked in\n"
                                             "CStaticMapMain::DrawExt."
                                           : "Requires an active mission.");

    // Click-to-teleport — left-click on the in-mission map teleports
    // the player's vehicle to the clicked spot instead of issuing the
    // normal move/watch order.
    const bool canTeleport = DebugCheats::Cmd_MapTeleport::Available();
    bool teleport = DebugCheats::Cmd_MapTeleport::IsActive();
    ImGui::BeginDisabled(!canTeleport);
    if (ImGui::Checkbox("Click-on-map to teleport", &teleport))
        DebugCheats::Cmd_MapTeleport::SetActive(teleport);
    ImGui::EndDisabled();
    if (ImGui::IsItemHovered(ImGuiHoveredFlags_AllowWhenDisabled))
        ImGui::SetTooltip("%s", canTeleport ? "Open the in-mission map (M) and left-click anywhere to\n"
                                              "teleport the player's vehicle.  Snaps to the ground\n"
                                              "surface height; if you're in a tank/chopper, the whole\n"
                                              "vehicle goes along.  Hooked in CStaticMapMain::OnLButtonClick."
                                            : "Requires an active mission.");

    if (!s_cheatsStatus.empty())
    {
        ImGui::Separator();
        ImGui::TextWrapped("Last: %s", s_cheatsStatus.c_str());
    }
}

// Game tab — text / voice language pickers + view distance slider.
// Reuses the same kLangRotation list the F12 / F11 dev hotkeys use
// (engine/poseidon/UI/optionsUI.cpp:2333) so picker + hotkey stay
// consistent.
namespace
{
static const char* const kGameLangs[] = {
    "English", "Czech", "French", "German", "Italian", "Spanish", "Russian",
};
constexpr int kGameLangsCount = (int)(sizeof(kGameLangs) / sizeof(kGameLangs[0]));

int FindLangIndex(const char* current)
{
    if (!current)
        return 0;
    for (int i = 0; i < kGameLangsCount; i++)
        if (stricmp(current, kGameLangs[i]) == 0)
            return i;
    return 0;
}

const char* DebugBool(bool value)
{
    return value ? "true" : "false";
}

RString DebugObjectName(Object* object)
{
    if (!object)
        return "<null>";
    return object->GetDebugName();
}

ControlsCategory DebugSettingsCategoryForContext(InputContext context)
{
    switch (context)
    {
        case InputContext::Infantry:
            return ControlsCategoryOnFoot;
        case InputContext::CarDriver:
        case InputContext::TankDriver:
        case InputContext::ShipDriver:
            return ControlsCategoryVehicles;
        case InputContext::HeliPilot:
        case InputContext::PlanePilot:
            return ControlsCategoryPilot;
        case InputContext::TankGunner:
        case InputContext::Gunner:
            return ControlsCategoryGunner;
        default:
            return ControlsCategoryCount;
    }
}

void DrawInputContextDiagnostics()
{
    ImGui::TextUnformatted("Input context");
    if (!GWorld)
    {
        ImGui::TextDisabled("world not loaded");
        return;
    }

    const auto& input = InputSubsystem::Instance();
    const InputContextResolution resolution = GWorld->ResolveInputContextResolution();
    const InputContext liveWorld = resolution.context;
    const InputContext cached = input.GetContext();

    Person* player = GWorld->PlayerOn();

    AIUnit* focus = GWorld->FocusOn();

    ImGui::Text("World: live=%s cached=%s manual=%s map=%s options=%s", InputContextName(liveWorld),
                InputContextName(cached), DebugBool(GWorld->PlayerManual()), DebugBool(GWorld->HasMap()),
                DebugBool(GWorld->HasOptions()));
    const ControlsCategory settingsCategory = DebugSettingsCategoryForContext(liveWorld);
    ImGui::Text("Settings: %s",
                settingsCategory == ControlsCategoryCount ? "<none>" : GetControlsCategoryName(settingsCategory));
    ImGui::Text("Resolved: %s | %s", static_cast<const char*>(DebugObjectName(resolution.transport)),
                InputSeatContextName(resolution.seat));
    ImGui::Text("Player: %s", static_cast<const char*>(DebugObjectName(player)));
    ImGui::Text("Focus:  %s", focus ? static_cast<const char*>(focus->GetDebugName()) : "<null>");
    ImGui::Text("Camera: %s", static_cast<const char*>(DebugObjectName(GWorld->CameraOn())));

    const InputContext ctx = resolution.context;
    ImGui::Text("Actions [%s]: F %.2f B %.2f L %.2f R %.2f Up %.2f Dn %.2f TL %.2f TR %.2f", InputContextName(ctx),
                input.GetAction(ctx, UAMoveForward, true), input.GetAction(ctx, UAMoveBack, true),
                input.GetAction(ctx, UAMoveLeft, true), input.GetAction(ctx, UAMoveRight, true),
                input.GetAction(ctx, UAMoveUp, true), input.GetAction(ctx, UAMoveDown, true),
                input.GetAction(ctx, UATurnLeft, true), input.GetAction(ctx, UATurnRight, true));
}
} // namespace

void DrawGameTab()
{
    ImGui::TextUnformatted("Language");
    ImGui::Separator();

    // Text language — drives stringtables, mission briefings, UI.
    const char* currentText = GLanguage;
    int textIdx = FindLangIndex(currentText);
    if (ImGui::Combo("Text", &textIdx, kGameLangs, kGameLangsCount))
        Defer([picked = std::string(kGameLangs[textIdx])] { SetLanguage(RString(picked.c_str())); });
    if (ImGui::IsItemHovered())
        ImGui::SetTooltip("Stringtables, mission briefings, UI labels.\nSame as the F12 dev-only hotkey.");

    // Voice language — drives <base>.<voiceLang>.<ext> sound lookups.
    const std::string voiceLang = GetSelectedVoiceLanguage();
    int voiceIdx = FindLangIndex(voiceLang.c_str());
    if (ImGui::Combo("Voice", &voiceIdx, kGameLangs, kGameLangsCount))
        Defer([picked = std::string(kGameLangs[voiceIdx])] { SetSelectedVoiceLanguage(picked); });
    if (ImGui::IsItemHovered())
        ImGui::SetTooltip("Voice-over track for say / playSound / radio.\nSame as the F11 dev-only hotkey.\nIn-flight "
                          "audio is unaffected; lookup applies on the next play.");

    ImGui::Spacing();
    ImGui::TextUnformatted("View distance");
    ImGui::Separator();

    // VD slider — clamped to the same range as the Options UI.  Engine
    // saturates internally too; we mirror so the slider can't request
    // a value that just gets clipped silently.
    static float s_vd = ENGINE_CONFIG.tacticalZ;
    s_vd = ENGINE_CONFIG.tacticalZ; // sync with whatever else set it
    if (ImGui::SliderFloat("VD (m)", &s_vd, GameSettingsConfig::kMinViewDistance, GameSettingsConfig::kMaxViewDistance,
                           "%.0f m"))
    {
        const float v = s_vd;
        Defer([v] { SetVisibility(v); });
    }
    if (ImGui::IsItemHovered())
        ImGui::SetTooltip("Terrain / horizon distance (the master).\nRange %.0f..%.0f m.  Bypasses the "
                          "per-tier graphics preset.",
                          GameSettingsConfig::kMinViewDistance, GameSettingsConfig::kMaxViewDistance);
    // Object and shadow distances are derived from VD (ViewDistanceResolver), so
    // there are no separate sliders — moving VD moves all three.

    ImGui::Spacing();
    ImGui::TextUnformatted("Diagnostics");
    ImGui::Separator();

    DrawInputContextDiagnostics();
    ImGui::Spacing();

    // The TXT / VO / VD localization-status block in the mission preview is a
    // diagnostic overlay, hidden from players by default.  Off shows the plain
    // mission overview; on prepends the per-language text/voice/view-distance table.
    bool showLoc = MissionLanguageDetector::ShowLocalizationDebugInfo();
    if (ImGui::Checkbox("Mission localization info (TXT/VO/VD)", &showLoc))
        MissionLanguageDetector::SetShowLocalizationDebugInfo(showLoc);
    if (ImGui::IsItemHovered())
        ImGui::SetTooltip("Prepend the per-language text/voice availability and view-distance\n"
                          "table to the mission preview.  Re-select a mission to refresh.");
}

// Console tab — SQF / DebugCommand runner.  Bare lines without a `:`
// prefix dispatch through DebugCommands::Run first (so "save",
// "endmission win", etc. work), then fall back to SQF evaluation.
// Lines starting with `:` are forced to SQF (use this if a SQF name
// happens to collide with a DebugCommand name).
namespace
{
struct ConsoleState
{
    char input[512] = "";
    std::vector<std::string> scrollback;
    bool autoScroll = true;
    bool focusOnShow = true;
};
ConsoleState s_console;

void ConsoleAppend(const std::string& line)
{
    s_console.scrollback.push_back(line);
    if (s_console.scrollback.size() > 200)
        s_console.scrollback.erase(s_console.scrollback.begin(),
                                   s_console.scrollback.begin() + (s_console.scrollback.size() - 200));
}

void ConsoleRun(std::string_view line)
{
    while (!line.empty() && (line.front() == ' ' || line.front() == '\t'))
        line.remove_prefix(1);
    if (line.empty())
        return;
    ConsoleAppend(std::string("> ") + std::string(line));

    const bool forceSqf = !line.empty() && line.front() == ':';
    if (forceSqf)
        line.remove_prefix(1);

    if (!forceSqf)
    {
        std::string out;
        if (DebugCommands::Run(line, out))
        {
            if (!out.empty())
                ConsoleAppend(out);
            return;
        }
    }

    // SQF path.  Mirrors the Evaluator/express.hpp idiom — runs in the
    // current game state's evaluation context.  No-op + error log when
    // no world is up (e.g. main-menu, before mission load).
    if (!GWorld || !GWorld->GetGameState())
    {
        ConsoleAppend("(no game state — SQF unavailable here)");
        return;
    }
    GameValue result = GWorld->GetGameState()->EvaluateMultiple(std::string(line).c_str());
    if (result.GetType() != GameNothing)
        ConsoleAppend(std::string("= ") + (const char*)result.GetText());
}
} // namespace

void DrawConsoleTab()
{
    ImGui::TextUnformatted("SQF / DebugCommands console");
    ImGui::Separator();

    // Scrollback region.  Reserve room for the input row at the bottom.
    const float inputRowH = ImGui::GetFrameHeightWithSpacing();
    if (ImGui::BeginChild("ConsoleScroll", ImVec2(0, -inputRowH), true, ImGuiWindowFlags_HorizontalScrollbar))
    {
        for (const auto& line : s_console.scrollback)
            ImGui::TextUnformatted(line.c_str());
        if (s_console.autoScroll && ImGui::GetScrollY() >= ImGui::GetScrollMaxY())
            ImGui::SetScrollHereY(1.0f);
    }
    ImGui::EndChild();

    // Input row: text box + Run button.  Enter inside the text box
    // also runs the line.  EnterReturnsTrue makes the InputText
    // produce true when Enter is hit, so we don't need a separate
    // key check.
    if (s_console.focusOnShow)
    {
        ImGui::SetKeyboardFocusHere();
        s_console.focusOnShow = false;
    }
    bool entered = ImGui::InputText("##ConsoleInput", s_console.input, sizeof(s_console.input),
                                    ImGuiInputTextFlags_EnterReturnsTrue);
    ImGui::SameLine();
    bool clicked = ImGui::Button("Run");
    if (entered || clicked)
    {
        std::string line(s_console.input);
        s_console.input[0] = 0;
        if (!line.empty())
            Defer([line] { ConsoleRun(line); });
        ImGui::SetKeyboardFocusHere(-1); // refocus the text box
    }
    ImGui::SameLine();
    if (ImGui::Button("Clear"))
        s_console.scrollback.clear();
    ImGui::SameLine();
    ImGui::Checkbox("Auto-scroll", &s_console.autoScroll);

    if (ImGui::IsItemHovered())
        ImGui::SetTooltip("Bare lines dispatch through DebugCommands first\n"
                          "(save, endmission win, weather 0.5, …).\n"
                          "Prefix `:` to force SQF (e.g. `:hint \"hi\"`)");
}

// Profile tab — FPS gauge + frame-time history graph.  Reads the
// engine's _lastFrameDuration each draw, keeps a 240-frame ring
// buffer (~4 seconds at 60 fps) for the plot.
namespace
{
constexpr int kProfHistory = 240;
float s_frameMs[kProfHistory] = {};
int s_frameMsHead = 0;

void ProfileSample()
{
    if (!GEngine)
        return;
    const uint32_t lastMs = GEngine->GetLastFrameDuration();
    const float ms = static_cast<float>(lastMs);
    s_frameMs[s_frameMsHead] = ms;
    s_frameMsHead = (s_frameMsHead + 1) % kProfHistory;
}

float ProfileFps()
{
    if (!GEngine)
        return 0.0f;
    const uint32_t lastMs = GEngine->GetLastFrameDuration();
    return lastMs > 0 ? 1000.0f / static_cast<float>(lastMs) : 0.0f;
}

float ProfileFrameMs()
{
    if (!GEngine)
        return 0.0f;
    return static_cast<float>(GEngine->GetLastFrameDuration());
}
} // namespace

void DrawProfileTab()
{
    ProfileSample(); // pump every draw

    ImGui::TextUnformatted("Frame stats");
    ImGui::Separator();

    const float fps = ProfileFps();
    const float ms = ProfileFrameMs();
    ImGui::Text("FPS:   %.1f", fps);
    ImGui::Text("Frame: %.2f ms", ms);

    // Frame-time plot.  PlotLines is fine for ring-buffered floats;
    // ImGui handles the visual stride.  Y-axis fixed 0..50 ms (~20fps
    // floor) so spikes are visible without auto-rescaling jitter.
    ImGui::Separator();
    ImGui::PlotLines("##frame_ms", s_frameMs, kProfHistory, s_frameMsHead, "frame ms (last 240)", 0.0f, 50.0f,
                     ImVec2(0, 80));

    if (ImGui::Button("Reset history"))
    {
        for (int i = 0; i < kProfHistory; i++)
            s_frameMs[i] = 0.0f;
        s_frameMsHead = 0;
    }
}

// Memory tab — live MemoryUsed() value + peak tracker + history plot.
namespace
{
constexpr int kMemHistory = 240;
float s_memMb[kMemHistory] = {};
int s_memHead = 0;
size_t s_memPeak = 0;

void MemorySample()
{
    const size_t used = Foundation::MemoryUsed();
    if (used > s_memPeak)
        s_memPeak = used;
    s_memMb[s_memHead] = static_cast<float>(used) / (1024.0f * 1024.0f);
    s_memHead = (s_memHead + 1) % kMemHistory;
}
} // namespace

inline float ToMB(size_t bytes)
{
    return static_cast<float>(bytes) / (1024.0f * 1024.0f);
}

void DrawMemoryTab()
{
    MemorySample();

    const Foundation::ProcessMemoryStats stats = Foundation::MemoryProcessStats();
    const float mb = ToMB(stats.used);
    const float peakMb = ToMB(s_memPeak);

    ImGui::TextUnformatted("Process heap");
    ImGui::Separator();
    ImGui::Text("Current: %.1f MB", mb);
    ImGui::Text("Peak:    %.1f MB", peakMb);
    if (stats.softLimit || stats.hardLimit)
    {
        ImGui::Text("Soft (trim):  %.0f MB%s", ToMB(stats.softLimit),
                    stats.softLimit && stats.used > stats.softLimit ? "  (OVER — trimming to budgets)" : "");
        if (ImGui::IsItemHovered())
            ImGui::SetTooltip("Pressure watermark. Over it, each cache is trimmed back to its own\n"
                              "declared budget once per frame (FrameMaintenance). Never refuses.");
        ImGui::Text("Hard (evict): %.0f MB%s", ToMB(stats.hardLimit),
                    stats.hardLimit && stats.used > stats.hardLimit ? "  (OVER — evicting caches)" : "");
        if (ImGui::IsItemHovered())
            ImGui::SetTooltip("Eviction target, not a wall. Over it the allocator additionally claws\n"
                              "memory back with cost-ordered cache eviction — but never refuses an\n"
                              "allocation: refusing would crash the engine's many unchecked `new` sites.");
    }
    else
    {
        ImGui::TextDisabled("No process limit set (unlimited).");
    }

    // Plot — same shape as the profile tab.  Y-axis floats around the
    // peak; ImPlot would give a nicer presentation but PlotHistogram
    // is sufficient and zero-dependency.
    char overlay[64];
    snprintf(overlay, sizeof(overlay), "MB used (last %d frames)", kMemHistory);
    ImGui::PlotHistogram("##mem_mb", s_memMb, kMemHistory, s_memHead, overlay, 0.0f, peakMb * 1.1f + 1.0f,
                         ImVec2(0, 80));

    if (ImGui::Button("Reset peak / history"))
    {
        for (int i = 0; i < kMemHistory; i++)
            s_memMb[i] = 0.0f;
        s_memHead = 0;
        s_memPeak = stats.used;
    }

    // ── Per-subsystem residency (the FreeOnDemand registry) ────────────────
    // One snapshot drives both the count and the table so they can't disagree.
    Foundation::MemoryDomainStat domains[32];
    const int n = Foundation::MemorySnapshotDomains(domains, 32);

    ImGui::Spacing();
    ImGui::Text("Subsystems (%d registered)", n);
    ImGui::SameLine();
    if (ImGui::Button("Trim caches now"))
        Defer([] { Foundation::MemoryEnforceBudgets(); });
    if (ImGui::IsItemHovered())
        ImGui::SetTooltip("Evict every registered cache back to its declared budget\n"
                          "(FreeOnDemand). Domains with no budget are untouched.");
    ImGui::Separator();

    if (n == 0)
    {
        ImGui::TextDisabled("(no subsystems registered yet)");
    }
    else if (ImGui::BeginTable("mem_domains", 3, ImGuiTableFlags_Borders | ImGuiTableFlags_RowBg))
    {
        ImGui::TableSetupColumn("domain");
        ImGui::TableSetupColumn("held");
        ImGui::TableSetupColumn("budget / usage");
        ImGui::TableHeadersRow();
        for (int i = 0; i < n; i++)
        {
            const Foundation::MemoryDomainStat& d = domains[i];
            ImGui::TableNextRow();
            ImGui::TableNextColumn();
            ImGui::TextUnformatted(d.name);
            ImGui::TableNextColumn();
            // Byte-accounted caches show MB; count-only registries (shapes,
            // materials) show their item count instead of a misleading 0 MB.
            if (d.heldBytes > 0 || d.heldItems == 0)
                ImGui::Text("%.1f MB", ToMB(d.heldBytes));
            else
                ImGui::Text("%zu items", d.heldItems);
            ImGui::TableNextColumn();
            if (d.budgetBytes > 0)
            {
                const float frac = static_cast<float>(d.heldBytes) / static_cast<float>(d.budgetBytes);
                char label[48];
                snprintf(label, sizeof(label), "%.0f / %.0f MB", ToMB(d.heldBytes), ToMB(d.budgetBytes));
                ImGui::ProgressBar(frac > 1.0f ? 1.0f : frac, ImVec2(-1, 0), label);
            }
            else
            {
                ImGui::TextDisabled("— (no budget)");
            }
        }
        ImGui::EndTable();
    }

    // ── Live limit controls — set a watermark and watch the tick trim/evict ──
    ImGui::Spacing();
    ImGui::TextDisabled("Set process limits (MB; soft=trim, hard=evict; 0 = unlimited):");
    static int s_softMB = -1, s_hardMB = -1;
    if (s_softMB < 0) // seed once from the live values
    {
        s_softMB = static_cast<int>(ToMB(stats.softLimit));
        s_hardMB = static_cast<int>(ToMB(stats.hardLimit));
    }
    ImGui::SetNextItemWidth(120);
    ImGui::InputInt("soft (trim) MB", &s_softMB, 32, 256);
    ImGui::SameLine();
    ImGui::SetNextItemWidth(120);
    ImGui::InputInt("hard (evict) MB", &s_hardMB, 32, 256);
    if (s_softMB < 0)
        s_softMB = 0;
    if (s_hardMB < 0)
        s_hardMB = 0;
    if (ImGui::Button("Apply limits"))
    {
        const size_t soft = static_cast<size_t>(s_softMB) * (1024 * 1024);
        const size_t hard = static_cast<size_t>(s_hardMB) * (1024 * 1024);
        Defer([soft, hard] { Foundation::SetProcessMemoryLimits(soft, hard); });
    }
    ImGui::SameLine();
    ImGui::TextDisabled("session-only; persist via EngineConfig");
}

// Live mod picker + in-process reload. Lists the @<mod> folders actually present
// in the user's mods folder, lets you tick a set, and re-mounts the game with
// exactly that set — only from the main menu (re-mounting mid-mission would evict
// assets the simulation still references).
void DrawModsTab()
{
    namespace fs = std::filesystem;
    const std::string modsRoot = GamePaths::Instance().ModsDir();

    ImGui::TextUnformatted("Mods folder (scanned live):");
    ImGui::TextWrapped("%s", modsRoot.c_str());
    ImGui::Separator();

    // Checkbox state persists across frames: modId ("@foo") -> checked.
    static std::map<std::string, bool> s_modChecked;

    // Live scan for @<mod> folders each frame.
    std::vector<std::string> mods;
    std::error_code ec;
    if (fs::is_directory(modsRoot, ec))
    {
        for (const auto& entry : fs::directory_iterator(modsRoot, ec))
        {
            std::error_code dirEc;
            if (!entry.is_directory(dirEc))
                continue;
            const std::string name = entry.path().filename().string();
            if (!name.empty() && name[0] == '@')
                mods.push_back(name);
        }
    }
    std::sort(mods.begin(), mods.end());

    if (mods.empty())
        ImGui::TextDisabled("(no @<mod> folders here yet — drop one in and it shows up)");

    for (const std::string& modId : mods)
    {
        auto it = s_modChecked.find(modId);
        bool checked = (it != s_modChecked.end()) ? it->second : false;
        if (ImGui::Checkbox(modId.c_str(), &checked) || it == s_modChecked.end())
            s_modChecked[modId] = checked;
    }

    // Build the mod path from the checked set — semicolon-separated absolute
    // mod-folder paths (empty string = base game only). modsRoot ends with a sep.
    std::string modPath;
    for (const std::string& modId : mods)
    {
        auto it = s_modChecked.find(modId);
        if (it != s_modChecked.end() && it->second)
        {
            if (!modPath.empty())
                modPath += ';';
            modPath += modsRoot + modId;
        }
    }

    ImGui::Spacing();
    ImGui::Separator();
    ImGui::TextWrapped("Apply set: %s", modPath.empty() ? "(none — base game only)" : modPath.c_str());

    const bool canReload = Poseidon::GApp != nullptr && Poseidon::GApp->m_canRender && GWorld != nullptr &&
                           GWorld->GetMode() == GModeIntro;
    CheatButton("Reload with selected mods", canReload,
                canReload ? "Re-mount the game with exactly the checked mods.\n"
                            "Keeps the window, shows the loading screen, and lands\n"
                            "back on a fresh main menu with the new mod set.\n"
                            "Uncheck everything to reload the base game."
                          : "Available from the main menu only (not during a mission).",
                [modPath]
                {
                    SetVisible(false);
                    // Queue for the next AppIdle (before simulate/draw); running the reload
                    // inside the swap crashed the rebuilt world's first Simulate.
                    RequestDeferredReload(modPath.c_str());
                });
}

void AspectReapply()
{
    // Re-resolve + apply the aspect settings for the current viewport.
    // Deferred so the engine mutation runs after ImGui::Render returns.
    Defer(
        []
        {
            if (GEngine)
                GEngine->FireResizePostHook(GEngine->Width(), GEngine->Height());
        });
}

// Release the game's mouse grab while the panel is open so the cursor can
// leave the window (to drag-resize it); restore on close.  The game keeps
// simulating — this only frees the cursor.
void ApplyDevPanelMouseState()
{
    if (!GEngine)
        return;
    if (s_visible && !s_mouseReleasedByPanel)
    {
        s_savedMouseGrab = GEngine->IsMouseGrabbed();
        GEngine->SetMouseGrab(false);
        s_mouseReleasedByPanel = true;
    }
    else if (!s_visible && s_mouseReleasedByPanel)
    {
        GEngine->SetMouseGrab(s_savedMouseGrab);
        s_mouseReleasedByPanel = false;
    }
}

// Resize the window to the largest box of the given aspect ratio that fits
// the current monitor's usable area, then center it (so it stays fully
// visible).  Drives the normal SDL resize path → aspect re-resolves.
void ResizeWindowToRatio(float ratio)
{
    if (!s_window || ratio <= 0.0f)
        return;
    int availW = 1920, availH = 1080;
    const SDL_DisplayID disp = SDL_GetDisplayForWindow(s_window);
    SDL_Rect ub{};
    if (SDL_GetDisplayUsableBounds(disp, &ub) && ub.w > 0 && ub.h > 0)
    {
        availW = ub.w;
        availH = ub.h;
    }
    const float margin = 0.90f;
    float w = static_cast<float>(availW) * margin;
    float h = w / ratio;
    if (h > static_cast<float>(availH) * margin)
    {
        h = static_cast<float>(availH) * margin;
        w = h * ratio;
    }
    int iw = static_cast<int>(w + 0.5f);
    int ih = static_cast<int>(h + 0.5f);
    if (iw < 320)
        iw = 320;
    if (ih < 240)
        ih = 240;
    SDL_SetWindowSize(s_window, iw, ih);
    SDL_SetWindowPosition(s_window, SDL_WINDOWPOS_CENTERED_DISPLAY(disp), SDL_WINDOWPOS_CENTERED_DISPLAY(disp));
}

void DrawAspectTab()
{
    AspectRatio::LiveControls& live = AspectRatio::Live();
    bool changed = false;

    // --- Window size + monitor info + resize-to-ratio presets ---
    if (s_window)
    {
        int ww = 0, wh = 0;
        SDL_GetWindowSize(s_window, &ww, &wh);
        ImGui::Text("window : %d x %d  (%.3f)", ww, wh,
                    wh > 0 ? static_cast<float>(ww) / static_cast<float>(wh) : 0.0f);
        const SDL_DisplayID disp = SDL_GetDisplayForWindow(s_window);
        SDL_Rect ub{};
        if (SDL_GetDisplayUsableBounds(disp, &ub) && ub.h > 0)
            ImGui::Text("monitor: %d x %d  (%.3f)", ub.w, ub.h, static_cast<float>(ub.w) / static_cast<float>(ub.h));
        ImGui::TextDisabled("resize window (fits monitor, centered):");
        struct RatioPreset
        {
            const char* label;
            float ratio;
        };
        static const RatioPreset presets[] = {
            {"32:9", 32.0f / 9.0f}, {"21:9", 21.0f / 9.0f}, {"16:9", 16.0f / 9.0f}, {"16:10", 16.0f / 10.0f},
            {"3:2", 3.0f / 2.0f},   {"4:3", 4.0f / 3.0f},   {"5:4", 5.0f / 4.0f},
        };
        for (int i = 0; i < static_cast<int>(sizeof(presets) / sizeof(presets[0])); ++i)
        {
            if (i > 0 && i != 4) // row break before "3:2"
                ImGui::SameLine();
            if (ImGui::Button(presets[i].label))
            {
                const float r = presets[i].ratio;
                Defer([r] { ResizeWindowToRatio(r); });
            }
        }
        ImGui::Separator();
    }

    changed |= ImGui::Checkbox("Override enabled", &live.overrideEnabled);
    ImGui::SameLine();
    ImGui::TextDisabled("(off = display.cfg policy)");
    ImGui::Separator();

    int style = static_cast<int>(live.style);
    if (ImGui::Combo("Display style", &style, "Modern\0Legacy\0"))
    {
        live.style = (style == 1) ? AspectRatio::Legacy : AspectRatio::Modern;
        changed = true;
    }
    int clamp = static_cast<int>(live.clamp);
    if (ImGui::Combo("Ultrawide clamp", &clamp,
                     "Off\0"
                     "21:9\0"
                     "16:9\0"))
    {
        live.clamp = static_cast<AspectRatio::UltrawideClamp>(clamp);
        changed = true;
    }

    ImGui::Separator();
    changed |= ImGui::Checkbox("Pillarbox  (crop world to band + black bars)", &live.pillarbox);
    changed |= ImGui::Checkbox("HUD clamp  (center UI in band, world full)", &live.hudClamp);

    ImGui::Separator();
    changed |= ImGui::Checkbox("Manual viewport (noodle)", &live.manualRect);
    changed |= ImGui::SliderFloat("rect Left", &live.rectL, 0.0f, 1.0f, "%.3f");
    changed |= ImGui::SliderFloat("rect Top", &live.rectT, 0.0f, 1.0f, "%.3f");
    changed |= ImGui::SliderFloat("rect Right", &live.rectR, 0.0f, 1.0f, "%.3f");
    changed |= ImGui::SliderFloat("rect Bottom", &live.rectB, 0.0f, 1.0f, "%.3f");
    if (ImGui::Button("Reset rect to full"))
    {
        live.rectL = 0.0f;
        live.rectT = 0.0f;
        live.rectR = 1.0f;
        live.rectB = 1.0f;
        live.manualRect = false;
        changed = true;
    }

    if (changed)
        AspectReapply();

    ImGui::Separator();
    if (GEngine)
    {
        Poseidon::AspectSettings a;
        GEngine->GetAspectSettings(a);
        ImGui::Text("viewport   %d x %d", GEngine->Width(), GEngine->Height());
        ImGui::Text("FOV        L=%.3f  T=%.3f", a.leftFOV, a.topFOV);
        ImGui::Text("UI rect    x[%.3f..%.3f] y[%.3f..%.3f]", a.uiTopLeftX, a.uiBottomRightX, a.uiTopLeftY,
                    a.uiBottomRightY);
        ImGui::Text("world rect x[%.3f..%.3f] y[%.3f..%.3f]", a.worldLeft, a.worldRight, a.worldTop, a.worldBottom);
    }
}

void DrawGrassTab()
{
    if (!GEngine)
    {
        ImGui::TextDisabled("No engine.");
        return;
    }

    struct GrassMapChoice
    {
        const char* label;
        const char* worldKey;
    };
    // CWA's legacy internal world names differ from their displayed island
    // names: Eden is Everon, Abel is Malden, Cain is Kolgujev, and Noe is
    // Nogova. Keep both names visible so this tool works with the actual
    // installed WRP files, not a guessed display name.
    static constexpr GrassMapChoice maps[] = {
        {"Intro", "Intro"},
        {"Everon (Eden)", "Eden"},
        {"Malden (Abel)", "Abel"},
        {"Kolgujev (Cain)", "Cain"},
        {"Nogova (Noe)", "Noe"},
    };
    static int selectedMap = 0;
    static int selectedSurface = 0;
    static std::string observedWorld;

    // The mission header can remain on the intro world while an in-game map
    // has already switched. TerrainWgpu records the actual WRP it uploaded,
    // which is the only name safe to use for grass layer selection.
    const char* loadedMapName = GEngine->GetGrassLoadedMapName();
    const std::string activeMapFile = loadedMapName && *loadedMapName ? loadedMapName : Glob.header.worldname;
    std::string activeWorld = activeMapFile;
    const size_t lastSlash = activeWorld.find_last_of("\\/");
    if (lastSlash != std::string::npos)
        activeWorld.erase(0, lastSlash + 1);
    const size_t extension = activeWorld.find_last_of('.');
    if (extension != std::string::npos)
        activeWorld.erase(extension);
    if (activeWorld != observedWorld)
    {
        observedWorld = activeWorld;
        selectedSurface = 0;
        for (int i = 0; i < static_cast<int>(std::size(maps)); ++i)
        {
            if (strcmpi(activeWorld.c_str(), maps[i].worldKey) == 0)
            {
                selectedMap = i;
                break;
            }
        }
    }

    Engine::GrassSettings grass = GEngine->GetGrassSettings();
    bool changed = false;

    ImGui::TextUnformatted("Map and terrain surface");
    ImGui::SetNextItemWidth(220.0f);
    if (ImGui::BeginCombo("Map", maps[selectedMap].label))
    {
        for (int i = 0; i < static_cast<int>(std::size(maps)); ++i)
        {
            const bool selected = selectedMap == i;
            if (ImGui::Selectable(maps[i].label, selected))
                selectedMap = i;
            if (selected)
                ImGui::SetItemDefaultFocus();
        }
        ImGui::EndCombo();
    }
    const bool selectedMapLoaded = strcmpi(activeWorld.c_str(), maps[selectedMap].worldKey) == 0;
    ImGui::SameLine();
    const bool canSwitchMap = GWorld != nullptr && GWorld->GetMode() == GModeIntro;
    bool loadMap = false;
    if (canSwitchMap)
    {
        ImGui::BeginDisabled(selectedMapLoaded);
        loadMap = ImGui::Button("Load selected map");
        ImGui::EndDisabled();
    }
    else if (!selectedMapLoaded && GWorld != nullptr)
    {
        loadMap = ImGui::Button("Force-load selected map (dev)");
    }
    if (loadMap)
    {
        // Resolve through CfgWorlds. This is the same authoritative mapping as
        // mission loading and handles modded/relocated WRP paths correctly.
        const RString resolvedWorld = GetWorldName(maps[selectedMap].worldKey);
        const std::string worldFile = static_cast<const char*>(resolvedWorld);
        SetVisible(false);
        // Landscape switching invalidates textures and the scene, so do it only
        // after ImGui has finished this frame, exactly like the reload control.
        Defer([worldFile]
        {
            if (GWorld != nullptr)
                GWorld->SwitchLandscape(worldFile.c_str());
        });
    }
    ImGui::TextDisabled("Active terrain WRP: %s. Everon uses the internal name Eden.", activeMapFile.c_str());
    const RString selectedWorldFile = GetWorldName(maps[selectedMap].worldKey);
    ImGui::TextDisabled("Selected WRP: %s", static_cast<const char*>(selectedWorldFile));
    if (!canSwitchMap && !selectedMapLoaded)
        ImGui::TextDisabled("Force-load replaces the active mission landscape; use it only for grass testing.");

    // The terrain combo is deliberately separate from the map combo. It always
    // comes from the loaded map, so an Everon selection cannot accidentally
    // apply Eden layer indices to the active geography texture.
    const int surfaceCount = GEngine->GetGrassSurfaceCount();
    if (surfaceCount == 0)
    {
        ImGui::TextDisabled("Loading map terrain materials...");
    }
    else
    {
        selectedSurface = std::clamp(selectedSurface, 0, surfaceCount - 1);
        ImGui::SetNextItemWidth(390.0f);
        if (ImGui::BeginCombo("Terrain surface", GEngine->GetGrassSurfaceName(selectedSurface)))
        {
            for (int i = 0; i < surfaceCount; ++i)
            {
                const bool selected = selectedSurface == i;
                char label[512];
                snprintf(label, sizeof(label), "%d: %s", i, GEngine->GetGrassSurfaceName(i));
                if (ImGui::Selectable(label, selected))
                    selectedSurface = i;
                if (selected)
                    ImGui::SetItemDefaultFocus();
            }
            ImGui::EndCombo();
        }
        bool selectedEnabled = GEngine->IsGrassSurfaceEnabled(selectedSurface);
        if (ImGui::Checkbox("Spawn on selected terrain", &selectedEnabled))
            GEngine->SetGrassSurfaceEnabled(selectedSurface, selectedEnabled);
        ImGui::SameLine();
        if (ImGui::Button("Use selected only"))
        {
            for (int i = 0; i < surfaceCount; ++i)
                GEngine->SetGrassSurfaceEnabled(i, i == selectedSurface);
        }
        if (ImGui::Button("Clear all surfaces"))
        {
            for (int i = 0; i < surfaceCount; ++i)
                GEngine->SetGrassSurfaceEnabled(i, false);
        }
        ImGui::SameLine();
        if (ImGui::Button("Enable all surfaces"))
        {
            for (int i = 0; i < surfaceCount; ++i)
                GEngine->SetGrassSurfaceEnabled(i, true);
        }
        ImGui::TextDisabled("Select a material, then toggle it. The selector contains every terrain layer of this map.");
    }

    ImGui::Separator();
    ImGui::TextDisabled("GPU-generated terrain blades. Placement follows the terrain grass pass and excludes water, roads, forests and buildings.");
    ImGui::Separator();
    changed |= ImGui::Checkbox("Enabled", &grass.enabled);
    changed |= ImGui::Checkbox("Cast close grass shadows", &grass.castShadows);
    changed |= ImGui::Checkbox("Apply grass distance fog", &grass.applyFog);
    ImGui::TextDisabled("Both are grass-only visual controls; turn either off to inspect the procedural field.");
    changed |= ImGui::Checkbox("Ignore terrain exclusions (diagnostic)", &grass.ignoreGeographyExclusions);
    ImGui::TextDisabled("Use only to diagnose a legacy map with no grass: this also permits grass on roads, forests and buildings.");
    changed |= ImGui::SliderFloat("Coverage", &grass.density, 0.05f, 1.0f, "%.2f");
    ImGui::TextDisabled("Retained fraction of procedural candidate blades. 1.00 uses every candidate.");
    changed |= ImGui::SliderFloat("Density boost", &grass.densityBoost, 1.0f, 4.0f, "%.1fx");
    ImGui::TextDisabled("Raises density beyond the base grid. Very high values reduce the maximum usable radius.");
    changed |= ImGui::SliderFloat("Base spacing (m)", &grass.spacing, 0.10f, 0.75f, "%.2f");
    ImGui::TextDisabled("Distance between candidates before the density boost; lower values make grass substantially denser.");
    changed |= ImGui::SliderFloat("Radius (m)", &grass.radius, 8.0f, 5000.0f, "%.0f");
    ImGui::TextDisabled("Total grass radius (up to 5 km). Dense cards cover the inner ring; scalable GPU distant grass fills the outer field.");
    changed |= ImGui::SliderFloat("Blade height", &grass.height, 0.10f, 3.0f, "%.2fx");
    changed |= ImGui::Checkbox("Use live world wind", &grass.useLiveWind);
    changed |= ImGui::SliderFloat("Wind strength", &grass.windStrength, 0.0f, 3.0f, "%.2f");
    changed |= ImGui::SliderFloat("Wind direction", &grass.windDirection, -180.0f, 180.0f, "%.0f deg");
    ImGui::TextDisabled("Live wind follows weather. Disable it to test a manual direction; 0 degrees points east (+X).");
    changed |= ImGui::SliderFloat("Field clumping", &grass.clumping, 0.0f, 1.0f, "%.2f");
    changed |= ImGui::SliderFloat("Colour variation", &grass.colorVariation, 0.0f, 1.0f, "%.2f");
    changed |= ImGui::SliderFloat("Backlight transmission", &grass.transmission, 0.0f, 1.0f, "%.2f");
    const float effectiveSpacing = std::max(0.10f, grass.spacing / std::sqrt(std::max(1.0f, grass.densityBoost)));
    const float nearDetailRadius = std::min(grass.radius, effectiveSpacing * 255.0f);
    ImGui::TextDisabled("LOD field: detailed %.0f m, mid blades beyond it, distant terrain-cover proxy to %.0f m.", nearDetailRadius, grass.radius);
    ImGui::TextDisabled("Wind: travelling direction field plus local gusts; roots stay pinned and player/vehicle tracks persist for one minute.");

    ImGui::Separator();
    if (ImGui::Button("Reset ultra dense"))
    {
        grass = Engine::GrassSettings{};
        changed = true;
    }
    ImGui::SameLine();
    if (ImGui::Button("Ultra dense"))
    {
        grass.enabled = true;
        grass.density = 1.0f;
        grass.densityBoost = 4.0f;
        grass.spacing = 0.20f;
        grass.radius = 60.0f;
        grass.height = 1.25f;
        grass.useLiveWind = true;
        grass.windStrength = 1.2f;
        grass.clumping = 0.55f;
        grass.colorVariation = 0.35f;
        grass.transmission = 0.45f;
        grass.castShadows = true;
        grass.applyFog = true;
        changed = true;
    }
    ImGui::SameLine();
      if (ImGui::Button("Disable"))
      {
          grass.enabled = false;
          changed = true;
      }

      if (changed)
        GEngine->SetGrassSettings(grass);
}

void DrawFoliageTab()
{
    if (!GEngine)
    {
        ImGui::TextDisabled("No engine.");
        return;
    }

    ImGui::TextDisabled("Emulated leaf subsurface scattering for alpha-tested vegetation (wgpu).");
    ImGui::TextDisabled("Evens out the hard lit/dark split on low-poly canopy at harsh sun angles.");
    ImGui::TextDisabled("Stage 1: applies to every alpha-tested cutout section.");
    ImGui::Separator();

    Engine::FoliageSettings f = GEngine->GetFoliageSettings();
    bool changed = false;

    changed |= ImGui::SliderFloat("Transmission", &f.transScale, 0.0f, 2.0f, "%.2f");
    ImGui::TextDisabled("  DICE fast-SSS: light through the leaf, lifting the dark/backlit side (0 = off)");
    changed |= ImGui::SliderFloat("Distortion", &f.distortion, 0.0f, 1.0f, "%.2f");
    ImGui::TextDisabled("  bends the transmitted light toward the normal; higher = broader wrap-around");
    changed |= ImGui::SliderFloat("Transmission power", &f.transPower, 1.0f, 16.0f, "%.1f");
    ImGui::TextDisabled("  lobe tightness; higher = a smaller, sharper backlit glow near the sun");
    changed |= ImGui::SliderFloat("Wrap", &f.wrap, 0.0f, 1.0f, "%.2f");
    ImGui::TextDisabled("  softens the front terminator; 0 = hard Lambert (lit side stays unchanged)");
    changed |= ImGui::SliderFloat("Ambient boost", &f.ambientBoost, 0.5f, 4.0f, "%.2f");
    ImGui::TextDisabled("  sky-ambient multiplier for foliage only (1 = off); fades with distance");
    changed |= ImGui::SliderFloat("GI (ambient x light)", &f.giStrength, 0.0f, 1.0f, "%.2f");
    ImGui::TextDisabled("  scale ambient by terrain light level so shadowed foliage stops glowing (0 = off)");
    changed |= ImGui::SliderFloat("Fill fade end (m)", &f.fillFadeEnd, 0.0f, 1000.0f, "%.0f");
    ImGui::TextDisabled("  distance where fill + ambient boost fade out (distant foliage -> plain sky-ambient; 0 = never)");

    ImGui::Separator();
    ImGui::TextDisabled("Spherical canopy normals (GPU-driven path; leaf sections only)");
    changed |= ImGui::SliderFloat("Bush bend", &f.normalBend, 0.0f, 1.0f, "%.2f");
    ImGui::TextDisabled("  bend bush leaf normals toward a radial crown normal so the blob shades round (0 = off)");
    changed |= ImGui::SliderFloat("Bush crown Y (m)", &f.crownYOffset, -5.0f, 5.0f, "%.2f");
    ImGui::TextDisabled("  lift the bush crown centre up into the canopy (bounding-sphere centre sits a bit low)");
    changed |= ImGui::SliderFloat("Tree bend", &f.treeBend, 0.0f, 1.0f, "%.2f");
    ImGui::TextDisabled("  same, for trees — applies only to leaf/canopy sections; the solid trunk is untouched");
    changed |= ImGui::SliderFloat("Tree crown Y (m)", &f.treeCrownY, -2.0f, 12.0f, "%.2f");
    ImGui::TextDisabled("  lift the tree crown centre up above the trunk (the centre sits mid-trunk)");

    ImGui::Separator();
    if (ImGui::Button("Reset foliage to defaults"))
    {
        f = Engine::FoliageSettings{};
        changed = true;
    }
    ImGui::SameLine();
    if (ImGui::Button("Disable (zero strengths)"))
    {
        f.transScale = 0.0f;
        f.wrap = 0.0f;
        f.ambientBoost = 1.0f;
        f.normalBend = 0.0f;
        changed = true;
    }

    if (changed)
        GEngine->SetFoliageSettings(f);

    ImGui::Separator();
    ImGui::TextDisabled("Current tuning (copy back to share):");
    char summary[256];
    snprintf(summary, sizeof(summary),
             "foliage: trans=%.2f dist=%.2f transPow=%.1f wrap=%.2f amb=%.2f gi=%.2f fadeEnd=%.0f | "
             "bush=%.2f/%.2f tree=%.2f/%.2f",
             f.transScale, f.distortion, f.transPower, f.wrap, f.ambientBoost, f.giStrength, f.fillFadeEnd,
             f.normalBend, f.crownYOffset, f.treeBend, f.treeCrownY);
    ImGui::SetNextItemWidth(-1.0f);
    ImGui::InputText("##foliageSummary", summary, sizeof(summary), ImGuiInputTextFlags_ReadOnly);
    if (ImGui::Button("Copy foliage summary to clipboard"))
        ImGui::SetClipboardText(summary);
}

void DrawShadowsTab()
{
    if (!GEngine)
    {
        ImGui::TextDisabled("No engine.");
        return;
    }

    Engine::ShadowMapTuning t = GEngine->GetShadowMapTuning();
    bool changed = false;

    ImGui::TextDisabled("Depth-buffer shadow maps (durable fix).");
    ImGui::TextDisabled("OFF = legacy projected shadows. ON = light-space shadow map (no flicker).");
    ImGui::Separator();

    changed |= ImGui::Checkbox("Enabled (shadow maps)", &t.enabled);
    ImGui::SameLine();
    ImGui::TextDisabled(t.enabled ? "(projected path skipped)" : "(projected path active)");

    ImGui::Separator();

    changed |= ImGui::SliderFloat("Darkness", &t.darkness, 0.0f, 1.0f, "%.3f");
    ImGui::TextDisabled("  lit-colour multiplier where shadowed; lower = darker (1.0 = no shadow)");

    changed |= ImGui::SliderInt("Cascades", &t.cascadeCount, 1, 4);
    ImGui::TextDisabled("  total tiers (omni + frustum); more = crisper across distance");

    changed |= ImGui::SliderFloat("Distance coef", &t.distanceCoef, 0.05f, 1.0f, "%.3f");
    ImGui::TextDisabled("  frustum-tier far distance as a fraction of view distance (1.0 = full VD)");

    changed |= ImGui::SliderFloat("Shadow distance (m)", &t.shadowDistance, 0.0f, 1500.0f, "%.0f");
    ImGui::TextDisabled("  explicit cascade reach, decoupled from the 250 m clamp (0 = use game slider)");

    changed |= ImGui::SliderFloat("Split coef", &t.splitCoef, 0.0f, 1.0f, "%.2f");
    ImGui::TextDisabled("  PSSM blend: 0 = uniform splits, 1 = logarithmic (FP 0.95)");

    ImGui::Separator();
    ImGui::TextDisabled("Omni near tiers — camera-centred spheres, all-direction coverage");
    ImGui::TextDisabled("(so a caster behind/beside you still casts a shadow into view)");
    changed |= ImGui::SliderInt("Omni tiers", &t.omniCount, 0, t.cascadeCount);
    ImGui::TextDisabled("  leading tiers fit a sphere around the camera (0 = pure frustum)");
    changed |= ImGui::SliderFloat("Omni radius 0", &t.omniCoef0, 0.02f, 0.5f, "%.3f");
    changed |= ImGui::SliderFloat("Omni radius 1", &t.omniCoef1, 0.02f, 0.8f, "%.3f");
    ImGui::TextDisabled("  sphere radii as a fraction of the shadow range (ascending)");

    changed |= ImGui::SliderFloat("Bias base", &t.biasBase, 0.0f, 0.0005f, "%.6f");
    ImGui::TextDisabled("  per-cascade depth bias base*(i+1)^2; raise to kill acne");

    changed |= ImGui::SliderFloat("Normal offset", &t.normalOffset, 0.0f, 4.0f, "%.2f");
    ImGui::TextDisabled("  receiver push toward the light in world texels; raise to kill acne (wgpu)");

    changed |= ImGui::SliderFloat("PCF spread", &t.pcf, 0.0f, 3.0f, "%.2f");
    ImGui::TextDisabled("  < 0.5 = single tap (crisp); >= 0.5 = 4 taps this many texels apart (wgpu)");

    changed |= ImGui::SliderFloat("Caster LOD bias", &t.casterLodBias, 1.0f, 8.0f, "%.1f");
    ImGui::TextDisabled("  casters pick their LOD as if this many times farther away");

    changed |= ImGui::SliderFloat("Far fade (m)", &t.fadeRange, 1.0f, 120.0f, "%.1f");
    ImGui::TextDisabled("  distant shadows dissolve over this band instead of a hard cut-off");

    static const int resOptions[] = {512, 1024, 2048, 4096};
    int resIdx = 2;
    for (int i = 0; i < 4; ++i)
        if (resOptions[i] == t.resolution)
            resIdx = i;
    if (ImGui::Combo("Resolution", &resIdx,
                     "512\0"
                     "1024\0"
                     "2048\0"
                     "4096\0"))
    {
        t.resolution = resOptions[resIdx];
        changed = true;
    }
    ImGui::TextDisabled("  per-cascade depth-map size; higher = sharper, more VRAM");

    ImGui::Separator();
    ImGui::TextDisabled("Terrain sun-shadows (wgpu)");
    changed |= ImGui::Checkbox("Enabled (terrain sun-shadow)", &t.terrainShadowEnabled);
    changed |= ImGui::SliderFloat("Strength", &t.terrainShadowStrength, 0.0f, 1.0f, "%.2f");
    ImGui::TextDisabled("  occlusion scale; 1 = physical");
    changed |= ImGui::SliderInt("Mask supersample", &t.terrainShadowScale, 1, 8);
    ImGui::TextDisabled("  mask resolution vs heightmap");
    changed |= ImGui::SliderInt("March steps", &t.terrainShadowSteps, 16, 2048);
    ImGui::TextDisabled("  hard range cap");
    changed |= ImGui::SliderFloat("Penumbra (deg)", &t.terrainShadowPenumbra, 0.0f, 8.0f, "%.2f");
    ImGui::TextDisabled("  soft-edge half-width; 0 = hard, larger = softer");

    ImGui::Separator();
    ImGui::TextDisabled("Terrain sky-visibility AO (wgpu) — darkens AMBIENT in valleys/gorges/coves");
    changed |= ImGui::Checkbox("Enabled (sky-visibility AO)", &t.terrainSkyVisEnabled);
    changed |= ImGui::SliderFloat("SkyVis strength", &t.terrainSkyVisStrength, 0.0f, 1.0f, "%.2f");
    ImGui::TextDisabled("  how strongly occluded columns lose ambient (0 = off)");
    changed |= ImGui::SliderFloat("SkyVis contrast", &t.terrainSkyVisContrast, 1.0f, 12.0f, "%.1f");
    ImGui::TextDisabled("  occ = 1-pow(V,contrast); smooth terrain gives V~1, so raise this to see it");
    changed |= ImGui::SliderFloat("SkyVis floor", &t.terrainSkyVisFloor, 0.0f, 1.0f, "%.2f");
    ImGui::TextDisabled("  minimum ambient kept where fully occluded (never fully black)");
    changed |= ImGui::SliderFloat("SkyVis radius (m)", &t.terrainSkyVisRadius, 50.0f, 2000.0f, "%.0f");
    ImGui::TextDisabled("  horizon-scan reach; larger = distant ridges occlude too (re-runs the scan)");
    changed |= ImGui::SliderInt("SkyVis azimuths", &t.terrainSkyVisAzimuths, 4, 32);
    ImGui::TextDisabled("  scan direction count; more = smoother (re-runs the scan)");
    changed |= ImGui::SliderInt("SkyVis downsample", &t.terrainSkyVisDownsample, 1, 4);
    ImGui::TextDisabled("  mask coarseness vs heightmap; 1 = sharpest cliffs, slower (re-runs the scan)");
    changed |= ImGui::Checkbox("SkyVis debug (greyscale factor)", &t.terrainSkyVisDebug);
    ImGui::TextDisabled("  terrain shows the contrast-shaped sky-view factor for tuning");

    ImGui::Separator();
    if (ImGui::Button("Reset knobs to defaults"))
    {
        const bool keepEnabled = t.enabled;
        t = Engine::ShadowMapTuning{};
        t.enabled = keepEnabled;
        changed = true;
    }

    if (changed)
        GEngine->SetShadowMapTuning(t);

    // Read-back: a one-line summary the user can copy and paste back so the
    // values they tuned by eye can be baked into the engine defaults.
    ImGui::Separator();
    ImGui::TextDisabled("Current tuning (copy back to share):");
    char summary[512];
    snprintf(summary, sizeof(summary),
             "shadows: enabled=%s darkness=%.3f cascades=%d omni=%d/%.3f/%.3f dist=%.3f shadowDist=%.0f split=%.2f "
             "bias=%.6f normOfs=%.2f pcf=%.2f lodBias=%.1f fade=%.1f res=%d | terrain: on=%s str=%.2f scale=%d "
             "steps=%d pen=%.2f | skyvis: on=%s str=%.2f contrast=%.1f floor=%.2f radius=%.0f az=%d ds=%d",
             t.enabled ? "true" : "false", t.darkness, t.cascadeCount, t.omniCount, t.omniCoef0, t.omniCoef1,
             t.distanceCoef, t.shadowDistance, t.splitCoef, t.biasBase, t.normalOffset, t.pcf, t.casterLodBias,
             t.fadeRange, t.resolution, t.terrainShadowEnabled ? "true" : "false", t.terrainShadowStrength,
             t.terrainShadowScale, t.terrainShadowSteps, t.terrainShadowPenumbra,
             t.terrainSkyVisEnabled ? "true" : "false", t.terrainSkyVisStrength, t.terrainSkyVisContrast,
             t.terrainSkyVisFloor, t.terrainSkyVisRadius, t.terrainSkyVisAzimuths, t.terrainSkyVisDownsample);
    ImGui::SetNextItemWidth(-1.0f);
    ImGui::InputText("##shadowSummary", summary, sizeof(summary), ImGuiInputTextFlags_ReadOnly);
    if (ImGui::Button("Copy summary to clipboard"))
        ImGui::SetClipboardText(summary);
}

// Live anti-aliasing knobs — MSAA sample count, SSAA render scale and
// alpha-to-coverage apply at the next frame boundary, so the effect is
// visible immediately while hunting for the shipped default.
// Live frame-phase breakdown from the always-on FrameProfiler ring
// (World::Simulate marks setup/draw/hud/ai+veh/sound/swap each frame).
void DrawPerfTab()
{
    Dev::FrameProfiler& perf = Dev::GFrameProfiler();
    const int frames = perf.FrameCount();
    if (frames == 0)
    {
        ImGui::TextDisabled("no frames recorded yet");
        return;
    }

    const Dev::FrameProfiler::PhaseStats total = perf.TotalStats();
    ImGui::Text("FPS %.1f", perf.AvgFps());
    ImGui::SameLine();
    ImGui::TextDisabled("frame %.2f ms avg / %.2f p95 / %.2f max (last %d frames)", total.avgMs, total.p95Ms,
                        total.maxMs, frames);

    static float history[Dev::FrameProfiler::kRingSize];
    const int n = frames;
    for (int i = 0; i < n; i++)
        history[i] = perf.Frame(n - 1 - i).totalMs; // oldest → newest
    ImGui::PlotLines("##frametimes", history, n, 0, "frame ms", 0.f, total.maxMs * 1.2f, ImVec2(-1, 64));

    if (ImGui::BeginTable("phases", 5, ImGuiTableFlags_Borders | ImGuiTableFlags_RowBg))
    {
        ImGui::TableSetupColumn("phase");
        ImGui::TableSetupColumn("avg ms");
        ImGui::TableSetupColumn("p95 ms");
        ImGui::TableSetupColumn("max ms");
        ImGui::TableSetupColumn("% frame");
        ImGui::TableHeadersRow();
        for (int p = 0; p < Dev::FrameProfiler::PhaseCount; p++)
        {
            const auto s = perf.Stats(static_cast<Dev::FrameProfiler::Phase>(p));
            ImGui::TableNextRow();
            ImGui::TableNextColumn();
            ImGui::TextUnformatted(Dev::FrameProfiler::PhaseName(p));
            ImGui::TableNextColumn();
            ImGui::Text("%.2f", s.avgMs);
            ImGui::TableNextColumn();
            ImGui::Text("%.2f", s.p95Ms);
            ImGui::TableNextColumn();
            ImGui::Text("%.2f", s.maxMs);
            ImGui::TableNextColumn();
            ImGui::Text("%.0f%%", total.avgMs > 0.001f ? 100.f * s.avgMs / total.avgMs : 0.f);
        }
        ImGui::EndTable();
    }
    ImGui::Text("draw calls %.0f avg", perf.AvgDrawCalls());
    ImGui::SameLine();
    if (ImGui::Button("Reset window"))
        perf.Reset();
}
void DrawRenderTab()
{
    if (!GEngine)
    {
        ImGui::TextDisabled("engine not up");
        return;
    }

    int samples = GEngine->GetMsaaSamples();
    int sampleIdx = samples >= 8 ? 3 : samples >= 4 ? 2 : samples >= 2 ? 1 : 0;
    if (ImGui::Combo("MSAA", &sampleIdx,
                     "Off\0"
                     "2x\0"
                     "4x\0"
                     "8x\0"))
    {
        static const int kSamples[] = {0, 2, 4, 8};
        GEngine->SetMsaaSamples(kSamples[sampleIdx]);
    }

    float scale = GEngine->GetRenderScale();
    if (ImGui::SliderFloat("Render scale (SSAA)", &scale, 1.0f, 2.0f, "%.2f"))
        GEngine->SetRenderScale(scale);
    ImGui::SameLine();
    if (ImGui::Button("1x"))
        GEngine->SetRenderScale(1.0f);
    ImGui::SameLine();
    if (ImGui::Button("1.5x"))
        GEngine->SetRenderScale(1.5f);
    ImGui::SameLine();
    if (ImGui::Button("2x"))
        GEngine->SetRenderScale(2.0f);

    bool a2c = GEngine->GetAlphaToCoverage();
    if (ImGui::Checkbox("Alpha-to-coverage (cutout AA; needs MSAA)", &a2c))
        GEngine->SetAlphaToCoverage(a2c);

    bool flat = GEngine->GetDebugFlatColor();
    if (ImGui::Checkbox("Flat shading (objects -> solid red; shading-vs-geometry probe)", &flat))
        GEngine->SetDebugFlatColor(flat);

    ImGui::Separator();
    ImGui::Text("window  %d x %d", GEngine->Width(), GEngine->Height());
    ImGui::Text("target  scale %.2fx, %dx MSAA", GEngine->GetRenderScale(), GEngine->GetMsaaSamples());
    ImGui::TextDisabled("settings are session-only; persist via graphics.cfg");
}
// HDR tonemap / look tuning (wgpu HDR path). The Hable curve is fixed; these are
// exposure + a colour-grade block. "Auto (time of day)" drives the grade from the
// per-ToD preset keyframes; uncheck to override and tune a keyframe by eye, then
// copy the preset line back. See engine/WgpuRenderer/docs/hdr-pipeline-plan.md.
void DrawTonemapTab()
{
    if (!GEngine)
    {
        ImGui::TextDisabled("engine not up");
        return;
    }
    if (!GEngine->SupportsTonemap())
    {
        ImGui::TextDisabled("HDR path off (run the wgpu backend with WGR_HDR=1)");
        return;
    }

    bool autoTod = GEngine->GetTonemapAuto();
    if (ImGui::Checkbox("Auto (time of day)", &autoTod))
        GEngine->SetTonemapAuto(autoTod);
    ImGui::SameLine();
    ImGui::TextDisabled("ToD %.2f h", Glob.clock.GetTimeOfDay() * 24.0f);
    if (autoTod)
        ImGui::TextDisabled("grade driven by per-ToD presets; uncheck to override + tune");

    // In auto mode the sliders reflect the live interpolated grade but are read-only
    // (UpdateAutoTonemap overwrites them each frame).
    auto t = GEngine->GetTonemapSettings();
    bool changed = false;

    ImGui::BeginDisabled(autoTod);

    changed |= ImGui::Checkbox("Hable filmic", &t.hable);
    ImGui::SameLine();
    changed |= ImGui::Checkbox("sRGB encode", &t.encode);
    ImGui::SetItemTooltip("Off = passthrough clamp / write-as-is (debug only)");

    changed |= ImGui::SliderFloat("Exposure", &t.exposure, 0.05f, 8.0f, "%.3f", ImGuiSliderFlags_Logarithmic);

    ImGui::Separator();
    ImGui::TextUnformatted("Grade");
    changed |= ImGui::SliderFloat("Temperature (warm+/cool-)", &t.temperature, -1.0f, 1.0f, "%.3f");
    changed |= ImGui::SliderFloat("Tint (magenta+/green-)", &t.tint, -1.0f, 1.0f, "%.3f");
    changed |= ImGui::SliderFloat("Contrast", &t.contrast, 0.5f, 2.0f, "%.3f");
    changed |= ImGui::SliderFloat("Saturation", &t.saturation, 0.0f, 2.0f, "%.3f");
    changed |= ImGui::SliderFloat("Shadow lift", &t.lift, 0.0f, 0.3f, "%.3f");
    changed |= ImGui::SliderFloat("Gain", &t.gain, 0.1f, 4.0f, "%.3f");

    if (ImGui::Button("Reset to defaults"))
    {
        t = decltype(t){};
        changed = true;
    }

    if (changed && !autoTod)
        GEngine->SetTonemapSettings(t);

    ImGui::EndDisabled();

    // Bloom is a global look setting (not per-ToD keyframed), so it stays editable even
    // in auto mode — its values are preserved across the per-frame preset overwrite.
    ImGui::Separator();
    ImGui::TextUnformatted("Bloom");
    bool bloomChanged = false;
    bloomChanged |= ImGui::SliderFloat("Intensity##bloom", &t.bloomIntensity, 0.0f, 0.3f, "%.3f");
    ImGui::SetItemTooltip("Linear weight of the bloom added to the scene (0 = off).");
    bloomChanged |= ImGui::SliderFloat("Threshold##bloom", &t.bloomThreshold, 0.0f, 4.0f, "%.3f");
    ImGui::SetItemTooltip("Scene-referred luminance where bloom begins (soft knee).");
    bloomChanged |= ImGui::SliderFloat("Knee##bloom", &t.bloomKnee, 0.0f, 2.0f, "%.3f");
    if (bloomChanged)
        GEngine->SetTonemapSettings(t);

    // Auto-exposure / eye adaptation. Separate from the grade (its own setter), off by
    // default so it doesn't fight manual per-ToD exposure. Independent of auto/manual.
    ImGui::Separator();
    ImGui::TextUnformatted("Auto exposure (eye adaptation)");
    auto ex = GEngine->GetExposureSettings();
    bool exChanged = false;
    exChanged |= ImGui::Checkbox("Enabled##exposure", &ex.enabled);
    ImGui::SetItemTooltip("Off by default so it doesn't fight manual per-ToD exposure tuning.\n"
                          "When on, exposure is scaled toward key / scene-average luminance.");
    ImGui::BeginDisabled(!ex.enabled);
    exChanged |= ImGui::SliderFloat("Key (target grey)##exposure", &ex.key, 0.02f, 1.0f, "%.3f",
                                    ImGuiSliderFlags_Logarithmic);
    exChanged |= ImGui::SliderFloat("Min scale##exposure", &ex.minScale, 0.05f, 1.0f, "%.3f");
    exChanged |= ImGui::SliderFloat("Max scale##exposure", &ex.maxScale, 1.0f, 16.0f, "%.3f");
    exChanged |= ImGui::SliderFloat("Adapt rate##exposure", &ex.rate, 0.005f, 0.5f, "%.3f",
                                    ImGuiSliderFlags_Logarithmic);
    ImGui::SetItemTooltip("Per-frame ease toward the target (framerate-dependent for now).");
    exChanged |= ImGui::SliderFloat("Sky weight##exposure", &ex.skyWeight, 0.0f, 1.0f, "%.2f");
    ImGui::SetItemTooltip("Metering weight of the top of the frame (sky) vs the bottom (ground).\n"
                          "1.0 = uniform; lower biases exposure toward the ground so a bright\n"
                          "sky in view doesn't over-darken the scene.");
    ImGui::EndDisabled();
    if (exChanged)
        GEngine->SetExposureSettings(ex);
    // Live scale the resolve is applying (blocking GPU readback — diagnostic). 1.0 =
    // neutral; if this never budges across scenes the reduction/adapt isn't feeding it.
    ImGui::Text("Current scale: %.3f", GEngine->GetAutoExposureScale());

    ImGui::Separator();
    ImGui::TextDisabled("Preset (copy back to bake into the ToD keyframes):");
    char preset[512];
    snprintf(preset, sizeof(preset),
             "tonemap: exposure=%.3f temp=%.3f tint=%.3f contrast=%.3f sat=%.3f lift=%.3f gain=%.3f "
             "hable=%s encode=%s",
             t.exposure, t.temperature, t.tint, t.contrast, t.saturation, t.lift, t.gain,
             t.hable ? "true" : "false", t.encode ? "true" : "false");
    ImGui::SetNextItemWidth(-1.0f);
    ImGui::InputText("##tonemapPreset", preset, sizeof(preset), ImGuiInputTextFlags_ReadOnly);
    if (ImGui::Button("Copy preset to clipboard"))
        ImGui::SetClipboardText(preset);
    ImGui::TextDisabled("session-only; paste back to bake into the kTonemapPresets keyframes");
}

// Procedural sky tuning (wgpu). Celestial inputs (sun/moon direction, night factor)
// come live from LightSun; these are the authored atmosphere + look knobs. Writes
// immediately (a renderer-param setter, like the Tonemap tab). See
// engine/WgpuRenderer/docs/procedural-sky-plan.md.
void DrawSkyTab()
{
    if (!GEngine)
    {
        ImGui::TextDisabled("engine not up");
        return;
    }
    if (!GEngine->SupportsSky())
    {
        ImGui::TextDisabled("procedural sky unavailable (run the wgpu backend)");
        return;
    }

    auto s = GEngine->GetSkySettings();
    bool changed = false;

    changed |= ImGui::Checkbox("Enabled", &s.enabled);
    ImGui::SetItemTooltip("Off = skip the sky pass and restore the legacy skydome");
    ImGui::SameLine();
    ImGui::TextDisabled("ToD %.2f h", Glob.clock.GetTimeOfDay() * 24.0f);

    changed |= ImGui::Checkbox("Auto (time-of-day presets)", &s.autoToD);
    ImGui::SetItemTooltip("On = drive the atmosphere look (exposure, sun intensity, rayleigh, mie, ozone, "
                          "turbidity, sun radius, night intensity) from the per-ToD preset table each frame, "
                          "interpolated like the tonemap grade — the sliders below show the live values but "
                          "edits are overwritten next frame. Off = hold your manual values so you can tune "
                          "(then copy the preset). The toggles (sky lighting, aerial shadow, fog falloff) stay live either way.");

    ImGui::BeginDisabled(!s.enabled);

    changed |= ImGui::SliderFloat("Exposure", &s.exposure, 0.05f, 8.0f, "%.3f", ImGuiSliderFlags_Logarithmic);

    ImGui::Separator();
    ImGui::TextUnformatted("Sun");
    changed |= ImGui::SliderFloat("Intensity", &s.sunIntensity, 1.0f, 60.0f, "%.2f");
    float sunDeg = s.sunAngularRadius * 180.0f / 3.14159265f;
    if (ImGui::SliderFloat("Angular radius (deg)", &sunDeg, 0.1f, 5.0f, "%.2f"))
    {
        s.sunAngularRadius = sunDeg * 3.14159265f / 180.0f;
        changed = true;
    }

    ImGui::Separator();
    ImGui::TextUnformatted("Atmosphere");
    // Rayleigh/Mie coeffs are tiny (1/m); edit in convenient 1e-6 units.
    float rayleigh[3] = {s.rayleigh[0] * 1e6f, s.rayleigh[1] * 1e6f, s.rayleigh[2] * 1e6f};
    if (ImGui::SliderFloat3("Rayleigh (x1e-6)", rayleigh, 0.0f, 60.0f, "%.2f"))
    {
        s.rayleigh[0] = rayleigh[0] * 1e-6f;
        s.rayleigh[1] = rayleigh[1] * 1e-6f;
        s.rayleigh[2] = rayleigh[2] * 1e-6f;
        changed = true;
    }
    float mie = s.mie * 1e6f;
    if (ImGui::SliderFloat("Mie (x1e-6)", &mie, 0.0f, 100.0f, "%.2f"))
    {
        s.mie = mie * 1e-6f;
        changed = true;
    }
    changed |= ImGui::SliderFloat("Mie anisotropy g", &s.mieG, 0.0f, 0.99f, "%.3f");
    changed |= ImGui::SliderFloat("Rayleigh height (m)", &s.rayleighHeight, 1000.0f, 16000.0f, "%.0f");
    changed |= ImGui::SliderFloat("Mie height (m)", &s.mieHeight, 200.0f, 4000.0f, "%.0f");
    changed |= ImGui::SliderFloat("Turbidity", &s.turbidity, 0.5f, 10.0f, "%.2f");
    changed |= ImGui::SliderFloat("Ozone", &s.ozone, 0.0f, 4.0f, "%.2f");
    ImGui::SetItemTooltip("Ozone absorption strength — higher keeps twilight blue (the blue-hour knob)");
    changed |= ImGui::ColorEdit3("Ground albedo", s.ground);
    changed |= ImGui::SliderFloat("Horizon haze", &s.horizonHaze, 0.0f, 1.0f, "%.2f");
    ImGui::SetItemTooltip("Blend the sky toward the scene fog colour at the horizon so it meets the fogged terrain");
    changed |= ImGui::SliderFloat("Aerial sun shadow", &s.aerialShadow, 0.0f, 4.0f, "%.2f");
    ImGui::SetItemTooltip("Terrain occlusion of the froxel fog: 0 = off (sun lights the haze everywhere, for A/B), "
                          "1 = physical, >1 exaggerated to make the shadowed fog / god-ray shafts obvious");
    changed |= ImGui::SliderFloat("Fog falloff", &s.fogFalloff, 0.5f, 8.0f, "%.2f", ImGuiSliderFlags_Logarithmic);
    ImGui::SetItemTooltip("Aerial fog distance ramp exponent. High (default 3) = clear near/mid, fog only near the "
                          "draw edge. Low (~1) = dense fog throughout the view, which makes the volumetric terrain "
                          "sun-shadowing visible. Drop this to see the aerial-shadow effect.");

    ImGui::Separator();
    ImGui::TextUnformatted("Scene lighting (experimental, HDR only)");
    changed |= ImGui::Checkbox("Sky-based lighting", &s.skyLighting);
    ImGui::SetItemTooltip("Light terrain + objects FROM the atmosphere: sun = sunIntensity*exposure*transmittance "
                          "(reddens at dusk, fades below the horizon), on the physical scale. Off = legacy GL33 sun. "
                          "Toggle for A/B; the whole scene shifts scale, so exposure/grade will need re-tuning.");
    changed |= ImGui::SliderFloat("Sky ambient", &s.skyAmbient, 0.0f, 2.0f, "%.2f");
    ImGui::SetItemTooltip("Ambient scale for sky-based lighting (Stage 1 bootstrap: the engine's ToD ambient scaled "
                          "to the physical range; real sky irradiance later).");

    ImGui::Separator();
    ImGui::TextUnformatted("Quality");
    changed |= ImGui::SliderInt("View samples", &s.viewSamples, 4, 64);
    changed |= ImGui::SliderInt("Light samples", &s.lightSamples, 2, 32);

    ImGui::Separator();
    ImGui::TextUnformatted("Clouds");
    ImGui::TextDisabled("raymarched cloud shell in the sky (also reflected in water + SH ambient)");
    changed |= ImGui::SliderFloat("Coverage", &s.cloudCoverage, 0.0f, 1.0f, "%.2f");
    ImGui::SetItemTooltip("0 = clear sky; low = isolated cumulus; high = solid overcast deck. Also dims the "
                          "directional sun / lifts ambient as it rises (overcast reads flat).");
    changed |= ImGui::SliderFloat("Density", &s.cloudDensity, 0.005f, 0.3f, "%.3f", ImGuiSliderFlags_Logarithmic);
    ImGui::SetItemTooltip("Cloud extinction (1/m): higher = more opaque / darker undersides");
    changed |= ImGui::SliderFloat("Base altitude (m)", &s.cloudBottom, 200.0f, 6000.0f, "%.0f");
    changed |= ImGui::SliderFloat("Top altitude (m)", &s.cloudTop, 400.0f, 10000.0f, "%.0f");
    changed |= ImGui::SliderFloat2("Wind (m/s)", s.cloudWind, -30.0f, 30.0f, "%.1f");
    changed |= ImGui::SliderFloat("Shape size (m)", &s.cloudShapeSize, 2000.0f, 20000.0f, "%.0f");
    ImGui::SetItemTooltip("World size of the base cloud blobs — LARGER = less visible tiling across the map");
    changed |= ImGui::SliderFloat("Detail size (m)", &s.cloudDetailSize, 400.0f, 5000.0f, "%.0f");
    ImGui::SetItemTooltip("Edge detail tile — keep INCOMMENSURATE with shape (not a simple multiple) so the "
                          "combined pattern's visual period is long");
    changed |= ImGui::SliderFloat("Warp amount (m)", &s.cloudWarpAmount, 0.0f, 3000.0f, "%.0f");
    ImGui::SetItemTooltip("Domain-warp displacement — the single highest-impact anti-repetition knob (breaks "
                          "the grid regularity that makes tiling legible)");
    changed |= ImGui::SliderFloat("Warp size (m)", &s.cloudWarpSize, 2000.0f, 20000.0f, "%.0f");
    changed |= ImGui::SliderFloat("Weather amount", &s.cloudWeatherAmount, 0.0f, 1.0f, "%.2f");
    ImGui::SetItemTooltip("How much coverage DRIFTS across the sky (0 = uniform everywhere, which reads same-y)");
    changed |= ImGui::SliderFloat("Weather size (m)", &s.cloudWeatherSize, 5000.0f, 40000.0f, "%.0f");
    ImGui::SetItemTooltip("World scale of the coverage drift — big, so cloudy/clear regions span the map");
    changed |= ImGui::SliderFloat("Forward scatter g", &s.cloudHgG, 0.0f, 0.9f, "%.2f");
    ImGui::SetItemTooltip("Henyey-Greenstein anisotropy: higher = brighter silver lining toward the sun");
    changed |= ImGui::SliderFloat("Powder", &s.cloudPowder, 0.0f, 1.0f, "%.2f");
    ImGui::SetItemTooltip("Beer-Powder dark-edge term (the fluffy look)");
    changed |= ImGui::SliderFloat("Ambient fill", &s.cloudAmbient, 0.0f, 2.0f, "%.2f");
    ImGui::SetItemTooltip("Sky-ambient scale on the shadowed cloud sides");
    changed |= ImGui::SliderFloat("Max distance (m)", &s.cloudMaxDist, 5000.0f, 80000.0f, "%.0f");
    ImGui::SetItemTooltip("March / visibility cap; the far deck dissolves into the horizon haze");

    ImGui::Separator();
    ImGui::TextUnformatted("Night floor");
    ImGui::TextDisabled("authored deep-blue that fills in as the sun sets (the physical model goes near-black)");
    // Colours are normalised (click the swatch for the picker); intensity scales them.
    changed |= ImGui::ColorEdit3("Zenith colour", s.nightZenith);
    changed |= ImGui::ColorEdit3("Horizon colour", s.nightHorizon);
    changed |= ImGui::SliderFloat("Night intensity", &s.nightIntensity, 0.0f, 0.2f, "%.4f",
                                  ImGuiSliderFlags_Logarithmic);
    changed |= ImGui::SliderFloat("Day at sun elev (deg)", &s.nightStartDeg, -10.0f, 20.0f, "%.1f");
    ImGui::SetItemTooltip("Sun elevation at/above which it's full day (night floor off)");
    changed |= ImGui::SliderFloat("Night at sun elev (deg)", &s.nightEndDeg, -20.0f, 5.0f, "%.1f");
    ImGui::SetItemTooltip("Sun elevation at/below which it's full night (night floor at full intensity)");

    if (ImGui::Button("Reset to defaults"))
    {
        s = decltype(s){};
        changed = true;
    }

    ImGui::EndDisabled();

    if (changed)
        GEngine->SetSkySettings(s);

    // Copy the full authored sky state so it can be pasted into per-ToD keyframes
    // (no auto-interpolation for the sky yet; this is the hand-authoring hook).
    ImGui::Separator();
    ImGui::TextDisabled("Preset (copy to hand-author keyframes):");
    char preset[768];
    snprintf(preset, sizeof(preset),
             "sky: exposure=%.3f sunInt=%.2f sunRad=%.4f rayleigh=%.2f,%.2f,%.2f mie=%.2f mieG=%.3f "
             "ozone=%.2f turbidity=%.2f ground=%.3f,%.3f,%.3f haze=%.2f "
             "night=%.3f,%.3f,%.3f/%.3f,%.3f,%.3f int=%.4f band=%.1f,%.1f",
             s.exposure, s.sunIntensity, s.sunAngularRadius, s.rayleigh[0] * 1e6f, s.rayleigh[1] * 1e6f,
             s.rayleigh[2] * 1e6f, s.mie * 1e6f, s.mieG, s.ozone, s.turbidity, s.ground[0], s.ground[1],
             s.ground[2], s.horizonHaze, s.nightZenith[0], s.nightZenith[1], s.nightZenith[2],
             s.nightHorizon[0], s.nightHorizon[1], s.nightHorizon[2], s.nightIntensity, s.nightStartDeg,
             s.nightEndDeg);
    ImGui::SetNextItemWidth(-1.0f);
    ImGui::InputText("##skyPreset", preset, sizeof(preset), ImGuiInputTextFlags_ReadOnly);
    if (ImGui::Button("Copy preset to clipboard"))
        ImGui::SetClipboardText(preset);
}
void DrawCullingTab()
{
    if (!GEngine)
    {
        ImGui::TextDisabled("engine not up");
        return;
    }
    if (!GEngine->SupportsCullDebug())
    {
        ImGui::TextDisabled("GPU-driven rendering off (run the wgpu backend with WGR_GPU_DRIVEN=1)");
        return;
    }

    auto s = GEngine->GetCullDebugSettings();
    bool changed = false;

    changed |= ImGui::Checkbox("Draw cull spheres", &s.drawSpheres);
    ImGui::SetItemTooltip("Green wireframe of each retained instance's frustum-cull sphere "
                          "(centre = Object Position, radius = GetRadius), drawn on top of the "
                          "scene. Shows whether a sphere sits on its object and how high it floats.");
    // Shown as "Enable" (checked = on, like the occlusion toggle below) for a coherent tab; the
    // stored flag is still disableFrustum, so invert around the checkbox.
    bool enableFrustum = !s.disableFrustum;
    if (ImGui::Checkbox("Enable frustum culling", &enableFrustum))
    {
        s.disableFrustum = !enableFrustum;
        changed = true;
    }
    ImGui::SetItemTooltip("GPU frustum test. Turn OFF to draw everything in range: if objects that "
                          "vanish at certain pitches reappear with this off, the frustum cull is the "
                          "cause; if they still vanish, the cull is innocent and it's the draw/LOD path.");
    changed |= ImGui::Checkbox("Enable occlusion culling", &s.occlusion);
    ImGui::SetItemTooltip("Cull retained objects hidden behind the depth-prepass occluders "
                          "(terrain + drawn objects) via a Hi-Z depth pyramid. When on, the "
                          "engine's built-in software occlusion is disabled (GPU Hi-Z replaces it).");

    ImGui::Separator();
    if (ImGui::Button("Dump nearby instances to log"))
    {
        s.dumpNearby = true;
        changed = true;
    }
    ImGui::SetItemTooltip("Log every GPU-driven instance within 60 m: live Position vs the "
                          "position captured at registration vs the terrain surface. Stand next "
                          "to a misbehaving object first. above >> 0 = floating placement; "
                          "stale > 0 = the retained buffer holds an outdated transform.");

    if (changed)
    {
        GEngine->SetCullDebugSettings(s);
    }
}

// WTR-003 water debug view names — file scope so both the Water tab combo and the
// Ctrl+Shift+W cycle hotkey can reference them. Index maps 1:1 onto WgrWaterDebugView.
static const char* const kWaterDebugViews[] = {
    "Off (normal shading)",        // 0
    "FFT displacement",            // 1
    "FFT horizontal",              // 2
    "FFT vertical",                // 3
    "FFT slope",                   // 4
    "Jacobian",                    // 5
    "Compression",                 // 6
    "Curvature",                   // 7
    "Crest energy",                // 8
    "Slope variance",              // 9
    "Material coordinate",         // 10
    "Displaced world coordinate",  // 11
    "Interaction height",          // 12
    "Interaction velocity",        // 13
    "Interaction foam/aeration",   // 14
    "Persistent foam source",      // 15
    "Persistent foam history",     // 16
    "Surface velocity",            // 17
    "Water-column depth",          // 18
    "Camera-to-surface distance",  // 19
    "SSR colour",                  // 20
    "SSR confidence",              // 21
    "Planar colour",               // 22
    "Planar geometry validity",    // 23
    "Directional sky/cloud refl.", // 24
    "Reflection-source selection", // 25
    "Refraction ray",              // 26
    "Refraction hit validity",     // 27
    "Refraction path length",      // 28
    "RGB transmittance",           // 29
    "Underwater extinction",       // 30 (reserved)
    "Underwater in-scattering",    // 31 (reserved)
    "God-ray shadow visibility",   // 32 (reserved)
    "Caustic intensity",           // 33 (reserved)
    "Whitewater particle state",   // 34 (reserved)
    "Whitewater pool occupancy",   // 35 (reserved)
    "Particle overflow",           // 36 (reserved)
};
static constexpr int kWaterDebugViewCount = static_cast<int>(std::size(kWaterDebugViews));
static_assert(kWaterDebugViewCount == 37, "Water debug view names must match WgrWaterDebugView (0..36)");

// WTR-004 standard test scene definitions
static const char* const kWaterTestScenes[] = {
    "None (Custom / Authored Defaults)",                      // 0
    "WTR-Test-01 — Seabed checkerboard (Refraction)",        // 1
    "WTR-Test-02 — Cloud pitch (Reflection pitch stability)",  // 2
    "WTR-Test-03 — Ocean altitude (Cascade filtering)",       // 3
    "WTR-Test-04 — Projectile grid (Interaction solver)",     // 4
    "WTR-Test-05 — Boat wake (Vessel wake propagation)",      // 5
    "WTR-Test-06 — Explosion (Impulse & aeration)",           // 6
    "WTR-Test-07 — Underwater light (God rays & volumetric)", // 7
    "WTR-Test-08 — Waterline (Near-field submersion)",        // 8
    "WTR-Test-09 — Shoreline (Swash, foam & wet band)",       // 9
    "WTR-Test-10 — Weather transition (Calm/storm spectrum)"  // 10
};
static constexpr int kWaterTestSceneCount = static_cast<int>(std::size(kWaterTestScenes));

static void ApplyWtrTestScenePreset(Poseidon::Engine::WaterSettings& s, int index)
{
    s.testScene = index;
    switch (index)
    {
    case 1: // WTR-Test-01 — Seabed checkerboard
        s.enabled = true;
        s.alpha = 0.35f;
        s.colorExt = 0.05f;
        s.coastFade = 0.05f;
        s.foamWidth = 0.0f;
        s.foamIntensity = 0.0f;
        s.freeze.freezeTime = true;
        s.freeze.fixedTime = 12.0f;
        s.debugView = 18; // Water-column depth
        break;
    case 2: // WTR-Test-02 — Cloud pitch
        s.enabled = true;
        s.waveAmp = 0.0f; // Calm water
        s.freeze.freezeTime = true;
        s.freeze.fixedTime = 42.0f;
        s.freeze.freezeClouds = true;
        s.debugView = 24; // Directional sky/cloud reflection
        break;
    case 3: // WTR-Test-03 — Ocean altitude
        s.enabled = true;
        s.fadeStart = 1000.0f;
        s.fadeEnd = 10000.0f;
        s.freeze.freezeTime = true;
        s.freeze.fixedTime = 100.0f;
        s.debugView = 0;
        break;
    case 4: // WTR-Test-04 — Projectile grid
        s.enabled = true;
        s.freeze.freezeInteraction = false;
        s.freeze.fixedDelta = 1.0f / 60.0f;
        s.debugView = 12; // Interaction height
        break;
    case 5: // WTR-Test-05 — Boat wake
        s.enabled = true;
        s.debugView = 17; // Surface velocity
        break;
    case 6: // WTR-Test-06 — Explosion
        s.enabled = true;
        s.debugView = 14; // Interaction foam/aeration
        break;
    case 7: // WTR-Test-07 — Underwater light
        s.enabled = true;
        s.debugView = 31; // Underwater in-scattering
        break;
    case 8: // WTR-Test-08 — Waterline
        s.enabled = true;
        s.debugView = 29; // RGB transmittance
        break;
    case 9: // WTR-Test-09 — Shoreline
        s.enabled = true;
        s.swashAmp = 0.50f;
        s.swashSpeed = 0.05f;
        s.coastFade = 1.50f;
        s.foamWidth = 4.00f;
        s.foamIntensity = 1.00f;
        s.wetHeight = 0.50f;
        s.wetDarken = 0.40f;
        s.debugView = 0;
        break;
    case 10: // WTR-Test-10 — Weather transition
        s.enabled = true;
        s.freeze.freezeWeather = false;
        s.debugView = 0;
        break;
    default:
        break;
    }
}

void DrawWaterTab()
{
    if (!GEngine)
    {
        ImGui::TextDisabled("engine not up");
        return;
    }
    if (!GEngine->SupportsWater())
    {
        ImGui::TextDisabled("GPU water unavailable (run the wgpu backend with WGR_GPU_WATER)");
        return;
    }

    auto s = GEngine->GetWaterSettings();
    SetRifleWaterImpactSprayEnabled(s.rifleImpactSpray);
    bool changed = false;

    changed |= ImGui::Checkbox("Enabled", &s.enabled);
    ImGui::SetItemTooltip("Off = draw no water surface (the seabed shows through), for A/B");

    // Keep this immediately below the master Water switch: it controls the old CPU
    // impact presentation and must be easy to find during gameplay testing.
    changed |= ImGui::Checkbox("Water splash particles", &s.rifleImpactSpray);
    ImGui::SetItemTooltip("Off by default. Enables/disables the GPU whitewater and water-impact particle billboards. Ripples and foam remain active.");
    SetRifleWaterImpactSprayEnabled(s.rifleImpactSpray);
    ImGui::BeginDisabled(!s.rifleImpactSpray);
    changed |= ImGui::SliderFloat("Splash particle activity", &s.waterSplashParticleActivity, 0.0f, 1.0f, "%.2f");
    ImGui::EndDisabled();
    ImGui::SetItemTooltip("Strength of the GPU water-spray emitter when enabled. 0.25 is the restrained default; 1.00 restores the original full effect.");

    ImGui::BeginDisabled(!s.enabled);

    ImGui::Separator();
    ImGui::TextUnformatted("Waves (cosmetic — buoyancy stays on the flat plane)");

    const char* cascadePresets[] = {
        "Production Non-Harmonic (37m, 89m, 211m, 503m - >50km repeat)",
        "GodotOceanWaves Reference Style (88m, 57m, 16m - 3 cascades)",
        "Legacy Harmonic (48m, 144m, 432m, 1296m - 1296m repeat)"
    };
    if (ImGui::Combo("Cascade Preset", &s.cascadePreset, cascadePresets, IM_ARRAYSIZE(cascadePresets)))
    {
        changed = true;
    }
    ImGui::SetItemTooltip("WTR-036C / WTR-037: Toggle between production non-harmonic coprime cascades, GodotOceanWaves reference parity preset, and legacy harmonic cascades.");

    changed |= ImGui::SliderFloat("Amplitude", &s.waveAmp, 0.0f, 4.0f, "%.2f");
    ImGui::SetItemTooltip("Overall wave height scale. Kept gentle so boats never float in air.");
    changed |= ImGui::SliderFloat("Choppiness", &s.waveChoppy, 0.0f, 1.5f, "%.2f");
    ImGui::SetItemTooltip("Horizontal steepness of the crests (Gerstner Q).");
    changed |= ImGui::SliderFloat("Speed", &s.waveSpeed, 0.0f, 3.0f, "%.2f");
    changed |= ImGui::SliderFloat("Scale (wavelength)", &s.waveScale, 0.25f, 8.0f, "%.2f",
                                  ImGuiSliderFlags_Logarithmic);
    ImGui::SetItemTooltip("Multiplies every wavelength: >1 makes larger, farther-apart waves — the main "
                          "knob for how the field reads from a distance.");

    ImGui::Separator();
    ImGui::TextUnformatted("Distance detail (kills far-field moiré / repetition)");
    changed |= ImGui::SliderFloat("Fade start (m)", &s.fadeStart, 0.0f, 4000.0f, "%.0f");
    ImGui::SetItemTooltip("Distance at which wave detail begins to flatten.");
    changed |= ImGui::SliderFloat("Fade end (m)", &s.fadeEnd, 0.0f, 20000.0f, "%.0f",
                                  ImGuiSliderFlags_Logarithmic);
    ImGui::SetItemTooltip("Distance by which the water is fully flat (a smooth horizon mirror). Lower this "
                          "if the airplane view still shimmers or looks tiled; raise it if distant water "
                          "looks too dead.");
    changed |= ImGui::SliderFloat("De-tile warp (m)", &s.warpAmp, 0.0f, 20.0f, "%.2f");
    ImGui::SetItemTooltip("Low-frequency domain warp that bends the wave field off the regular grid.");

    ImGui::Separator();
    ImGui::TextUnformatted("Shading");
    changed |= ImGui::SliderFloat("Specular power", &s.specPower, 8.0f, 2000.0f, "%.0f",
                                  ImGuiSliderFlags_Logarithmic);
    ImGui::SetItemTooltip("Sun-glint sharpness (higher = tighter highlight).");
    changed |= ImGui::SliderFloat("Specular intensity", &s.specIntensity, 0.0f, 60.0f, "%.2f");
    ImGui::SetItemTooltip("Sun-glint brightness. Un-clamped on HDR so it blooms.");
    changed |= ImGui::SliderFloat("Opacity", &s.alpha, 0.0f, 1.0f, "%.2f");
    ImGui::SetItemTooltip("Base opacity looking straight down (grazing angles go opaque via Fresnel).");
    changed |= ImGui::SliderFloat("Shadow dim", &s.shadowDim, 0.0f, 1.0f, "%.2f");
    ImGui::SetItemTooltip("Terrain + CSM sun-shadow always removes the glint/direct sun on shadowed water; "
                          "this additionally darkens the shadowed surface (0 = physical sun-only removal).");

    ImGui::Separator();
    ImGui::TextUnformatted("Coast (depth-based colour + soft shoreline)");
    changed |= ImGui::ColorEdit3("Shallow colour", s.shallowColor);
    ImGui::SetItemTooltip("Body tint of shallow water (near the coast).");
    changed |= ImGui::ColorEdit3("Deep colour", s.deepColor);
    ImGui::SetItemTooltip("Body tint of deep water; the surface blends shallow -> deep with the water "
                          "column depth reconstructed from the opaque-depth prepass.");
    changed |= ImGui::SliderFloat("Colour clarity", &s.colorExt, 0.02f, 3.0f, "%.3f",
                                  ImGuiSliderFlags_Logarithmic);
    ImGui::SetItemTooltip("Extinction (1/m): higher = the tint reaches the deep colour in shallower water, "
                          "so the depth colouring reads stronger. Lower = subtler, more uniform colour.");
    changed |= ImGui::SliderFloat("Soft edge width (m)", &s.coastFade, 0.0f, 3.0f, "%.2f");
    ImGui::SetItemTooltip("Metres of water depth over which the shoreline fades transparent -> opaque. "
                          "Large values look misty/foggy at the coast; lower it for a crisper waterline.");
    changed |= ImGui::SliderFloat("Foam width (m)", &s.foamWidth, 0.0f, 8.0f, "%.2f");
    ImGui::SetItemTooltip("Column-depth band the churning foam spans (peaks ~1/4 in). 0 = no foam.");
    changed |= ImGui::SliderFloat("Foam intensity", &s.foamIntensity, 0.0f, 2.0f, "%.2f");
    ImGui::SetItemTooltip("Brightness / coverage of the shoreline foam.");
    changed |= ImGui::SliderFloat("Swash amplitude (m)", &s.swashAmp, 0.0f, 1.0f, "%.2f");
    ImGui::SetItemTooltip("How far the near-shore waterline oscillates in/out over the wet beach "
                          "(cosmetic — buoyancy stays on the flat plane).");
    changed |= ImGui::SliderFloat("Swash speed (Hz)", &s.swashSpeed, 0.0f, 1.0f, "%.3f",
                                  ImGuiSliderFlags_Logarithmic);
    ImGui::SetItemTooltip("Swash cycles per second (slow = long, lazy wash).");
    changed |= ImGui::SliderFloat("Wet band height (m)", &s.wetHeight, 0.0f, 4.0f, "%.2f");
    ImGui::SetItemTooltip("Terrain side: metres above sea level the damp/darkened intertidal band "
                          "reaches, on near-flat ground only (cliffs stay dry).");
    changed |= ImGui::SliderFloat("Wet darkening", &s.wetDarken, 0.3f, 1.0f, "%.2f");
    ImGui::SetItemTooltip("Albedo multiplier for wet sand (lower = darker). 1 = off.");

    // WTR-001 — deterministic water debug controls (dev / capture / A-B / shader-diff use only).
    // All freezes are renderer-local substitutions: they replace the UBO time/dt/seed the water,
    // interaction, foam, cloud, and underwater caustic shaders see, WITHOUT touching Glob.time
    // (gameplay + net clock) or any non-water subsystem other than the cloud wind offset (which
    // rides the same water sim clock by design). Leave "Freeze time" off to retain live animation.
    ImGui::Separator();
    ImGui::TextUnformatted("Debug (WTR-001 — deterministic capture / A-B)");
    ImGui::SetItemTooltip("Holds the water-sim clock, FFT, interaction solver, foam, or clouds "
                          "at a fixed value so the same frame reproduces across launches for "
                          "before/after captures and shader-diff work. Dev-only.");
    auto& fz = s.freeze;
    bool freezeFft = fz.freezeFft;
    if (ImGui::Checkbox("Freeze FFT##bool", &freezeFft))
    {
        fz.freezeFft = freezeFft;
        changed = true;
    }
    ImGui::SetItemTooltip("Skip Fft::dispatch: the wave-spectrum holds at its last computed state. "
                          "Combine with Freeze time to capture one frame's spectrum exactly.");
    bool freezeInteraction = fz.freezeInteraction;
    if (ImGui::Checkbox("Freeze interaction solver##bool", &freezeInteraction))
    {
        fz.freezeInteraction = freezeInteraction;
        changed = true;
    }
    ImGui::SetItemTooltip("dt = 0 + skip Interaction::dispatch: the local ripple field holds its "
                          "last state (no decay, no propagation, no event injection).");
    bool freezeFoam = fz.freezeFoam;
    if (ImGui::Checkbox("Freeze foam##bool", &freezeFoam))
    {
        fz.freezeFoam = freezeFoam;
        changed = true;
    }
    ImGui::SetItemTooltip("Skip Foam::dispatch: persistent foam stops advection + ageing at the "
                          "last state (use with Freeze time so the advecting surface velocity is 0).");
    bool freezeClouds = fz.freezeClouds;
    if (ImGui::Checkbox("Freeze clouds##bool", &freezeClouds))
    {
        fz.freezeClouds = freezeClouds;
        changed = true;
    }
    ImGui::SetItemTooltip("Hold the cloud wind world offset at fixed time, so the cloud shell does "
                          "not drift between captures. Implicit when Freeze time is on.");
    bool freezeWeather = fz.freezeWeather;
    if (ImGui::Checkbox("Freeze weather##bool", &freezeWeather))
    {
        fz.freezeWeather = freezeWeather;
        changed = true;
    }
    ImGui::SetItemTooltip("Reserve bit for future weather threading (no per-frame weather "
                          "recomputation today). Implicit when Freeze time is on, since the "
                          "interaction weather vector recomputes off the frozen time.");
    bool freezeTime = fz.freezeTime;
    if (ImGui::Checkbox("Freeze water-sim clock##bool", &freezeTime))
    {
        fz.freezeTime = freezeTime;
        changed = true;
    }
    ImGui::SetItemTooltip("Hold the water-sim clock passed to the FFT, interaction, foam and "
                          "underwater caustic shaders at fixed time. Clouds honour this too.");
    changed |= ImGui::SliderFloat("Fixed time (s)", &fz.fixedTime, 0.0f, 3600.0f, "%.2f");
    ImGui::SetItemTooltip("Seconds (replaces Glob.time when Freeze time or Freeze clouds is on). "
                          "One value keeps the four sim clocks (water, interaction, cloud, "
                          "underwater caustic) coherent for a single reproducible test frame.");
    changed |= ImGui::SliderInt("FFT seed override", &fz.fftSeed, -1, 0x00ff'ffff);
    ImGui::SetItemTooltip("Replaces fft_control[1] (authored default 1337). -1 = use 1337 (no "
                          "swap). Any non-negative value rewrites the spectrum's random field on "
                          "the next dispatch; two runs with the same seed reproduce h0 bit-for-bit.");
    changed |= ImGui::SliderFloat("Fixed delta (s)", &fz.fixedDelta, 0.0f, 1.0f / 30.0f, "%.4f");
    ImGui::SetItemTooltip("Fixes the interaction-solver step regardless of render FPS (0 = use the "
                          "live frame delta clamped to 1/30). For WTR-063 fixed-timestep validation; "
                          "leave 0 for capture mode (Freeze interaction is the standard freeze).");
    changed |= ImGui::SliderInt("Camera path frame", &fz.cameraPathFrame, -1, 100000);
    ImGui::SetItemTooltip("WTR-001 foundation only: when >= 0 the renderer tags each frame's water "
                          "UBO digest with this integer so two runs compare frame-by-frame. The "
                          "camera-path recorder itself is a separate WTR-004 work package; here we "
                          "expose just the integer index for manual capture-then-replay audits.");

    if (ImGui::Button("Reset to defaults"))
    {
        s = decltype(s){};
        changed = true;
    }

    ImGui::EndDisabled();

    // WTR-003 — water debug views. Replaces the water surface shading with a single diagnostic
    // (WgrWaterDebugView). Kept outside the disabled block so it works even with the water
    // surface toggled off. Reserved slots (underwater/god-ray/caustic/whitewater) render black
    // until their passes exist. The combo index maps 1:1 onto WgrWaterDebugView.
    ImGui::Separator();
    ImGui::TextUnformatted("Debug views (WTR-003)  [Ctrl+Shift+W cycles]");
    // kWaterDebugViews / kWaterDebugViewCount are at file scope (shared with the hotkey).
    int debugView = (s.debugView >= 0 && s.debugView < kWaterDebugViewCount) ? s.debugView : 0;
    if (ImGui::Combo("Debug view", &debugView, kWaterDebugViews, (int)std::size(kWaterDebugViews)))
    {
        s.debugView = debugView;
        changed = true;
    }
    ImGui::SetItemTooltip("Replaces the water surface output with the selected diagnostic. FFT / "
                          "interaction / foam views aggregate the four cascades; interaction & foam "
                          "fields read zero outside the 256 m camera domain. Reserved entries have no "
                          "backing pass yet and render black. wgpu backend only.");

    // WTR-004 — Standard test harness (deterministic animation, frame-stepping, snapshot/restore)
    ImGui::Separator();
    ImGui::TextUnformatted("Standard test harness (WTR-004)");
    auto& harness = Poseidon::WtrTestHarness::Instance();
    int testScene = harness.IsActive() ? harness.GetCurrentPresetId() : ((s.testScene >= 0 && s.testScene < kWaterTestSceneCount) ? s.testScene : 0);
    if (ImGui::Combo("Test scene preset", &testScene, kWaterTestScenes, kWaterTestSceneCount))
    {
        s.testScene = testScene;
        harness.SelectPreset(testScene, s, s.debugView);
        changed = true;
    }
    ImGui::SetItemTooltip("Selects a standard WTR-Test-01..10 test scene preset.");

    if (testScene > 0)
    {
        const auto* info = harness.GetPresetInfo(testScene);
        if (info)
        {
            if (info->availability == Poseidon::WtrTestAvailability::Available)
            {
                ImGui::TextColored(ImVec4(0.2f, 0.9f, 0.3f, 1.0f), "Status: Available");
            }
            else if (info->availability == Poseidon::WtrTestAvailability::Partial)
            {
                ImGui::TextColored(ImVec4(0.9f, 0.8f, 0.2f, 1.0f), "Status: %s", info->statusReason);
            }
            else
            {
                ImGui::TextColored(ImVec4(0.9f, 0.3f, 0.2f, 1.0f), "Status: %s", info->statusReason);
            }
        }

        ImGui::Spacing();
        if (!harness.IsActive())
        {
            if (ImGui::Button("Start Test Harness"))
            {
                harness.Start(s, s.debugView);
                changed = true;
            }
        }
        else
        {
            if (ImGui::Button(harness.IsPaused() ? "Resume" : "Pause"))
            {
                harness.Pause();
            }
            ImGui::SameLine();
            if (ImGui::Button("Step Frame"))
            {
                harness.StepFrame(s);
                changed = true;
            }
            ImGui::SameLine();
            if (ImGui::Button("Restart"))
            {
                harness.Restart(s);
                changed = true;
            }
            ImGui::SameLine();
            if (ImGui::Button("Stop & Restore Settings"))
            {
                int restoredDebugView = s.debugView;
                harness.Stop(s, restoredDebugView);
                s.debugView = restoredDebugView;
                changed = true;
            }

            ImGui::Text("Active Frame: %llu | Time: %.3f s | Triggers: %u",
                        static_cast<unsigned long long>(harness.GetFrameIndex()),
                        static_cast<double>(harness.GetFrameIndex() * harness.GetFixedDeltaTime()),
                        harness.GetTriggeredEventCount());

            if (ImGui::Button("Copy Metadata Log JSON"))
            {
                Vector3 dummyPos(100.0f, 5.0f, 100.0f);
                Vector3 dummyRot(0.0f, 0.0f, 0.0f);
                std::string logJson = harness.GenerateMetadataLog(s, dummyPos, dummyRot);
                ImGui::SetClipboardText(logJson.c_str());
            }
        }
    }

    if (changed)
        GEngine->SetWaterSettings(s);

    // Copy the full authored water look so the tuned values can be pasted back as the
    // Engine::WaterSettings defaults (like the Sky / Tonemap tabs).
    ImGui::Separator();
    ImGui::TextDisabled("Preset (copy to persist as defaults):");
    char preset[720];
    snprintf(preset, sizeof(preset),
             "water: amp=%.2f choppy=%.2f speed=%.2f scale=%.2f fade=%.0f,%.0f warp=%.2f "
             "spec=%.0f,%.2f alpha=%.2f shadowDim=%.2f shallow=%.3f,%.3f,%.3f deep=%.3f,%.3f,%.3f "
             "clarity=%.3f coastFade=%.2f foam=%.2f,%.2f swash=%.2f,%.3f wet=%.2f,%.2f",
             s.waveAmp, s.waveChoppy, s.waveSpeed, s.waveScale, s.fadeStart, s.fadeEnd, s.warpAmp,
             s.specPower, s.specIntensity, s.alpha, s.shadowDim, s.shallowColor[0], s.shallowColor[1],
             s.shallowColor[2], s.deepColor[0], s.deepColor[1], s.deepColor[2], s.colorExt, s.coastFade,
             s.foamWidth, s.foamIntensity, s.swashAmp, s.swashSpeed, s.wetHeight, s.wetDarken);
    ImGui::SetNextItemWidth(-1.0f);
    ImGui::InputText("##waterPreset", preset, sizeof(preset), ImGuiInputTextFlags_ReadOnly);
    if (ImGui::Button("Copy preset to clipboard"))
        ImGui::SetClipboardText(preset);

    // WTR-002 — per-region GPU pass timings (timestamp queries; the renderer harvests the
    // readback asynchronously, so values lag the displayed frame by the ring depth, ~2-3
    // frames). "n/a" rows are reserved spec slots (no standalone pass yet) or passes that
    // haven't run since launch (e.g. frozen dispatches, spectrum init after the first frame).
    ImGui::Separator();
    ImGui::TextUnformatted("GPU timings (WTR-002)");
    float gpuMs[32];
    const int gpuRegions = GEngine->GetWaterGpuTimings(gpuMs, 32);
    if (gpuRegions <= 0)
    {
        ImGui::TextDisabled("Unavailable (adapter lacks TIMESTAMP_QUERY / non-wgpu backend).");
    }
    else if (ImGui::BeginTable("wtrGpuTimings", 2, ImGuiTableFlags_Borders | ImGuiTableFlags_RowBg))
    {
        float gpuTotal = 0.0f;
        for (int i = 0; i < gpuRegions; ++i)
        {
            ImGui::TableNextRow();
            ImGui::TableNextColumn();
            ImGui::TextUnformatted(GEngine->GetWaterGpuTimingName(i));
            ImGui::TableNextColumn();
            if (gpuMs[i] < 0.0f)
                ImGui::TextDisabled("n/a");
            else
            {
                ImGui::Text("%.3f ms", gpuMs[i]);
                gpuTotal += gpuMs[i];
            }
        }
        ImGui::EndTable();
        ImGui::Text("Measured total: %.3f ms", gpuTotal);
        ImGui::SetItemTooltip("Sum of the rows above (last completed frame). Not the water "
                              "pipeline's wall-clock cost: passes may overlap on the GPU and "
                              "reserved rows are folded into their host pass (SSR/refraction "
                              "inside Water draw, caustics inside Underwater composite).");
    }
}
void DrawMouseTab()
{
    // Plain field writes into live GInput.mouse — no Defer needed (cf. DrawCheatsTab).
    auto& sub = InputSubsystem::Instance();
    MouseTuning& t = sub.GetMouseTuning();

    ImGui::TextUnformatted("Player settings (final)");
    ImGui::Separator();

    int dpiIdx = 0; // Off
    if (t.dpiNormalize)
    {
        int bestDiff = 1 << 30;
        for (int i = 1; i < kMouseDpiPresetCount; ++i)
        {
            int d = t.mouseDpi - kMouseDpiPresets[i];
            if (d < 0)
                d = -d;
            if (d < bestDiff)
            {
                bestDiff = d;
                dpiIdx = i;
            }
        }
    }
    if (ImGui::Combo("Mouse DPI", &dpiIdx, kMouseDpiLabels, kMouseDpiPresetCount))
    {
        t.dpiNormalize = dpiIdx > 0;
        if (dpiIdx > 0)
            t.mouseDpi = kMouseDpiPresets[dpiIdx];
    }
    if (ImGui::IsItemHovered())
        ImGui::SetTooltip("Set this to your mouse's DPI. The game then feels like %d DPI at any hardware.\n"
                          "Off = classic (no compensation).",
                          t.referenceDpi);

    float sx = sub.GetMouseSensitivityX();
    if (ImGui::SliderFloat("Sensitivity X", &sx, t.SensMin(), t.SensMax(), "%.3f"))
        sub.SetMouseSensitivityX(sx);
    float sy = sub.GetMouseSensitivityY();
    if (ImGui::SliderFloat("Sensitivity Y", &sy, t.SensMin(), t.SensMax(), "%.3f"))
        sub.SetMouseSensitivityY(sy);

    bool rev = sub.IsReverseMouse();
    if (ImGui::Checkbox("Invert Y axis", &rev))
        sub.SetReverseMouse(rev);
    bool swap = sub.IsMouseButtonsReversed();
    if (ImGui::Checkbox("Swap mouse buttons", &swap))
        sub.SetMouseButtonsReversed(swap);

    ImGui::Spacing();
    ImGui::TextUnformatted("Final values (live)");
    ImGui::Separator();

    // Cursor math mirrors MouseState::Update (kCursorScaleX = 1/200; screen = 2 NDC).
    const float dpiF = t.DpiFactor();
    const float perCountX = sx * t.baseScale * dpiF / 200.0f;
    ImGui::Text("Reference DPI: %d   |   DPI factor: %.3f", t.referenceDpi, dpiF);
    ImGui::Text("Effective sensitivity X (x baseScale): %.3f", sx * t.baseScale * dpiF);
    if (perCountX > 0.0f)
    {
        const float countsCrossX = 2.0f / perCountX;
        ImGui::Text("Counts to cross screen (X): %.0f", countsCrossX);
        if (t.dpiNormalize && t.mouseDpi > 0)
        {
            // Normalized: physical hand travel is DPI-independent (mouseDpi cancels).
            const float inch = countsCrossX / static_cast<float>(t.mouseDpi);
            ImGui::Text("Hand travel to cross screen (X): %.2f in / %.1f cm", inch, inch * 2.54f);
            ImGui::TextDisabled("  same physical feel at every DPI — normalization working");
        }
        else
        {
            ImGui::TextDisabled("  Off: raw counts — physical feel depends on your hardware DPI");
        }
    }
    if (s_window)
        ImGui::Text("SDL display scale: %.2f   pixel density: %.2f", SDL_GetWindowDisplayScale(s_window),
                    SDL_GetWindowPixelDensity(s_window));
    ImGui::TextDisabled("Mouse moves over this panel are captured by ImGui — close it to feel changes in game.");

    ImGui::Spacing();
    if (ImGui::CollapsingHeader("Advanced (dev only)"))
    {
        ImGui::SliderFloat("Base scale", &t.baseScale, 0.1f, 3.0f, "%.3f");
        if (ImGui::IsItemHovered())
            ImGui::SetTooltip("Master look scale (was the hard-coded 1.5).");
        ImGui::SliderInt("Reference DPI", &t.referenceDpi, 100, 3200);
        ImGui::SliderFloat("Smoothing", &t.smoothing, 0.0f, 0.95f, "%.2f");
        ImGui::Checkbox("Acceleration", &t.acceleration);
        ImGui::BeginDisabled(!t.acceleration);
        ImGui::SliderFloat("Accel exponent", &t.accelExponent, 1.0f, 2.0f, "%.2f");
        ImGui::EndDisabled();
        ImGui::SliderFloat("Menu cursor scale", &t.menuCursorScale, 0.1f, 4.0f, "%.2f");
        if (ImGui::Checkbox("Extended sensitivity range", &t.extendedRange))
        {
            sub.SetMouseSensitivityX(std::clamp(sub.GetMouseSensitivityX(), t.SensMin(), t.SensMax()));
            sub.SetMouseSensitivityY(std::clamp(sub.GetMouseSensitivityY(), t.SensMin(), t.SensMax()));
        }
        if (ImGui::IsItemHovered())
            ImGui::SetTooltip("Off = legacy 0.5..2.0 sensitivity range. On = 0.05..3.0.");
    }

    ImGui::Spacing();
    ImGui::Separator();
    if (ImGui::Button("Reset tuning to classic"))
        t = MouseTuning{};
    ImGui::SameLine();
    if (ImGui::Button("Save to mouse.cfg"))
        Defer([] { InputSubsystem::Instance().SaveKeys(); });
    if (ImGui::IsItemHovered())
        ImGui::SetTooltip("Persist sensitivity + tuning to mouse.cfg (also written for old builds).");
}

void DrawMainWindow()
{
    ImGui::SetNextWindowSize(ImVec2(560, 480), ImGuiCond_FirstUseEver);
    ImGui::Begin("Poseidon Dev Panel");

    if (ImGui::BeginTabBar("DevPanelTabs"))
    {
        if (ImGui::BeginTabItem("Cheats"))
        {
            DrawCheatsTab();
            ImGui::EndTabItem();
        }
        if (ImGui::BeginTabItem("Mods"))
        {
            DrawModsTab();
            ImGui::EndTabItem();
        }
        if (ImGui::BeginTabItem("Game"))
        {
            DrawGameTab();
            ImGui::EndTabItem();
        }
        if (ImGui::BeginTabItem("Mouse"))
        {
            DrawMouseTab();
            ImGui::EndTabItem();
        }
        if (ImGui::BeginTabItem("Console"))
        {
            DrawConsoleTab();
            ImGui::EndTabItem();
        }
        if (ImGui::BeginTabItem("Profile"))
        {
            DrawProfileTab();
            ImGui::EndTabItem();
        }
        ImGuiTabItemFlags memoryFlags = 0;
        if (s_selectMemoryTab)
        {
            memoryFlags = ImGuiTabItemFlags_SetSelected;
            s_selectMemoryTab = false;
        }
        if (ImGui::BeginTabItem("Memory", nullptr, memoryFlags))
        {
            DrawMemoryTab();
            ImGui::EndTabItem();
        }
        if (ImGui::BeginTabItem("Perf"))
        {
            DrawPerfTab();
            ImGui::EndTabItem();
        }
        if (ImGui::BeginTabItem("Font"))
        {
            DrawFontTab();
            ImGui::EndTabItem();
        }
        if (ImGui::BeginTabItem("Aspect"))
        {
            DrawAspectTab();
            ImGui::EndTabItem();
        }
        if (ImGui::BeginTabItem("Render"))
        {
            DrawRenderTab();
            ImGui::EndTabItem();
        }
        if (ImGui::BeginTabItem("Tonemap"))
        {
            DrawTonemapTab();
            ImGui::EndTabItem();
        }
        if (ImGui::BeginTabItem("Sky"))
        {
            DrawSkyTab();
            ImGui::EndTabItem();
        }
        if (ImGui::BeginTabItem("Water"))
        {
            DrawWaterTab();
            ImGui::EndTabItem();
        }
        if (ImGui::BeginTabItem("Culling"))
        {
            DrawCullingTab();
            ImGui::EndTabItem();
        }
        if (ImGui::BeginTabItem("Foliage"))
        {
            DrawFoliageTab();
            ImGui::EndTabItem();
        }
        if (ImGui::BeginTabItem("Grass"))
        {
            DrawGrassTab();
            ImGui::EndTabItem();
        }
        ImGuiTabItemFlags shadowFlags = 0;
        if (s_selectShadowsTab)
        {
            shadowFlags = ImGuiTabItemFlags_SetSelected;
            s_selectShadowsTab = false;
        }
        if (ImGui::BeginTabItem("Shadows", nullptr, shadowFlags))
        {
            DrawShadowsTab();
            ImGui::EndTabItem();
        }
        ImGui::EndTabBar();
    }

    ImGui::Separator();
    ImGui::TextDisabled("Ctrl+` / Ctrl+; to hide");
    ImGui::End();
}
} // namespace

namespace
{
void CreateSharedContext(SDL_Window* window)
{
    s_window = window;
    IMGUI_CHECKVERSION();
    ImGui::CreateContext();
    ImGuiIO& io = ImGui::GetIO();
    io.ConfigFlags |= ImGuiConfigFlags_NavEnableKeyboard;
    io.IniFilename = nullptr; // no imgui.ini side-effects
    ImGui::StyleColorsDark();
}

void UpdateEngineTextures(ImVector<ImTextureData*>* textures)
{
    if (!textures || !GEngine || !GEngine->SupportsOverlayRenderer())
        return;
    for (ImTextureData* tex : *textures)
    {
        if (tex->Status == ImTextureStatus_WantCreate)
        {
            IM_ASSERT(tex->Format == ImTextureFormat_RGBA32);
            const uint64_t id =
                GEngine->OverlayTextureCreate(tex->Width, tex->Height, static_cast<const uint8_t*>(tex->GetPixels()));
            if (id == 0)
                continue;
            tex->SetTexID(static_cast<ImTextureID>(id));
            tex->SetStatus(ImTextureStatus_OK);
        }
        else if (tex->Status == ImTextureStatus_WantUpdates)
        {
            // Full re-upload; the FFI has no sub-rect update and atlas textures are small.
            GEngine->OverlayTextureUpdate(static_cast<uint64_t>(tex->TexID), tex->Width, tex->Height,
                                          static_cast<const uint8_t*>(tex->GetPixels()));
            tex->SetStatus(ImTextureStatus_OK);
        }
        else if (tex->Status == ImTextureStatus_WantDestroy && tex->UnusedFrames > 0)
        {
            GEngine->OverlayTextureDestroy(static_cast<uint64_t>(tex->TexID));
            tex->SetTexID(ImTextureID_Invalid);
            tex->SetStatus(ImTextureStatus_Destroyed);
        }
    }
}

// Flatten ImGui's draw lists into one vertex/index pool + scissored draw
// records and hand them to the engine for composition over the frame.
void RenderDrawDataEngine(ImDrawData* dd)
{
    static_assert(sizeof(ImDrawIdx) == 2, "engine overlay indices are 16-bit");
    static_assert(sizeof(Engine::OverlayVertex) == sizeof(ImDrawVert), "OverlayVertex must mirror ImDrawVert");

    UpdateEngineTextures(dd->Textures);
    if (!GEngine || !GEngine->SupportsOverlayRenderer())
        return;

    const ImVec2 off = dd->DisplayPos;
    const ImVec2 scale = dd->FramebufferScale;

    static std::vector<Engine::OverlayVertex> verts;
    static std::vector<uint16_t> indices;
    static std::vector<Engine::OverlayDrawCmd> cmds;
    verts.clear();
    indices.clear();
    cmds.clear();
    verts.reserve(dd->TotalVtxCount);
    indices.reserve(dd->TotalIdxCount);

    for (int li = 0; li < dd->CmdListsCount; li++)
    {
        const ImDrawList* dl = dd->CmdLists[li];
        const uint32_t vtxBase = static_cast<uint32_t>(verts.size());
        const uint32_t idxBase = static_cast<uint32_t>(indices.size());
        for (const ImDrawVert& v : dl->VtxBuffer)
        {
            verts.push_back({(v.pos.x - off.x) * scale.x, (v.pos.y - off.y) * scale.y, v.uv.x, v.uv.y, v.col});
        }
        indices.insert(indices.end(), dl->IdxBuffer.begin(), dl->IdxBuffer.end());
        for (const ImDrawCmd& c : dl->CmdBuffer)
        {
            if (c.UserCallback)
            {
                if (c.UserCallback != ImDrawCallback_ResetRenderState)
                    c.UserCallback(dl, &c);
                continue;
            }
            if (c.ElemCount == 0)
                continue;
            Engine::OverlayDrawCmd oc;
            oc.clip[0] = (c.ClipRect.x - off.x) * scale.x;
            oc.clip[1] = (c.ClipRect.y - off.y) * scale.y;
            oc.clip[2] = (c.ClipRect.z - off.x) * scale.x;
            oc.clip[3] = (c.ClipRect.w - off.y) * scale.y;
            if (oc.clip[2] <= oc.clip[0] || oc.clip[3] <= oc.clip[1])
                continue;
            oc.texture = static_cast<uint64_t>(c.GetTexID());
            oc.firstIndex = idxBase + c.IdxOffset;
            oc.indexCount = c.ElemCount;
            oc.baseVertex = vtxBase + c.VtxOffset;
            cmds.push_back(oc);
        }
    }

    GEngine->SubmitOverlay(verts.data(), static_cast<int>(verts.size()), indices.data(),
                           static_cast<int>(indices.size()), cmds.data(), static_cast<int>(cmds.size()));
}
} // namespace

void Init(SDL_Window* window, void* glContext)
{
    if (s_initialized)
        return;
    CreateSharedContext(window);

    if (!ImGui_ImplSDL3_InitForOpenGL(window, glContext))
    {
        LOG_ERROR(Graphics, "DebugOverlay: ImGui_ImplSDL3_InitForOpenGL failed");
        return;
    }
    if (!ImGui_ImplOpenGL3_Init("#version 330"))
    {
        LOG_ERROR(Graphics, "DebugOverlay: ImGui_ImplOpenGL3_Init failed");
        return;
    }

    s_backend = RenderBackend::OpenGL3;
    s_initialized = true;
    LOG_INFO(Graphics, "DebugOverlay: ImGui initialized (press Ctrl+` / Ctrl+; to toggle)");
}

void InitForEngine(SDL_Window* window)
{
    if (s_initialized)
        return;
    CreateSharedContext(window);

    if (!ImGui_ImplSDL3_InitForOther(window))
    {
        LOG_ERROR(Graphics, "DebugOverlay: ImGui_ImplSDL3_InitForOther failed");
        return;
    }
    ImGuiIO& io = ImGui::GetIO();
    io.BackendRendererName = "imgui_impl_poseidon_overlay";
    io.BackendFlags |= ImGuiBackendFlags_RendererHasVtxOffset;
    io.BackendFlags |= ImGuiBackendFlags_RendererHasTextures;

    s_backend = RenderBackend::Engine;
    s_initialized = true;
    LOG_INFO(Graphics, "DebugOverlay: ImGui initialized on the engine overlay backend "
                       "(press Ctrl+` / Ctrl+; to toggle)");
}

void Shutdown()
{
    if (!s_initialized)
        return;
    if (s_backend == RenderBackend::OpenGL3)
    {
        ImGui_ImplOpenGL3_Shutdown();
    }
    else
    {
        // Release engine textures while the engine is still alive; if it is
        // already gone the renderer teardown frees them anyway.
        for (ImTextureData* tex : ImGui::GetPlatformIO().Textures)
        {
            if (tex->RefCount != 1)
                continue;
            if (GEngine && GEngine->SupportsOverlayRenderer() && tex->TexID != ImTextureID_Invalid)
                GEngine->OverlayTextureDestroy(static_cast<uint64_t>(tex->TexID));
            tex->SetTexID(ImTextureID_Invalid);
            tex->SetStatus(ImTextureStatus_Destroyed);
        }
    }
    ImGui_ImplSDL3_Shutdown();
    ImGui::DestroyContext();
    s_initialized = false;
}

void ProcessEvent(const SDL_Event& event)
{
    if (!s_initialized)
        return;
    ImGui_ImplSDL3_ProcessEvent(&event);

    if (event.type == SDL_EVENT_KEY_DOWN && !event.key.repeat)
    {
        // Ctrl+Grave + F5 are dev-only hotkeys (toggle dev panel +
        // role-slot flicker) gated by --dev.
        if (!AppConfig::Instance().DevMode())
            return;
        // Ctrl+` (US) / Ctrl+; (CZ) — toggle the dev panel.  Bound by physical
        // scancode (GRAVE = the key above Tab) so the same key works regardless
        // of keyboard layout.  Ctrl is required so the unmodified key stays
        // available to the game (it's used in radio/chat commands).
        const bool ctrlDown = (event.key.mod & SDL_KMOD_CTRL) != 0;
        if (event.key.scancode == SDL_SCANCODE_GRAVE && ctrlDown)
        {
            ToggleVisible();
            return;
        }
        // Ctrl+Shift+W — cycle the WTR-003 water debug view (works without
        // opening the dev panel).  Wraps 0→1→…→36→0.
        const bool shiftDown = (event.key.mod & SDL_KMOD_SHIFT) != 0;
        if (event.key.scancode == SDL_SCANCODE_W && ctrlDown && shiftDown && GEngine && GEngine->SupportsWater())
        {
            auto ws = GEngine->GetWaterSettings();
            ws.debugView = (ws.debugView + 1) % kWaterDebugViewCount;
            GEngine->SetWaterSettings(ws);
            LOG_INFO(Core, "Water debug view: [{}] {}", ws.debugView, kWaterDebugViews[ws.debugView]);
            return;
        }
    }
}

void NewFrame()
{
    if (!s_initialized)
        return;
    if (!AppConfig::Instance().DevMode() && s_visible)
        SetVisible(false);
    // Toggle the software cursor with panel visibility.  When the panel is
    // shown, the engine's UI cursor renders BEHIND ImGui (we composite ImGui
    // after the game render), so we draw our own cursor as part of ImGui's
    // drawlist to stay on top.  When hidden, fall back to the engine cursor.
    ImGui::GetIO().MouseDrawCursor = s_visible;
    if (s_backend == RenderBackend::OpenGL3)
    {
        ImGui_ImplOpenGL3_NewFrame();
    }
    ImGui_ImplSDL3_NewFrame();
    ImGui::NewFrame();
    if (s_visible)
        DrawMainWindow();
}

void Render()
{
    if (!s_initialized)
        return;
    ImGui::Render();
    if (s_backend == RenderBackend::OpenGL3)
    {
        // Make sure we draw to the default framebuffer in case the engine left
        // an FBO bound — happens with post-FX in GL33.  Other state (blend,
        // scissor, vao, depth) is saved/restored inside RenderDrawData.
        glBindFramebuffer(GL_FRAMEBUFFER, 0);
        ImGui_ImplOpenGL3_RenderDrawData(ImGui::GetDrawData());
    }
    else
    {
        RenderDrawDataEngine(ImGui::GetDrawData());
    }

    // Drain deferred actions queued by UI click handlers.  See the
    // s_pendingActions comment for the why — running cheats here
    // (after ImGui::Render returns) means engine code in the cheat
    // can freely realloc / clean up without trashing ImGui state.
    if (!s_pendingActions.empty())
    {
        auto local = std::move(s_pendingActions);
        s_pendingActions.clear();
        for (auto& fn : local)
            fn();
    }
}

bool IsVisible()
{
    return s_visible;
}
void SetVisible(bool v)
{
    if (v && !AppConfig::Instance().DevMode())
        v = false;
    s_visible = v;
    ApplyDevPanelMouseState();
}
void ToggleVisible()
{
    SetVisible(!s_visible);
}
void SelectShadowsTab()
{
    s_selectShadowsTab = true;
}
void SelectMemoryTab()
{
    s_selectMemoryTab = true;
}

void RequestDeferredReload(const char* modPath)
{
    // Route through the Application's between-frames re-mount request (serviced at the top
    // of AppIdle, before any simulate/draw). Running the reload inside Render()/BackToFront
    // instead — mid-frame, after Simulate — left the rebuilt world's first Simulate touching
    // a torn-down sensor list (null SensorList, SensorList::CheckPos crash).
    Poseidon::GApp->RequestRemountWithMods(modPath);
}

bool WantsKeyboard()
{
    if (!s_initialized || !s_visible)
        return false;
    return ImGui::GetIO().WantCaptureKeyboard;
}

bool WantsMouse()
{
    // Claim every mouse event while the panel is open to prevent the camera from moving
    return s_initialized && s_visible;
}

} // namespace DebugOverlay
} // namespace Poseidon::Dev
