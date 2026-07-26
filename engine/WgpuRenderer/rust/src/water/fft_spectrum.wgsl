struct WaterParams {
    world_origin: vec2<f32>, terrain_grid: f32, sea_level: f32, hm_width: u32, hm_height: u32,
    time: f32, wave_amp: f32, wave_choppy: f32, wave_speed: f32, wave_scale: f32, fade_start: f32,
    fade_end: f32, warp_amp: f32, spec_power: f32, spec_intensity: f32, alpha: f32, shadow_dim: f32,
    color_ext: f32, coast_fade: f32, shallow_color: vec4<f32>, deep_color: vec4<f32>, foam_width: f32,
    foam_intensity: f32, swash_amp: f32, swash_speed: f32, fft_control: vec4<f32>,
    fft_wind_sea: vec4<f32>, fft_cascade_lengths: vec4<f32>, flow_direction_speed: vec4<f32>,
};
@group(0) @binding(0) var<uniform> water: WaterParams;
@group(0) @binding(1) var h0_texture: texture_2d_array<f32>;
@group(0) @binding(2) var pack0: texture_storage_2d_array<rgba32float, write>;
@group(0) @binding(3) var pack1: texture_storage_2d_array<rgba32float, write>;
@group(0) @binding(4) var pack2: texture_storage_2d_array<rgba32float, write>;
struct CascadeConfig {
    enabled: u32, resolution: u32, tile_length_x: f32, tile_length_y: f32,
    displacement_scale: f32, horiz_displacement_scale: f32, normal_scale: f32, foam_scale: f32,
    wind_speed: f32, wind_direction_rad: f32, fetch_meters: f32, water_depth_meters: f32,
    swell: f32, directional_spread: f32, short_wave_detail: f32, whitecap_threshold: f32,
    spectrum_seed: u32, phase_offset_seconds: f32, update_rate_hz: f32, pad: f32,
};
struct CascadeConfigs { config: array<CascadeConfig, 4>, };
@group(0) @binding(5) var<uniform> cascades: CascadeConfigs;
const TAU: f32 = 6.28318530718;
const GRAVITY: f32 = 9.81;
fn cmul(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> { return vec2<f32>(a.x * b.x - a.y * b.y, a.x * b.y + a.y * b.x); }
fn cexp(a: f32) -> vec2<f32> { return vec2<f32>(cos(a), sin(a)); }
fn cascade_length(layer: u32) -> f32 { return water.fft_cascade_lengths[layer]; }
fn tanh_approx(x: f32) -> f32 {
    if (x >= 3.0) { return 1.0; }
    let e2x = exp(2.0 * x);
    return (e2x - 1.0) / (e2x + 1.0);
}

fn dispersion_omega(kl: f32, depth: f32) -> f32 {
    let kh = clamp(kl * max(depth, 1.0), 0.01, 15.0);
    return sqrt(GRAVITY * kl * tanh_approx(kh));
}

@compute @workgroup_size(8, 8, 1)
fn fft_spectrum_evolve(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(pack0);
    if (id.x >= dims.x || id.y >= dims.y || id.z >= 4u) { return; }
    let cfg = cascades.config[id.z];
    let k = TAU * (vec2<f32>(id.xy) - vec2<f32>(f32(dims.x) * 0.5)) / max(cascade_length(id.z), 1.0); let kl = length(k);
    if (kl < 1e-5 || water.fft_control.x < 0.5 || cfg.enabled == 0u) { textureStore(pack0, vec2<i32>(id.xy), i32(id.z), vec4<f32>(0.0)); textureStore(pack1, vec2<i32>(id.xy), i32(id.z), vec4<f32>(0.0)); textureStore(pack2, vec2<i32>(id.xy), i32(id.z), vec4<f32>(0.0)); return; }
    let h0 = textureLoad(h0_texture, vec2<i32>(id.xy), i32(id.z), 0);
    let depth = max(cfg.water_depth_meters, 1.0);
    let omega = dispersion_omega(kl, depth);
    let phase = cexp(omega * (water.time + cfg.phase_offset_seconds) * water.wave_speed);
    // The modular opposite index covers DC and even-resolution Nyquist bins. Self-paired bins
    // reduce to h0*phase + conj(h0*phase), so the inverse transform remains real.
    let h_positive = cmul(h0.xy, phase);
    let h_negative = cmul(h0.zw, vec2<f32>(phase.x, -phase.y));
    let h = h_positive + h_negative;
    let h_i = vec2<f32>(-h.y, h.x);
    let k_unit = k / max(kl, 1e-6);

    // These are the reference spectrum_modulate.glsl equations verbatim. Godot's
    // compute path intentionally addresses its spectral X/Z axes as y/x.
    let hx = h_i * k_unit.y;
    let hy = h;
    let hz = h_i * k_unit.x;
    let dhy_dx = h_i * k.y;
    let dhy_dz = h_i * k.x;
    let dhx_dx = -h * k.y * k_unit.y;
    let dhz_dz = -h * k.x * k_unit.x;
    let dhz_dx = -h * k.y * k_unit.x;

    // Pack two real-valued spatial fields into each complex inverse transform,
    // exactly as GodotOceanWaves does. After IFFT, real/imaginary lanes unpack to:
    // pack0=(hx,hy,hz,dhy_dx), pack1=(dhy_dz,dhx_dx,dhz_dz,dhz_dx).
    textureStore(pack0, vec2<i32>(id.xy), i32(id.z), vec4<f32>(
        hx.x - hy.y, hx.y + hy.x,
        hz.x - dhy_dx.y, hz.y + dhy_dx.x));
    textureStore(pack1, vec2<i32>(id.xy), i32(id.z), vec4<f32>(
        dhy_dz.x - dhx_dx.y, dhy_dz.y + dhx_dx.x,
        dhz_dz.x - dhz_dx.y, dhz_dz.y + dhz_dx.x));
    textureStore(pack2, vec2<i32>(id.xy), i32(id.z), vec4<f32>(0.0));
}
