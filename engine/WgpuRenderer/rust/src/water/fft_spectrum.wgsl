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
const TAU: f32 = 6.28318530718;
const GRAVITY: f32 = 9.81;
fn cmul(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> { return vec2<f32>(a.x * b.x - a.y * b.y, a.x * b.y + a.y * b.x); }
fn cexp(a: f32) -> vec2<f32> { return vec2<f32>(cos(a), sin(a)); }
fn cascade_length(layer: u32) -> f32 { return water.fft_cascade_lengths[layer]; }
@compute @workgroup_size(8, 8, 1)
fn fft_spectrum_evolve(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(pack0);
    if (id.x >= dims.x || id.y >= dims.y || id.z >= 4u) { return; }
    let n = i32(dims.x); let sx = select(i32(id.x), i32(id.x) - n, id.x > dims.x / 2u); let sy = select(i32(id.y), i32(id.y) - n, id.y > dims.y / 2u);
    let k = TAU * vec2<f32>(f32(sx), f32(sy)) / max(cascade_length(id.z), 1.0); let kl = length(k);
    if (kl < 1e-5 || water.fft_control.x < 0.5) { textureStore(pack0, vec2<i32>(id.xy), i32(id.z), vec4<f32>(0.0)); textureStore(pack1, vec2<i32>(id.xy), i32(id.z), vec4<f32>(0.0)); textureStore(pack2, vec2<i32>(id.xy), i32(id.z), vec4<f32>(0.0)); return; }
    let opposite = vec2<i32>(i32((dims.x - id.x) % dims.x), i32((dims.y - id.y) % dims.y));
    let h0 = textureLoad(h0_texture, vec2<i32>(id.xy), i32(id.z), 0).xy;
    let h0_opposite = textureLoad(h0_texture, opposite, i32(id.z), 0).xy;
    let omega = sqrt(GRAVITY * kl);
    let phase = cexp(omega * water.time * water.wave_speed);
    // The modular opposite index covers DC and even-resolution Nyquist bins. Self-paired bins
    // reduce to h0*phase + conj(h0*phase), so the inverse transform remains real.
    let h_positive = cmul(h0, phase);
    let h_negative = cmul(vec2<f32>(h0_opposite.x, -h0_opposite.y), vec2<f32>(phase.x, -phase.y));
    let h = h_positive + h_negative;
    // An even-size Nyquist coordinate aliases its negative. Its odd spatial derivative must be
    // zero, otherwise the derivative packs cease to be Hermitian.
    let derivative_k = vec2<f32>(select(k.x, 0.0, id.x == dims.x / 2u), select(k.y, 0.0, id.y == dims.y / 2u));
    let chop = water.wave_choppy / max(kl, 0.02);
    let dx = vec2<f32>(h.y, -h.x) * derivative_k.x / kl * chop;
    let dz = vec2<f32>(h.y, -h.x) * derivative_k.y / kl * chop;
    // D = -i * h * k / |k| * chop. Multiplying D by i*k for each spatial
    // derivative gives these three Hermitian spectral fields. The cross term is
    // shared: dDx/dz == dDz/dx, so all four Jacobian entries need only three packs.
    let d_dxdx = h * derivative_k.x * derivative_k.x / kl * chop;
    let d_dxdz = h * derivative_k.x * derivative_k.y / kl * chop;
    let d_dzdz = h * derivative_k.y * derivative_k.y / kl * chop;
    // Pack layout after inverse FFT:
    // pack0 = height, dDx/dx; pack1 = dDx/dz (= dDz/dx), dDz/dz;
    // pack2 = Dx, Dz. Compose reconstructs normal slopes from height samples.
    textureStore(pack0, vec2<i32>(id.xy), i32(id.z), vec4<f32>(h, d_dxdx));
    textureStore(pack1, vec2<i32>(id.xy), i32(id.z), vec4<f32>(d_dxdz, d_dzdz));
    textureStore(pack2, vec2<i32>(id.xy), i32(id.z), vec4<f32>(dx, dz));
}
