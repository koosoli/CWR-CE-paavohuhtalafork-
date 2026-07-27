#pragma once

#include <Poseidon/Graphics/Core/Engine.hpp>
#include <string>
#include <vector>
#include <cstdint>

namespace Poseidon
{

enum class WtrTestAvailability
{
    Available,
    Partial,
    Blocked
};

struct WtrTestPresetInfo
{
    int id;
    const char* name;
    const char* description;
    WtrTestAvailability availability;
    const char* statusReason;
};

class WtrTestHarness
{
public:
    static WtrTestHarness& Instance();

    WtrTestHarness();
    ~WtrTestHarness() = default;

    // Preset information query
    const std::vector<WtrTestPresetInfo>& GetPresets() const { return _presets; }
    const WtrTestPresetInfo* GetPresetInfo(int presetId) const;

    // State query
    bool IsActive() const { return _active; }
    bool IsPaused() const { return _paused; }
    int GetCurrentPresetId() const { return _currentPresetId; }
    uint64_t GetFrameIndex() const { return _frameIndex; }
    float GetFixedDeltaTime() const { return _fixedDeltaTime; }
    uint32_t GetTriggeredEventCount() const { return _triggeredEventCount; }

    // Harness control functions (snapshot & restoration of prior settings)
    void SelectPreset(int presetId, Engine::WaterSettings& settings, int debugView);
    void Start(Engine::WaterSettings& settings, int debugView);
    void Pause();
    void StepFrame(Engine::WaterSettings& settings);
    void Restart(Engine::WaterSettings& settings);
    void Stop(Engine::WaterSettings& settings, int& outDebugView);

    // Frame update — drives camera path & edge-triggered event injection outside DrawWater.
    // Returns true when a frame advanced and camPos/camRot were written (false when inactive
    // or paused without a pending single-step, so the caller must leave the camera alone).
    bool Update(float frameDt, Engine::WaterSettings& settings, Vector3& camPos, Vector3& camRot);

    // Reproducible metadata generation across identical runs
    std::string GenerateMetadataLog(const Engine::WaterSettings& settings, const Vector3& camPos, const Vector3& camRot) const;

private:
    void ApplyPresetSettings(int presetId, Engine::WaterSettings& settings);
    void ComputeCameraTransform(float time, Vector3& camPos, Vector3& camRot);
    void InjectEdgeTriggeredEvents(Engine::WaterSettings& settings);

    std::vector<WtrTestPresetInfo> _presets;
    bool _active = false;
    bool _paused = false;
    bool _singleStep = false;
    int _currentPresetId = 0;
    uint64_t _frameIndex = 0;
    float _testTime = 0.0f;
    float _fixedDeltaTime = 1.0f / 60.0f;
    uint32_t _triggeredEventCount = 0;

    // Snapshot of user settings prior to test harness start
    Engine::WaterSettings _savedSettings{};
    int _savedDebugView = 0;
    bool _hasSavedState = false;
};

} // namespace Poseidon
