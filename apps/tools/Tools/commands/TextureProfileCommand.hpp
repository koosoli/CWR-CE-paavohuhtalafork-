#pragma once

#include <CLI/CLI.hpp>

namespace PoseidonTools
{

/// Non-destructive Arma texture profile authoring. Profiles are packaged as
/// normal --mod folders, so the original game PBOs are never edited in place.
class TextureProfileCommand
{
  public:
    static void Setup(CLI::App& app);
};

} // namespace PoseidonTools
