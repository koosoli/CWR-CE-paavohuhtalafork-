# Texture Workbench

`PoseidonTools texture` makes isolated Arma Cold War Assault texture profiles.
It never edits the original game PBOs. Each profile is packaged as its own
`@mod`, so choosing a different launcher immediately restores the unmodified
game assets.

The tool is installed beside the game as:

```text
Tools\TextureWorkbench\PoseidonTools.exe
```

## Quick start

Open PowerShell in the TextureWorkbench folder and create a profile:

```powershell
.\PoseidonTools.exe texture --game-dir "D:\SteamLibrary\steamapps\common\ARMA Cold War Assault" profile create summer-grass
```

Add a replacement image using the exact original virtual PAA path. The command
accepts common source images and converts them to a profile-local DXT5 PAA:

```powershell
.\PoseidonTools.exe texture --game-dir "D:\SteamLibrary\steamapps\common\ARMA Cold War Assault" profile add summer-grass "D:\Art\grass.png" "landtext\trava1.paa"
```

Build the profile when ready. It creates a PBO and a `Launch-summer-grass.cmd`
file under `TextureProfiles`; double-click that launcher to play with only that
profile enabled.

```powershell
.\PoseidonTools.exe texture --game-dir "D:\SteamLibrary\steamapps\common\ARMA Cold War Assault" profile build summer-grass
```

## Normal maps

You can create a normal map from an albedo or height image, then add it under a
normal-map target path:

```powershell
.\PoseidonTools.exe texture --game-dir "D:\SteamLibrary\steamapps\common\ARMA Cold War Assault" profile add-normal summer-grass "D:\Art\grass-height.png" "landtext\trava1_nohq.paa" --strength 2.5
```

This generates a standard tangent-space normal map. It affects the game only
when the texture/material you replace is actually configured to consume that
normal-map path; creating a `_nohq.paa` alone does not add normal-map support to
an unrelated material.

## Finding the target path

The target needs to match the PAA path requested by the game exactly (for
example `landtext\trava1.paa`). Use PoseidonTools' PBO listing/extraction
commands to inspect the original PBO first, and keep the same relative path in
your profile. The profile overrides only files it contains; all other textures
continue to come from the original game.

## Safety model

- Profile sources: `TextureProfiles\@<name>\source`
- Packed profile: `TextureProfiles\@<name>\addons\zz_texture_profile_<name>.pbo`
- One-click launcher: `TextureProfiles\Launch-<name>.cmd`
- Original game PBOs are read-only from the tool's point of view.

`texture profile list` shows every profile and whether it has been built.
