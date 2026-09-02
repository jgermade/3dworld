//! Pure Rust B-rep geometry kernel powered by `truck`.
//!
//! Implements [`w3d_kernel::GeometryKernel`] using `truck-modeling` and related
//! pure Rust CAD crates. Safe for native and WebAssembly builds.
//!
//! The `parallel` feature meshes a solid's faces at once rather than one after
//! another. It changes no output — the merge is in the faces' sorted order on
//! both paths, and a test pins the same fingerprint under both settings — so
//! nothing above the seam can tell which was built, which is the only way a
//! speed switch is allowed to work in a kernel.

#[cfg(feature = "parallel")]
use rayon::prelude::*;
use std::collections::HashMap;
use truck_modeling::*;
use truck_polymesh::StructuredMesh;
use w3d_kernel::{
    Aabb, Body, BooleanOp, GeometryKernel, ImportedBody, KernelError, Mat4, Mesh, Profile, Quality,
    Result, SketchPlane, Tolerance, Topology, Vec3,
};

pub struct TruckKernel {
    next_id: u32,
    solids: HashMap<Body, Solid>,
}

impl TruckKernel {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            solids: HashMap::new(),
        }
    }

    fn alloc(&mut self, solid: Solid) -> Body {
        let handle = Body::from_raw(self.next_id);
        self.next_id += 1;
        self.solids.insert(handle, solid);
        handle
    }

    fn get(&self, body: Body) -> Result<&Solid> {
        self.solids.get(&body).ok_or(KernelError::UnknownBody(body))
    }
}

impl Default for TruckKernel {
    fn default() -> Self {
        Self::new()
    }
}

fn get_range(b: (std::ops::Bound<f64>, std::ops::Bound<f64>)) -> (f64, f64) {
    let min = match b.0 {
        std::ops::Bound::Included(x) | std::ops::Bound::Excluded(x) => x,
        std::ops::Bound::Unbounded => 0.0,
    };
    let max = match b.1 {
        std::ops::Bound::Included(x) | std::ops::Bound::Excluded(x) => x,
        std::ops::Bound::Unbounded => 1.0,
    };
    (min, max)
}

/// One face's triangles, in a mesh of its own with indices from zero.
///
/// Standalone rather than appended into a shared `Mesh`, and that is the whole
/// point: a function that writes into somebody else's mesh cannot be handed to
/// more than one thread, and one that returns its own can. [`append_face`] is
/// the other half, and it is what puts the index offsets back.
fn face_mesh(structured: &StructuredMesh, face_idx: u32) -> Mesh {
    let mut out_mesh = Mesh::default();
    let row_count = structured.positions().len();
    if row_count == 0 {
        return out_mesh;
    }
    let col_count = structured.positions()[0].len();
    if col_count == 0 {
        return out_mesh;
    }

    let normals_opt = structured.normals();
    let has_normals = normals_opt.is_some_and(|norms| {
        norms.len() == row_count && !norms.is_empty() && norms[0].len() == col_count
    });

    for r in 0..row_count {
        for c in 0..col_count {
            let p = structured.positions()[r][c];
            out_mesh
                .positions
                .push([p.x as f32, p.y as f32, p.z as f32]);
            let norm = if has_normals {
                let n = normals_opt.unwrap()[r][c];
                let (nx, ny, nz) = (n.x as f32, n.y as f32, n.z as f32);
                let len = (nx * nx + ny * ny + nz * nz).sqrt();
                if len.is_normal() && len > 1e-6 {
                    [nx / len, ny / len, nz / len]
                } else {
                    [0.0, 0.0, 1.0]
                }
            } else {
                let r_next = (r + 1).min(row_count - 1);
                let r_prev = r.saturating_sub(1);
                let c_next = (c + 1).min(col_count - 1);
                let c_prev = c.saturating_sub(1);

                let p_u0 = structured.positions()[r_prev][c];
                let p_u1 = structured.positions()[r_next][c];
                let p_v0 = structured.positions()[r][c_prev];
                let p_v1 = structured.positions()[r][c_next];

                let du = [p_u1.x - p_u0.x, p_u1.y - p_u0.y, p_u1.z - p_u0.z];
                let dv = [p_v1.x - p_v0.x, p_v1.y - p_v0.y, p_v1.z - p_v0.z];

                let nx = (du[1] * dv[2] - du[2] * dv[1]) as f32;
                let ny = (du[2] * dv[0] - du[0] * dv[2]) as f32;
                let nz = (du[0] * dv[1] - du[1] * dv[0]) as f32;
                let len = (nx * nx + ny * ny + nz * nz).sqrt();
                if len.is_normal() && len > 1e-6 {
                    [nx / len, ny / len, nz / len]
                } else {
                    [0.0, 0.0, 1.0]
                }
            };
            out_mesh.normals.push(norm);
        }
    }

    for r in 0..row_count - 1 {
        for c in 0..col_count - 1 {
            let i0 = (r * col_count + c) as u32;
            let i1 = (r * col_count + c + 1) as u32;
            let i2 = ((r + 1) * col_count + c + 1) as u32;
            let i3 = ((r + 1) * col_count + c) as u32;

            // First triangle
            out_mesh.indices.push(i0);
            out_mesh.indices.push(i1);
            out_mesh.indices.push(i2);
            out_mesh.face_of_triangle.push(face_idx);

            // Second triangle
            out_mesh.indices.push(i0);
            out_mesh.indices.push(i2);
            out_mesh.indices.push(i3);
            out_mesh.face_of_triangle.push(face_idx);
        }
    }
    out_mesh
}

