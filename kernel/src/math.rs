//! The minimum of linear algebra the contract needs.
//!
//! Deliberately not a dependency. Everything here is `f64`: the kernel is the
//! one place where precision is a correctness property rather than a budget.
//! Tessellation output narrows to `f32` at the boundary with the GPU, and
//! nowhere else.

/// A point or direction in model space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Vec3 {
    pub const ZERO: Self = Self::new(0.0, 0.0, 0.0);

    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    pub const fn splat(v: f64) -> Self {
        Self::new(v, v, v)
    }

    pub fn min(self, o: Self) -> Self {
        Self::new(self.x.min(o.x), self.y.min(o.y), self.z.min(o.z))
    }

    pub fn max(self, o: Self) -> Self {
        Self::new(self.x.max(o.x), self.y.max(o.y), self.z.max(o.z))
    }

    pub fn dot(self, o: Self) -> f64 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }

    pub fn cross(self, o: Self) -> Self {
        Self::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }

    pub fn length(self) -> f64 {
        self.dot(self).sqrt()
    }

    /// `None` when the vector is shorter than `tol`, which is the only honest
    /// answer: normalising a degenerate direction is where kernels go wrong.
    pub fn normalize(self, tol: f64) -> Option<Self> {
        let len = self.length();
        (len > tol).then(|| self * (1.0 / len))
    }

    pub fn to_f32(self) -> [f32; 3] {
        [self.x as f32, self.y as f32, self.z as f32]
    }

    /// Component by axis index: 0 is x, 1 is y, anything else z.
    pub fn axis(self, i: usize) -> f64 {
        match i {
            0 => self.x,
            1 => self.y,
            _ => self.z,
        }
    }

    pub fn set_axis(&mut self, i: usize, v: f64) {
        match i {
            0 => self.x = v,
            1 => self.y = v,
            _ => self.z = v,
        }
    }
}

impl core::ops::Add for Vec3 {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        Self::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}

impl core::ops::Sub for Vec3 {
    type Output = Self;
    fn sub(self, o: Self) -> Self {
        Self::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}

impl core::ops::Neg for Vec3 {
    type Output = Self;
    fn neg(self) -> Self {
        Self::new(-self.x, -self.y, -self.z)
    }
}

impl core::ops::Mul<f64> for Vec3 {
    type Output = Self;
    fn mul(self, s: f64) -> Self {
        Self::new(self.x * s, self.y * s, self.z * s)
    }
}

/// A 4x4 affine transform, row-major, applied as `p' = M * p`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mat4(pub [[f64; 4]; 4]);

impl Mat4 {
    pub const IDENTITY: Self = Self([
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]);

    pub fn from_translation(t: Vec3) -> Self {
        let mut m = Self::IDENTITY;
        m.0[0][3] = t.x;
        m.0[1][3] = t.y;
        m.0[2][3] = t.z;
        m
    }

    pub fn from_scale(s: Vec3) -> Self {
        let mut m = Self::IDENTITY;
        m.0[0][0] = s.x;
        m.0[1][1] = s.y;
        m.0[2][2] = s.z;
        m
    }

    /// Rodrigues. `axis` is normalised here; a degenerate axis yields the
    /// identity rather than NaNs propagating into the document.
    pub fn from_axis_angle(axis: Vec3, radians: f64, tol: f64) -> Self {
        let Some(a) = axis.normalize(tol) else {
            return Self::IDENTITY;
        };
        let (s, c) = radians.sin_cos();
        let t = 1.0 - c;
        Self([
            [
                t * a.x * a.x + c,
                t * a.x * a.y - s * a.z,
                t * a.x * a.z + s * a.y,
                0.0,
            ],
            [
                t * a.x * a.y + s * a.z,
                t * a.y * a.y + c,
                t * a.y * a.z - s * a.x,
                0.0,
            ],
            [
                t * a.x * a.z - s * a.y,
                t * a.y * a.z + s * a.x,
                t * a.z * a.z + c,
                0.0,
            ],
            [0.0, 0.0, 0.0, 1.0],
        ])
    }

    pub fn mul(&self, rhs: &Self) -> Self {
        let mut out = [[0.0; 4]; 4];
        for (i, row) in out.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().enumerate() {
                *cell = (0..4).map(|k| self.0[i][k] * rhs.0[k][j]).sum();
            }
        }
        Self(out)
    }

    pub fn transform_point(&self, p: Vec3) -> Vec3 {
        let f = |i: usize| (0..3).map(|k| self.0[i][k] * p.axis(k)).sum::<f64>() + self.0[i][3];
        Vec3::new(f(0), f(1), f(2))
    }

    pub fn transform_vector(&self, v: Vec3) -> Vec3 {
        let f = |i: usize| (0..3).map(|k| self.0[i][k] * v.axis(k)).sum::<f64>();
        Vec3::new(f(0), f(1), f(2))
    }
}

/// An axis-aligned box. The empty box is `min > max` on every axis, which makes
/// `expand` on a fresh one correct without a special case.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub const EMPTY: Self = Self {
        min: Vec3::splat(f64::INFINITY),
        max: Vec3::splat(f64::NEG_INFINITY),
    };

    pub fn new(min: Vec3, max: Vec3) -> Self {
        Self { min, max }
    }

    /// Centred on the origin, `size` across. The primitive constructors all
    /// agree on origin-centred, which is what makes `create_box` bounds an
    /// exact assertion in the conformance suite.
    pub fn centered(size: Vec3) -> Self {
        let h = size * 0.5;
        Self::new(-h, h)
    }

    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y || self.min.z > self.max.z
    }

    pub fn expand(&mut self, p: Vec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    pub fn union(&self, o: &Self) -> Self {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        Self::new(self.min.min(o.min), self.max.max(o.max))
    }

    /// May return an empty box, and that is a meaningful answer: the operands
    /// cannot intersect.
    pub fn intersection(&self, o: &Self) -> Self {
        Self::new(self.min.max(o.min), self.max.min(o.max))
    }

    pub fn contains(&self, p: Vec3, tol: f64) -> bool {
        let lo = self.min - Vec3::splat(tol);
        let hi = self.max + Vec3::splat(tol);
        p.x >= lo.x && p.y >= lo.y && p.z >= lo.z && p.x <= hi.x && p.y <= hi.y && p.z <= hi.z
    }

    /// Contains `o` entirely, within `tol`.
    pub fn contains_box(&self, o: &Self, tol: f64) -> bool {
        o.is_empty() || (self.contains(o.min, tol) && self.contains(o.max, tol))
    }

    pub fn transformed(&self, m: &Mat4) -> Self {
        if self.is_empty() {
            return *self;
        }
        // All eight corners: transforming min/max alone is wrong under rotation,
        // and that mistake survives a long time because it looks right on
        // axis-aligned tests.
        let mut out = Self::EMPTY;
        for i in 0..8 {
            let corner = Vec3::new(
                if i & 1 == 0 { self.min.x } else { self.max.x },
                if i & 2 == 0 { self.min.y } else { self.max.y },
                if i & 4 == 0 { self.min.z } else { self.max.z },
            );
            out.expand(m.transform_point(corner));
        }
        out
    }

    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    pub fn size(&self) -> Vec3 {
        self.max - self.min
    }
}
