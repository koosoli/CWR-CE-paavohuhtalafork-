#include <Poseidon/Graphics/Shared/SdlWindow.hpp>

#include <Poseidon/Foundation/Framework/Log.hpp>
#include <Poseidon/Graphics/Shared/WindowPlacement.hpp>

namespace Poseidon
{

SdlGameWindow CreateGameWindow(const SdlGameWindowDesc& desc)
{
    SdlGameWindow result;

    if (!SDL_Init(SDL_INIT_VIDEO))
    {
        LOG_ERROR(Graphics, "CreateGameWindow: SDL_Init(VIDEO) failed: {}", SDL_GetError());
        return result;
    }

    if (desc.preCreate)
    {
        desc.preCreate();
    }

    int desktopW = 0, desktopH = 0, desktopRefresh = 0;
    if (const SDL_DisplayMode* dm = SDL_GetDesktopDisplayMode(SDL_GetPrimaryDisplay()))
    {
        desktopW = dm->w;
        desktopH = dm->h;
        desktopRefresh = static_cast<int>(dm->refresh_rate + 0.5f);
    }

    // Resolve final placement (mode + size + position + refresh) from the
    // display config and the target display's desktop mode.  `useWindow`
    // (the --window override) wins over the saved displayMode either way.
    DisplayPlacementInput displayCfg;
    displayCfg.displayMode = desc.displayMode.empty() ? (desc.useWindow ? "windowed" : "borderless") : desc.displayMode;
    if (desc.useWindow && displayCfg.displayMode != "windowed")
    {
        displayCfg.displayMode = "windowed";
    }
    if (!desc.useWindow && displayCfg.displayMode == "windowed")
    {
        displayCfg.displayMode = "borderless";
    }
    displayCfg.width = desc.width;
    displayCfg.height = desc.height;

    const WindowPlacement placement = ResolveWindowPlacement(displayCfg, desktopW, desktopH, desktopRefresh);

    // SDL_WINDOW_HIGH_PIXEL_DENSITY: opt into native-pixel rendering on HighDPI
    // displays — without it SDL renders at logical pixels and blits up (blurry).
    // The SDL_GetWindowSizeInPixels readback below returns the true pixel size.
    Uint32 flags = SDL_WINDOW_HIGH_PIXEL_DENSITY | desc.extraFlags;
    switch (placement.mode)
    {
        case WindowMode::Fullscreen:
        case WindowMode::Borderless:
            flags |= SDL_WINDOW_BORDERLESS;
            break;
        case WindowMode::Windowed:
            flags |= SDL_WINDOW_RESIZABLE;
            break;
    }

    SDL_Window* window = SDL_CreateWindow(desc.title, placement.width, placement.height, flags);
    if (!window)
    {
        LOG_ERROR(Graphics, "CreateGameWindow: SDL_CreateWindow failed: {}", SDL_GetError());
        return result;
    }

    // On Windows, Borderless avoids SDL's fullscreen state machine because
    // Win11 + OpenGL can promote that path to exclusive on the first SwapWindow
    // (libsdl-org/SDL#12791); position it instead.  On Linux/macOS we want the
    // compositor's real desktop-fullscreen state so shell work-area
    // reservations don't treat the game as a regular borderless window.
    if (placement.mode == WindowMode::Borderless)
    {
#ifndef _WIN32
        SDL_SetWindowFullscreenMode(window, nullptr);
        if (!SDL_SetWindowFullscreen(window, true))
        {
            LOG_WARN(Graphics, "CreateGameWindow: SDL_SetWindowFullscreen(true) failed for borderless startup: {}",
                     SDL_GetError());
        }
#else
        if (placement.posX != WindowPlacement::kCentered)
        {
            SDL_SetWindowPosition(window, placement.posX, placement.posY);
        }
#endif
    }
    else if (placement.posX != WindowPlacement::kCentered)
    {
        SDL_SetWindowPosition(window, placement.posX, placement.posY);
    }

    int wPx = placement.width;
    int hPx = placement.height;
    SDL_GetWindowSizeInPixels(window, &wPx, &hPx);

    result.window = window;
    result.mode = placement.mode;
    result.widthPx = wPx;
    result.heightPx = hPx;
    result.refreshHz = placement.refreshHz;
    result.windowed = (placement.mode == WindowMode::Windowed);
    return result;
}

} // namespace Poseidon
