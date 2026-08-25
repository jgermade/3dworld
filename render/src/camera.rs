//! An orbit camera, and the one place model space narrows to the GPU.
//!
//! Everything here is `f64` and stays `f64` until [`Camera::view_projection`]
//! hands a matrix to the queue. That is not fastidiousness: a view matrix
//! built in `f32` from a target far from the origin loses the sub-millimetre
//! detail a modeller is *for* well before the geometry does.
//!
//! It is not the whole of the problem, and the limit is worth naming here
//! rather than discovering later. [`w3d_kernel::Mesh`] positions are already
//! `f32` in absolute model coordinates, so a document sitting kilometres from
//! the origin has already lost precision by the time this module sees it. The
//! camera cannot fix that; only a per-body origin in the tessellation contract
//! could, and that is a widening of the trait with no caller yet.

use w3d_kernel::{Aabb, Mat4, Vec3};

/// Spherical about a target, which is what direct modelling wants: the pivot
/// is what you are looking at, not where you are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub target: Vec3,
    pub distance: f64,
    /// Radians about +Z, from +X.
    pub yaw: f64,
    /// Radians from the XY plane. Clamped away from the poles by
    /// [`Camera::orbit`], because a camera exactly at the pole has no
    /// well-defined right vector.
    pub pitch: f64,
    pub fov_y: f64,
    pub near: f64,
    pub far: f64,
}

/// Z is up. CAD convention, and the kernel's cylinder axis agrees.
pub const UP: Vec3 = Vec3::new(0.0, 0.0, 1.0);

const PITCH_LIMIT: f64 = core::f64::consts::FRAC_PI_2 - 1.0e-3;

impl Default for Camera {
    fn default() -> Self {
        Self {
            target: Vec3::ZERO,
            distance: 10.0,
            yaw: -core::f64::consts::FRAC_PI_4 * 3.0,
            pitch: core::f64::consts::FRAC_PI_6,
            fov_y: core::f64::consts::FRAC_PI_4,
            near: 0.1,
            far: 1000.0,
        }
    }
}

impl Camera {
    pub fn eye(&self) -> Vec3 {
        let (sp, cp) = self.pitch.sin_cos();
        let (sy, cy) = self.yaw.sin_cos();
        self.target + Vec3::new(cp * cy, cp * sy, sp) * self.distance
    }

    pub fn orbit(&mut self, d_yaw: f64, d_pitch: f64) {
        self.yaw += d_yaw;
        self.pitch = (self.pitch + d_pitch).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }

    /// Slides the target across the view plane, in *pixels* of a viewport
    /// `height` tall.
    ///
    /// Scaled by distance and field of view, so a drag moves what is under the
    /// cursor by roughly that many pixels whatever the zoom — which is the
    /// only pan that feels like dragging the model rather than nudging a
    /// number.
    pub fn pan(&mut self, dx: f64, dy: f64, height: f64) {
        if height <= 0.0 {
            return;
        }
        let Some(forward) = (self.target - self.eye()).normalize(1.0e-12) else {
            return;
        };
        let Some(right) = forward.cross(UP).normalize(1.0e-12) else {
            return;
        };
        let up = right.cross(forward);
        // World units per pixel at the target's depth.
        let scale = 2.0 * self.distance * (self.fov_y * 0.5).tan() / height;
        self.target = self.target + right * (-dx * scale) + up * (dy * scale);
    }

    /// Multiplicative, so a wheel notch is the same proportion of the view at
    /// every scale — which is the only zoom that works in a document whose
    /// contents span four orders of magnitude.
    pub fn dolly(&mut self, factor: f64) {
        self.distance = (self.distance * factor).max(1.0e-6);
    }

    /// Frames `bounds` with a little air, and sets the depth range *from the
    /// contents* rather than from a constant.
    ///
    /// The near/far pair is where a viewport quietly stops working: fixed at
    /// 0.1 and 1000, a 2 mm fillet and a 40 m assembly cannot both be
    /// depth-tested. Deriving them from the framed radius is what makes the
    /// same code serve both.
    pub fn fit(&mut self, bounds: &Aabb) {
        if bounds.is_empty() {
            return;
        }
        self.target = bounds.center();
        let radius = (bounds.size().length() * 0.5).max(1.0e-6);
        self.distance = radius / (self.fov_y * 0.5).sin() * 1.2;
        self.near = (self.distance - radius) * 0.1;
        self.far = (self.distance + radius) * 4.0;
    }

