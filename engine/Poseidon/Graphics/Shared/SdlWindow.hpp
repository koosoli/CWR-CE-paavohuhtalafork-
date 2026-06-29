#pragma once

#include <Poseidon/Graphics/Shared/WindowMode.hpp>

#include <SDL3/SDL.h>

#include <functional>
#include <string>

namespace Poseidon
{

struct SdlGameWindow
{
    SDL_Window* window = nullptr;
    WindowMode mode = WindowMode::Windowed;
    int widthPx = 0;
    int heightPx = 0;
    // resolved refresh, 0 if the driver didn't report one
    int refreshHz = 0;
    bool windowed = false;
};

struct SdlGameWindowDesc
{
    const char* title = "Poseidon";
    int width = 640;
    int height = 480;
    bool useWindow = false;
    // "" / "windowed" / "borderless" / "exclusive"
    std::string displayMode; 
    Uint32 extraFlags = 0;
    // Run after SDL_Init(VIDEO) but before SDL_CreateWindow.
    std::function<void()> preCreate;
};

SdlGameWindow CreateGameWindow(const SdlGameWindowDesc& desc);

} // namespace Poseidon