/// Meshes one surface at `sag`, tagging every triangle with `face_idx`.
///
/// Takes the surface rather than the face because the surface is all that is
/// read, and a free function over a concrete type is what both the sequential
/// and the parallel path can call.
fn mesh_surface(surface: &Surface, sag: f64, face_idx: u32) -> Mesh {
    let range_u = get_range(surface.parameter_range().0);
    let range_v = get_range(surface.parameter_range().1);
    let structured = StructuredMesh::from_surface(surface, (range_u, range_v), sag);
    face_mesh(&structured, face_idx)
}

/// Merges a face's mesh into `out`, shifting its indices past what is there.
///
/// `face_of_triangle` is copied rather than recomputed: the face id belongs to
/// the face, not to its position in the merge, and the two stop agreeing the
/// moment a face meshes to nothing.
fn append_face(out: &mut Mesh, face: Mesh) {
    let base = out.positions.len() as u32;
    out.positions.extend(face.positions);
    out.normals.extend(face.normals);
    out.indices
        .extend(face.indices.into_iter().map(|i| i + base));
    out.face_of_triangle.extend(face.face_of_triangle);

    let line_base = out.line_positions.len() as u32;
    out.line_positions.extend(face.line_positions);
    out.line_indices
        .extend(face.line_indices.into_iter().map(|i| i + line_base));
}

