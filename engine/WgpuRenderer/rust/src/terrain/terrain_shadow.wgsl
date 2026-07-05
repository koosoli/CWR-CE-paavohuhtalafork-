// Long-distance terrain sun-shadow sweep (phase 1: linear fixed-step march).
//
// A compute pass ray-marches the heightfield toward the sun once per mask texel
// and writes a world-aligned "shadow ceiling" the terrain (and, later, objects)
// sample with one bilinear tap. Instead of a fixed-height occlusion factor, each
// texel stores the *height* below which that column is in terrain shadow:
//
//     ceiling(x,z) = max over the march of ( terrain_height - distance * tan(sun_elev) )
//
// so any point at (x,z,Y) — terrain surface, a soldier's head, a helicopter — is
// shadowed iff Y < ceiling(x,z). This makes the shadow correct at any altitude,
// not just glued to the ground. The mask depends only on (heightmap, sun), so the
// dispatch is amortized (recompute only when the sun moved or the heightmap did).
//
// The mask grid is `scale`x finer than the heightmap: the occluder heightfield is
// coarse (~50 m texels) but the shadow boundary it casts is sharp, so a finer
// output grid keeps the boundary crisp. `inv_scale` maps a mask texel back to
// (fractional) heightfield-texel space, where the march samples the heightfield.
//
// `sun_dir.xyz` is the surface-to-light direction (the negation of the frame's
// sun_dir_world travel direction), so sun above the horizon means sun_dir.y > 0.
// `sun_dir.w` carries the tallest terrain height (the march's self-limit).

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
    strength: f32,          // occlusion scale baked into .b (0 = off, 1 = physical)
    sun_dir: vec4<f32>,     // xyz = surface-to-light (unit); w = max terrain height
};

@group(0) @binding(0) var<uniform> sp: ShadowSweep;
@group(0) @binding(1) var heightmap: texture_2d<f32>; // R32Float, textureLoad only
@group(0) @binding(2) var mask: texture_storage_2d<rgba16float, write>;

// Ceiling well below any terrain: a column with no occluder toward the sun (its
// points are all lit). Finite so the filterable mask never interpolates an inf.
const NO_CEILING: f32 = -1.0e4;

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
    // Sun on/below the horizon (or a degenerate straight-down sun): no directional
    // shadow (lighting is in the ambient-only regime), and a near-horizontal ray
    // would march forever. Ceiling below everything -> everything lit.
    if (sp.sun_dir.y <= 1e-3 || horiz <= 1e-4) {
        textureStore(mask, store_coord, vec4<f32>(NO_CEILING, 0.0, sp.strength, 0.0));
        return;
    }
    let dir = sp.sun_dir.xz / horiz;   // unit xz march direction (toward the sun)
    let slope = sp.sun_dir.y / horiz;  // ray metres up per metre of horizontal travel
    let sun_elev = atan(slope);
    let max_h = sp.sun_dir.w;

    // March one heightfield texel per iteration (the heightfield's Nyquist spacing),
    // regardless of the mask scale, so a thin ridge is not stepped over. Track the
    // occluder that sets the ceiling (its height + distance) to size the penumbra.
    // Self-limiting: the best ceiling any occluder at distance >= d could give is
    // max_h - d*slope; once that can't beat the current ceiling, stop. High sun
    // exits in a few steps; a low dusk sun marches far — where km-long shadows need it.
    var ceiling = NO_CEILING;
    var occ_h = 0.0;
    var occ_d = 0.0;
    for (var s = 1u; s <= sp.max_steps; s = s + 1u) {
        let d = f32(s) * sp.terrain_grid;         // horizontal distance
        if (max_h - d * slope <= ceiling) {
            break;
        }
        let cell = hcell0 + dir * f32(s);         // heightfield-texel space
        let ht = sample_hm_bilinear(cell);
        let c = ht - d * slope;                   // shadow ceiling this occluder casts here
        if (c > ceiling) {
            ceiling = c;
            occ_h = ht;
            occ_d = d;
        }
    }

    // Penumbra as a height band from the ceiling-setting occluder: the sun's finite
    // angular radius maps to a height spread proportional to the occluder distance,
    // so distant ridges cast softer edges for free. hi = fully lit above, lo = fully
    // dark below; half-width feeds the fragment's smoothstep(ceiling +- hb).
    let a_lit = max(sun_elev - sp.penumbra, 0.0);
    let a_dark = min(sun_elev + sp.penumbra, 1.5533); // clamp < 89deg so tan stays finite
    let hi = occ_h - occ_d * tan(a_lit);
    let lo = occ_h - occ_d * tan(a_dark);
    let halfband = max((hi - lo) * 0.5, 0.0);

    textureStore(mask, store_coord, vec4<f32>(ceiling, halfband, sp.strength, 0.0));
}
