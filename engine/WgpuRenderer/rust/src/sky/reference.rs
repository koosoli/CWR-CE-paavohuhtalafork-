// CPU reference port of the sky atmosphere math in sky.wgsl, for OBJECTIVE colour
// tests (there is no GUI in CI). It mirrors the shader's single + multi-scattering
// model and computes sky radiance directly (integrating transmittance per sample
// rather than via the LUTs) — i.e. the ground truth the GPU LUTs approximate. If the
// reference produces blue twilight, the model is right and any residual green in-game
// is LUT resolution; if the reference is green too, the model/params are wrong.
//
// f32 throughout to match the shader's precision. Keep in sync with sky.wgsl.
#![cfg(test)]

use glam::Vec3;

const MIE_EXT: f32 = 1.11;
const OZONE_ABSORPTION: Vec3 = Vec3::new(0.650e-6, 1.881e-6, 0.085e-6);

#[derive(Clone, Copy)]
pub struct Atmosphere {
    pub rayleigh: Vec3,
    pub rayleigh_h: f32,
    pub mie: f32,
    pub mie_g: f32,
    pub mie_h: f32,
    pub ground_albedo: Vec3,
    pub planet_r: f32,
    pub atmos_h: f32,
    pub ozone: f32, // strength multiplier (control.w)
    pub sun_intensity: f32,
}

impl Default for Atmosphere {
    // Mirrors WgrSky::default() (Earth-like).
    fn default() -> Self {
        Self {
            rayleigh: Vec3::new(5.8e-6, 13.5e-6, 33.1e-6),
            rayleigh_h: 8000.0,
            mie: 21e-6,
            mie_g: 0.76,
            mie_h: 1200.0,
            ground_albedo: Vec3::splat(0.1),
            planet_r: 6_360_000.0,
            atmos_h: 60_000.0,
            ozone: 1.0,
            sun_intensity: 22.0,
        }
    }
}

fn ray_sphere(origin: Vec3, dir: Vec3, radius: f32) -> (f32, f32) {
    let b = origin.dot(dir);
    let c = origin.dot(origin) - radius * radius;
    let d = b * b - c;
    if d < 0.0 {
        return (1.0, -1.0);
    }
    let s = d.sqrt();
    (-b - s, -b + s)
}

impl Atmosphere {
    fn top_r(&self) -> f32 {
        self.planet_r + self.atmos_h
    }

    fn ozone_density(&self, alt: f32) -> f32 {
        (1.0 - (alt - self.atmos_h * 0.417).abs() / (self.atmos_h * 0.25)).max(0.0)
    }

    /// Returns (rayleigh scattering, mie scattering scalar, extinction).
    fn scattering(&self, pos: Vec3) -> (Vec3, f32, Vec3) {
        let alt = pos.length() - self.planet_r;
        let rho_r = (-alt / self.rayleigh_h).exp();
        let rho_m = (-alt / self.mie_h).exp();
        let rayleigh = self.rayleigh * rho_r;
        let mie = self.mie * rho_m;
        let extinction = rayleigh
            + Vec3::splat(mie * MIE_EXT)
            + OZONE_ABSORPTION * (self.ozone_density(alt) * self.ozone);
        (rayleigh, mie, extinction)
    }

    /// Transmittance from `pos` along `dir` to space (zero if the ray hits ground).
    fn transmittance(&self, pos: Vec3, dir: Vec3) -> Vec3 {
        if ray_sphere(pos, dir, self.planet_r).0 > 0.0 {
            return Vec3::ZERO;
        }
        let t_max = ray_sphere(pos, dir, self.top_r()).1;
        let steps = 40.0;
        let (mut od_r, mut od_m, mut od_o) = (0.0f32, 0.0f32, 0.0f32);
        let mut t = 0.0f32;
        let mut i = 0.0f32;
        while i < steps {
            let new_t = ((i + 0.5) / steps) * t_max;
            let dt = new_t - t;
            t = new_t;
            let p = pos + dir * t;
            let alt = p.length() - self.planet_r;
            od_r += (-alt / self.rayleigh_h).exp() * dt;
            od_m += (-alt / self.mie_h).exp() * dt;
            od_o += self.ozone_density(alt) * dt;
            i += 1.0;
        }
        let tau = self.rayleigh * od_r
            + Vec3::splat(self.mie * MIE_EXT) * od_m
            + OZONE_ABSORPTION * od_o * self.ozone;
        (-tau).exp()
    }

    fn rayleigh_phase(cos_t: f32) -> f32 {
        3.0 / (16.0 * std::f32::consts::PI) * (1.0 + cos_t * cos_t)
    }

    fn mie_phase(&self, cos_t: f32) -> f32 {
        let g = self.mie_g;
        let num = (1.0 - g * g) * (1.0 + cos_t * cos_t);
        let den = (2.0 + g * g) * (1.0 + g * g - 2.0 * g * cos_t).powf(1.5);
        3.0 / (8.0 * std::f32::consts::PI) * num / den
    }

