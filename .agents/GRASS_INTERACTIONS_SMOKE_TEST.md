# Grass Interaction Smoke Test

Launch the game with the WGPU renderer and a development log:

```powershell
cmd /c ".\ColdWarAssault.exe --render wgpu --window --dev --log-file cwr.log"
```

## Setup

1. Load a mission with a grassy open field, a helicopter, and hand grenades.
2. Use a third-person or free camera view that keeps both the grass and the helicopter's ground footprint visible.
3. Open the developer overlay with `Ctrl + \`` and select the **Grass** tab. Ensure procedural grass is enabled and use a near radius of at least 50 m.

## Rotor wash

1. Hover an airborne helicopter 5–30 m above grass.
2. Watch the area beneath the rotor disc.

Expected result: grass is pressed to roughly half height in a steady circular footprint, while retaining reduced wind movement. The affected circle grows as the helicopter rises. It springs upright once the helicopter leaves or climbs above roughly 65 m.

## Explosive impacts

1. Throw a hand grenade into grass at least a few metres from the camera.
2. Repeat with a rocket or another explosive projectile if available.

Expected result: at detonation, grass in a circular patch bends outward from the impact. The patch is larger for stronger explosions and recovers gradually over about one minute. Bullet impacts must not create these patches.

## Pass / fail

Pass when both effects appear only on grass-capable terrain, do not affect terrain geometry or gameplay physics, and do not introduce renderer errors. Confirm `cwr.log` includes:

```text
[INFO] [GRAPHICS] Wgpu: creating renderer WGPU (Rust / wgpu)
```
