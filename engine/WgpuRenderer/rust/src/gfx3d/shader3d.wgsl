// Unlit textured 3D, plain (vs_main) and GPU-skinned (vs_skinned) variants
// sharing fs_main. The two group(1) declarations coexist because each entry
// point statically uses only one (binding collisions are validated per entry
// point). The cascade shadow kernel is a port of the GL33 lit shader, which
// mirrors the unit-tested ShadowMath::SampleShadow reference.

struct FrameParams {
    fog_start: f32,
    fog_inv_range: f32,
    fog_enabled: f32, // 0 = off, 1 = on
    shadow_strength: f32,
};

struct ShadowBlock {
    cascade_vp: array<mat4x4<f32>, 4>,
    splits: vec4<f32>, // per-tier select distance (omni radius / frustum eye-depth)
    omni_radius: vec4<f32>,
    ctl: vec4<f32>,  // {count, omni_count, fade_range, bias_const}
    ctl2: vec4<f32>, // {texel_size, darkness, normal_offset_scale, pcf}
    cam_fwd: vec4<f32>,
    sun_dir: vec4<f32>,
};

struct Frame {
    proj: mat4x4<f32>,
    view: mat4x4<f32>,
    fog_color: vec4<f32>,
    params: FrameParams,
    shadow: ShadowBlock,
    cam_pos: vec4<f32>, // world-space camera position (used by the terrain pipeline)
};

struct Object {
    world: mat4x4<f32>,
};

// 128 = the engine's own bone-palette cap (MATRIX_4_ARRAY(matrix, 128)). The
// palette has the per-object world pre-multiplied in (palette[i] = world * bone[i]).
struct Palette {
    m: array<mat4x4<f32>, 128>,
};

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var shadow_map: texture_depth_2d_array;
@group(0) @binding(2) var shadow_samp: sampler_comparison;
@group(1) @binding(0) var<uniform> object: Object;   // plain pipeline
@group(1) @binding(0) var<uniform> palette: Palette; // skinned pipeline
@group(2) @binding(0) var tex: texture_2d<f32>;
@group(3) @binding(0) var samp: sampler;

// Baked per-pipeline (pipeline-overridable constants): the alpha-test cutout
// threshold and whether this is a shadow-darken pipeline. Keeping them out of a
// per-draw binding avoids a 5th bind group (wgpu's default maxBindGroups is 4).
override alpha_ref: f32 = 0.0;   // discard fragments with alpha below this (0 = off)
override is_shadow: f32 = 0.0;   // 1 = output black + shadow-strength alpha
// Decal/overlay depth bias in reversed-NDC depth units, pulling the draw toward the
// camera. Applied in the vertex shader (not DepthBiasState, which is a no-op on the
// float depth format this backend gets) so roads/decals/overlays win the depth test
// against coplanar geometry.
override depth_bias: f32 = 0.0;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) fog: f32,             // 1 = keep colour, 0 = full fog
    @location(2) world_pos: vec3<f32>, // camera-relative
    @location(3) normal: vec3<f32>,    // world space, outward
};

// world_pos is camera-relative (the world matrix / palette is offset by the
// camera position on the C++ side), so its length is the camera distance.
fn finish_vertex(world_pos: vec4<f32>, normal_ws: vec3<f32>, uv: vec2<f32>) -> VsOut {
    var out: VsOut;
    out.clip = frame.proj * frame.view * world_pos;
    // Reversed-Z: the shared projection is forward (near->0, far->1). Remap to
    // near->1, far->0 so the float depth buffer spends its exponent bits where
    // geometry actually is (far from 0), which massively improves precision at
    // range vs forward float depth. Pipelines use GreaterEqual + clear-to-0.
    out.clip.z = out.clip.w - out.clip.z;
    // Bias toward the camera (larger reversed depth) so decals/overlays win the
    // depth test against coplanar geometry.
    out.clip.z += depth_bias * out.clip.w;
    out.uv = uv;
    out.world_pos = world_pos.xyz;
    out.normal = normal_ws;

    let dist = length(world_pos.xyz);
    let fog_factor = clamp(1.0 - (dist - frame.params.fog_start) * frame.params.fog_inv_range, 0.0, 1.0);
    out.fog = select(1.0, fog_factor, frame.params.fog_enabled > 0.5);
    return out;
}

@vertex
fn vs_main(
    @location(0) pos: vec3<f32>,
    @location(1) norm: vec3<f32>,
    @location(2) uv: vec2<f32>,
) -> VsOut {
    let world_pos = object.world * vec4<f32>(pos, 1.0);
    // Vertex normals are uploaded negated (D3D convention); un-negate for the
    // outward world normal.
    let rot = mat3x3<f32>(object.world[0].xyz, object.world[1].xyz, object.world[2].xyz);
    return finish_vertex(world_pos, -(rot * norm), uv);
}

