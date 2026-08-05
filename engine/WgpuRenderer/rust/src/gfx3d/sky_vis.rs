// Interior sky visibility, Stage 1 (docs/interior-sky-visibility-plan.md §4).
//
// A top-down orthographic DEPTH map of the retained object set over a world box around the
// camera. A fragment whose depth in that map sits BELOW the stored occluder depth has geometry
// above it — it is indoors, or under a porch/canopy — and its SKY AMBIENT is attenuated toward a
// floor. Direct sun and local lights are never touched: sun occlusion is already the cascade
// shadow maps' and the terrain sun-shadow mask's job, and the gap this closes is the ambient.
//
// Two things this module is responsible for getting right, both from the plan's risk list:
//
//   * The ortho origin is SNAPPED to the map's texel size. An unsnapped view resamples the world
//     every frame and the result crawls; there is no TAA here to hide it (which is precisely why
//     the GTAO mip march ended up default-off).
//   * Terrain is not in this map at all — it is not part of the retained instance set the sky cull
//     runs over. That is deliberate: terrain is never "above" the player, and including it would
//     make every hillside a roof.
//
// Stage 1 is ONE direction, straight down. The window criterion ("a room next to a window receives
// light") is structurally out of reach here and belongs to the Stage 2 per-model bake — see §3c.

use crate::ffi::WgrSkyVis;

/// Runtime knobs. Default OFF: this changes global lighting, so it opts in.
#[derive(Clone, Copy, Debug)]
pub struct SkyVisSettings {
    pub enabled: bool,
    /// Depth-map resolution (square). 1024 over a 256 m box is 25 cm/texel.
    pub resolution: u32,
    /// HALF the world extent covered, in metres (the box is 2*extent on a side).
    pub extent: f32,
    /// How far above / below the camera the ortho box reaches, in metres. Must clear the
    /// tallest roof the player can stand under, plus the deepest floor they can stand on.
    pub height: f32,
    /// Blend toward the occluded result, 0 = feature inert, 1 = full effect.
    pub strength: f32,
    /// Minimum ambient multiplier in a fully enclosed volume. OFP interiors carry very few
    /// local lights, so a hard 0 here is an unplayable black box — see the plan's risk list.
    pub floor: f32,
    /// Softening kernel radius in METRES. Roughly how far light appears to reach in past an
    /// opening: under the middle of a roof every tap is blocked, at its edge some see sky.
    pub kernel: f32,
    /// Depth bias in metres, subtracted from the receiver's depth before the comparison, so a
    /// surface that is its OWN highest geometry (open ground, a crate in the street) does not
    /// shadow itself.
    pub bias: f32,
    /// 1 = draw the reach factor as greyscale instead of lighting with it. Shipped WITH the
    /// effect, not after it: judging this through sun + SH ambient + fog + tonemap is much
    /// harder than looking at the buffer.
    pub debug: bool,
}

impl Default for SkyVisSettings {
    fn default() -> Self {
        SkyVisSettings {
            enabled: false,
            resolution: 1024,
            extent: 128.0,
            height: 300.0,
            strength: 1.0,
            floor: 0.35,
            kernel: 1.5,
            bias: 0.25,
            debug: false,
        }
    }
}

impl SkyVisSettings {
    /// Apply the C++-pushed knobs. Mirrors the GTAO settings path: the layer furthest from the
    /// renderer wins, so a renderer-side default is only ever the pre-push value.
    pub fn apply(&mut self, p: &WgrSkyVis) {
        self.enabled = p.enabled != 0;
        self.resolution = p.resolution.clamp(256, 4096);
        self.extent = p.extent.clamp(16.0, 2048.0);
        self.height = p.height.clamp(16.0, 4096.0);
        self.strength = p.strength.clamp(0.0, 1.0);
        self.floor = p.floor.clamp(0.0, 1.0);
        self.kernel = p.kernel.clamp(0.0, 32.0);
        self.bias = p.bias.clamp(0.0, 16.0);
        self.debug = p.debug != 0;
    }

    /// World metres per depth-map texel.
    pub fn texel_size(&self) -> f32 {
        2.0 * self.extent / self.resolution.max(1) as f32
    }
}

