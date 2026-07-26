struct WaterParams {
    world_origin: vec2<f32>, terrain_grid: f32, sea_level: f32, hm_width: u32, hm_height: u32,
    time: f32, wave_amp: f32, wave_choppy: f32, wave_speed: f32, wave_scale: f32, fade_start: f32,
    fade_end: f32, warp_amp: f32, spec_power: f32, spec_intensity: f32, alpha: f32, shadow_dim: f32,
    color_ext: f32, coast_fade: f32, shallow_color: vec4<f32>, deep_color: vec4<f32>, foam_width: f32,
    foam_intensity: f32, swash_amp: f32, swash_speed: f32, fft_control: vec4<f32>,
    fft_wind_sea: vec4<f32>, fft_cascade_lengths: vec4<f32>, flow_direction_speed: vec4<f32>,
};
@group(0) @binding(0) var<uniform> water: WaterParams;
@group(0) @binding(1) var h0_out: texture_storage_2d_array<rgba32float, write>;
struct CascadeConfig {
    enabled: u32, resolution: u32, tile_length_x: f32, tile_length_y: f32,
    displacement_scale: f32, horiz_displacement_scale: f32, normal_scale: f32, foam_scale: f32,
    wind_speed: f32, wind_direction_rad: f32, fetch_meters: f32, water_depth_meters: f32,
    swell: f32, directional_spread: f32, short_wave_detail: f32, whitecap_threshold: f32,
    spectrum_seed: u32, phase_offset_seconds: f32, update_rate_hz: f32, pad: f32,
};
struct CascadeConfigs { config: array<CascadeConfig, 4>, };
@group(0) @binding(2) var<uniform> cascades: CascadeConfigs;
const PI: f32 = 3.141592653589793;
const TAU: f32 = 6.28318530718;
const INV_TWO_PI: f32 = 0.15915494309;
const GRAVITY: f32 = 9.81;
// Exact integer hash and Box-Muller transform used by spectrum_compute.glsl.
fn reference_hash(x: vec2<u32>) -> vec2<f32> {
    var h = x.y + 374761393u + x.x * 3266489917u;
    h = 2246822519u * (h ^ (h >> 15u));
    h = 3266489917u * (h ^ (h >> 13u));
    let n = h ^ (h >> 16u);
    let bits = (vec2<u32>(n, n * 48271u) >> vec2<u32>(1u)) & vec2<u32>(0x7fffffffu);
    return vec2<f32>(bits) / 2147483647.0;
}
fn gaussian(id: vec2<u32>, seed: u32) -> vec2<f32> {
    let uniform = reference_hash(id + vec2<u32>(seed));
    let radius = sqrt(-2.0 * log(max(uniform.x, 1e-7)));
    let theta = TAU * uniform.y;
    return radius * vec2<f32>(cos(theta), sin(theta));
}
fn cascade_length(layer: u32) -> f32 { return water.fft_cascade_lengths[layer]; }
// The following functions intentionally mirror 2Retr0/GodotOceanWaves'
// spectrum_compute.glsl: frequency-domain TMA/JONSWAP, Hasselmann spreading,
// and dω/dk energy conversion. Do not replace these with a k-space approximation.
fn dispersion_relation(k: f32, depth: f32) -> vec2<f32> {
    let kd = k * depth;
    let b = tanh(kd);
    let omega = sqrt(GRAVITY * k * b);
    let derivative = 0.5 * GRAVITY * (b + kd * (1.0 - b * b)) / max(omega, 1e-6);
    return vec2<f32>(omega, derivative);
}
fn longuet_higgins_normalization(s: f32) -> f32 {
    let a = sqrt(max(s, 1e-6));
    return select(inverseSqrt(3.14159265359) * (a * 0.5 + 0.0625 / a),
        INV_TWO_PI + s * (0.220636 + s * (-0.109 + s * 0.090)), s < 0.4);
}
fn hasselmann_spread(omega: f32, peak: f32, wind_speed: f32, alignment: f32, swell: f32) -> f32 {
    let p = max(omega / peak, 1e-5);
    let s = select(9.77 * pow(p, -2.33 - 1.45 * (wind_speed * peak / GRAVITY - 1.17)),
        6.97 * pow(p, 4.06), omega <= peak);
    let sx = 16.0 * tanh(peak / max(omega, 1e-5)) * swell * swell;
    return longuet_higgins_normalization(s + sx) * pow(max(0.5 * (1.0 + alignment), 0.0), s + sx);
}
fn tma_spectrum(omega: f32, peak: f32, alpha: f32, depth: f32) -> f32 {
    let sigma = select(0.09, 0.07, omega <= peak);
    let r = exp(-(omega - peak) * (omega - peak) / max(2.0 * sigma * sigma * peak * peak, 1e-8));
    let jonswap = alpha * GRAVITY * GRAVITY / pow(max(omega, 1e-5), 5.0)
        * exp(-1.25 * pow(peak / max(omega, 1e-5), 4.0)) * pow(3.3, r);
    let wh = min(omega * sqrt(depth / GRAVITY), 2.0);
    let attenuation = select(1.0 - 0.5 * (2.0 - wh) * (2.0 - wh), 0.5 * wh * wh, wh <= 1.0);
    return jonswap * attenuation;
}
@compute @workgroup_size(8, 8, 1)
fn fft_spectrum_init(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(h0_out);
    if (id.x >= dims.x || id.y >= dims.y || id.z >= 4u) { return; }
    let cfg = cascades.config[id.z];
    let n = f32(dims.x);
    let k = TAU * (vec2<f32>(id.xy) - vec2<f32>(n * 0.5)) / max(cascade_length(id.z), 1.0);
    let kl = length(k);
    if (kl < 1e-5 || water.fft_control.x < 0.5 || cfg.enabled == 0u) {
        textureStore(h0_out, vec2<i32>(id.xy), i32(id.z), vec4<f32>(0.0));
        return;
    }
    // The reference defines theta as atan(k.x, k.y), so a zero-degree wind points
    // along +Z/+k.y. This component order is intentional.
    let wind = vec2<f32>(sin(cfg.wind_direction_rad), cos(cfg.wind_direction_rad));
    let speed = max(cfg.wind_speed, 0.5);
    let alignment = dot(normalize(k), wind);
    // GodotOceanWaves' JONSWAP fetch model: alpha and peak angular frequency
    // are independently authored for every cascade, not inferred from one sea state.
    let fetch = max(cfg.fetch_meters, 1.0);
    let alpha = 0.076 * pow(speed * speed / (fetch * GRAVITY), 0.22);
    let peak_omega = 22.0 * pow(GRAVITY * GRAVITY / max(speed * fetch, 1.0), 1.0 / 3.0);
    let dispersion = dispersion_relation(kl, max(cfg.water_depth_meters, 1.0));
    let directional = hasselmann_spread(dispersion.x, peak_omega, speed, alignment, cfg.swell);
    let spread = mix(directional, INV_TWO_PI, clamp(cfg.directional_spread, 0.0, 1.0));
    let depth = max(cfg.water_depth_meters, 1.0);
    let wind_spectrum = tma_spectrum(dispersion.x, peak_omega, alpha, depth) * spread;
    let seed = cfg.spectrum_seed;
    let detail = clamp(cfg.short_wave_detail, 0.0, 1.0);
    let short_wave_damping = exp(-(1.0 - detail) * (1.0 - detail) * kl * kl);
    let cell_area = TAU / max(cascade_length(id.z), 1.0) * TAU / max(cascade_length(id.z), 1.0);
    // Spectral amplitudes are the square root of variance.  Square the authored
    // amplitude here so the Water-tab control is perceptually and physically
    // linear: a 2x slider value produces 2x surface displacement, not sqrt(2)x.
    let amplitude_energy = max(water.wave_amp, 0.0) * max(water.wave_amp, 0.0);
    let variance = wind_spectrum * short_wave_damping * dispersion.y / kl * cell_area * amplitude_energy;
    let h0 = gaussian(id.xy, seed) * sqrt(max(2.0 * variance, 0.0));
    // Store h0(k) and conj(h0(-k)) exactly as the reference modulation pass does.
    let opposite = (dims.xy - id.xy) % dims.xy;
    let opposite_directional = hasselmann_spread(dispersion.x, peak_omega, speed, -alignment, cfg.swell);
    let opposite_spread = mix(opposite_directional, INV_TWO_PI, clamp(cfg.directional_spread, 0.0, 1.0));
    let opposite_variance = tma_spectrum(dispersion.x, peak_omega, alpha, depth)
        * opposite_spread * short_wave_damping * dispersion.y / kl * cell_area * amplitude_energy;
    let h0_opposite = gaussian(opposite, seed) * sqrt(max(2.0 * opposite_variance, 0.0));
    textureStore(h0_out, vec2<i32>(id.xy), i32(id.z), vec4<f32>(h0, h0_opposite.x, -h0_opposite.y));
}
