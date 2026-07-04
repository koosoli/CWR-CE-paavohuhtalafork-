#define_import_path lighting

#import frame::frame

// Sun lighting matching GL33's lit path: diffuse * N.L + ambient (eye
// accommodation folded in on the CPU), saturated like the vertex-colour pack it
// replaces. sun_dir_world is the light's travel direction (GL33's sunDir
// constant, negated against the true up normal exactly as GL33's vertex shader
// does); at night/dawn it points at or up through the horizon, so level ground
// falls back to ambient. Not the shadow block's sun_dir, which is only valid
// while the cascade pass runs.
fn sun_light(normal_ws: vec3<f32>) -> vec3<f32> {
    let cos_fi = max(dot(normal_ws, -frame.sun_dir_world.xyz), 0.0);
    return min(frame.sun_diffuse.rgb * cos_fi + frame.sun_ambient.rgb, vec3<f32>(1.0));
}
