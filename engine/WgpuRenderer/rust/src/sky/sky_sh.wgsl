// Projects the sky reflection env map (equirect, linear radiance) into 9 spherical-harmonic RGB
// coefficients for diffuse sky irradiance (object + terrain ambient — water look plan Stage 4a
// follow-up). Once per frame, right after the env bake. One workgroup / one thread integrates
// radiance * Y_i * dOmega over a coarse grid of the env: SH-9 is very low frequency, so a subset
// suffices and a single serial pass is simplest + trivially correct (the pass is tiny). The lit
// shaders evaluate irradiance(n) from these coeffs — see frame::sky_irradiance.

@group(0) @binding(0) var env: texture_2d<f32>;

struct Sh {
    c: array<vec4<f32>, 9>,
};
@group(0) @binding(1) var<storage, read_write> sh: Sh;

const PI: f32 = 3.14159265359;

@compute @workgroup_size(1)
fn cs_sky_sh() {
    let dims = vec2<i32>(textureDimensions(env));
    let w = dims.x;
    let h = dims.y;
    // Coarse integration grid (cap the sample count; SH-9 needs no more).
    let sx = max(w / 128, 1);
    let sy = max(h / 64, 1);
    let dphi = 2.0 * PI / f32(w);
    let dtheta = PI / f32(h);

    var c: array<vec3<f32>, 9>;
    for (var i = 0; i < 9; i = i + 1) {
        c[i] = vec3<f32>(0.0);
    }

    var y = 0;
    loop {
        if (y >= h) { break; }
        // Texel-centre polar angle (v = 0 at zenith .. 1 at nadir), matching fs_sky_env.
        let v = (f32(y) + 0.5) / f32(h);
        let polar = v * PI;
        let sp = sin(polar);
        let cp = cos(polar);
        // Solid angle of the (sx x sy) cell this sample represents.
        let domega = sp * dtheta * dphi * f32(sx) * f32(sy);
        var x = 0;
        loop {
            if (x >= w) { break; }
            let u = (f32(x) + 0.5) / f32(w);
            let azimuth = (u - 0.5) * 2.0 * PI;
            let dir = vec3<f32>(sp * cos(azimuth), cp, sp * sin(azimuth));
            let rad = textureLoad(env, vec2<i32>(x, y), 0).rgb;
            let wr = rad * domega;
            c[0] = c[0] + wr * 0.282095;
            c[1] = c[1] + wr * 0.488603 * dir.y;
            c[2] = c[2] + wr * 0.488603 * dir.z;
            c[3] = c[3] + wr * 0.488603 * dir.x;
            c[4] = c[4] + wr * 1.092548 * dir.x * dir.y;
            c[5] = c[5] + wr * 1.092548 * dir.y * dir.z;
            c[6] = c[6] + wr * 0.315392 * (3.0 * dir.z * dir.z - 1.0);
            c[7] = c[7] + wr * 1.092548 * dir.x * dir.z;
            c[8] = c[8] + wr * 0.546274 * (dir.x * dir.x - dir.y * dir.y);
            x = x + sx;
        }
        y = y + sy;
    }

    for (var i = 0; i < 9; i = i + 1) {
        sh.c[i] = vec4<f32>(c[i], 0.0);
    }
}
