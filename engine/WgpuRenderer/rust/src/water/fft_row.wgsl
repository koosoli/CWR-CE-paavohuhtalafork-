// One workgroup per line (row or column); the entire FFT for that line runs in
// workgroup storage, behind barriers, and touches global memory exactly twice — one load
// and one store.
//
// This replaces a per-stage kernel that ran 9 stages x 2 axes x 2 packs = 36 full-resolution
// global dispatches, each re-reading and re-writing all 512x512x4 texels. The arithmetic is
// identical; only the memory traffic changes.
//
// Adapted from GodotOceanWaves' compute/fft_compute.glsl (a coalesced Stockham kernel) with
// one deliberate difference: the reference stores its spectrum in a flat SSBO, so its FFT can
// only walk rows and it needs a separate 32x32 tiled transpose pass to reach the second axis
// (4 global passes per pack). Our spectrum lives in a texture_2d_array, which indexes either
// axis directly, so `axis` picks the walk direction and the transpose passes disappear
// entirely (2 global passes per pack).
//
// Structure is in-place decimation-in-time rather than the reference's ping-pong Stockham.
// A fixed 256-thread workgroup handles 256/512/1024-point transforms; at 1024 every thread
// owns two butterflies. The maximum line occupies exactly WebGPU's guaranteed 16 KiB of
// workgroup storage, so the Godot reference resolution remains portable.

struct StageParams { data: vec4<u32> };
@group(0) @binding(0) var<uniform> stage_params: StageParams;
@group(0) @binding(1) var source: texture_2d_array<f32>;
@group(0) @binding(2) var destination: texture_storage_2d_array<rgba32float, write>;

const TAU: f32 = 6.28318530718;
// One fixed workgroup handles every supported power-of-two resolution.
const FFT_THREADS: u32 = 256u;

fn cmul(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(a.x * b.x - a.y * b.y, a.x * b.y + a.y * b.x);
}

fn bit_reverse(v: u32, bits: u32) -> u32 {
    var x = v;
    var out = 0u;
    for (var i = 0u; i < bits; i = i + 1u) {
        out = (out << 1u) | (x & 1u);
        x = x >> 1u;
    }
    return out;
}

// One line of the transform. Each texel is RGBA32F = two independent complex signals
// (.xy and .zw), transformed together exactly as the previous per-stage kernel did.
var<workgroup> line_data: array<vec4<f32>, 1024>;

@compute @workgroup_size(256, 1, 1)
fn fft_row(
    @builtin(workgroup_id) wg: vec3<u32>,
    @builtin(local_invocation_index) local_index: u32,
) {
    // data.x = N, data.y = axis (0 = along x / rows, 1 = along y / columns),
    // data.z = log2(N). These are structural and uniform for the whole dispatch.
    let fft_n = stage_params.data.x;
    let axis = stage_params.data.y;
    let fft_bits = stage_params.data.z;
    let line_index = i32(wg.x);
    let layer = i32(wg.y);
    let thread = local_index;

    // Decimation-in-time wants the input in bit-reversed order; do the permutation on the
    // way into workgroup storage so the butterflies below are pure in-place work.
    for (var i = thread; i < fft_n; i = i + FFT_THREADS) {
        let src = i32(bit_reverse(i, fft_bits));
        var coord = vec2<i32>(src, line_index);
        if (axis == 1u) {
            coord = vec2<i32>(line_index, src);
        }
        line_data[i] = textureLoad(source, coord, layer, 0);
    }

    // `span` is the half-width of the current butterfly (avoid naming it `half`: WGSL
    // reserves that word, and a reserved identifier fails composition at runtime).
    for (var stage = 0u; stage < fft_bits; stage = stage + 1u) {
        // Publishes the previous stage's writes (and the initial load) to the whole
        // workgroup. Uniform control flow: FFT_BITS is a module constant.
        workgroupBarrier();
        let butterflies = fft_n >> 1u;
        for (var butterfly = thread; butterfly < butterflies; butterfly = butterfly + FFT_THREADS) {
            let span = 1u << stage;
            let width = span << 1u;
            let group = butterfly / span;
            let j = butterfly % span;
            let i0 = group * width + j;
            let i1 = i0 + span;
            // Positive exponent, matching the previous kernel's convention: the spectrum is
            // synthesised as a physical Fourier series and the transform is left unnormalised.
            // A 1/N^2 here would divide the ocean into flatness.
            let angle = TAU * f32(j) / f32(width);
            let w = vec2<f32>(cos(angle), sin(angle));
            let a = line_data[i0];
            let b = line_data[i1];
            let t0 = cmul(b.xy, w);
            let t1 = cmul(b.zw, w);
            // The butterfly index maps bijectively onto disjoint pairs for this stage.
            line_data[i0] = vec4<f32>(a.xy + t0, a.zw + t1);
            line_data[i1] = vec4<f32>(a.xy - t0, a.zw - t1);
        }
    }
    workgroupBarrier();

    for (var i = thread; i < fft_n; i = i + FFT_THREADS) {
        var coord = vec2<i32>(i32(i), line_index);
        if (axis == 1u) {
            coord = vec2<i32>(line_index, i32(i));
        }
        textureStore(destination, coord, layer, line_data[i]);
    }
}
