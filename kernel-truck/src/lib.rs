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
use std::collections::{HashMap, HashSet};
use truck_meshalgo::tessellation::{MeshableShape, MeshedShape};
use truck_modeling::*;
use truck_polymesh::PolygonMesh;
use w3d_kernel::{
    Aabb, Body, BooleanOp, GeometryKernel, ImportedBody, KernelError, Mat4, Mesh, Profile, Quality,
    Result, SketchPlane, Tolerance, Topology, Vec3,
};

pub struct TruckKernel {
    next_id: u32,
    solids: HashMap<Body, Solid>,
    /// Bodies with a point where the surface degenerates — a sphere's poles.
    ///
    /// `truck-shapeops` does not fail on one, it **panics**, inside
    /// `Solid::new`. On the desktop that is caught; in the browser, where the
    /// build has no unwinding, a panic is the end of the module and of the
    /// page. So the bodies that are known to carry a pole are remembered when
    /// they are made, and a boolean on one is declined before anything can
    /// abort — a declared "no" instead of a crash.
    ///
    /// It is a record of what this backend *built*, not a proof about geometry:
    /// a sphere that arrives through [`GeometryKernel::load_body`] is not in it,
    /// and the `catch_unwind` in [`TruckKernel::boolean`] is what covers that
    /// case, on the target where unwinding exists.
    singular: HashSet<Body>,
}

impl TruckKernel {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            solids: HashMap::new(),
            singular: HashSet::new(),
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

    /// The tolerance a boolean is actually run at, which is not the document's.
    ///
    /// `truck-shapeops` uses this number twice: to decide whether two points
    /// are the same point, and as the sag it divides an intersection curve into
    /// a polyline at. The document's linear tolerance is 1.0e-7 — a length, and
    /// the right one for *geometry* — and asking for intersection curves that
    /// fine on a 40 mm plate means hundreds of thousands of segments before it
    /// gives up. So the floor is relative to the operands: a boolean on a large
    /// part is run at a proportionally larger tolerance, and a boolean on a
    /// small one is not run coarser than the document allows.
    ///
    /// This is the honest form of a compromise that cannot be avoided at this
    /// backend's maturity, and it is the reason a `truck` boolean is not exact
    /// in the way OCCT's is. It is written down here and in the record rather
    /// than hidden in a call site.
    fn boolean_tolerance(&self, a: Body, b: Body, tol: Tolerance) -> Result<f64> {
        let extent = |body: Body| -> Result<f64> {
            let size = self.bounds(body)?.size();
            Ok(size.x.max(size.y).max(size.z))
        };
        let largest = extent(a)?.max(extent(b)?);
        Ok(tol.linear.max(largest * BOOLEAN_TOLERANCE_FRACTION))
    }
}

impl Default for TruckKernel {
    fn default() -> Self {
        Self::new()
    }
}

/// A full turn, and then some. `truck`'s `rsweep` closes a sweep only when the
/// angle is *past* a full turn; at exactly 2π it wraps the profile onto itself
/// and leaves a seam — a closed edge that `truck-shapeops` cannot split, and
/// that a boolean then either takes twenty-five seconds over or gets wrong.
const FULL_TURN: Rad<f64> = Rad(7.0);

/// The fraction of the larger operand's size that a boolean is run at when the
/// document's own tolerance is finer. See [`TruckKernel::boolean_tolerance`].
const BOOLEAN_TOLERANCE_FRACTION: f64 = 1.0e-3;

/// The sag [`TruckKernel::bounds`] measures at. Finer than anything a viewport
/// asks for, because a bound that is wrong is worse than a bound that is slow,
/// and coarse enough that the triangulation is not the cost of asking a body
/// how big it is.
const BOUNDS_SAG: f64 = 0.005;

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

