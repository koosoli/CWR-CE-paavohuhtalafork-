struct WaterParams {
    world_origin: vec2<f32>, terrain_grid: f32, sea_level: f32, hm_width: u32, hm_height: u32,
    time: f32, wave_amp: f32, wave_choppy: f32, wave_speed: f32, wave_scale: f32, fade_start: f32,
    fade_end: f32, warp_amp: f32, spec_power: f32, spec_intensity: f32, alpha: f32, shadow_dim: f32,
    color_ext: f32, coast_fade: f32, shallow_color: vec4<f32>, deep_color: vec4<f32>, foam_width: f32,
    foam_intensity: f32, swash_amp: f32, swash_speed: f32, fft_control: vec4<f32>,
    fft_wind_sea: vec4<f32>, fft_cascade_lengths: vec4<f32>, flow_direction_speed: vec4<f32>,
};
@group(0) @binding(0) var<uniform> water: WaterParams;
@group(0) @binding(1) var pack0: texture_storage_2d_array<rgba32float, write>;
@group(0) @binding(2) var pack1: texture_storage_2d_array<rgba32float, write>;
@group(0) @binding(3) var pack2: texture_storage_2d_array<rgba32float, write>;
const TAU: f32 = 6.28318530718;
const GRAVITY: f32 = 9.81;
fn cmul(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> { return vec2<f32>(a.x * b.x - a.y * b.y, a.x * b.y + a.y * b.x); }
fn cexp(a: f32) -> vec2<f32> { return vec2<f32>(cos(a), sin(a)); }
fn hash(v: u32) -> u32 { var x = v; x = (x ^ (x >> 16u)) * 0x7feb352du; x = (x ^ (x >> 15u)) * 0x846ca68bu; return x ^ (x >> 16u); }
fn gaussian(seed: u32) -> vec2<f32> { let u = max(f32(hash(seed) & 0x00ffffffu) / 16777216.0, 1e-5); let v = f32(hash(seed ^ 0x9e3779b9u) & 0x00ffffffu) / 16777216.0; let r = sqrt(-2.0 * log(u)); return r * vec2<f32>(cos(TAU * v), sin(TAU * v)); }
fn cascade_length(layer: u32) -> f32 { return water.fft_cascade_lengths[layer]; }
fn cascade_band(kl: f32, layer: u32, resolution: f32) -> f32 {
    let domain = max(cascade_length(layer), 1.0);
    let k_min = TAU / domain;
    let k_max = 3.14159265359 * resolution / domain;
    // Adjacent cascades overlap softly, but no longer duplicate the whole spectrum.
    return smoothstep(k_min * 1.15, k_min * 2.4, kl) * (1.0 - smoothstep(k_max * 0.42, k_max * 0.70, kl));
}
@compute @workgroup_size(8, 8, 1)
fn fft_spectrum_evolve(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(pack0);
    if (id.x >= dims.x || id.y >= dims.y || id.z >= 4u) { return; }
    let n = i32(dims.x); let sx = select(i32(id.x), i32(id.x) - n, id.x > dims.x / 2u); let sy = select(i32(id.y), i32(id.y) - n, id.y > dims.y / 2u);
    let k = TAU * vec2<f32>(f32(sx), f32(sy)) / max(cascade_length(id.z), 1.0); let kl = length(k);
    if (kl < 1e-5 || water.fft_control.x < 0.5) { textureStore(pack0, vec2<i32>(id.xy), i32(id.z), vec4<f32>(0.0)); textureStore(pack1, vec2<i32>(id.xy), i32(id.z), vec4<f32>(0.0)); textureStore(pack2, vec2<i32>(id.xy), i32(id.z), vec4<f32>(0.0)); return; }
    let wind = normalize(water.fft_wind_sea.xy + vec2<f32>(1e-4, 0.0)); let speed = max(water.fft_wind_sea.z, 0.5); let sea = clamp(water.fft_wind_sea.w, 0.0, 1.0);
    let alignment = dot(normalize(k), wind); let forward = max(alignment, 0.0); let backward = max(-alignment, 0.0); let spread = 0.07 + 0.87 * pow(forward, mix(2.0, 6.0, sea)) + 0.06 * backward * backward;
    let peak = GRAVITY / max(speed * speed, 1.0); let short_wave_damping = exp(-kl * kl * 0.0025); let spectrum = exp(-peak * peak / max(kl * kl, 1e-5)) / max(kl * kl * kl * kl, 1e-5) * spread * cascade_band(kl, id.z, f32(n)) * short_wave_damping * sea * water.wave_amp;
    let seed = u32(max(water.fft_control.y, 0.0)); let h0 = gaussian(seed ^ id.x * 1973u ^ id.y * 9277u ^ id.z * 26699u) * sqrt(max(spectrum, 0.0) * 0.5) * f32(n * n) * TAU / max(cascade_length(id.z), 1.0);
    let omega = sqrt(GRAVITY * kl); let h = cmul(h0, cexp(omega * water.time * water.wave_speed)); let slope_x = vec2<f32>(-h.y, h.x) * k.x; let slope_z = vec2<f32>(-h.y, h.x) * k.y; let chop = water.wave_choppy / max(kl, 0.02); let dx = vec2<f32>(h.y, -h.x) * k.x / kl * chop; let dz = vec2<f32>(h.y, -h.x) * k.y / kl * chop;
    textureStore(pack0, vec2<i32>(id.xy), i32(id.z), vec4<f32>(h, slope_x)); textureStore(pack1, vec2<i32>(id.xy), i32(id.z), vec4<f32>(slope_z, dx)); textureStore(pack2, vec2<i32>(id.xy), i32(id.z), vec4<f32>(dz, vec2<f32>(-h.y, h.x) * omega));
}