    /// Isotropic multiple-scattering factor Psi(pos, sun) — mirrors fs_multiscatter.
    fn multiscatter(&self, pos: Vec3, sun: Vec3) -> Vec3 {
        let sqrt_samples = 8i32;
        let inv_samples = 1.0 / (sqrt_samples * sqrt_samples) as f32;
        let mut lum_total = Vec3::ZERO;
        let mut fms_total = Vec3::ZERO;
        for i in 0..sqrt_samples {
            for j in 0..sqrt_samples {
                let u = (i as f32 + 0.5) / sqrt_samples as f32;
                let v = (j as f32 + 0.5) / sqrt_samples as f32;
                let cos_th = 1.0 - 2.0 * u;
                let sin_th = (1.0f32 - cos_th * cos_th).max(0.0).sqrt();
                let phi = 2.0 * std::f32::consts::PI * v;
                let ray = Vec3::new(sin_th * phi.cos(), cos_th, sin_th * phi.sin());

                let mut t_max = ray_sphere(pos, ray, self.top_r()).1;
                let ground = ray_sphere(pos, ray, self.planet_r);
                let hit_ground = ground.0 > 0.0;
                if hit_ground {
                    t_max = ground.0;
                }
                let cos_t = ray.dot(sun);
                let rp = Self::rayleigh_phase(cos_t);
                let mp = self.mie_phase(cos_t);

                let mut lum = Vec3::ZERO;
                let mut lum_factor = Vec3::ZERO;
                let mut trans = Vec3::ONE;
                let mut t = 0.0f32;
                let steps = 20.0f32;
                let mut s = 0.0f32;
                while s < steps {
                    let new_t = ((s + 0.5) / steps) * t_max;
                    let dt = new_t - t;
                    t = new_t;
                    let p = pos + ray * t;
                    let (rayleigh, mie, ext) = self.scattering(p);
                    let safe_ext = ext.max(Vec3::splat(1e-9));
                    let sample_trans = (-ext * dt).exp();
                    let scat_no_phase = rayleigh + Vec3::splat(mie);
                    let scat_f = (scat_no_phase - scat_no_phase * sample_trans) / safe_ext;
                    lum_factor += trans * scat_f;
                    let sun_t = self.transmittance(p, sun);
                    let in_scat = (rayleigh * rp + Vec3::splat(mie) * mp) * sun_t;
                    let scat_int = (in_scat - in_scat * sample_trans) / safe_ext;
                    lum += scat_int * trans;
                    trans *= sample_trans;
                    s += 1.0;
                }
                if hit_ground {
                    let hit_p = pos + ray * t_max;
                    let up = hit_p.normalize();
                    let ndl = up.dot(sun).max(0.0);
                    lum += trans * self.ground_albedo * ndl * self.transmittance(hit_p, sun)
                        / std::f32::consts::PI;
                }
                lum_total += lum * inv_samples;
                fms_total += lum_factor * inv_samples;
            }
        }
        lum_total / (Vec3::ONE - fms_total).max(Vec3::splat(1e-3))
    }

    fn raymarch(&self, pos: Vec3, ray: Vec3, sun: Vec3, t_max: f32) -> Vec3 {
        let cos_t = ray.dot(sun);
        let rp = Self::rayleigh_phase(cos_t);
        let mp = self.mie_phase(cos_t);
        let steps = 32.0f32;
        let mut lum = Vec3::ZERO;
        let mut trans = Vec3::ONE;
        let mut t = 0.0f32;
        let mut i = 0.0f32;
        while i < steps {
            let new_t = ((i + 0.3) / steps) * t_max;
            let dt = new_t - t;
            t = new_t;
            let p = pos + ray * t;
            let (rayleigh, mie, ext) = self.scattering(p);
            let safe_ext = ext.max(Vec3::splat(1e-9));
            let sample_trans = (-ext * dt).exp();
            let sun_t = self.transmittance(p, sun);
            let psi = self.multiscatter(p, sun);
            let rayleigh_in = rayleigh * (rp * sun_t + psi);
            let mie_in = Vec3::splat(mie) * (mp * sun_t + psi);
            let in_scat = rayleigh_in + mie_in;
            let scat_int = (in_scat - in_scat * sample_trans) / safe_ext;
            lum += scat_int * trans;
            trans *= sample_trans;
            i += 1.0;
        }
        lum
    }

    /// Sky radiance (linear, pre-exposure) for a world view direction and sun
    /// direction (both y-up unit vectors; sun_dir points TOWARD the sun).
    pub fn radiance(&self, view_dir: Vec3, sun_dir: Vec3) -> Vec3 {
        let pos = Vec3::new(0.0, self.planet_r + 200.0, 0.0);
        let march_dir = Vec3::new(view_dir.x, view_dir.y.max(0.0), view_dir.z).normalize();
        let atmo = ray_sphere(pos, march_dir, self.top_r());
        if atmo.1 <= 0.0 {
            return Vec3::ZERO;
        }
        self.raymarch(pos, march_dir, sun_dir, atmo.1) * self.sun_intensity
    }
}