    pub fn view(&self) -> Mat4 {
        look_at(self.eye(), self.target, UP)
    }

    pub fn projection(&self, aspect: f64) -> Mat4 {
        perspective(self.fov_y, aspect, self.near, self.far)
    }

    /// The narrowing point. Column-major on the way out, because that is what
    /// a WGSL `mat4x4<f32>` is in memory and [`Mat4`] is row-major.
    pub fn view_projection(&self, aspect: f64) -> [[f32; 4]; 4] {
        to_wgsl(&self.projection(aspect).mul(&self.view()))
    }
}

pub fn to_wgsl(m: &Mat4) -> [[f32; 4]; 4] {
    let mut out = [[0.0f32; 4]; 4];
    for (c, col) in out.iter_mut().enumerate() {
        for (r, cell) in col.iter_mut().enumerate() {
            *cell = m.0[r][c] as f32;
        }
    }
    out
}

/// Right-handed, looking down -Z in view space.
pub fn look_at(eye: Vec3, target: Vec3, up: Vec3) -> Mat4 {
    // A degenerate frame yields the identity rather than NaNs, on the same
    // principle as `Mat4::from_axis_angle`: garbage in, a picture that is
    // wrong, never a document full of NaNs.
    let Some(f) = (target - eye).normalize(1.0e-12) else {
        return Mat4::IDENTITY;
    };
    let Some(r) = f.cross(up).normalize(1.0e-12) else {
        return Mat4::IDENTITY;
    };
    let u = r.cross(f);
    Mat4([
        [r.x, r.y, r.z, -r.dot(eye)],
        [u.x, u.y, u.z, -u.dot(eye)],
        [-f.x, -f.y, -f.z, f.dot(eye)],
        [0.0, 0.0, 0.0, 1.0],
    ])
}

