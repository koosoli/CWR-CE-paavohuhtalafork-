// GPU terrain: a shared grid mesh instanced per node, heightmap-displaced in the
// vertex shader. Shares group 0 (the camera UBO + cascade shadow map) with the
// lit 3D pipeline, so terrain receives the same CSM shadows. The fragment shader
// blends the four surrounding land cells' detail-array layers (indexed by a
// per-cell index map) and modulates by a tiled high-frequency noise texture.

struct FrameParams {
    fog_start: f32,
    fog_inv_range: f32,
    fog_enabled: f32,
    shadow_strength: f32,
};

struct ShadowBlock {
    cascade_vp: array<mat4x4<f32>, 4>,
    splits: vec4<f32>,
    omni_radius: vec4<f32>,
    ctl: vec4<f32>,  // {count, omni_count, fade_range, bias_const}
    ctl2: vec4<f32>, // {texel_size, darkness, normal_offset, pcf}
    cam_fwd: vec4<f32>,
    sun_dir: vec4<f32>,
};

struct Frame {
    proj: mat4x4<f32>,
    view: mat4x4<f32>,
    fog_color: vec4<f32>,
    params: FrameParams,
    shadow: ShadowBlock,
    cam_pos: vec4<f32>,
    sun_diffuse: vec4<f32>,
    sun_ambient: vec4<f32>,
    sun_dir_world: vec4<f32>,
};

struct TerrainParams {
    world_origin: vec2<f32>,
    land_grid: f32,
    terrain_grid: f32,
    hm_width: u32,
    hm_height: u32,
    land_range: u32,
    data_scale: f32,
};

// Must match GRID_N in terrain/mod.rs.
const GRID_N: f32 = 32.0;

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var shadow_map: texture_depth_2d_array;
@group(0) @binding(2) var shadow_samp: sampler_comparison;
@group(1) @binding(0) var<uniform> tp: TerrainParams;
@group(1) @binding(1) var heightmap: texture_2d<f32>;
// Bindless ground textures: one texture_2d per Landscape texture index, native
// size/format/mips. Indexed non-uniformly per fragment (needs the device's
// SAMPLED_TEXTURE_..._NON_UNIFORM_INDEXING feature).
@group(2) @binding(0) var ground: binding_array<texture_2d<f32>>;
@group(2) @binding(1) var ground_samp: sampler;
@group(2) @binding(2) var index_map: texture_2d<u32>;
@group(2) @binding(3) var detail: texture_2d<f32>;
@group(2) @binding(4) var ground_clamp_samp: sampler;
@group(2) @binding(5) var jitter_map: texture_2d<f32>;

fn hm_load(ix: i32, iz: i32) -> f32 {
    let cx = clamp(ix, 0, i32(tp.hm_width) - 1);
    let cz = clamp(iz, 0, i32(tp.hm_height) - 1);
    return textureLoad(heightmap, vec2<i32>(cx, cz), 0).x;
}

// World height at a world-xz, matching Landscape::SurfaceY's per-cell triangle
// interpolation (not bilinear) so surface decals stay coplanar with the mesh.
fn sample_height(world_xz: vec2<f32>) -> f32 {
    let t = (world_xz - tp.world_origin) / tp.terrain_grid;
    let base = floor(t);
    let ix = i32(base.x);
    let iz = i32(base.y);
    let f = t - base; // f.x = x within cell, f.y = z within cell
    let y00 = hm_load(ix, iz);
    let y01 = hm_load(ix + 1, iz);
    let y10 = hm_load(ix, iz + 1);
    let y11 = hm_load(ix + 1, iz + 1);
    if (f.x <= 1.0 - f.y)
    {
        return y00 + (y10 - y00) * f.y + (y01 - y00) * f.x;
    }
    return y10 + (y01 - y11) - (y10 - y11) * f.x - (y01 - y11) * f.y;
}