/// The ortho view-projection for this frame, plus the shader-side sampling constants derived
/// from it. Absolute world space in, clip space out (w = 1; the projection is orthographic).
#[derive(Clone, Copy, Debug)]
pub struct SkyVisView {
    pub view_proj: glam::Mat4,
    /// Softening kernel radius expressed in UV units (the shader offsets taps in UV).
    pub kernel_uv: f32,
    /// Depth bias in NDC depth units (the box spans 2*height metres over 0..1).
    pub bias_ndc: f32,
}

/// Build the snapped top-down ortho view for a camera position.
///
/// SNAPPING is the whole point of doing this in one place: the eye's X/Z are quantised to the
/// map's texel size so that as the camera walks, the world falls on the SAME texel centres and
/// the map's contents are stable. Y is quantised to a metre for the same reason at a scale where
/// it matters far less (a Y shift moves the stored depth and the receiver's reference depth
/// together, so the comparison is nearly invariant to it — but the depth quantisation is not).
///
/// The view looks straight down (-Y) with +Z as "up" in view space, so the view's X/Y axes are
/// world X/Z and quantising world X/Z is exactly quantising the texel grid.
pub fn build_view(cam_pos: glam::Vec3, s: &SkyVisSettings) -> SkyVisView {
    let texel = s.texel_size().max(1e-4);
    let snap = |v: f32, q: f32| (v / q).floor() * q;
    let eye = glam::Vec3::new(
        snap(cam_pos.x, texel),
        snap(cam_pos.y, 1.0) + s.height,
        snap(cam_pos.z, texel),
    );
    // World -> view for an eye looking straight down. Written out rather than via a look_at
    // helper so the axis mapping is inspectable:
    //     view.x = world.x - eye.x        (view right  = world +X)
    //     view.y = eye.z - world.z        (view up     = world -Z)
    //     view.z = world.y - eye.y        (the camera looks down its own -Z, right-handed, so
    //                                      points BELOW the eye get negative view z)
    // view up is world -Z rather than +Z so the rotation is a proper one (det +1) instead of a
    // reflection — a mirrored view would flip triangle winding under any pipeline that culled.
    let view = glam::Mat4::from_cols(
        glam::Vec4::new(1.0, 0.0, 0.0, 0.0),
        glam::Vec4::new(0.0, 0.0, 1.0, 0.0),
        glam::Vec4::new(0.0, -1.0, 0.0, 0.0),
        glam::Vec4::new(-eye.x, eye.z, -eye.y, 1.0),
    );
    // Depth 0 at the top of the box, 1 at the bottom: a receiver under a roof has a LARGER
    // depth than the roof, which is what the LessEqual comparison sampler tests.
    // "directx" is glam's name for the NDC convention wgpu uses: Z in [0, 1], Y-up.
    let proj = glam::camera::rh::proj::directx::orthographic(
        -s.extent,
        s.extent,
        -s.extent,
        s.extent,
        0.0,
        2.0 * s.height,
    );
    SkyVisView {
        view_proj: proj * view,
        // UV spans the full box: 2*extent metres across 1.0 of UV.
        kernel_uv: s.kernel / (2.0 * s.extent).max(1e-4),
        bias_ndc: s.bias / (2.0 * s.height).max(1e-4),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(v: &SkyVisView, p: glam::Vec3) -> glam::Vec3 {
        let clip = v.view_proj * p.extend(1.0);
        // Orthographic: w == 1, so clip IS ndc.
        glam::Vec3::new(
            clip.x * 0.5 + 0.5,
            -clip.y * 0.5 + 0.5, // wgpu UV has +V down
            clip.z,
        )
    }

    // The receiver-below-occluder relationship the whole feature rests on: a point lower in the
    // world must land at a GREATER depth, so the LessEqual comparison reads it as occluded.
    #[test]
    fn lower_points_have_greater_depth() {
        let s = SkyVisSettings::default();
        let v = build_view(glam::Vec3::ZERO, &s);
        let roof = project(&v, glam::Vec3::new(0.0, 6.0, 0.0));
        let floor = project(&v, glam::Vec3::new(0.0, 0.0, 0.0));
        assert!(
            floor.z > roof.z,
            "floor depth {} must exceed roof depth {}",
            floor.z,
            roof.z
        );
        // ...and both must be inside the box's depth range, or the comparison is meaningless.
        assert!((0.0..=1.0).contains(&roof.z) && (0.0..=1.0).contains(&floor.z));
    }

    // A point directly under the camera lands at the middle of the map, and the box covers
    // exactly ±extent — the mapping the shader's UV derivation assumes.
    #[test]
    fn box_covers_extent_around_camera() {
        let s = SkyVisSettings::default();
        let cam = glam::Vec3::new(1000.0, 20.0, -2000.0);
        let v = build_view(cam, &s);
        let under = project(&v, glam::Vec3::new(cam.x, 0.0, cam.z));
        // Snapping moves the centre by at most one texel, which is well under half a percent
        // of the box.
        assert!((under.x - 0.5).abs() < 0.01, "u = {}", under.x);
        assert!((under.y - 0.5).abs() < 0.01, "v = {}", under.y);
        let edge = project(&v, glam::Vec3::new(cam.x + s.extent * 0.98, 0.0, cam.z));
        assert!(edge.x > 0.9 && edge.x < 1.0, "edge u = {}", edge.x);
    }

    // The anti-crawl guarantee, stated as a test: sub-texel camera motion must not move the
    // world's image in the map AT ALL. This is the property the plan calls non-negotiable, and
    // it is the one that silently regresses if someone "simplifies" the snap away.
    #[test]
    fn subtexel_camera_motion_does_not_move_the_map() {
        let s = SkyVisSettings::default();
        let texel = s.texel_size();
        // BOTH positions inside the SAME texel cell — the claim is that motion which does not
        // cross a cell boundary changes nothing. (Stepping to -0.3 * texel would land in the
        // cell below, which legitimately moves the map by one texel; see the next test.)
        let a = build_view(glam::Vec3::new(texel * 0.1, 10.0, texel * 0.2), &s);
        let b = build_view(glam::Vec3::new(texel * 0.9, 10.4, texel * 0.8), &s);
        let p = glam::Vec3::new(7.0, 3.0, -11.0);
        let (pa, pb) = (project(&a, p), project(&b, p));
        assert!((pa.x - pb.x).abs() < 1e-6, "u moved: {} -> {}", pa.x, pb.x);
        assert!((pa.y - pb.y).abs() < 1e-6, "v moved: {} -> {}", pa.y, pb.y);
    }

    // Crossing a texel boundary moves the image by EXACTLY one texel, never a fraction — the
    // other half of the snap contract (a snap that quantised to the wrong grid would still pass
    // the test above while smearing here).
    #[test]
    fn texel_crossing_moves_the_map_by_one_texel() {
        let s = SkyVisSettings::default();
        let texel = s.texel_size();
        let a = build_view(glam::Vec3::ZERO, &s);
        let b = build_view(glam::Vec3::new(texel, 0.0, 0.0), &s);
        let p = glam::Vec3::new(7.0, 3.0, -11.0);
        let du = project(&a, p).x - project(&b, p).x;
        let expect = texel / (2.0 * s.extent);
        assert!((du - expect).abs() < 1e-6, "du = {du}, expected {expect}");
    }

    // The kernel is authored in metres but consumed in UV; a wrong conversion here is invisible
    // (it just looks like the wrong softness) which is exactly why it is pinned.
    #[test]
    fn kernel_converts_metres_to_uv() {
        let mut s = SkyVisSettings::default();
        s.kernel = 2.0;
        s.extent = 100.0;
        let v = build_view(glam::Vec3::ZERO, &s);
        assert!((v.kernel_uv - 0.01).abs() < 1e-6, "{}", v.kernel_uv);
    }

    // Bias is authored in metres too, against the box's FULL vertical span.
    #[test]
    fn bias_converts_metres_to_ndc_depth() {
        let mut s = SkyVisSettings::default();
        s.bias = 1.0;
        s.height = 250.0;
        let v = build_view(glam::Vec3::ZERO, &s);
        assert!((v.bias_ndc - 1.0 / 500.0).abs() < 1e-9, "{}", v.bias_ndc);
    }
}