// Linear-blend skinning: up to 4 bone indices + normalised weights. Vertices
// with no skin weight carry a single weight of 1.0 on a reserved bone whose
// palette entry is just `world`, so no zero-weight fallback is needed.
@vertex
fn vs_skinned(
    @location(0) pos: vec3<f32>,
    @location(1) norm: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) bones: vec4<u32>,   // Uint8x4: palette indices
    @location(4) weights: vec4<f32>, // Unorm8x4: normalised weights
) -> VsOut {
    let p = vec4<f32>(pos, 1.0);
    let world_pos = weights.x * (palette.m[bones.x] * p)
                  + weights.y * (palette.m[bones.y] * p)
                  + weights.z * (palette.m[bones.z] * p)
                  + weights.w * (palette.m[bones.w] * p);
    let n = vec4<f32>(norm, 0.0);
    let normal_ws = weights.x * (palette.m[bones.x] * n).xyz
                  + weights.y * (palette.m[bones.y] * n).xyz
                  + weights.z * (palette.m[bones.z] * n).xyz
                  + weights.w * (palette.m[bones.w] * n).xyz;
    return finish_vertex(world_pos, -normal_ws, uv);
}

// Cascaded shadow strength in [0,1] for a camera-relative position (0 = lit).
// Tier select (omni by 3D distance, frustum by eye-depth) with coverage
// fallthrough, cross-tier blend band, HW-PCF compare, far fade, fog dimming.
// dwx/dwy are screen-space derivatives of world_pos (receiver-plane bias).
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

    // Normal-offset receiver bias (ShadowMath::ShadowBias): push the receiver
    // along its normal by ~2 world-texels * sin(angle to the light).
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
        // p0 = primary, p1 = blend partner; while nothing has covered yet a
        // later p force-samples the next looser tier (coverage fallthrough).
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
        // World metres per texel from the ortho x row (unit rotation * 2/width).
        let sx = max(length(vec3<f32>(vp[0][0], vp[1][0], vp[2][0])), 1e-6);
        let texel_world = 2.0 * ts / sx;
        let offset = frame.shadow.ctl2.z * 2.0 * texel_world * sin_t;

        let cp = vp * vec4<f32>(world_pos + normal_ws * offset, 1.0);
        let sc = cp.xyz / cp.w;
        // wgpu texture v runs top-down (vs GL's bottom-up in the GL33 kernel).
        let suv = vec2<f32>(sc.x * 0.5 + 0.5, 0.5 - sc.y * 0.5);
        if (suv.x > 0.0 && suv.x < 1.0 && suv.y > 0.0 && suv.y < 1.0 && sc.z > 0.0 && sc.z < 1.0) {
            // Receiver-plane depth bias (Isidoro): the receiver's light-space
            // depth gradient over shadow UV. Each comparison then tests against
            // the plane's exact depth at that tap, which kills the texel-
            // quantisation acne bands on surfaces nearly parallel to the sun —
            // an error no constant/slope knob can bound.
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
            // Near-grazing the solve goes singular and the gradient explodes;
            // cap the depth change per texel so the taps stay sane.
            let lim = 0.02 / max(ts, 1e-6);
            dz_duv = clamp(dz_duv, vec2<f32>(-lim, -lim), vec2<f32>(lim, lim));
            // Cover the up-to-one-texel error of the bilinear comparison footprint.
            let plane_bias = min(2.0 * ts * (abs(dz_duv.x) + abs(dz_duv.y)), 0.01);
            let bias = frame.shadow.ctl.w * f32(c + 1) * f32(c + 1);
            let ref_z = sc.z - bias - plane_bias;
            var lit: f32;
            let pcf = frame.shadow.ctl2.w;
            if (pcf >= 0.5) {
                // 3x3 tent, plane-corrected per tap: smooths the texel-quantised
                // silhouette teeth that grazing light stretches across receivers.
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

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Derivatives must run in uniform control flow — before the discard.
    let dwx = dpdx(in.world_pos);
    let dwy = dpdy(in.world_pos);
    let base = textureSample(tex, samp, in.uv);
    if (base.a < alpha_ref) {
        discard;
    }
    if (is_shadow > 0.5) {
        return vec4<f32>(0.0, 0.0, 0.0, frame.params.shadow_strength * base.a);
    }
    var rgb = base.rgb;
    let s = shadow_strength(in.world_pos, normalize(in.normal), in.fog, dwx, dwy);
    rgb *= mix(1.0, frame.shadow.ctl2.y, s);
    // Blend toward the scene fog colour (matches GL33's mix(fogColor, r0, vFogTC)).
    rgb = mix(frame.fog_color.rgb, rgb, in.fog);
    return vec4<f32>(rgb, base.a);
}
