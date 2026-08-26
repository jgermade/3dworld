//! A backend that satisfies the contract without doing any geometry.
//!
//! It exists so that `w3d-core` — the document, history, selection, the
//! tessellation cache — can be written, driven and tested before a decision on
//! the real kernel is made, and so that the conformance suite has something to
//! be run against on day one.
//!
//! **What it actually is:** a CSG tree that is never evaluated. Bounds are
//! computed exactly for primitives and combined by the rules set operations
//! obey — a union's bounds are the union of the operands', a difference's are
//! contained in the minuend's, an intersection's in both. Those are true
//! statements about the real answer, just weak ones. Tessellation returns the
//! bounding box, subdivided by quality.
//!
//! So it is honest about *shape* and silent about *geometry*, which is exactly
//! the split the conformance suite tests. **Prefer extending this over mocking
//! `w3d-core`**: a test that stubs the document proves nothing, one that stubs
//! the kernel proves the whole modeller.

use w3d_kernel::{
    Aabb, Body, BooleanOp, GeometryKernel, KernelError, Mat4, Mesh, Quality, Result, Tolerance,
    Topology, Vec3,
};

#[derive(Clone, Debug)]
enum Shape {
    Box(Vec3),
    Sphere(f64),
    Cylinder { radius: f64, height: f64 },
    Boolean(BooleanOp, Box<Shape>, Box<Shape>),
    Transformed(Mat4, Box<Shape>),
}

impl Shape {
    fn bounds(&self) -> Aabb {
        match self {
            Self::Box(size) => Aabb::centered(*size),
            Self::Sphere(r) => Aabb::centered(Vec3::splat(2.0 * r)),
            Self::Cylinder { radius, height } => {
                Aabb::centered(Vec3::new(2.0 * radius, 2.0 * radius, *height))
            }
            Self::Boolean(op, a, b) => match op {
                // Exact.
                BooleanOp::Union => a.bounds().union(&b.bounds()),
                // Conservative, and the only bound available without evaluating.
                BooleanOp::Difference => a.bounds(),
                BooleanOp::Intersection => a.bounds().intersection(&b.bounds()),
            },
            Self::Transformed(m, inner) => inner.bounds().transformed(m),
        }
    }

    /// Counts invented to be plausible and consistent, never to be believed.
    /// A caller that depends on these numbers is depending on the fake.
    fn topology(&self) -> Topology {
        match self {
            Self::Box(_) => Topology {
                solids: 1,
                faces: 6,
                edges: 12,
                vertices: 8,
            },
            Self::Sphere(_) => Topology {
                solids: 1,
                faces: 1,
                edges: 1,
                vertices: 2,
            },
            Self::Cylinder { .. } => Topology {
                solids: 1,
                faces: 3,
                edges: 2,
                vertices: 2,
            },
            Self::Boolean(_, a, b) => {
                let (a, b) = (a.topology(), b.topology());
                Topology {
                    solids: 1,
                    faces: a.faces + b.faces,
                    edges: a.edges + b.edges,
                    vertices: a.vertices + b.vertices,
                }
            }
            Self::Transformed(_, inner) => inner.topology(),
        }
    }
}

/// Subdivisions per box edge for a given quality.
///
/// Monotone in `sag` and clamped at both ends: the conformance suite requires
/// that finer quality never yields fewer triangles, and nothing requires that
/// a fake spend a second producing them.
fn subdivisions(bounds: &Aabb, quality: Quality) -> u32 {
    let extent = {
        let s = bounds.size();
        s.x.max(s.y).max(s.z).max(f64::MIN_POSITIVE)
    };
    let sag = quality.sag.max(1.0e-3);
    (extent / sag).sqrt().ceil().clamp(1.0, 16.0) as u32
}