// Sun/view helpers: elevation above horizon (deg), azimuth from +x toward +z (deg).
fn dir_from(elev_deg: f32, az_deg: f32) -> Vec3 {
    let e = elev_deg.to_radians();
    let a = az_deg.to_radians();
    Vec3::new(e.cos() * a.cos(), e.sin(), e.cos() * a.sin()).normalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bg_ratio(c: Vec3) -> f32 {
        c.z / (c.y + 1e-12)
    }

    #[test]
    fn daytime_zenith_is_blue() {
        let a = Atmosphere::default();
        let sun = dir_from(70.0, 0.0);
        let c = a.radiance(Vec3::Y, sun);
        println!(
            "daytime zenith rgb = {c:?}  (B/R = {:.3})",
            c.z / (c.x + 1e-12)
        );
        assert!(
            c.z > c.x,
            "daytime zenith should be blue-dominant (B>R): {c:?}"
        );
    }

    #[test]
    fn sunset_toward_sun_is_reddened() {
        let a = Atmosphere::default();
        let sun = dir_from(1.0, 0.0);
        // Look just above the horizon toward the sun.
        let view = dir_from(3.0, 0.0);
        let c = a.radiance(view, sun);
        println!(
            "sunset toward-sun rgb = {c:?}  (R/B = {:.3})",
            c.x / (c.z + 1e-12)
        );
        assert!(
            c.x > c.z,
            "sunset toward the sun should be red-dominant (R>B): {c:?}"
        );
    }

    #[test]
    fn twilight_zenith_ozone_beats_green() {
        // Sun 4 deg below the horizon: the blue hour.
        let sun = dir_from(-4.0, 0.0);
        let with_oz = Atmosphere::default();
        let no_oz = Atmosphere {
            ozone: 0.0,
            ..Atmosphere::default()
        };
        let c_oz = with_oz.radiance(Vec3::Y, sun);
        let c_no = no_oz.radiance(Vec3::Y, sun);
        println!(
            "twilight zenith  ozone=1: {c_oz:?}  B/G={:.3}",
            bg_ratio(c_oz)
        );
        println!(
            "twilight zenith  ozone=0: {c_no:?}  B/G={:.3}",
            bg_ratio(c_no)
        );
        // Objective: ozone must raise the blue-to-green ratio (undo the green cast).
        assert!(
            bg_ratio(c_oz) > bg_ratio(c_no),
            "ozone should raise B/G at twilight: with={:.3} without={:.3}",
            bg_ratio(c_oz),
            bg_ratio(c_no)
        );
    }

    #[test]
    #[ignore = "aspirational target: model does not yet produce a blue hour (see elevation_sweep_data)"]
    fn twilight_zenith_is_actually_blue() {
        // The real target: with ozone, twilight zenith should be blue-dominant, not green.
        let a = Atmosphere::default();
        let sun = dir_from(-4.0, 0.0);
        let c = a.radiance(Vec3::Y, sun);
        assert!(
            c.z > c.y,
            "twilight zenith should be blue-dominant (B>G): {c:?}"
        );
    }

    #[test]
    fn elevation_sweep_data() {
        // How zenith radiance + colour evolve through sunset. Reveals whether the
        // twilight collapse is a smooth physical dimming or a cliff, and where blue
        // dies. Magnitude vs the daytime ~0.5 shows how dark twilight gets.
        let a = Atmosphere::default();
        println!("elev   R          G          B          lum        B/G     B/R");
        for elev in [20.0f32, 10.0, 5.0, 2.0, 0.0, -1.0, -2.0, -4.0, -6.0] {
            let sun = dir_from(elev, 0.0);
            let c = a.radiance(Vec3::Y, sun);
            let lum = 0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z;
            println!(
                "{elev:5.1}  {:.3e}  {:.3e}  {:.3e}  {:.3e}  {:.3}  {:.3}",
                c.x,
                c.y,
                c.z,
                lum,
                c.z / (c.y + 1e-30),
                c.z / (c.x + 1e-30)
            );
        }
    }

    #[test]
    fn ozone_strength_sweep_data() {
        // Print a sweep so we can read off how much ozone the blue hour needs.
        let sun = dir_from(-4.0, 0.0);
        for oz in [0.0f32, 0.5, 1.0, 2.0, 3.0, 4.0] {
            let a = Atmosphere {
                ozone: oz,
                ..Atmosphere::default()
            };
            let c = a.radiance(Vec3::Y, sun);
            println!(
                "ozone={oz:.1}  zenith={c:?}  B/G={:.3}  B/R={:.3}",
                c.z / (c.y + 1e-12),
                c.z / (c.x + 1e-12)
            );
        }
    }
}