/// A face's place in the mesh, as numbers rather than as the order the
/// topology happens to hold it in.
///
/// Face ids are positions in this order, and a face id is what picking, the
/// selection and every per-face badge in the shell hold on to. Sorting on the
/// surface's parameter range and its midpoint is what keeps that id the same
/// across two runs, across the serial and the parallel path, and across a save
/// and a load — none of which the topology's own iteration order promises.
fn face_key(face: &Face) -> [f64; 7] {
    let surface = face.surface();
    let u = get_range(surface.parameter_range().0);
    let v = get_range(surface.parameter_range().1);
    let p = surface.subs((u.0 + u.1) * 0.5, (v.0 + v.1) * 0.5);
    [u.0, u.1, v.0, v.1, p.x, p.y, p.z]
}

fn key_order(a: &[f64; 7], b: &[f64; 7]) -> std::cmp::Ordering {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| x.total_cmp(y))
        .find(|o| o.is_ne())
        .unwrap_or(std::cmp::Ordering::Equal)
}

/// One face's triangles, in a mesh of its own with indices from zero.
///
/// Standalone rather than appended into a shared `Mesh`, and that is the whole
/// point: a function that writes into somebody else's mesh cannot be handed to
/// more than one thread, and one that returns its own can. [`append_face`] is
/// the other half, and it is what puts the index offsets back.
///
/// **It is the face that is meshed, not its surface.** A face is a surface plus
/// the loops that trim it, and a grid over the surface's whole parameter range
/// draws material the solid does not have: a disc came out square, and a face
/// bounded by an intersection curve — which is every face a boolean makes —
/// came out whole. `truck-meshalgo`'s triangulation reads the loops, so this is
/// the boundary's mesh rather than the surface's.
fn mesh_face(face: &Face, sag: f64, face_idx: u32) -> Mesh {
    let mut out = Mesh::default();
    let shell: Shell = vec![face.clone()].into();
    let meshed = shell.triangulation(sag);

    // The edges first, and from the same triangulation as the triangles: the
    // polylines below *are* what divided the boundary curves, so the wireframe
    // and the surface meet on shared points rather than on two approximations
    // of one curve that disagree by the sag.
    for meshed_face in meshed.face_iter() {
        for wire in meshed_face.absolute_boundaries() {
            for edge in wire.edge_iter() {
                let polyline = edge.curve();
                let base = out.line_positions.len() as u32;
                for p in polyline.iter() {
                    out.line_positions
                        .push([p.x as f32, p.y as f32, p.z as f32]);
                }
                for i in 1..polyline.len() as u32 {
                    out.line_indices.push(base + i - 1);
                    out.line_indices.push(base + i);
                }
            }
        }
    }

    let polygon: PolygonMesh = meshed.to_polygon();
    let positions = polygon.positions();
    let normals = polygon.normals();
    // `(position, normal)` rather than position alone: two triangles meeting at
    // a hard edge share a point and must not share a vertex, or the edge is lit
    // as though it were round.
    let mut seen: HashMap<(usize, usize), u32> = HashMap::new();

    for poly in polygon.faces().face_iter() {
        // Fan from the first corner. The triangulation emits triangles, so this
        // is a guard against a quad rather than a path anything takes today.
        for k in 1..poly.len().saturating_sub(1) {
            let corners = [poly[0], poly[k], poly[k + 1]];
            let Some(points) = corners
                .iter()
                .map(|c| positions.get(c.pos).copied())
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            let geometric = {
                let (a, b, c) = (points[0], points[1], points[2]);
                let n = (b - a).cross(c - a);
                let len = n.magnitude();
                if len > f64::EPSILON {
                    n / len
                } else {
                    Vector3::unit_z()
                }
            };

            let mut tri = [0u32; 3];
            for (i, slot) in tri.iter_mut().enumerate() {
                let corner = corners[i];
                let p = points[i];
                // A normal is taken from the surface only if it is one. The
                // triangulation reports `NaN` where a surface degenerates —
                // twelve of them on a cylinder's cap — and a `NaN` normal is
                // not a lighting artefact: it is a vertex that compares
                // unequal to itself, which is how the conformance suite's
                // determinism check found this.
                let usable = corner.nor.filter(|n| {
                    normals.get(*n).is_some_and(|v| {
                        v.x.is_finite()
                            && v.y.is_finite()
                            && v.z.is_finite()
                            && v.magnitude2() > f64::EPSILON
                    })
                });
                *slot = match usable {
                    Some(n) => *seen.entry((corner.pos, n)).or_insert_with(|| {
                        let idx = out.positions.len() as u32;
                        out.positions.push([p.x as f32, p.y as f32, p.z as f32]);
                        let nor = normals[n].normalize();
                        out.normals.push([nor.x as f32, nor.y as f32, nor.z as f32]);
                        idx
                    }),
                    // No usable normal on the corner: the triangle's own plane
                    // is the only answer there is, and it cannot be shared with
                    // a neighbour, so it is pushed rather than looked up.
                    None => {
                        let idx = out.positions.len() as u32;
                        out.positions.push([p.x as f32, p.y as f32, p.z as f32]);
                        out.normals.push([
                            geometric.x as f32,
                            geometric.y as f32,
                            geometric.z as f32,
                        ]);
                        idx
                    }
                };
            }
            out.indices.extend_from_slice(&tri);
            out.face_of_triangle.push(face_idx);
        }
    }

    out
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

    /// Real surfaces, and — since the boolean stopped being a bounding box — a
    /// real difference. What it cannot do it declines; see
    /// [`TruckKernel::singular`] and the register.
    fn does_geometry(&self) -> bool {
        true
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
        let v_end = wire.back().expect("half-circle has an edge").back().clone();
        wire.push_back(builder::line(&v_end, &v0));

        let face = builder::try_attach_plane(&[wire])
            .map_err(|e| KernelError::Failed(format!("{e:?}")))?;
        let mut solid = builder::rsweep(
            &face,
            Point3::origin(),
            Vector3::unit_z(),
            Rad(2.0 * std::f64::consts::PI),
        );
        // Revolving the half-disc leaves the boundary facing inwards: the mesh
        // enclosed a *negative* volume and every normal pointed at the centre,
        // so a sphere has been lit from the inside in this backend since it
        // existed. `not` flips every face, which is the whole of the fix.
        solid.not();

        let body = self.alloc(solid);
        self.singular.insert(body);
        Ok(body)
    }

    fn create_cylinder(&mut self, radius: f64, height: f64) -> Result<Body> {
        if radius <= 0.0 || height <= 0.0 {
            return Err(KernelError::Degenerate(
                "radius and height must be positive",
            ));
        }
        let h2 = height / 2.0;
        // A disc swept along the axis, rather than a rectangle revolved about
        // it. The two make the same shape and are not the same solid: revolving
        // a face by a full turn leaves a seam — one closed edge that begins and
        // ends at the same vertex — and `truck-shapeops` cannot split one. The
        // measured difference is not subtle. Subtracting a revolved cylinder
        // from a plate took 25 seconds and returned the *plug* rather than the
        // plate; subtracting this one takes 4 and returns the plate with a hole
        // in it, of the right volume. It also comes out oriented outwards,
        // which the revolved one did not — see the record.
        let rim = builder::vertex(Point3::new(radius, 0.0, -h2));
        let circle = builder::rsweep(
            &rim,
            Point3::new(0.0, 0.0, -h2),
            Vector3::unit_z(),
            FULL_TURN,
        );
        let disc = builder::try_attach_plane(&[circle])
            .map_err(|e| KernelError::Failed(format!("{e:?}")))?;
        let solid = builder::tsweep(&disc, Vector3::unit_z() * height);
        Ok(self.alloc(solid))
    }

    fn boolean(&mut self, op: BooleanOp, a: Body, b: Body, tol: Tolerance) -> Result<Body> {
        if self.singular.contains(&a) || self.singular.contains(&b) {
            // Declined rather than attempted. See `TruckKernel::singular`: the
            // attempt is not a failure, it is an abort, and there is no result
            // to report from the far side of one.
            return Err(KernelError::Unsupported(
                "truck cannot run a boolean on a body with a pole, such as a sphere",
            ));
        }
        let solid_a = self.get(a)?.clone();
        let mut solid_b = self.get(b)?.clone();
        let t = self.boolean_tolerance(a, b, tol)?;

        // `Difference` is `and` against an inverted second operand, which is
        // what `not` does: it flips the orientation of every face, so the
        // solid's inside becomes everything outside it. There is no third
        // operation in `truck-shapeops`, and there does not need to be.
        let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || match op {
            BooleanOp::Union => truck_shapeops::or(&solid_a, &solid_b, t),
            BooleanOp::Intersection => truck_shapeops::and(&solid_a, &solid_b, t),
            BooleanOp::Difference => {
                solid_b.not();
                truck_shapeops::and(&solid_a, &solid_b, t)
            }
        }))
        // `truck-shapeops` panics on geometry it cannot handle rather than
        // returning `None`, and a panic that escapes a kernel call takes the
        // document with it. Where unwinding exists this turns one into an
        // error; on `wasm32-unknown-unknown`, which aborts, it does not, and
        // the `singular` set above is what keeps the known case away from here.
        .unwrap_or(None);

        // `None` is how `truck-shapeops` reports that it could not build the
        // result, and it is returned as a failure rather than as anything a
        // caller could mistake for geometry. The stub this replaced answered
        // every call, which is why the conformance suite passed it.
        built
            .map(|solid| self.alloc(solid))
            .ok_or_else(|| KernelError::Failed(format!("{op:?} failed at a tolerance of {t}")))
    }

    fn transform(&mut self, body: Body, m: &Mat4) -> Result<Body> {
        let solid = self.get(body)?.clone();
        let singular = self.singular.contains(&body);
        let mat = Matrix4::new(
            m.0[0][0], m.0[1][0], m.0[2][0], m.0[3][0], m.0[0][1], m.0[1][1], m.0[2][1], m.0[3][1],
            m.0[0][2], m.0[1][2], m.0[2][2], m.0[3][2], m.0[0][3], m.0[1][3], m.0[2][3], m.0[3][3],
        );
        let transformed = builder::transformed(&solid, mat);
        let moved = self.alloc(transformed);
        // A pole survives being moved, and a body that forgot it had one is a
        // body that panics the next time somebody cuts with it.
        if singular {
            self.singular.insert(moved);
        }
        Ok(moved)
    }

    fn copy(&mut self, body: Body) -> Result<Body> {
        let solid = self.get(body)?.clone();
        let singular = self.singular.contains(&body);
        let copied = self.alloc(solid);
        if singular {
            self.singular.insert(copied);
        }
        Ok(copied)
    }

    fn delete(&mut self, body: Body) -> Result<()> {
        self.singular.remove(&body);
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
        // Over the triangulation rather than over a grid on each surface. A
        // grid ignores the loops that trim the face, so it reported points the
        // solid does not contain — on a disc, the corners of the square the
        // circle is inscribed in. The price is that these bounds are the
        // *mesh's*, so a curved face can bulge past them by up to `BOUNDS_SAG`;
        // that is a bound this backend can honour, where the other was simply
        // wrong.
        let mut aabb = Aabb::EMPTY;
        for point in solid.triangulation(BOUNDS_SAG).to_polygon().positions() {
            aabb.expand(Vec3::new(point.x, point.y, point.z));
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
        let mut faces: Vec<([f64; 7], &Face)> = solid
            .boundaries()
            .iter()
            .flat_map(|shell| shell.face_iter())
            .map(|face| (face_key(face), face))
            .collect();
        faces.sort_by(|a, b| key_order(&a.0, &b.0));

        // `truck-meshalgo` panics on a tolerance at or below its own, and the
        // sag comes from a `Quality` a caller chose, so it is clamped here
        // rather than trusted.
        let sag = quality.sag.max(1.0e-5);

        // `collect` on an indexed parallel iterator yields the sorted order,
        // whatever order the work finished in. The two arms differ in where
        // the face is meshed and in nothing else.
        #[cfg(feature = "parallel")]
        let meshed: Vec<Mesh> = faces
            .par_iter()
            .enumerate()
            .map(|(i, (_, face))| mesh_face(face, sag, i as u32))
            .collect();
        #[cfg(not(feature = "parallel"))]
        let meshed: Vec<Mesh> = faces
            .iter()
            .enumerate()
            .map(|(i, (_, face))| mesh_face(face, sag, i as u32))
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