/// The bounding box, as triangles, with one face id per side.
fn tessellate_box(bounds: &Aabb, n: u32) -> Mesh {
    let mut mesh = Mesh::default();
    if bounds.is_empty() {
        return mesh;
    }
    let (min, max) = (bounds.min, bounds.max);

    for face in 0..6u32 {
        let axis = (face / 2) as usize;
        let positive = face % 2 == 1;
        let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);

        let mut normal = [0.0f32; 3];
        normal[axis] = if positive { 1.0 } else { -1.0 };

        let base = mesh.positions.len() as u32;
        for i in 0..=n {
            for j in 0..=n {
                let mut p = Vec3::ZERO;
                p.set_axis(
                    axis,
                    if positive {
                        max.axis(axis)
                    } else {
                        min.axis(axis)
                    },
                );
                let t = |k: u32| f64::from(k) / f64::from(n);
                p.set_axis(u, min.axis(u) + (max.axis(u) - min.axis(u)) * t(i));
                p.set_axis(v, min.axis(v) + (max.axis(v) - min.axis(v)) * t(j));
                mesh.positions.push(p.to_f32());
                mesh.normals.push(normal);
            }
        }

        let stride = n + 1;
        for i in 0..n {
            for j in 0..n {
                let a = base + i * stride + j;
                let (b, c, d) = (a + 1, a + stride, a + stride + 1);
                // Winding follows the face direction so that back-face culling
                // is meaningful even on a fake.
                let quad = if positive {
                    [a, c, b, b, c, d]
                } else {
                    [a, b, c, b, d, c]
                };
                mesh.indices.extend_from_slice(&quad);
                mesh.face_of_triangle.push(face);
                mesh.face_of_triangle.push(face);
            }
        }
    }

    let corners = [
        Vec3::new(min.x, min.y, min.z).to_f32(),
        Vec3::new(max.x, min.y, min.z).to_f32(),
        Vec3::new(max.x, max.y, min.z).to_f32(),
        Vec3::new(min.x, max.y, min.z).to_f32(),
        Vec3::new(min.x, min.y, max.z).to_f32(),
        Vec3::new(max.x, min.y, max.z).to_f32(),
        Vec3::new(max.x, max.y, max.z).to_f32(),
        Vec3::new(min.x, max.y, max.z).to_f32(),
    ];
    mesh.line_positions = corners.to_vec();
    mesh.line_indices = vec![
        0, 1, 1, 2, 2, 3, 3, 0, 4, 5, 5, 6, 6, 7, 7, 4, 0, 4, 1, 5, 2, 6, 3, 7,
    ];

    mesh
}

/// Slots are never reused. A stale handle therefore stays an error forever
/// rather than silently naming somebody else's body — the failure mode that
/// makes a generational index worth having, and the one the conformance suite
/// checks by deleting twice.
#[derive(Default)]
pub struct FakeKernel {
    slots: Vec<Option<Shape>>,
}

impl FakeKernel {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many bodies are alive. For tests that assert the document is not
    /// leaking kernel storage.
    pub fn live_bodies(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    fn insert(&mut self, shape: Shape) -> Body {
        self.slots.push(Some(shape));
        Body::from_raw((self.slots.len() - 1) as u32)
    }

    fn get(&self, body: Body) -> Result<&Shape> {
        self.slots
            .get(body.raw() as usize)
            .and_then(Option::as_ref)
            .ok_or(KernelError::UnknownBody(body))
    }
}

impl GeometryKernel for FakeKernel {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn create_box(&mut self, size: Vec3) -> Result<Body> {
        if size.x <= 0.0 || size.y <= 0.0 || size.z <= 0.0 {
            return Err(KernelError::Degenerate("box extent must be positive"));
        }
        Ok(self.insert(Shape::Box(size)))
    }

    fn create_sphere(&mut self, radius: f64) -> Result<Body> {
        if radius <= 0.0 {
            return Err(KernelError::Degenerate("sphere radius must be positive"));
        }
        Ok(self.insert(Shape::Sphere(radius)))
    }

    fn create_cylinder(&mut self, radius: f64, height: f64) -> Result<Body> {
        if radius <= 0.0 || height <= 0.0 {
            return Err(KernelError::Degenerate(
                "cylinder radius and height must be positive",
            ));
        }
        Ok(self.insert(Shape::Cylinder { radius, height }))
    }

    fn boolean(&mut self, op: BooleanOp, a: Body, b: Body, _tol: Tolerance) -> Result<Body> {
        let (sa, sb) = (self.get(a)?.clone(), self.get(b)?.clone());
        Ok(self.insert(Shape::Boolean(op, Box::new(sa), Box::new(sb))))
    }

    fn transform(&mut self, body: Body, m: &Mat4) -> Result<Body> {
        let inner = self.get(body)?.clone();
        Ok(self.insert(Shape::Transformed(*m, Box::new(inner))))
    }

    fn copy(&mut self, body: Body) -> Result<Body> {
        let shape = self.get(body)?.clone();
        Ok(self.insert(shape))
    }

    fn delete(&mut self, body: Body) -> Result<()> {
        let slot = self
            .slots
            .get_mut(body.raw() as usize)
            .ok_or(KernelError::UnknownBody(body))?;
        slot.take()
            .map(|_| ())
            .ok_or(KernelError::UnknownBody(body))
    }

    fn topology(&self, body: Body) -> Result<Topology> {
        Ok(self.get(body)?.topology())
    }

    fn bounds(&self, body: Body) -> Result<Aabb> {
        Ok(self.get(body)?.bounds())
    }

    fn tessellate(&self, body: Body, quality: Quality) -> Result<Mesh> {
        let bounds = self.get(body)?.bounds();
        Ok(tessellate_box(&bounds, subdivisions(&bounds, quality)))
    }

    fn geometry_format(&self) -> &'static str {
        FORMAT
    }

