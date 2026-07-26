// GodotOceanWaves-style sea spray.  The reference uses a GPUParticles3D emitter
// over a 10m grid and lets each particle follow the FFT displacement at its start
// point.  This port uses the same principle, but derives all particle state from the
// instance index and time so it stays entirely GPU resident and needs no readback.

#import frame::{frame, reverse_z, fog_factor}
#import water_fft_sampling::fft_aperiodic_uv

const TAU: f32 = 6.28318530718;
const EMITTER_SIDE: u32 = 181u; // ceil(sqrt(32768)); matches reference density.
const EMITTER_SPAN: f32 = 150.0;

struct WaterParams {
    world_origin: vec2<f32>, terrain_grid: f32, sea_level: f32,
    hm_width: u32, hm_height: u32, time: f32,
    wave_amp: f32, wave_choppy: f32, wave_speed: f32, wave_scale: f32,
    fade_start: f32, fade_end: f32, warp_amp: f32, spec_power: f32,
    spec_intensity: f32, alpha: f32, shadow_dim: f32, color_ext: f32,
    coast_fade: f32, shallow_color: vec4<f32>, deep_color: vec4<f32>,
    foam_width: f32, foam_intensity: f32, swash_amp: f32, swash_speed: f32,
    fft_control: vec4<f32>, fft_wind_sea: vec4<f32>,
    fft_cascade_lengths: vec4<f32>, flow_direction_speed: vec4<f32>,
    debug_params: vec4<f32>,
};

// This is deliberately the same group-1 interface as water.wgsl.  The unused
// bindings remain in the shared layout; spray only needs parameters and the FFT
// displacement/crest fields.
@group(1) @binding(0) var<uniform> wp: WaterParams;
@group(1) @binding(4) var interaction_field: texture_2d<f32>;
@group(1) @binding(5) var interaction_samp: sampler;
@group(1) @binding(6) var fft_displacement: texture_2d_array<f32>;
@group(1) @binding(8) var fft_auxiliary: texture_2d_array<f32>;
@group(1) @binding(9) var fft_samp: sampler;
@group(1) @binding(10) var foam_history: texture_2d<f32>;
@group(1) @binding(11) var foam_samp: sampler;

struct SprayOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) alpha: f32,
    @location(2) fog: f32,
};

fn hash11(x: u32) -> f32 {
    var n = x;
    n = (n ^ (n >> 16u)) * 0x7feb352du;
    n = (n ^ (n >> 15u)) * 0x846ca68bu;
    n = n ^ (n >> 16u);
    return f32(n & 0x00ffffffu) / 16777216.0;
}

fn hash21(x: u32) -> vec2<f32> {
    return vec2<f32>(hash11(x * 1597334677u + 17u), hash11(x * 3812015801u + 53u));
}

fn quad_corner(vertex: u32) -> vec2<f32> {
    // Two triangles: (-1,-1), (1,-1), (-1,1), (-1,1), (1,-1), (1,1).
    let corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0));
    return corners[vertex];
}

fn crest_source(world_xz: vec2<f32>) -> vec4<f32> {
    var displacement = vec3<f32>(0.0);
    var crest = 0.0;
    var compression = 0.0;
    var slope = 0.0;
    for (var layer = 0; layer < 4; layer = layer + 1) {
        let tile = wp.fft_cascade_lengths[layer];
        if (tile > 0.5) {
            let uv = fft_aperiodic_uv(world_xz, tile, layer, wp.warp_amp);
            let d = textureSampleLevel(fft_displacement, fft_samp, uv, layer, 0.0);
            let a = textureSampleLevel(fft_auxiliary, fft_samp, uv, layer, 0.0);
            displacement += d.xyz;
            crest = max(crest, d.w);
            compression = max(compression, a.y);
            slope = max(slope, sqrt(max(a.w, 0.0)));
        }
    }
    // The reference emitter activates only when accumulated normal-map foam exceeds
    // 0.9. Sample our camera-domain history with the same threshold.
    let domain_origin = floor((frame.cam_pos.xz - vec2<f32>(128.0)) / 4.0) * 4.0;
    let foam_uv = (world_xz - domain_origin) / 256.0;
    let foam_inside = step(0.0, foam_uv.x) * step(0.0, foam_uv.y) *
        step(foam_uv.x, 1.0) * step(foam_uv.y, 1.0);
    let foam = textureSampleLevel(foam_history, foam_samp,
        clamp(foam_uv, vec2<f32>(0.001), vec2<f32>(0.999)), 0.0).r * foam_inside;
    let breaking = smoothstep(0.90, 1.0, foam);
    return vec4<f32>(displacement, breaking);
}