// Central-difference normal at a fixed heightmap step. Taken per-fragment (not
// per-vertex) so it depends only on world position, never on the patch's LOD or
// morph state — that independence is what keeps even terrain evenly lit. A
// mesh/morph-derived normal ramps with camera distance within each patch and
// banded the lighting into radial stripes under grazing sun.
fn sample_normal(world_xz: vec2<f32>, step: f32) -> vec3<f32> {
    let hx0 = sample_height(world_xz - vec2<f32>(step, 0.0));
    let hx1 = sample_height(world_xz + vec2<f32>(step, 0.0));
    let hz0 = sample_height(world_xz - vec2<f32>(0.0, step));
    let hz1 = sample_height(world_xz + vec2<f32>(0.0, step));
    return normalize(vec3<f32>(-(hx1 - hx0), 2.0 * step, -(hz1 - hz0)));
}

// Index-map entry for a land cell, clamped to the map (row = z, column = x;
// same orientation as the heightmap and Landscape::GetTexture(z, x)). Bits
// 0-14 = ground-array layer; bit 15 = clamped transition tile (GL33's
// ClampU|ClampV: the texture maps exactly once onto its cell, edges extended,
// instead of tiling).
const CELL_LAYER_MASK: u32 = 0x7fffu;
const CELL_CLAMPED: u32 = 0x8000u;

fn cell_entry(cell: vec2<i32>) -> u32 {
    let cx = clamp(cell.x, 0, i32(tp.land_range) - 1);
    let cz = clamp(cell.y, 0, i32(tp.land_range) - 1);
    return textureLoad(index_map, vec2<i32>(cx, cz), 0).x;
}

// One land cell's contribution to the ground blend. Simple (tileable) layers
// sample at the global one-period-per-cell UV, so where neighbouring cells
// share a layer the blend stays an exact no-op. Clamped transition tiles
// sample in their own cell's [0,1] frame through the edge-extending sampler:
// a neighbour's contribution near the shared border is then that tile's
// matching edge row, not its wrapped-around opposite edge (which both smeared
// the wrong ground type across the border and put the tile's own wrap seam on
// the cell edge). Explicit gradients keep mip selection continuous across the
// frame switch and keep the call legal in non-uniform control flow.
fn sample_cell(entry: u32, cell: vec2<f32>, tile_uv: vec2<f32>,
               ddx: vec2<f32>, ddy: vec2<f32>) -> vec3<f32> {
    let layer = entry & CELL_LAYER_MASK;
    if ((entry & CELL_CLAMPED) != 0u)
    {
        return textureSampleGrad(ground[layer], ground_clamp_samp, tile_uv - cell, ddx, ddy).rgb;
    }
    return textureSampleGrad(ground[layer], ground_samp, tile_uv, ddx, ddy).rgb;
}

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_xz: vec2<f32>,  // absolute world-xz
    @location(1) fog: f32,             // 1 = keep colour, 0 = full fog
    @location(2) world_pos: vec3<f32>, // camera-relative
};

// Skirt drop, as a multiple of the patch's vertex spacing. Tunable via
// WGR_TERRAIN_SKIRT_K (0 = skirts flush with the surface, i.e. effectively off).
override skirt_k: f32 = 4.0;