    fn save_body(&self, body: Body) -> Result<Vec<u8>> {
        let mut out = MAGIC.to_vec();
        encode(self.get(body)?, &mut out);
        Ok(out)
    }

    fn load_body(&mut self, bytes: &[u8]) -> Result<Body> {
        let Some(rest) = bytes.strip_prefix(MAGIC) else {
            return Err(KernelError::Unsupported(
                "these bytes were not written by the fake kernel",
            ));
        };
        let mut at = 0;
        let shape = decode(rest, &mut at)?;
        if at != rest.len() {
            return Err(KernelError::Failed(format!(
                "{} trailing bytes after the shape",
                rest.len() - at
            )));
        }
        Ok(self.insert(shape))
    }

    // A STEP file is a boundary representation: faces, with surfaces under
    // them. This kernel has none — it is a CSG tree that is never evaluated,
    // so there is nothing to write down and nowhere to put what is read. The
    // refusal is the honest answer and not a stub waiting to be filled in: it
    // could only be filled in by evaluating the tree, which is the whole of
    // what a real kernel does.
    //
    // Both directions, and the conformance suite holds it to both.

    fn export_step(&self, _bodies: &[Body]) -> Result<Vec<u8>> {
        Err(KernelError::Unsupported(
            "the fake kernel has no surfaces to write to STEP",
        ))
    }

    fn import_step(&mut self, _bytes: &[u8]) -> Result<Vec<w3d_kernel::ImportedBody>> {
        Err(KernelError::Unsupported(
            "the fake kernel cannot represent what is in a STEP file",
        ))
    }
}

/// Named in `geometry_format`, and versioned because changing what `encode`
/// writes without changing this would break every file already saved.
const FORMAT: &str = "fake-csg-1";

/// A prefix, so that bytes from a different kernel are refused rather than
/// misread. The conformance suite checks that they are.
const MAGIC: &[u8] = b"w3d-fake-csg-1\0";

fn push_f64(out: &mut Vec<u8>, v: f64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn encode(shape: &Shape, out: &mut Vec<u8>) {
    match shape {
        Shape::Box(size) => {
            out.push(1);
            for v in [size.x, size.y, size.z] {
                push_f64(out, v);
            }
        }
        Shape::Sphere(r) => {
            out.push(2);
            push_f64(out, *r);
        }
        Shape::Cylinder { radius, height } => {
            out.push(3);
            push_f64(out, *radius);
            push_f64(out, *height);
        }
        Shape::Boolean(op, a, b) => {
            out.push(4);
            out.push(match op {
                BooleanOp::Union => 0,
                BooleanOp::Difference => 1,
                BooleanOp::Intersection => 2,
            });
            encode(a, out);
            encode(b, out);
        }
        Shape::Transformed(m, inner) => {
            out.push(5);
            for row in m.0 {
                for v in row {
                    push_f64(out, v);
                }
            }
            encode(inner, out);
        }
    }
}

fn take_f64(bytes: &[u8], at: &mut usize) -> Result<f64> {
    let end = *at + 8;
    let slice = bytes
        .get(*at..end)
        .ok_or_else(|| KernelError::Failed(String::from("the file ends inside a number")))?;
    *at = end;
    Ok(f64::from_le_bytes(slice.try_into().expect("eight bytes")))
}

/// Recursive, and so is the data — a deeply nested document would recurse
/// deeply here. Bounded in practice by how many booleans a person performs,
/// and named in the session file rather than defended against.
fn decode(bytes: &[u8], at: &mut usize) -> Result<Shape> {
    let tag = *bytes.get(*at).ok_or_else(|| {
        KernelError::Failed(String::from("the file ends where a shape should be"))
    })?;
    *at += 1;
    Ok(match tag {
        1 => Shape::Box(Vec3::new(
            take_f64(bytes, at)?,
            take_f64(bytes, at)?,
            take_f64(bytes, at)?,
        )),
        2 => Shape::Sphere(take_f64(bytes, at)?),
        3 => Shape::Cylinder {
            radius: take_f64(bytes, at)?,
            height: take_f64(bytes, at)?,
        },
        4 => {
            let op = match bytes.get(*at) {
                Some(0) => BooleanOp::Union,
                Some(1) => BooleanOp::Difference,
                Some(2) => BooleanOp::Intersection,
                _ => return Err(KernelError::Failed(String::from("unknown boolean"))),
            };
            *at += 1;
            let a = decode(bytes, at)?;
            let b = decode(bytes, at)?;
            Shape::Boolean(op, Box::new(a), Box::new(b))
        }
        5 => {
            let mut m = [[0.0f64; 4]; 4];
            for row in &mut m {
                for cell in row {
                    *cell = take_f64(bytes, at)?;
                }
            }
            Shape::Transformed(Mat4(m), Box::new(decode(bytes, at)?))
        }
        other => {
            return Err(KernelError::Failed(format!("unknown shape tag {other}")));
        }
    })
}