// The interaction solver follows the camera in a snapped 256m domain.  Reading
// its aeration channel lets vessel wakes and impact splashes use the exact same
// billboard path as wind-torn wave crests, rather than being limited to surface foam.
fn interaction_source(world_xz: vec2<f32>) -> vec4<f32> {
    let domain_origin = floor((frame.cam_pos.xz - vec2<f32>(128.0)) / 4.0) * 4.0;
    let uv = (world_xz - domain_origin) / 256.0;
    let inside = step(0.0, uv.x) * step(0.0, uv.y) * step(uv.x, 1.0) * step(uv.y, 1.0);
    return textureSampleLevel(interaction_field, interaction_samp,
        clamp(uv, vec2<f32>(0.001), vec2<f32>(0.999)), 0.0) * inside;
}

@vertex
fn vs_whitewater(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance_index: u32,
) -> SprayOut {
    let random = hash21(instance_index);
    let cell = vec2<f32>(
        f32(instance_index % EMITTER_SIDE), f32(instance_index / EMITTER_SIDE));
    // Snap the emitter rather than following the camera every pixel.  The cycle
    // phase and jitter conceal the 150m relocation, while the snap keeps spray
    // anchored to world-space wave crests.
    let anchor = floor(frame.cam_pos.xz / EMITTER_SPAN) * EMITTER_SPAN;
    let local = ((cell + random) / f32(EMITTER_SIDE) - vec2<f32>(0.5)) * EMITTER_SPAN;
    let source_xz = anchor + local;

    let lifetime = mix(1.45, 2.65, hash11(instance_index * 747796405u + 2891336453u));
    let age = fract(wp.time / lifetime + hash11(instance_index * 277803737u + 1u));
    let crest = crest_source(source_xz);
    let interaction = interaction_source(source_xz);
    // Interaction events write aeration into .b and radial impulse velocity into .g.
    // Their physical values are intentionally small, so the old 0.08..0.45 gate
    // discarded normal footsteps, wakes, and impact splashes before any billboard
    // was emitted.  Keep both sources independent: a local impact now makes a
    // visible splash even when the surrounding wind waves are not breaking.
    let interaction_splash = max(
        smoothstep(0.010, 0.090, interaction.b),
        smoothstep(0.060, 0.38, abs(interaction.g)));
    // Water-tab "Water splash particles" switch. Keep the draw allocation stable
    // but make all generated billboards fully transparent when disabled.
    let spray_enabled = step(0.5, wp.debug_params.y);
    let spray_activity = clamp(wp.debug_params.z, 0.0, 1.0);
    let source_strength = max(crest.w, interaction_splash) * spray_enabled * spray_activity;
    let wind = normalize(wp.fft_wind_sea.xy + vec2<f32>(1e-4, 0.0));
    let horizontal_drift = wind * age * age * (0.30 + max(wp.fft_wind_sea.z, 0.0) * 0.075);
    // Ballistic envelope: particles leave a crest, peak midway through life, then
    // merge back into the surface.  Its amplitude follows breaking strength.
    let lift = 4.0 * age * (1.0 - age) * (0.35 + source_strength * 1.95);
    let world = vec3<f32>(
        source_xz.x + crest.x * 0.75 + horizontal_drift.x,
        wp.sea_level + crest.y + interaction.r * 2.5 + lift,
        source_xz.y + crest.z * 0.75 + horizontal_drift.y,
    );
    let world_rel = world - frame.cam_pos.xyz;
    let corner = quad_corner(vertex_index);
    let size = mix(0.12, 0.92, source_strength) * mix(0.65, 1.0, sin(age * 3.14159265));
    // Form the quad in view space: this is exact camera billboarding without
    // depending on a CPU-side emitter transform.
    let view_pos = frame.view * vec4<f32>(world_rel, 1.0);
    let billboard = view_pos + vec4<f32>(corner * size, 0.0, 0.0);
    var out: SprayOut;
    out.clip = reverse_z(frame.proj * billboard);
    out.uv = corner * 0.5 + vec2<f32>(0.5);
    out.alpha = source_strength * smoothstep(0.0, 0.12, lift) * (1.0 - smoothstep(0.72, 1.0, age));
    out.fog = fog_factor(length(world_rel));
    return out;
}

override linear: f32 = 0.0;

@fragment
fn fs_whitewater(in: SprayOut) -> @location(0) vec4<f32> {
    // A procedural soft droplet replaces the reference PNG atlas, avoiding an
    // additional asset/load path while retaining its rounded, dissolving silhouette.
    let q = in.uv * 2.0 - vec2<f32>(1.0);
    let radial = max(1.0 - dot(q, q), 0.0);
    let wispy = pow(radial, 1.75) * (0.72 + 0.28 * sin((q.x - q.y) * 13.0 + wp.time * 3.0));
    let alpha = in.alpha * wispy * 0.58;
    if (alpha < 0.004) { discard; }
    var colour = vec3<f32>(0.82, 0.92, 0.96);
    if (linear <= 0.5) { colour = min(colour, vec3<f32>(1.0)); }
    // fog_factor above uses the same camera-relative distance as the water path.
    colour = mix(frame.fog_color.rgb, colour, in.fog);
    return vec4<f32>(colour, alpha);
}
