// GPU water: a flat CDLOD surface at the global sea level. The shared grid mesh is
// instanced per node (mirroring the terrain path) and placed on a horizontal plane
// at `sea_level`, camera-relative, reversed-Z. Shares group(0) (the camera UBO +
// aerial fog) with the lit 3D + terrain pipelines via the frame module, so distant
// water dissolves into the same procedural sky/horizon. This geometry pass keeps the
// surface flat and shades it a single deep-water tint; the look (waves, depth colour,
// refraction, reflection) is the sibling water-rendering plan.

#import frame::{frame, reverse_z, fog_factor, apply_fog}
#import color::srgb_to_linear

struct WaterParams {
    world_origin: vec2<f32>,
    terrain_grid: f32,
    sea_level: f32,
    hm_width: u32,
    hm_height: u32,
};

// Must match GRID_N in water/mod.rs (and the terrain grid).
const GRID_N: f32 = 32.0;

@group(1) @binding(0) var<uniform> wp: WaterParams;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_pos: vec3<f32>, // camera-relative
    @location(1) fog: f32,             // 1 = keep colour, 0 = full fog
};

// Skirt drop, as a multiple of the patch's vertex spacing (WGR_WATER_SKIRT_K). The
// plane is flat so the skirt only walls off LOD-transition cracks; it never shows.
override skirt_k: f32 = 4.0;

@vertex
fn vs_water(
    @location(0) grid_in: vec3<f32>, // xy = unit grid position in [0,1]^2, z = skirt flag
    @location(1) origin: vec2<f32>,  // node world-xz origin
    @location(2) size: f32,          // node world size
    @location(3) lod: u32,
    @location(4) morph: vec2<f32>,   // (morph_start, morph_end) camera-distance band
) -> VsOut {
    let grid = grid_in.xy;
    let world_xz_fine = origin + grid * size;
    let dist = length(vec3<f32>(world_xz_fine.x, wp.sea_level, world_xz_fine.y) - frame.cam_pos.xyz);

    // Snap toward the coarser even lattice as the vertex nears morph_end, matching
    // the terrain morph so LOD seams stay crack-free (a no-op in y for a flat plane,
    // but load-bearing once the sibling plan displaces the surface with waves).
    var morph_k = 0.0;
    if (morph.y > morph.x)
    {
        morph_k = clamp((dist - morph.x) / (morph.y - morph.x), 0.0, 1.0);
    }
    let gidx = grid * GRID_N;
    let grid_coarse = (round(gidx * 0.5) * 2.0) / GRID_N;
    let world_xz = origin + mix(grid, grid_coarse, morph_k) * size;

    let y = wp.sea_level - grid_in.z * (size / GRID_N) * skirt_k;
    let world_rel = vec3<f32>(world_xz.x, y, world_xz.y) - frame.cam_pos.xyz;

    var out: VsOut;
    out.clip = reverse_z(frame.proj * frame.view * vec4<f32>(world_rel, 1.0));
    out.world_pos = world_rel;
    out.fog = fog_factor(length(world_rel));
    return out;
}

// HDR path (docs/hdr-pipeline-plan.md): 1 = decode the gamma-space tint + fog colour
// to linear radiance; 0 = gamma-naive (LDR-direct).
override linear: f32 = 0.0;

@fragment
fn fs_water(in: VsOut) -> @location(0) vec4<f32> {
    // Flat deep-water tint (a gamma-space stand-in until the look plan lands the
    // depth-based colour, refraction and reflection).
    var rgb = vec3<f32>(0.02, 0.09, 0.13);
    if (linear > 0.5) {
        rgb = srgb_to_linear(rgb);
    }

    // fog_enabled: 2 = aerial perspective (froxel volume); 1 = flat distance fog; 0 = off.
    if (frame.params.fog_enabled >= 1.5) {
        rgb = apply_fog(rgb, in.world_pos);
    } else {
        var fog_color = frame.fog_color.rgb;
        if (linear > 0.5) {
            fog_color = srgb_to_linear(fog_color);
        }
        rgb = mix(fog_color, rgb, in.fog);
    }

    // Slightly translucent: with depth-write off + alpha blend, the seabed shows
    // through faintly. Full opacity/clarity is the sibling plan's job.
    return vec4<f32>(rgb, 0.85);
}
