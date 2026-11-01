//! Camera and light maths for `img.render`: right-handed world (Y up), a
//! camera looking down its −Z, WebGPU clip space (depth 0 to 1). Matrices
//! are column-major `[[f32; 4]; 4]` (column `c` is `m[c]`), as WGSL reads a
//! `mat4x4f` from a uniform buffer. Computed in `f64`, stored as `f32`.

pub type Mat4 = [[f32; 4]; 4];
type V3 = [f64; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(a: V3) -> Option<V3> {
    let l = dot(a, a).sqrt();
    (l > 1e-12 && l.is_finite()).then(|| [a[0] / l, a[1] / l, a[2] / l])
}

/// The unit direction at `azimuth` degrees around +Y (0 looks from +Z,
/// 90 from +X) and `elevation` degrees above the XZ plane.
pub fn direction(azimuth: f64, elevation: f64) -> V3 {
    let (az, el) = (azimuth.to_radians(), elevation.to_radians());
    [el.cos() * az.sin(), el.sin(), el.cos() * az.cos()]
}

/// A camera for one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    pub eye: V3,
    pub target: V3,
    /// Vertical field of view in degrees (perspective).
    pub fov: f64,
    /// World units from the bottom to the top of the image (orthographic).
    pub ortho_height: f64,
    pub near: f64,
    pub far: f64,
    pub perspective: bool,
    /// Width over height of the image.
    pub aspect: f64,
}

/// What the shaders need from a camera.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrices {
    pub view: Mat4,
    pub view_proj: Mat4,
    /// Projection scale of view-space X and Y (`proj[0][0]`, `proj[1][1]`).
    pub scale: [f32; 2],
    /// The camera's forward direction (for orthographic view vectors).
    pub forward: [f32; 3],
}

fn mul(a: &[[f64; 4]; 4], b: &[[f64; 4]; 4]) -> [[f64; 4]; 4] {
    let mut out = [[0.0; 4]; 4];
    for (c, col) in out.iter_mut().enumerate() {
        for (r, v) in col.iter_mut().enumerate() {
            *v = (0..4).map(|k| a[k][r] * b[c][k]).sum();
        }
    }
    out
}

fn to_f32(m: &[[f64; 4]; 4]) -> Mat4 {
    m.map(|c| c.map(|v| v as f32))
}

impl Camera {
    /// Clamps the parameters to a usable camera: near above zero, far
    /// beyond near, field of view between 1° and 179°, a camera not on its
    /// target. Non-finite values fall back to the defaults.
    pub fn sanitised(mut self) -> Self {
        let finite = |v: f64, d: f64| if v.is_finite() { v } else { d };
        self.eye = self.eye.map(|v| finite(v, 0.0));
        self.target = self.target.map(|v| finite(v, 0.0));
        self.fov = finite(self.fov, 45.0).clamp(1.0, 179.0);
        self.ortho_height = finite(self.ortho_height, 3.0).max(1e-6);
        self.near = finite(self.near, 0.05).max(1e-6);
        self.far = finite(self.far, 100.0).max(self.near * 1.001);
        self.aspect = finite(self.aspect, 1.0).max(1e-6);
        if normalize(sub(self.target, self.eye)).is_none() {
            self.eye[2] += 1.0;
        }
        self
    }

    pub fn matrices(&self) -> Matrices {
        let c = self.sanitised();
        let f = normalize(sub(c.target, c.eye)).expect("sanitised");
        // Up is +Y unless the camera looks straight up or down.
        let up = if f[1].abs() > 0.999_999 {
            [0.0, 0.0, -f[1].signum()]
        } else {
            [0.0, 1.0, 0.0]
        };
        let s = normalize(cross(f, up)).expect("not parallel");
        let u = cross(s, f);
        let e = c.eye;
        let view = [
            [s[0], u[0], -f[0], 0.0],
            [s[1], u[1], -f[1], 0.0],
            [s[2], u[2], -f[2], 0.0],
            [-dot(s, e), -dot(u, e), dot(f, e), 1.0],
        ];
        let (n, fa) = (c.near, c.far);
        let proj = if c.perspective {
            let y = 1.0 / (c.fov.to_radians() * 0.5).tan();
            [
                [y / c.aspect, 0.0, 0.0, 0.0],
                [0.0, y, 0.0, 0.0],
                [0.0, 0.0, fa / (n - fa), -1.0],
                [0.0, 0.0, n * fa / (n - fa), 0.0],
            ]
        } else {
            let h = c.ortho_height;
            let w = h * c.aspect;
            [
                [2.0 / w, 0.0, 0.0, 0.0],
                [0.0, 2.0 / h, 0.0, 0.0],
                [0.0, 0.0, 1.0 / (n - fa), 0.0],
                [0.0, 0.0, n / (n - fa), 1.0],
            ]
        };
        Matrices {
            view: to_f32(&view),
            view_proj: to_f32(&mul(&proj, &view)),
            scale: [proj[0][0] as f32, proj[1][1] as f32],
            forward: [f[0] as f32, f[1] as f32, f[2] as f32],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(m: &Mat4, p: [f32; 3]) -> [f32; 4] {
        let mut out = [0.0; 4];
        for (r, o) in out.iter_mut().enumerate() {
            *o = m[0][r] * p[0] + m[1][r] * p[1] + m[2][r] * p[2] + m[3][r];
        }
        out
    }

    fn cam(perspective: bool) -> Camera {
        Camera {
            eye: [0.0, 0.0, 5.0],
            target: [0.0, 0.0, 0.0],
            fov: 90.0,
            ortho_height: 2.0,
            near: 1.0,
            far: 9.0,
            perspective,
            aspect: 2.0,
        }
    }

    #[test]
    fn perspective_maps_the_frustum_to_clip_space() {
        let m = cam(true).matrices();
        let ndc = |p| {
            let c = apply(&m.view_proj, p);
            [c[0] / c[3], c[1] / c[3], c[2] / c[3]]
        };
        let centre_near = ndc([0.0, 0.0, 4.0]);
        assert!(centre_near[2].abs() < 1e-6, "near plane at depth 0");
        let far = ndc([0.0, 0.0, -4.0]);
        assert!((far[2] - 1.0).abs() < 1e-5, "far plane at depth 1");
        // 90° vertical: at distance 5 the top of the view is y = 5.
        let top = ndc([0.0, 5.0, 0.0]);
        assert!((top[1] - 1.0).abs() < 1e-5, "{top:?}");
        let right = ndc([10.0, 0.0, 0.0]);
        assert!((right[0] - 1.0).abs() < 1e-5, "aspect 2: {right:?}");
    }

    #[test]
    fn orthographic_and_degenerate_cameras() {
        let m = cam(false).matrices();
        let c = apply(&m.view_proj, [2.0, 1.0, 0.0]);
        assert!((c[0] - 1.0).abs() < 1e-6 && (c[1] - 1.0).abs() < 1e-6 && c[3] == 1.0);
        // Looking straight down, at its own target, with NaN: still finite.
        for c in [
            Camera {
                eye: [0.0, 5.0, 0.0],
                ..cam(true)
            },
            Camera {
                eye: [0.0, 0.0, 0.0],
                ..cam(true)
            },
            Camera {
                fov: f64::NAN,
                near: -1.0,
                far: 0.0,
                ..cam(true)
            },
        ] {
            let m = c.matrices();
            assert!(m.view_proj.iter().flatten().all(|v| v.is_finite()), "{c:?}");
        }
        let d = direction(90.0, 0.0);
        assert!((d[0] - 1.0).abs() < 1e-12 && d[1].abs() < 1e-12);
    }
}