@vertex
fn vs_terrain(
    @location(0) grid_in: vec3<f32>, // xy = unit grid position in [0,1]^2, z = skirt flag
    @location(1) origin: vec2<f32>,  // node world-xz origin
    @location(2) size: f32,          // node world size
    @location(3) lod: u32,
    @location(4) morph: vec2<f32>,   // (morph_start, morph_end) camera-distance band
) -> VsOut {
    let grid = grid_in.xy;
    let world_xz_fine = origin + grid * size;
    let height_fine = sample_height(world_xz_fine);
    let dist = length(vec3<f32>(world_xz_fine.x, height_fine, world_xz_fine.y) - frame.cam_pos.xyz);

    // Snap toward the coarser even lattice as the vertex nears morph_end, so the
    // edge meets the parent grid at the LOD switch without a crack.
    var morph_k = 0.0;
    if (morph.y > morph.x)
    {
        morph_k = clamp((dist - morph.x) / (morph.y - morph.x), 0.0, 1.0);
    }
    let gidx = grid * GRID_N;
    let grid_coarse = (round(gidx * 0.5) * 2.0) / GRID_N;
    let world_xz = origin + mix(grid, grid_coarse, morph_k) * size;

    let height = sample_height(world_xz) - grid_in.z * (size / GRID_N) * skirt_k;
    let world_rel = vec3<f32>(world_xz.x, height, world_xz.y) - frame.cam_pos.xyz;

    var out: VsOut;
    out.clip = frame.proj * frame.view * vec4<f32>(world_rel, 1.0);
    // Reversed-Z: forward projection (near->0, far->1) remapped to near->1, far->0.
    out.clip.z = out.clip.w - out.clip.z;
    // Texture + normal use the same morphed world position the geometry is drawn
    // at, so the UV stays locked to the mesh and morphs smoothly with it. (Using
    // the un-morphed position instead decouples the UV from the screen-space
    // interpolation and compresses the tiling wherever the morph collapses
    // vertices -> broken tiling at LOD > 0.)
    out.world_xz = world_xz;
    out.world_pos = world_rel;

    let fog_dist = length(world_rel);
    let fog_factor = clamp(1.0 - (fog_dist - frame.params.fog_start) * frame.params.fog_inv_range, 0.0, 1.0);
    out.fog = select(1.0, fog_factor, frame.params.fog_enabled > 0.5);
    return out;
}

