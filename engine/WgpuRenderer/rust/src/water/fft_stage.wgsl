struct StageParams { data: vec4<u32> };
@group(0) @binding(0) var<uniform> stage_params: StageParams;
@group(0) @binding(1) var source: texture_2d_array<f32>;
@group(0) @binding(2) var destination: texture_storage_2d_array<rgba32float, write>;
const TAU: f32 = 6.28318530718;
fn cmul(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> { return vec2<f32>(a.x * b.x - a.y * b.y, a.x * b.y + a.y * b.x); }
fn bit_reverse(v: u32, bits: u32) -> u32 { var x = v; var out = 0u; for (var i = 0u; i < bits; i = i + 1u) { out = (out << 1u) | (x & 1u); x = x >> 1u; } return out; }
@compute @workgroup_size(8, 8, 1)
fn fft_stage(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(destination); if (id.x >= dims.x || id.y >= dims.y || id.z >= 4u) { return; }
    let n = stage_params.data.x; let stage = stage_params.data.y; let axis = stage_params.data.z; let width = 1u << (stage + 1u); let half = width >> 1u; let output = select(id.x, id.y, axis == 1u); let base = (output / width) * width; let lane = output % width; let a_index = base + lane % half; let b_index = a_index + half;
    var a_coord = vec2<i32>(id.xy); var b_coord = a_coord; if (axis == 0u) { a_coord.x = i32(a_index); b_coord.x = i32(b_index); if (stage == 0u) { a_coord.x = i32(bit_reverse(a_index, 7u)); b_coord.x = i32(bit_reverse(b_index, 7u)); } } else { a_coord.y = i32(a_index); b_coord.y = i32(b_index); if (stage == 0u) { a_coord.y = i32(bit_reverse(a_index, 7u)); b_coord.y = i32(bit_reverse(b_index, 7u)); } }
    let a = textureLoad(source, a_coord, i32(id.z), 0); let b = textureLoad(source, b_coord, i32(id.z), 0); let w = vec2<f32>(cos(TAU * f32(lane % half) / f32(width)), sin(TAU * f32(lane % half) / f32(width))); let t0 = cmul(b.xy, w); let t1 = cmul(b.zw, w); var result = select(vec4<f32>(a.xy - t0, a.zw - t1), vec4<f32>(a.xy + t0, a.zw + t1), lane < half); if (stage_params.data.w != 0u) { result = result / f32(n * n); } textureStore(destination, vec2<i32>(id.xy), i32(id.z), result);
}