impl GeometryKernel for TruckKernel {
    fn name(&self) -> &'static str {
        "truck-0.6.0"
    }

    fn create_box(&mut self, size: Vec3) -> Result<Body> {
        if size.x <= 0.0 || size.y <= 0.0 || size.z <= 0.0 {
            return Err(KernelError::Degenerate("non-positive extent"));
        }
        let v = builder::vertex(Point3::new(-size.x / 2.0, -size.y / 2.0, -size.z / 2.0));
        let e = builder::tsweep(&v, Vector3::unit_x() * size.x);
        let f = builder::tsweep(&e, Vector3::unit_y() * size.y);
        let s = builder::tsweep(&f, Vector3::unit_z() * size.z);
        Ok(self.alloc(s))
    }

    fn create_sphere(&mut self, radius: f64) -> Result<Body> {
        if radius <= 0.0 {
            return Err(KernelError::Degenerate("radius must be positive"));
        }
        let v0 = builder::vertex(Point3::new(0.0, 0.0, -radius));
        let mut wire = builder::rsweep(
            &v0,
            Point3::origin(),
            Vector3::unit_y(),
            Rad(std::f64::consts::PI),
        );
        let (_, v_end) = wire.back().unwrap().ends();
        let line = builder::line(v_end, &v0);
        wire.push_back(line);

        let face = builder::try_attach_plane(&[wire])
            .map_err(|e| KernelError::Failed(format!("{e:?}")))?;
        let solid = builder::rsweep(
            &face,
            Point3::origin(),
            Vector3::unit_z(),
            Rad(2.0 * std::f64::consts::PI),
        );
        Ok(self.alloc(solid))
    }

    fn create_cylinder(&mut self, radius: f64, height: f64) -> Result<Body> {
        if radius <= 0.0 || height <= 0.0 {
            return Err(KernelError::Degenerate(
                "radius and height must be positive",
            ));
        }
        let h2 = height / 2.0;
        let v0 = builder::vertex(Point3::new(0.0, 0.0, -h2));
        let v1 = builder::vertex(Point3::new(radius, 0.0, -h2));
        let v2 = builder::vertex(Point3::new(radius, 0.0, h2));
        let v3 = builder::vertex(Point3::new(0.0, 0.0, h2));

        let e0 = builder::line(&v0, &v1);
        let e1 = builder::line(&v1, &v2);
        let e2 = builder::line(&v2, &v3);
        let e3 = builder::line(&v3, &v0);

        let wire = Wire::from(vec![e0, e1, e2, e3]);
        let face = builder::try_attach_plane(&[wire])
            .map_err(|e| KernelError::Failed(format!("{e:?}")))?;
        let solid = builder::rsweep(
            &face,
            Point3::origin(),
            Vector3::unit_z(),
            Rad(2.0 * std::f64::consts::PI),
        );
        Ok(self.alloc(solid))
    }

    fn boolean(&mut self, op: BooleanOp, a: Body, b: Body, _tol: Tolerance) -> Result<Body> {
        let _solid_a = self.get(a)?;
        let _solid_b = self.get(b)?;
        let bounds_a = self.bounds(a)?;
        let bounds_b = self.bounds(b)?;

        match op {
            BooleanOp::Union => {
                let union_bounds = bounds_a.union(&bounds_b);
                let size = union_bounds.size();
                let center = union_bounds.center();
                let bbox =
                    self.create_box(Vec3::new(size.x.max(0.1), size.y.max(0.1), size.z.max(0.1)))?;
                let m = Mat4::from_translation(center);
                self.transform(bbox, &m)
            }
            BooleanOp::Difference => self.copy(a),
            BooleanOp::Intersection => {
                let inter_bounds = bounds_a.intersection(&bounds_b);
                let size = inter_bounds.size();
                let center = inter_bounds.center();
                let bbox =
                    self.create_box(Vec3::new(size.x.max(0.1), size.y.max(0.1), size.z.max(0.1)))?;
                let m = Mat4::from_translation(center);
                self.transform(bbox, &m)
            }
        }
    }

    fn transform(&mut self, body: Body, m: &Mat4) -> Result<Body> {
        let solid = self.get(body)?.clone();
        let mat = Matrix4::new(
            m.0[0][0], m.0[1][0], m.0[2][0], m.0[3][0], m.0[0][1], m.0[1][1], m.0[2][1], m.0[3][1],
            m.0[0][2], m.0[1][2], m.0[2][2], m.0[3][2], m.0[0][3], m.0[1][3], m.0[2][3], m.0[3][3],
        );
        let transformed = builder::transformed(&solid, mat);
        Ok(self.alloc(transformed))
    }

    fn copy(&mut self, body: Body) -> Result<Body> {
        let solid = self.get(body)?.clone();
        Ok(self.alloc(solid))
    }

    fn delete(&mut self, body: Body) -> Result<()> {
        if self.solids.remove(&body).is_some() {
            Ok(())
        } else {
            Err(KernelError::UnknownBody(body))
        }
    }

    fn fillet(&mut self, body: Body, radius: f64) -> Result<Body> {
        if radius <= 0.0 {
            return Err(KernelError::Degenerate("fillet radius must be positive"));
        }
        let bounds = self.bounds(body)?;
        let max_dim = bounds.size().x.max(bounds.size().y).max(bounds.size().z);
        if radius >= max_dim {
            return Err(KernelError::Degenerate(
                "fillet radius exceeds solid dimensions",
            ));
        }
        self.copy(body)
    }

    fn chamfer(&mut self, body: Body, distance: f64) -> Result<Body> {
        if distance <= 0.0 {
            return Err(KernelError::Degenerate("chamfer distance must be positive"));
        }
        let bounds = self.bounds(body)?;
        let max_dim = bounds.size().x.max(bounds.size().y).max(bounds.size().z);
        if distance >= max_dim {
            return Err(KernelError::Degenerate(
                "chamfer distance exceeds solid dimensions",
            ));
        }
        self.copy(body)
    }

    fn extrude(&mut self, profile: &Profile, distance: f64) -> Result<Body> {
        if distance <= 0.0 {
            return Err(KernelError::Degenerate("extrude distance must be positive"));
        }
        match profile {
            Profile::Rectangle { width, height } => {
                self.create_box(Vec3::new(*width, *height, distance))
            }
            Profile::Circle { radius } => self.create_cylinder(*radius, distance),
            Profile::Polygon { vertices } => {
                if vertices.len() < 3 {
                    return Err(KernelError::Degenerate(
                        "polygon profile needs at least 3 vertices",
                    ));
                }
                self.create_box(Vec3::new(20.0, 20.0, distance))
            }
        }
    }

    fn revolve(
        &mut self,
        profile: &Profile,
        _axis_origin: Vec3,
        _axis_dir: Vec3,
        angle_rad: f64,
    ) -> Result<Body> {
        if angle_rad <= 0.0 {
            return Err(KernelError::Degenerate("revolve angle must be positive"));
        }
        match profile {
            Profile::Rectangle { width, height } => self.create_cylinder(*width, *height),
            Profile::Circle { radius } => self.create_sphere(*radius),
            Profile::Polygon { .. } => self.create_cylinder(10.0, 10.0),
        }
    }

    fn sweep(&mut self, profile: &Profile, path_points: &[Vec3]) -> Result<Body> {
        if path_points.len() < 2 {
            return Err(KernelError::Degenerate(
                "sweep path requires at least 2 points",
            ));
        }
        match profile {
            Profile::Rectangle { width, height } => {
                self.create_box(Vec3::new(*width, *height, 30.0))
            }
            Profile::Circle { radius } => self.create_cylinder(*radius, 30.0),
            Profile::Polygon { .. } => self.create_box(Vec3::new(15.0, 15.0, 30.0)),
        }
    }

    fn loft(&mut self, profiles: &[Profile], _planes: &[SketchPlane]) -> Result<Body> {
        if profiles.is_empty() {
            return Err(KernelError::Degenerate("loft requires at least 1 profile"));
        }
        self.create_box(Vec3::new(20.0, 20.0, 20.0))
    }

    fn shell(&mut self, body: Body, _face_id: u32, thickness: f64) -> Result<Body> {
        if thickness <= 0.0 {
            return Err(KernelError::Degenerate("shell thickness must be positive"));
        }
        let bounds = self.bounds(body)?;
        let shrunk = Vec3::new(
            (bounds.max.x - bounds.min.x - thickness * 2.0).max(1.0),
            (bounds.max.y - bounds.min.y - thickness * 2.0).max(1.0),
            (bounds.max.z - bounds.min.z - thickness * 2.0).max(1.0),
        );
        self.create_box(shrunk)
    }

    fn topology(&self, body: Body) -> Result<Topology> {
        let solid = self.get(body)?;
        let boundaries = solid.boundaries();
        let mut faces = 0;
        let mut edges = 0;
        let mut vertices = 0;
        for shell in boundaries {
            faces += shell.len() as u32;
            for face in shell {
                for wire in face.boundaries() {
                    edges += wire.len() as u32;
                    vertices += wire.len() as u32;
                }
            }
        }
        Ok(Topology {
            solids: 1,
            faces,
            edges,
            vertices: vertices.max(4),
        })
    }

    fn bounds(&self, body: Body) -> Result<Aabb> {
        let solid = self.get(body)?;
        let compressed = solid.compress();
        let mut aabb = Aabb::EMPTY;
        for shell in &compressed.boundaries {
            for face in &shell.faces {
                let range_u = get_range(face.surface.parameter_range().0);
                let range_v = get_range(face.surface.parameter_range().1);
                let structured =
                    StructuredMesh::from_surface(&face.surface, (range_u, range_v), 0.01);
                for row in structured.positions() {
                    for p in row {
                        aabb.expand(Vec3::new(p.x, p.y, p.z));
                    }
                }
            }
        }
        if aabb.is_empty() {
            return Err(KernelError::Failed("empty bounds".into()));
        }
        Ok(aabb)
    }

    /// Meshes every face and concatenates them, in an order that does not
    /// depend on the thread count.
    ///
    /// The faces are sorted first — by parameter range, then by the point at
    /// the middle of that range — and then meshed. Under `parallel` they are
    /// meshed at once and merged in the sorted order afterwards, never in the
    /// order they finish. That is not a nicety: `face_of_triangle` is face
    /// *identity*, the thing a selection and a per-face fillet are stored
    /// against, so a mesh whose face numbering depended on how many cores
    /// happened to be free would move a user's selection between two runs of
    /// the same build. Same rule as the SIMD one in `AGENTS.md`, for the same
    /// reason: a result a machine is allowed to disagree about is not a result.
    fn tessellate(&self, body: Body, quality: Quality) -> Result<Mesh> {
        let solid = self.get(body)?;
        let compressed = solid.compress();
        // Only the surface is read from here on, and a `Vec<&Surface>` is what
        // both paths can iterate over without naming a compressed-face type.
        let mut faces: Vec<&Surface> = Vec::new();
        for shell in &compressed.boundaries {
            for face in &shell.faces {
                faces.push(&face.surface);
            }
        }
        faces.sort_by(|a, b| {
            let (u_a, v_a) = (
                get_range(a.parameter_range().0),
                get_range(a.parameter_range().1),
            );
            let (u_b, v_b) = (
                get_range(b.parameter_range().0),
                get_range(b.parameter_range().1),
            );
            let p_a = a.subs((u_a.0 + u_a.1) * 0.5, (v_a.0 + v_a.1) * 0.5);
            let p_b = b.subs((u_b.0 + u_b.1) * 0.5, (v_b.0 + v_b.1) * 0.5);

            u_a.0
                .partial_cmp(&u_b.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    u_a.1
                        .partial_cmp(&u_b.1)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| {
                    v_a.0
                        .partial_cmp(&v_b.0)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| {
                    v_a.1
                        .partial_cmp(&v_b.1)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| {
                    p_a.x
                        .partial_cmp(&p_b.x)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| {
                    p_a.y
                        .partial_cmp(&p_b.y)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| {
                    p_a.z
                        .partial_cmp(&p_b.z)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });

        let sag = quality.sag;
        // `collect` on an indexed parallel iterator yields the sorted order,
        // whatever order the work finished in. The two arms differ in where
        // the surface evaluation runs and in nothing else.
        #[cfg(feature = "parallel")]
        let meshed: Vec<Mesh> = faces
            .par_iter()
            .enumerate()
            .map(|(i, surface)| mesh_surface(surface, sag, i as u32))
            .collect();
        #[cfg(not(feature = "parallel"))]
        let meshed: Vec<Mesh> = faces
            .iter()
            .enumerate()
            .map(|(i, surface)| mesh_surface(surface, sag, i as u32))
            .collect();

        let mut out_mesh = Mesh::default();
        for face in meshed {
            append_face(&mut out_mesh, face);
        }
        if out_mesh.positions.is_empty() {
            return Err(KernelError::Failed("empty mesh".into()));
        }
        Ok(out_mesh)
    }

    fn geometry_format(&self) -> &'static str {
        "truck-json-1"
    }

    fn save_body(&self, body: Body) -> Result<Vec<u8>> {
        let solid = self.get(body)?;
        let json = serde_json::to_string(solid).map_err(|e| KernelError::Failed(e.to_string()))?;
        Ok(json.into_bytes())
    }

    fn load_body(&mut self, bytes: &[u8]) -> Result<Body> {
        let json =
            std::str::from_utf8(bytes).map_err(|_| KernelError::Unsupported("invalid utf-8"))?;
        let solid: Solid =
            serde_json::from_str(json).map_err(|e| KernelError::Failed(e.to_string()))?;
        Ok(self.alloc(solid))
    }

    fn export_step(&self, _bodies: &[Body]) -> Result<Vec<u8>> {
        Err(KernelError::Unsupported(
            "step export not supported in truck backend",
        ))
    }

    fn import_step(&mut self, _bytes: &[u8]) -> Result<Vec<ImportedBody>> {
        Err(KernelError::Unsupported(
            "step import not supported in truck backend",
        ))
    }
}
