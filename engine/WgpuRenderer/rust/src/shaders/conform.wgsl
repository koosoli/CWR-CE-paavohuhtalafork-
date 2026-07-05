#define_import_path conform

// The terrain heightmap + its sampling params, bound at group(4) by both the lit
// mesh pipeline (shader3d) and the shadow depth pass (shadow_depth), so each conforms
// ClipLand vegetation to the ground (SurfaceY) per vertex without the CPU rewriting the
// shared mesh. The R32Float heightmap is read with textureLoad in the vertex stage
// (non-filterable); HmParams mirrors the Rust TerrainConformParams (32 bytes). Both the
// color and shadow passes import surface_y from HERE — the CPU gameplay conform
// (Object::Animate → occlusion/LOS) evaluates the same SurfaceY, so a single shared
// definition keeps the rendered geometry aligned with the gameplay occluders.
struct HmParams {
    origin: vec2<f32>,   // world xz of heightmap texel (0,0)
    terrain_grid: f32,   // world metres per heightmap texel
    enabled: f32,        // 1 when a heightmap is loaded
    hm_width: u32,
    hm_height: u32,
};
@group(4) @binding(0) var hm: texture_2d<f32>;
@group(4) @binding(1) var<uniform> hm_params: HmParams;

fn hm_load(ix: i32, iz: i32) -> f32 {
    let cx = clamp(ix, 0, i32(hm_params.hm_width) - 1);
    let cz = clamp(iz, 0, i32(hm_params.hm_height) - 1);
    return textureLoad(hm, vec2<i32>(cx, cz), 0).x;
}

// Absolute terrain height at world xz, matching Landscape::SurfaceY and the terrain
// shader's sample_height exactly (per-cell two-triangle interpolation, NOT bilinear),
// so conformed vegetation sits on the same ground the terrain pass renders.
fn surface_y(world_xz: vec2<f32>) -> f32 {
    if (hm_params.enabled < 0.5) {
        return 0.0;
    }
    let t = (world_xz - hm_params.origin) / hm_params.terrain_grid;
    let base = floor(t);
    let ix = i32(base.x);
    let iz = i32(base.y);
    let f = t - base;
    let y00 = hm_load(ix, iz);
    let y01 = hm_load(ix + 1, iz);
    let y10 = hm_load(ix, iz + 1);
    let y11 = hm_load(ix + 1, iz + 1);
    if (f.x <= 1.0 - f.y) {
        return y00 + (y10 - y00) * f.y + (y01 - y00) * f.x;
    }
    return y10 + (y01 - y11) - (y10 - y11) * f.x - (y01 - y11) * f.y;
}
