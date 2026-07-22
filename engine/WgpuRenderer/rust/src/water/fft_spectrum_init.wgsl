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
const TAU: f32 = 6.28318530718;
const GRAVITY: f32 = 9.81;
const PM_ALPHA: f32 = 0.0081;
// Calibrated visual energy for the JONSWAP lobes. Combined with wave_amp this
// intentionally restores a visibly stronger open-ocean displacement budget.
const SPECTRUM_ENERGY: f32 = 1.05;
fn hash(v: u32) -> u32 { var x = v; x = (x ^ (x >> 16u)) * 0x7feb352du; x = (x ^ (x >> 15u)) * 0x846ca68bu; return x ^ (x >> 16u); }
fn gaussian(seed: u32) -> vec2<f32> { let u = max(f32(hash(seed) & 0x00ffffffu) / 16777216.0, 1e-5); let v = f32(hash(seed ^ 0x9e3779b9u) & 0x00ffffffu) / 16777216.0; let r = sqrt(-2.0 * log(u)); return r * vec2<f32>(cos(TAU * v), sin(TAU * v)); }
fn cascade_length(layer: u32) -> f32 { return water.fft_cascade_lengths[layer]; }
fn cascade_split(lower: u32, upper: u32) -> f32 {
    // Adjacent fundamental frequencies meet at their geometric mean in log-k space.
    return sqrt(TAU / max(cascade_length(lower), 1.0) * TAU / max(cascade_length(upper), 1.0));
}
fn cascade_transition(kl: f32, split: f32) -> f32 {
    return smoothstep(split * 0.72, split * 1.38, kl);
}
fn cascade_band(kl: f32, layer: u32) -> f32 {
    let low_to_mid_low = cascade_transition(kl, cascade_split(2u, 3u));
    let mid_low_to_mid_high = cascade_transition(kl, cascade_split(1u, 2u));
    let mid_high_to_high = cascade_transition(kl, cascade_split(0u, 1u));
    if (layer == 0u) { return mid_high_to_high; }
    if (layer == 1u) { return mid_low_to_mid_high * (1.0 - mid_high_to_high); }
    if (layer == 2u) { return low_to_mid_low * (1.0 - mid_low_to_mid_high); }
    return 1.0 - low_to_mid_low;
}
// Deep-water JONSWAP expressed in k-space. The alpha ratio preserves the old
// parameter scale while the shape supplies the physical omega^-5/Jacobian result.
fn jonswap_k_shape(kl: f32, peak_k: f32, alpha: f32, gamma: f32) -> f32 {
    let ratio = max(kl / peak_k, 1e-4);
    let sigma = select(0.09, 0.07, ratio <= 1.0);
    let peak = exp(-0.5 * pow((ratio - 1.0) / sigma, 2.0));
    return alpha / PM_ALPHA * exp(-1.25 / (ratio * ratio))
        * pow(max(gamma, 1.0), peak) / max(kl * kl * kl * kl, 1e-5);
}
fn spread_power(omega: f32, peak_omega: f32) -> f32 {
    let ratio = max(omega / peak_omega, 1e-4);
    return select(9.77 * pow(ratio, -2.5), 6.97 * pow(ratio, 5.0), ratio <= 1.0);
}
fn cosine2s_normalization(s: f32) -> f32 {
    let s2 = s * s;
    let s3 = s2 * s;
    let s4 = s3 * s;
    return select(
        -4.80e-8 * s4 + 1.07e-5 * s3 - 9.53e-4 * s2 + 5.90e-2 * s + 3.93e-1,
        -5.64e-4 * s4 + 7.76e-3 * s3 - 4.40e-2 * s2 + 1.92e-1 * s + 1.63e-1,
        s < 5.0,
    );
}
fn directional_spreading(alignment: f32, omega: f32, peak_omega: f32, swell: f32) -> f32 {
    let s = spread_power(omega, peak_omega) + 16.0 * tanh(min(omega / peak_omega, 20.0)) * swell * swell;
    // cos(theta / 2)^(2s), without atan2; alignment is cos(theta).
    return cosine2s_normalization(s) * pow(max(0.5 * (1.0 + alignment), 0.0), s);
}
@compute @workgroup_size(8, 8, 1)
fn fft_spectrum_init(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(h0_out);
    if (id.x >= dims.x || id.y >= dims.y || id.z >= 4u) { return; }
    let n = i32(dims.x);
    let sx = select(i32(id.x), i32(id.x) - n, id.x > dims.x / 2u);
    let sy = select(i32(id.y), i32(id.y) - n, id.y > dims.y / 2u);
    let k = TAU * vec2<f32>(f32(sx), f32(sy)) / max(cascade_length(id.z), 1.0);
    let kl = length(k);
    if (kl < 1e-5 || water.fft_control.x < 0.5) {
        textureStore(h0_out, vec2<i32>(id.xy), i32(id.z), vec4<f32>(0.0));
        return;
    }
    let wind = normalize(water.fft_wind_sea.xy + vec2<f32>(1e-4, 0.0));
    let speed = max(water.fft_wind_sea.z, 0.5);
    let sea = clamp(water.fft_wind_sea.w, 0.0, 1.0);
    let alignment = dot(normalize(k), wind);
    let peak_k = GRAVITY / max(speed * speed, 1.0);
    let peak_omega = sqrt(GRAVITY * peak_k);
    let alpha = mix(0.0060, PM_ALPHA, sea);
    let gamma = mix(1.25, 3.0, sea);
    let wind_spectrum = jonswap_k_shape(kl, peak_k, alpha, gamma)
        * directional_spreading(alignment, sqrt(GRAVITY * kl), peak_omega, 0.20 + 0.50 * sea);
    let seed = u32(max(water.fft_control.y, 0.0));
    let swell_random = f32(hash(seed ^ 0x68bc21ebu) & 0x00ffffffu) / 16777216.0;
    let swell_sign = select(-1.0, 1.0, (hash(seed ^ 0x02e5be93u) & 1u) == 0u);
    let swell_angle = swell_sign * mix(0.45, 0.95, swell_random);
    let swell_direction = vec2<f32>(
        wind.x * cos(swell_angle) - wind.y * sin(swell_angle),
        wind.x * sin(swell_angle) + wind.y * cos(swell_angle),
    );
    let swell_alignment = dot(normalize(k), swell_direction);
    let swell_peak = peak_k * mix(0.14, 0.24, swell_random);
    let swell_omega = sqrt(GRAVITY * swell_peak);
    // Seeded cross-swell is a separately directed, deliberately low-energy JONSWAP lobe.
    let swell_spectrum = 0.03 * pow(swell_peak / peak_k, 4.0)
        * jonswap_k_shape(kl, swell_peak, alpha, 1.20 + 0.35 * sea)
        * directional_spreading(swell_alignment, sqrt(GRAVITY * kl), swell_omega, 0.85);
    let short_wave_damping = exp(-kl * kl * 0.0025);
    let spectrum = SPECTRUM_ENERGY * (wind_spectrum + swell_spectrum)
        * cascade_band(kl, id.z) * short_wave_damping * sea * water.wave_amp;
    let h0 = gaussian(seed ^ id.x * 1973u ^ id.y * 9277u ^ id.z * 26699u) * sqrt(max(spectrum, 0.0) * 0.5) * f32(n * n) * TAU / max(cascade_length(id.z), 1.0);
    textureStore(h0_out, vec2<i32>(id.xy), i32(id.z), vec4<f32>(h0, 0.0, 0.0));
}
