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
    Aabb, Body, BooleanOp, Capability, GeometryKernel, Import, KernelError, Mat4, Mesh, Profile,
    Quality, Result, SketchPlane, Tolerance, Topology, Vec3,
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

    /// The distinct edges of a solid, counted the way [`number_edges`] numbers
    /// them — so an id this accepts is an id a mesh of the same body could have
    /// reported, which is what makes the check worth making on a backend that
    /// then declines the blend anyway.
    fn distinct_edges(&self, body: Body) -> Result<u32> {
        let solid = self.get(body)?;
        let mut faces: Vec<([f64; 7], &Face)> = solid
            .boundaries()
            .iter()
            .flat_map(|shell| shell.face_iter())
            .map(|face| (face_key(face), face))
            .collect();
        faces.sort_by(|a, b| key_order(&a.0, &b.0));
        Ok(number_edges(&faces).len() as u32)
    }

    /// The argument half of a per-edge blend. Checked before the decline, so
    /// that "this build cannot blend" and "that is not an edge of this body"
    /// remain two different answers to two different mistakes.
    fn check_edge_ids(&self, body: Body, edges: &[u32]) -> Result<()> {
        if edges.is_empty() {
            return Err(KernelError::Degenerate(
                "no edges were named, and a per-edge blend does not mean all of them",
            ));
        }
        let count = self.distinct_edges(body)?;
        if edges.iter().any(|e| *e >= count) {
            return Err(KernelError::Degenerate(
                "an edge id is not an edge of this body",
            ));
        }
        Ok(())
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

/// A solid's distinct edges, numbered — the id space `Mesh::edge_of_line` and
/// the per-edge blends both speak.
///
/// It is not [`Topology::edges`], and the difference is not a rounding: that
/// counts each edge *once per face that uses it*, so a cube reports 24 where it
/// has 12. A blend names an edge, not a face's use of one, so the numbering here
/// is over distinct edges and a shared edge gets one id.
type EdgeIds = HashMap<EdgeID, u32>;

/// Numbered in the order the *sorted* faces walk them, which is the same
/// determinism argument the sort itself exists for: an id that depended on which
/// thread finished first would be an id a saved selection could not survive.
fn number_edges(faces: &[([f64; 7], &Face)]) -> EdgeIds {
    let mut ids = EdgeIds::new();
    for (_, face) in faces {
        for wire in face.absolute_boundaries() {
            for edge in wire.edge_iter() {
                let next = ids.len() as u32;
                ids.entry(edge.id()).or_insert(next);
            }
        }
    }
    ids
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
fn mesh_face(face: &Face, sag: f64, face_idx: u32, edge_ids: &EdgeIds) -> Mesh {
    let mut out = Mesh::default();
    let shell: Shell = vec![face.clone()].into();
    let meshed = shell.triangulation(sag);

    // The edges first, and from the same triangulation as the triangles: the
    // polylines below *are* what divided the boundary curves, so the wireframe
    // and the surface meet on shared points rather than on two approximations
    // of one curve that disagree by the sag.
    //
    // The *identity* of each edge comes from the untriangulated face zipped
    // alongside, not from the meshed one: `triangulation` rebuilds the wires
    // with polylines for curves, and an id read off the copy is an id in a
    // numbering nothing else shares. The two walks have the same shape, which
    // is what makes the zip sound.
    for meshed_face in meshed.face_iter() {
        for (wire, source_wire) in meshed_face
            .absolute_boundaries()
            .iter()
            .zip(face.absolute_boundaries().iter())
        {
            for (edge, source_edge) in wire.edge_iter().zip(source_wire.edge_iter()) {
                let polyline = edge.curve();
                let base = out.line_positions.len() as u32;
                for p in polyline.iter() {
                    out.line_positions
                        .push([p.x as f32, p.y as f32, p.z as f32]);
                }
                let edge_id = edge_ids.get(&source_edge.id()).copied().unwrap_or(u32::MAX);
                for i in 1..polyline.len() as u32 {
                    out.line_indices.push(base + i - 1);
                    out.line_indices.push(base + i);
                    out.edge_of_line.push(edge_id);
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
    // Edge ids are the solid's, not the face's, so they are appended as they
    // are — the offset above is for vertex indices only.
    out.edge_of_line.extend(face.edge_of_line);
}

/// The face a [`Profile`] describes, on the XY plane at z = 0.
///
/// This is what `extrude`, `revolve` and `sweep` all needed and none of them
/// had: until 2026-09-14 each of them matched on the profile and called
/// `create_box` or `create_cylinder`, so a `Polygon` — the only thing the
/// app's sketcher produces — was thrown away and replaced with a 20 x 20 slab.
///
/// Counter-clockwise, always. A sketch is drawn in whichever direction the
/// user's mouse went, and a clockwise wire attaches a plane whose normal points
/// the other way: sweeping it along +Z then gives a solid that is inside out.
fn profile_wire(profile: &Profile, plane: &SketchPlane) -> Result<Wire> {
    profile.validate()?;
    let at = |u: f64, v: f64| {
        let p = plane.origin + plane.x_axis * u + plane.y_axis * v;
        Point3::new(p.x, p.y, p.z)
    };
    let corners = |points: &[(f64, f64)]| {
        let verts: Vec<Vertex> = points
            .iter()
            .map(|(u, v)| builder::vertex(at(*u, *v)))
            .collect();
        let mut wire = Wire::new();
        for i in 0..verts.len() {
            wire.push_back(builder::line(&verts[i], &verts[(i + 1) % verts.len()]));
        }
        wire
    };
    let wire = match profile {
        Profile::Rectangle { width, height } => {
            let (w, h) = (width / 2.0, height / 2.0);
            corners(&[(-w, -h), (w, -h), (w, h), (-w, h)])
        }
        Profile::Circle { radius } => {
            let normal = plane.x_axis.cross(plane.y_axis);
            let rim = builder::vertex(at(*radius, 0.0));
            builder::rsweep(
                &rim,
                Point3::new(plane.origin.x, plane.origin.y, plane.origin.z),
                Vector3::new(normal.x, normal.y, normal.z),
                FULL_TURN,
            )
        }
        Profile::Polygon { vertices } => {
            let mut points: Vec<(f64, f64)> = vertices.clone();
            if profile.signed_area() < 0.0 {
                points.reverse();
            }
            corners(&points)
        }
    };
    Ok(wire)
}

/// The face that wire bounds, facing along the plane's normal.
///
/// Asking the face which way it faces, rather than assuming. Each profile kind
/// is built by a different route — a polygon from its own vertices, a disc from
/// `rsweep` about the normal — and the routes do not agree on which way round
/// they go. It did not matter while `tsweep` was the only consumer, because
/// sweeping a face along its own normal comes out right either way. `rsweep` is
/// not so forgiving: a revolved disc enclosed **-393.2** where the answer is
/// 394.8, inside out, while a revolved rectangle beside it was correct.
///
/// Measuring the wire's winding does not settle it either — a full-turn
/// `rsweep` produces a circle whose two vertices have no winding to measure. The
/// face's own oriented surface does.
fn profile_face_on(profile: &Profile, plane: &SketchPlane) -> Result<Face> {
    let wire = profile_wire(profile, plane)?;
    let face =
        builder::try_attach_plane(&[wire]).map_err(|e| KernelError::Failed(format!("{e:?}")))?;
    let wanted = plane.x_axis.cross(plane.y_axis);
    let normal = match face.oriented_surface() {
        Surface::Plane(p) => p.normal(),
        // Every profile here attaches a plane; anything else is not this
        // function's to interpret, so it is left as it came.
        _ => return Ok(face),
    };
    let along = normal.x * wanted.x + normal.y * wanted.y + normal.z * wanted.z;
    if along < 0.0 {
        Ok(face.inverse())
    } else {
        Ok(face)
    }
}

fn profile_face(profile: &Profile) -> Result<Face> {
    profile_face_on(profile, &SketchPlane::default())
}

/// Turns a solid the right way out, by weighing it.
///
/// `rsweep`'s sense depends on which side of the axis the profile sits: the same
/// face revolved about a line 5 to its left and 5 to its right comes out facing
/// outwards once and inwards once. So neither always inverting nor never
/// inverting is right, and the only honest test is the answer itself — a coarse
/// triangulation's signed volume, which is negative exactly when every normal
/// points at the interior.
///
/// Measured on 2026-09-14: a disc revolved about a line 5 away enclosed
/// **-393.2** where the answer is 394.8, while a rectangle revolved about a line
/// 5 the other way was already correct. An inside-out solid is lit from within
/// and, since nothing here is back-face culled, looks like a hole.
fn faced_outwards(mut solid: Solid) -> Solid {
    let mut volume = 0.0;
    let mesh = solid.triangulation(BOUNDS_SAG).to_polygon();
    let points = mesh.positions();
    for face in mesh.tri_faces() {
        let (a, b, c) = (
            points[face[0].pos],
            points[face[1].pos],
            points[face[2].pos],
        );
        volume += a.x * (b.y * c.z - b.z * c.y) - a.y * (b.x * c.z - b.z * c.x)
            + a.z * (b.x * c.y - b.y * c.x);
    }
    if volume < 0.0 {
        solid.not();
    }
    solid
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

    /// Four `false`s, and every one of them is a decline this backend already
    /// makes in words — the query says the same thing before the user presses
    /// the button rather than after.
    ///
    /// `Blend` is one answer for four methods because there is one missing
    /// thing behind them: a rolling-ball surface, which `truck` has no builder
    /// for. `Shell` wants an offset surface, and it has none of that either.
    /// `BentSweep` and `MultiSectionLoft` are both really this backend's
    /// boolean: following a polyline means unioning the segments, and a third
    /// section means stitching two shells, and the coincident-face case both
    /// are made of is the one it declines.
    ///
    /// **`EdgeIdentity` is the `true` in the list**, and it is the one worth
    /// saying out loud: this backend cannot blend an edge and can still *name*
    /// one. `number_edges` numbers the solid's distinct edges and `mesh_face`
    /// reads the id off the untriangulated face, so a caller that only wants to
    /// tell a user which edge they are hovering gets an answer here. A single
    /// "can this do edges" bit would have had to choose, and either choice is
    /// wrong about half of what this backend does.
    ///
    /// What is *not* here is the boolean. It declines a body with a pole, and
    /// two boxes sharing a coplanar face, and that is a property of the
    /// operands rather than of the build — see [`Capability`].
    fn supports(&self, cap: Capability) -> bool {
        match cap {
            Capability::Blend
            | Capability::Shell
            | Capability::BentSweep
            | Capability::MultiSectionLoft
            | Capability::StepImport
            | Capability::StepExport => false,
            Capability::EdgeIdentity => true,
        }
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
        // Operands that share no interior are decided here rather than handed
        // to `truck-shapeops`, because it gets one of the three wrong and does
        // so silently.
        //
        // `Difference` is `and` against an inverted operand (see below), and
        // `truck_shapeops::and` answers with an **empty solid** when the two
        // boundaries never meet — which is right for an intersection and
        // exactly wrong for a difference, where A lies wholly inside the
        // inverted B and the answer is A. Measured before this existed: a
        // 2 mm cube minus an identical cube 50 mm away returned `Ok` carrying
        // a body with no faces, empty bounds and no mesh. A user who drags a
        // drill off the edge of the part watches the part disappear, and
        // nothing in the suite asked.
        //
        // Disjoint *bounding boxes* are what is tested, so this fires only
        // when the operands provably share no interior point. Boxes that
        // overlap in a set of zero volume — touching on a face, an edge or a
        // corner — count as disjoint, which is right: a solid is a closed
        // regular set and removing a measure-zero slice of its boundary
        // leaves it alone. Overlapping boxes prove nothing and are left to
        // `truck-shapeops`, so this is a shortcut on the certain half and
        // never a guess on the other.
        let overlap = self.bounds(a)?.intersection(&self.bounds(b)?);
        let size = overlap.size();
        let separable = overlap.is_empty() || size.x <= 0.0 || size.y <= 0.0 || size.z <= 0.0;
        if separable {
            match op {
                // A minus something that does not touch it is A.
                BooleanOp::Difference => return Ok(self.alloc(self.get(a)?.clone())),
                // Empty, and there is no empty `Body` in this contract — so it
                // is refused rather than answered with something a caller
                // could mistake for geometry. `conformance` accepts "empty or
                // refused" and this is the honest half of that.
                BooleanOp::Intersection => {
                    return Err(KernelError::Failed(
                        "the operands share no volume, and there is no empty body".into(),
                    ));
                }
                // Not shortcut. Two solids meeting on a face are a real merge
                // with a real seam to build, and `truck_shapeops::or` already
                // answers the wholly-separate case correctly.
                BooleanOp::Union => {}
            }
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
        // A copy of the body is what this returned until 2026-09-14, and every
        // check it passed asked only whether a *different handle* came back with
        // bounds and a mesh. A user pressed Fillet and nothing happened, on the
        // backend the browser runs. Blending an edge needs a rolling-ball
        // surface, which `truck` has no builder for; the OpenCASCADE build has
        // `BRepFilletAPI_MakeFillet`.
        Err(KernelError::Unsupported(
            "this backend has no edge blending, so it cannot fillet",
        ))
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
        // As with `fillet`: a copy, until 2026-09-14. Cutting a corner flat is
        // the easier of the two — a plane, and a boolean against it — but the
        // boolean this backend would need is the coincident-face case it
        // declines, so claiming it would be claiming the same thing twice.
        Err(KernelError::Unsupported(
            "this backend has no edge blending, so it cannot chamfer",
        ))
    }

    /// Declined with `fillet`, and for the same missing surface. The arguments
    /// are still checked first: a caller debugging its edge ids should get the
    /// same answer here as on the backend that can blend, so that "this build
    /// cannot" and "that id is not an edge" stay two different sentences.
    fn fillet_edges(&mut self, body: Body, edges: &[u32], radius: f64) -> Result<Body> {
        self.check_edge_ids(body, edges)?;
        self.fillet(body, radius)
    }

    fn chamfer_edges(&mut self, body: Body, edges: &[u32], distance: f64) -> Result<Body> {
        self.check_edge_ids(body, edges)?;
        self.chamfer(body, distance)
    }

    /// The profile, swept along +Z — which is what the trait says and what this
    /// did not do. It called `create_box` or `create_cylinder` with the
    /// profile's two numbers, so a polygon became a 20 x 20 slab and every
    /// extrusion straddled the plane it was drawn on instead of standing on it.
    fn extrude(&mut self, profile: &Profile, distance: f64) -> Result<Body> {
        if distance <= 0.0 {
            return Err(KernelError::Degenerate("extrude distance must be positive"));
        }
        let face = profile_face(profile)?;
        let solid = builder::tsweep(&face, Vector3::unit_z() * distance);
        Ok(self.alloc(solid))
    }

    /// The profile, turned about the axis it was given. The axis was ignored
    /// entirely until 2026-09-14, and the profile with it: a rectangle became a
    /// cylinder of the rectangle's own two numbers, at the origin, whatever axis
    /// the caller named.
    ///
    /// **A full turn leaves a seam**, and that is worth knowing at the call
    /// site: one closed edge that begins and ends at the same vertex, which
    /// `truck-shapeops` cannot split. `create_cylinder` sweeps a disc along the
    /// axis instead of revolving a rectangle about it for exactly this reason —
    /// the note there records a revolved cylinder taking 25 seconds to subtract
    /// and returning the plug rather than the plate. So a revolved solid is
    /// marked `singular`: it is a body this backend can draw and measure and
    /// will not put into a boolean.
    fn revolve(
        &mut self,
        profile: &Profile,
        axis_origin: Vec3,
        axis_dir: Vec3,
        angle_rad: f64,
    ) -> Result<Body> {
        if angle_rad <= 0.0 {
            return Err(KernelError::Degenerate("revolve angle must be positive"));
        }
        if axis_dir.length() <= 0.0 {
            return Err(KernelError::Degenerate("revolve axis has no direction"));
        }
        let face = profile_face(profile)?;
        let solid = faced_outwards(builder::rsweep(
            &face,
            Point3::new(axis_origin.x, axis_origin.y, axis_origin.z),
            Vector3::new(axis_dir.x, axis_dir.y, axis_dir.z),
            Rad(angle_rad),
        ));
        let body = self.alloc(solid);
        self.singular.insert(body);
        Ok(body)
    }

    /// The profile, carried along a **straight** path. It ignored the path
    /// completely until 2026-09-14 and made a solid 30 long whatever was asked
    /// for.
    ///
    /// A bent path is refused rather than approximated. `builder::tsweep` takes
    /// a vector, not a spine: following a polyline would mean sweeping each
    /// segment and unioning the results, and this backend's boolean declines
    /// exactly the coincident-face case those unions are made of. An honest
    /// `Unsupported` sends a caller to the OpenCASCADE build, which does have a
    /// pipe.
    fn sweep(&mut self, profile: &Profile, path_points: &[Vec3]) -> Result<Body> {
        if path_points.len() < 2 {
            return Err(KernelError::Degenerate(
                "sweep path requires at least 2 points",
            ));
        }
        if path_points.len() > 2 {
            return Err(KernelError::Unsupported(
                "this backend sweeps along a straight path only",
            ));
        }
        let along = path_points[1] - path_points[0];
        if along.length() <= 0.0 {
            return Err(KernelError::Degenerate("a sweep path of no length"));
        }
        let face = profile_face(profile)?;
        let solid = builder::tsweep(&face, Vector3::new(along.x, along.y, along.z));
        Ok(self.alloc(solid))
    }

    /// Two profiles, on their two planes, joined edge by edge — where it used
    /// to answer any loft at all with a 20-cube.
    ///
    /// Two and no more: `try_wire_homotopy` pairs the edges of one wire with the
    /// edges of another, so both profiles must be the same *kind* of outline as
    /// well as the same count. A chain of three would be two shells to stitch,
    /// and stitching is what this backend's boolean is worst at.
    fn loft(&mut self, profiles: &[Profile], planes: &[SketchPlane]) -> Result<Body> {
        if profiles.is_empty() {
            return Err(KernelError::Degenerate("loft requires at least 1 profile"));
        }
        if profiles.len() != 2 {
            return Err(KernelError::Unsupported(
                "this backend lofts between two profiles only",
            ));
        }
        if planes.len() < profiles.len() {
            return Err(KernelError::Degenerate("a loft needs a plane per profile"));
        }
        let wires = [
            profile_wire(&profiles[0], &planes[0])?,
            profile_wire(&profiles[1], &planes[1])?,
        ];
        let mut shell = builder::try_wire_homotopy(&wires[0], &wires[1]).map_err(|e| {
            KernelError::Failed(format!(
                "the two profiles could not be joined edge to edge: {e:?}"
            ))
        })?;
        // The walls, and then the two ends: the first cap faces back along the
        // loft and so goes in inverted, which is what makes the shell closed
        // rather than a tube.
        let caps = [
            builder::try_attach_plane(&[wires[0].clone()])
                .map_err(|e| KernelError::Failed(format!("{e:?}")))?
                .inverse(),
            builder::try_attach_plane(&[wires[1].clone()])
                .map_err(|e| KernelError::Failed(format!("{e:?}")))?,
        ];
        shell.push(caps[0].clone());
        shell.push(caps[1].clone());
        let solid = Solid::try_new(vec![shell])
            .map_err(|e| KernelError::Failed(format!("the loft did not close: {e:?}")))?;
        Ok(self.alloc(solid))
    }

    /// Refused, because this backend has no offset surface to build a wall out
    /// of.
    ///
    /// What it did until 2026-09-14 was return a *smaller box*: not a hollow
    /// solid, not the body that was asked about, and not even the same shape —
    /// a hollowed sphere came back as a cube. Nothing about it was true, and
    /// every check it passed was a check that only asked whether a body came
    /// back. Offsetting a B-rep surface is a feature, not a workaround, and
    /// `truck` does not have one: the OpenCASCADE build does
    /// (`BRepOffsetAPI_MakeThickSolid`).
    fn shell(&mut self, body: Body, _face_id: u32, thickness: f64) -> Result<Body> {
        if thickness <= 0.0 {
            return Err(KernelError::Degenerate("shell thickness must be positive"));
        }
        // Still an error for a handle that is not ours, so a caller cannot tell
        // "no such body" from "not implemented" by accident.
        self.get(body)?;
        Err(KernelError::Unsupported(
            "this backend cannot offset a surface, so it cannot hollow a solid",
        ))
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

        // Built once, on this thread, from the sorted faces — so every face's
        // mesh reports the same id for a shared edge whichever thread meshes it.
        let edge_ids = number_edges(&faces);

        // `collect` on an indexed parallel iterator yields the sorted order,
        // whatever order the work finished in. The two arms differ in where
        // the face is meshed and in nothing else.
        #[cfg(feature = "parallel")]
        let meshed: Vec<Mesh> = faces
            .par_iter()
            .enumerate()
            .map(|(i, (_, face))| mesh_face(face, sag, i as u32, &edge_ids))
            .collect();
        #[cfg(not(feature = "parallel"))]
        let meshed: Vec<Mesh> = faces
            .iter()
            .enumerate()
            .map(|(i, (_, face))| mesh_face(face, sag, i as u32, &edge_ids))
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

    fn import_step(&mut self, _bytes: &[u8]) -> Result<Import> {
        Err(KernelError::Unsupported(
            "step import not supported in truck backend",
        ))
    }
}