// Cascaded shadow strength in [0,1] for a camera-relative position (0 = lit).
// Verbatim port of the lit mesh kernel (gfx3d/shader3d.wgsl fs_main) so terrain
// self-shadows consistently with objects.
fn shadow_strength(world_pos: vec3<f32>, normal_ws: vec3<f32>, fog: f32,
                   dwx: vec3<f32>, dwy: vec3<f32>) -> f32 {
    let n_cascades = i32(frame.shadow.ctl.x);
    if (n_cascades <= 0) {
        return 0.0;
    }
    let omni_n = i32(frame.shadow.ctl.y);
    let eye_depth = dot(world_pos, frame.shadow.cam_fwd.xyz);
    let dist3d = length(world_pos);

    var ci = n_cascades;
    for (var i = 0; i < 4; i++) {
        if (i >= n_cascades) {
            break;
        }
        let metric = select(eye_depth, dist3d, i < omni_n);
        if (metric <= frame.shadow.splits[i]) {
            ci = i;
            break;
        }
    }
    if (ci >= n_cascades) {
        return 0.0;
    }

    let cos_t = dot(normal_ws, -frame.shadow.sun_dir.xyz);
    let sin_t = sqrt(max(0.0, 1.0 - cos_t * cos_t));

    var prev_edge = 0.0;
    if (ci > 0) {
        prev_edge = frame.shadow.splits[ci - 1];
    }
    let ci_metric = select(eye_depth, dist3d, ci < omni_n);
    let band = (frame.shadow.splits[ci] - prev_edge) * 0.15;
    var bw = 0.0;
    if (ci + 1 < n_cascades) {
        bw = clamp((ci_metric - (frame.shadow.splits[ci] - band)) / max(band, 0.001), 0.0, 1.0);
    }

    let ts = frame.shadow.ctl2.x;
    var lit_sum = 0.0;
    var w_sum = 0.0;
    for (var p = 0; p < 4; p++) {
        let c = ci + p;
        if (c >= n_cascades) {
            break;
        }
        var w: f32;
        if (p == 0) {
            w = 1.0 - bw;
        } else if (w_sum <= 0.0) {
            w = 1.0;
        } else if (p == 1) {
            w = bw;
        } else {
            w = 0.0;
        }
        if (w <= 0.0) {
            continue;
        }

        let vp = frame.shadow.cascade_vp[c];
        let sx = max(length(vec3<f32>(vp[0][0], vp[1][0], vp[2][0])), 1e-6);
        let texel_world = 2.0 * ts / sx;
        let offset = frame.shadow.ctl2.z * 2.0 * texel_world * sin_t;

        let cp = vp * vec4<f32>(world_pos + normal_ws * offset, 1.0);
        let sc = cp.xyz / cp.w;
        let suv = vec2<f32>(sc.x * 0.5 + 0.5, 0.5 - sc.y * 0.5);
        if (suv.x > 0.0 && suv.x < 1.0 && suv.y > 0.0 && suv.y < 1.0 && sc.z > 0.0 && sc.z < 1.0) {
            let dsx = vp * vec4<f32>(dwx, 0.0);
            let dsy = vp * vec4<f32>(dwy, 0.0);
            let duv_dx = vec2<f32>(0.5 * dsx.x, -0.5 * dsx.y);
            let duv_dy = vec2<f32>(0.5 * dsy.x, -0.5 * dsy.y);
            let det = duv_dx.x * duv_dy.y - duv_dx.y * duv_dy.x;
            var dz_duv = vec2<f32>(0.0, 0.0);
            if (abs(det) > 1e-12) {
                dz_duv = vec2<f32>(dsx.z * duv_dy.y - dsy.z * duv_dx.y,
                                   dsy.z * duv_dx.x - dsx.z * duv_dy.x) / det;
            }
            let lim = 0.02 / max(ts, 1e-6);
            dz_duv = clamp(dz_duv, vec2<f32>(-lim, -lim), vec2<f32>(lim, lim));
            let plane_bias = min(2.0 * ts * (abs(dz_duv.x) + abs(dz_duv.y)), 0.01);
            let bias = frame.shadow.ctl.w * f32(c + 1) * f32(c + 1);
            let ref_z = sc.z - bias - plane_bias;
            var lit: f32;
            let pcf = frame.shadow.ctl2.w;
            if (pcf >= 0.5) {
                let o = ts * pcf;
                var sum = 0.0;
                for (var dy = -1; dy <= 1; dy++) {
                    for (var dx = -1; dx <= 1; dx++) {
                        let off = vec2<f32>(f32(dx), f32(dy)) * o;
                        let wt = (2.0 - abs(f32(dx))) * (2.0 - abs(f32(dy)));
                        let adj = clamp(dot(off, dz_duv), -0.02, 0.02);
                        sum += wt * textureSampleCompareLevel(shadow_map, shadow_samp, suv + off, c, ref_z + adj);
                    }
                }
                lit = sum / 16.0;
            } else {
                lit = textureSampleCompareLevel(shadow_map, shadow_samp, suv, c, ref_z);
            }
            lit_sum += w * lit;
            w_sum += w;
        }
    }
    if (w_sum <= 0.0) {
        return 0.0;
    }
    let lit = lit_sum / w_sum;
    let last_split = frame.shadow.splits[n_cascades - 1];
    let fade = clamp((last_split - eye_depth) / max(frame.shadow.ctl.z, 0.001), 0.0, 1.0);
    return (1.0 - lit) * fade * clamp(fog, 0.0, 1.0);
}

// Half-width (in land-cell fractions) of the texture cross-fade band centred on
// each cell boundary. Land cells are large (~50 m), so a full-cell linear blend
// smears a wide muddy seam; narrowing it to a band near the boundary keeps cell
// interiors crisp. 0 -> hard edges (GL33-like); 0.5 -> full-cell blend.
override blend_width: f32 = 0.15;

