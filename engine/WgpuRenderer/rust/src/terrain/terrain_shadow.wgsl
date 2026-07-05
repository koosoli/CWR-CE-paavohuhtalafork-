// Long-distance terrain sun-shadow sweep (phase 1: linear fixed-step march).
//
// A compute pass ray-marches the heightfield toward the sun once per mask texel
// and writes a soft occlusion factor into a world-aligned mask texture. The
// terrain fragment shader then pays one bilinear tap for terrain-on-terrain
// self-shadowing at any range, independent of the camera frustum and the CSM
// cascades. The mask depends only on (heightmap, sun direction), so the dispatch
// is amortized (recompute only when the sun moved past a small angular threshold
// or the heightmap changed).
//
// The mask grid is `scale`x finer than the heightmap: the occluder heightfield is
// coarse (~50 m texels) but the shadow *boundary* it casts can be located far more
// precisely than that, so a finer output grid keeps the boundary sharp instead of
// smearing it over a heightmap texel. `inv_scale` maps a mask texel back to
// (fractional) heightfield-texel space, where the march samples the heightfield.
//
// `sun_dir` is the surface-to-light direction (the negation of the frame's
// sun_dir_world travel direction), so sun above the horizon means sun_dir.y > 0.

struct ShadowSweep {
    world_origin: vec2<f32>,
    terrain_grid: f32,      // world metres per heightfield texel
    penumbra: f32,          // radians of soft band around the sun's elevation
    inv_scale: vec2<f32>,   // heightfield texels per mask texel (1/scale per axis)
    hm_width: u32,
    hm_height: u32,
    mask_width: u32,
    mask_height: u32,
    max_steps: u32,
    strength: f32,          // occlusion scale (0 = off, 1 = physical, >1 = exaggerated)
    sun_dir: vec4<f32>,     // xyz = surface-to-light (unit); w = max terrain height
};

@group(0) @binding(0) var<uniform> sp: ShadowSweep;
@group(0) @binding(1) var heightmap: texture_2d<f32>; // R32Float, textureLoad only
@group(0) @binding(2) var mask: texture_storage_2d<rgba8unorm, write>;

fn hm_load(ix: i32, iz: i32) -> f32 {
    let cx = clamp(ix, 0, i32(sp.hm_width) - 1);
    let cz = clamp(iz, 0, i32(sp.hm_height) - 1);
    return textureLoad(heightmap, vec2<i32>(cx, cz), 0).x;
}

// Manual 4-tap bilinear: the heightmap is filterable:false, so textureSample is
// unavailable here (same reason sample_height taps by hand in terrain.wgsl). `cell`
// is a (fractional) heightfield-texel coordinate.
fn sample_hm_bilinear(cell: vec2<f32>) -> f32 {
    let base = floor(cell);
    let f = cell - base;
    let ix = i32(base.x);
    let iz = i32(base.y);
    let h00 = hm_load(ix, iz);
    let h10 = hm_load(ix + 1, iz);
    let h01 = hm_load(ix, iz + 1);
    let h11 = hm_load(ix + 1, iz + 1);
    return mix(mix(h00, h10, f.x), mix(h01, h11, f.x), f.y);
}

@compute @workgroup_size(8, 8)
fn sweep(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= sp.mask_width || gid.y >= sp.mask_height) {
        return;
    }
    let store_coord = vec2<i32>(i32(gid.x), i32(gid.y));
    // Heightfield-texel coordinate this mask texel sits at (fractional when the
    // mask is finer than the heightmap).
    let hcell0 = vec2<f32>(f32(gid.x), f32(gid.y)) * sp.inv_scale;

    let horiz = length(sp.sun_dir.xz);
    // Sun on/below the horizon (or a degenerate straight-down sun): everything
    // lit. Lighting already falls back to ambient there, and a near-horizontal
    // ray would march forever.
    if (sp.sun_dir.y <= 1e-3 || horiz <= 1e-4) {
        textureStore(mask, store_coord, vec4<f32>(1.0));
        return;
    }
    let dir = sp.sun_dir.xz / horiz;   // unit xz march direction (toward the sun)
    let slope = sp.sun_dir.y / horiz;  // ray metres up per metre of horizontal travel
    let sun_elev = atan(slope);

    // Small upward bias so a texel does not self-occlude on its own quantised height.
    let h0 = sample_hm_bilinear(hcell0) + 0.1;
    let max_h = sp.sun_dir.w;
    // Step one heightfield texel per iteration (the heightfield's Nyquist spacing),
    // regardless of the mask scale, so a thin ridge is not stepped over. The march
    // self-limits: once the ray has climbed above the tallest terrain, nothing
    // ahead can occlude, so it stops. High sun (steep ray) exits in a few steps;
    // a low dusk sun (shallow ray) marches far — exactly where km-long shadows need
    // it — with max_steps only as a hard safety cap.
    var max_ang = -1e9;
    for (var s = 1u; s <= sp.max_steps; s = s + 1u) {
        let d = f32(s) * sp.terrain_grid;         // horizontal distance
        if (h0 + d * slope > max_h) {
            break;
        }
        let cell = hcell0 + dir * f32(s);         // heightfield-texel space
        let ht = sample_hm_bilinear(cell);
        max_ang = max(max_ang, atan((ht - h0) / d));
    }

    // 1 = lit (max occluder well below the sun), 0 = shadowed (occluder above it).
    // Soft edge over the sun's angular radius (widened for artistic penumbra),
    // which also hides the heightmap-resolution stepping at km scale. `strength`
    // scales the occlusion so the fragment tap needs no separate knob (0 = the mask
    // is fully lit -> no effect; >1 exaggerates for debugging).
    let lit = 1.0 - smoothstep(sun_elev - sp.penumbra, sun_elev + sp.penumbra, max_ang);
    let occ = sp.strength * (1.0 - lit);
    textureStore(mask, store_coord, vec4<f32>(clamp(1.0 - occ, 0.0, 1.0)));
}