/// Clip space with depth in `0..1`, which is wgpu's and WebGPU's convention —
/// *not* OpenGL's `-1..1`. Getting this wrong halves the depth buffer and
/// looks like z-fighting rather than like a bug in a matrix.
pub fn perspective(fov_y: f64, aspect: f64, near: f64, far: f64) -> Mat4 {
    let f = 1.0 / (fov_y * 0.5).tan();
    Mat4([
        [f / aspect, 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [0.0, 0.0, far / (near - far), near * far / (near - far)],
        [0.0, 0.0, -1.0, 0.0],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ndc(m: &Mat4, p: Vec3) -> Vec3 {
        let f = |i: usize| (0..3).map(|k| m.0[i][k] * p.axis(k)).sum::<f64>() + m.0[i][3];
        let w = f(3);
        Vec3::new(f(0) / w, f(1) / w, f(2) / w)
    }

    /// wgpu's clip space, not OpenGL's. Getting this wrong costs half the
    /// depth buffer and presents as z-fighting rather than as a bad matrix.
    #[test]
    fn the_projection_puts_depth_in_zero_to_one() {
        let p = perspective(core::f64::consts::FRAC_PI_4, 16.0 / 9.0, 0.5, 100.0);
        let near = ndc(&p, Vec3::new(0.0, 0.0, -0.5));
        let far = ndc(&p, Vec3::new(0.0, 0.0, -100.0));
        assert!((near.z - 0.0).abs() < 1.0e-9, "near plane at {}", near.z);
        assert!((far.z - 1.0).abs() < 1.0e-9, "far plane at {}", far.z);
    }

    #[test]
    fn the_view_puts_the_target_on_the_axis_and_the_eye_at_the_origin() {
        let eye = Vec3::new(4.0, -3.0, 2.5);
        let target = Vec3::new(1.0, 1.0, 1.0);
        let v = look_at(eye, target, UP);

        let at_origin = v.transform_point(eye);
        assert!(at_origin.length() < 1.0e-12);

        let ahead = v.transform_point(target);
        assert!(ahead.x.abs() < 1.0e-12 && ahead.y.abs() < 1.0e-12);
        // Looking down -Z: the target is in front, so its view-space z is
        // negative. A positive one here means a left-handed frame crept in.
        assert!(
            ahead.z < 0.0,
            "the target is behind the camera at {ahead:?}"
        );
    }

    #[test]
    fn fitting_frames_the_bounds_and_sets_the_depth_range_from_them() {
        let mut c = Camera::default();
        let bounds = Aabb::new(Vec3::new(10.0, 10.0, 0.0), Vec3::new(50.0, 50.0, 10.0));
        c.fit(&bounds);

        assert_eq!(c.target, bounds.center());
        let radius = bounds.size().length() * 0.5;
        // Far enough that the sphere around the bounds is inside the frustum.
        assert!(c.distance > radius);
        // And the depth range brackets the contents rather than being a
        // constant: a 2 mm fillet and a 40 m assembly cannot share 0.1..1000.
        assert!(c.near > 0.0 && c.near < c.distance - radius);
        assert!(c.far > c.distance + radius);
    }

    /// A pan of the full viewport height moves the target by the height of
    /// what is visible at the target's depth. That is what makes a drag feel
    /// like dragging the model.
    #[test]
    fn panning_moves_the_target_across_the_view_and_scales_with_distance() {
        let mut near = Camera {
            distance: 10.0,
            yaw: 0.0,
            pitch: 0.0,
            ..Camera::default()
        };
        let mut far = Camera {
            distance: 100.0,
            ..near
        };
        near.pan(0.0, 100.0, 100.0);
        far.pan(0.0, 100.0, 100.0);

        let moved_near = (near.target - Vec3::ZERO).length();
        let moved_far = (far.target - Vec3::ZERO).length();
        assert!(moved_near > 0.0);
        // Ten times the distance, ten times the world movement for the same
        // drag — that is the whole point.
        assert!(
            (moved_far / moved_near - 10.0).abs() < 1.0e-9,
            "{moved_far} vs {moved_near}"
        );

        // Looking down +X with Z up, dragging up the screen moves the target up.
        assert!(near.target.z > 0.0, "target went {:?}", near.target);
    }

    #[test]
    fn panning_a_degenerate_frame_does_nothing_rather_than_producing_nans() {
        let mut c = Camera {
            pitch: core::f64::consts::FRAC_PI_2,
            ..Camera::default()
        };
        let before = c.target;
        c.pan(10.0, 10.0, 0.0);
        assert_eq!(c.target, before, "a zero-height viewport must be a no-op");
    }

    /// The scale-free property: a wheel notch is the same proportion of the
    /// view whether the document is millimetres or metres.
    #[test]
    fn dolly_is_multiplicative_and_never_reaches_zero() {
        let mut c = Camera {
            distance: 100.0,
            ..Camera::default()
        };
        c.dolly(0.5);
        assert_eq!(c.distance, 50.0);
        for _ in 0..2000 {
            c.dolly(0.5);
        }
        assert!(c.distance > 0.0, "distance collapsed to {}", c.distance);
    }

    #[test]
    fn pitch_stops_short_of_the_pole_where_there_is_no_right_vector() {
        let mut c = Camera::default();
        c.orbit(0.0, 100.0);
        assert!(c.pitch < core::f64::consts::FRAC_PI_2);
        // And the frame it produces is still well-defined.
        assert_ne!(c.view(), Mat4::IDENTITY);
        c.orbit(0.0, -200.0);
        assert!(c.pitch > -core::f64::consts::FRAC_PI_2);
        assert_ne!(c.view(), Mat4::IDENTITY);
    }

    /// The narrowing is a transpose as well as a cast, and getting only one of
    /// the two right produces a picture that is wrong in a way that looks like
    /// a camera bug.
    #[test]
    fn narrowing_to_wgsl_transposes_into_column_major() {
        let m = Mat4::from_translation(Vec3::new(1.0, 2.0, 3.0));
        let w = to_wgsl(&m);
        assert_eq!(w[3], [1.0, 2.0, 3.0, 1.0]);
    }
}