@fragment
fn fs_terrain(in: VsOut) -> @location(0) vec4<f32> {
    // Receiver-plane derivatives must run in uniform control flow.
    let dwx = dpdx(in.world_pos);
    let dwy = dpdy(in.world_pos);

    // Continuous land-cell position; the ground texture repeats once per cell,
    // so this (plus jitter) doubles as the tiling UV. Blend the four cells
    // around the sample by fractional distance to their centres — where
    // neighbours share a layer the blend is a no-op (seamless), elsewhere it
    // cross-fades the hard cell edge.
    let cell_pos = (in.world_xz - tp.world_origin) / tp.land_grid;
    // Random UV offset breaking up the per-cell tiling repetition: the jitter
    // map holds Landscape::_random's per-grid-point offset (texel (x,z) = grid
    // point (x,z)), so a bilinear tap at cell_pos + half a texel reproduces the
    // corner interpolation GL33 bakes into its vertex UVs. Geography zeroes and
    // smooths the field around non-simple cells, so clamped transition tiles
    // are never warped off their designed edges.
    let jdim = vec2<f32>(textureDimensions(jitter_map));
    let jitter = textureSampleLevel(jitter_map, ground_clamp_samp,
                                    (cell_pos + vec2<f32>(0.5)) / jdim, 0.0).xy;
    let tile_uv = cell_pos + jitter;
    // Gradients of the shared tiling UV, for every ground tap (see sample_cell).
    let duvdx = dpdx(tile_uv);
    let duvdy = dpdy(tile_uv);
    let cc = cell_pos - vec2<f32>(0.5);
    let base = floor(cc);
    let f = cc - base;
    let bi = vec2<i32>(i32(base.x), i32(base.y));
    let e00 = cell_entry(bi + vec2<i32>(0, 0));
    let e10 = cell_entry(bi + vec2<i32>(1, 0));
    let e01 = cell_entry(bi + vec2<i32>(0, 1));
    let e11 = cell_entry(bi + vec2<i32>(1, 1));

    // Sharpen the fraction so the cross-fade concentrates in a band around the
    // cell boundary (f = 0.5) rather than ramping across the whole cell. Endpoints
    // stay 0/1, so neighbouring samples remain seamless.
    let bw = clamp(blend_width, 0.001, 0.5);
    let fx = smoothstep(0.5 - bw, 0.5 + bw, f.x);
    let fy = smoothstep(0.5 - bw, 0.5 + bw, f.y);
    let w00 = (1.0 - fx) * (1.0 - fy);
    let w10 = fx * (1.0 - fy);
    let w01 = (1.0 - fx) * fy;
    let w11 = fx * fy;
    var rgb = w00 * sample_cell(e00, base, tile_uv, duvdx, duvdy)
            + w10 * sample_cell(e10, base + vec2<f32>(1.0, 0.0), tile_uv, duvdx, duvdy)
            + w01 * sample_cell(e01, base + vec2<f32>(0.0, 1.0), tile_uv, duvdx, duvdy)
            + w11 * sample_cell(e11, base + vec2<f32>(1.0, 1.0), tile_uv, duvdx, duvdy);

    // High-frequency detail noise: alpha modulates around neutral (matches GL33's
    // r0.rgb *= t1.a * 2.0, detail UV = base UV * 32).
    let detail_a = textureSample(detail, ground_samp, tile_uv * 32.0).a;
    rgb *= detail_a * 2.0;

    // Per-pixel normal at a fixed heightmap step (independent of patch LOD/morph).
    let n = sample_normal(in.world_xz, tp.terrain_grid);
    // Sun light matching GL33's lit path: diffuse * N.L + ambient (eye
    // accommodation folded in on the CPU), saturated like the vertex-colour
    // pack it replaces. sun_dir_world is the light's travel direction (GL33's
    // sunDir constant, negated against the true up normal exactly as GL33's
    // vertex shader does); at night/dawn it points at or up through the
    // horizon, so level ground falls back to ambient. Not the shadow block's
    // sun_dir, which is only valid while the cascade pass runs.
    let cos_fi = max(dot(n, -frame.sun_dir_world.xyz), 0.0);
    let light = min(frame.sun_diffuse.rgb * cos_fi + frame.sun_ambient.rgb, vec3<f32>(1.0));
    rgb *= light;

    let s = shadow_strength(in.world_pos, n, in.fog, dwx, dwy);
    rgb *= mix(1.0, frame.shadow.ctl2.y, s);

    rgb = mix(frame.fog_color.rgb, rgb, in.fog);
    return vec4<f32>(rgb, 1.0);
}
