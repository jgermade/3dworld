//! The OpenCASCADE backend.
//!
//! The only crate in this workspace that contains `unsafe`, and it contains it
//! for one reason: everything here is a call across `native/w3d_occt.h`. That
//! header is the specification of what an OCCT build must keep exported, one
//! entry point per [`GeometryKernel`] method plus what C needs and Rust does
//! not — see its comments. Neither file says how many there are any more; both
//! said, and both were wrong.
//!
//! Nothing OCCT-shaped reaches Rust. Error codes become [`KernelError`],
//! meshes are copied out and freed on the C++ side, and `TopoDS_Shape` never
//! appears at all.

use std::ffi::{CStr, c_char};
use w3d_kernel::{
    Aabb, Body, BooleanOp, GeometryKernel, KernelError, Mat4, Mesh, Profile, Quality, Result,
    SketchPlane, Tolerance, Topology, Vec3,
};

const OK: i32 = 0;
const ERR_UNKNOWN_BODY: i32 = 1;
const ERR_DEGENERATE: i32 = 2;
const ERR_UNSUPPORTED: i32 = 3;

/// Serialised geometry owned by the C++ side until `w3d_occt_bytes_free`.
#[repr(C)]
struct RawBytes {
    data: *const u8,
    len: u32,
    owner: *mut core::ffi::c_void,
}

impl RawBytes {
    const fn empty() -> Self {
        Self {
            data: core::ptr::null(),
            len: 0,
            owner: core::ptr::null_mut(),
        }
    }
}

/// What one STEP file held, owned by the C++ side until
/// `w3d_occt_import_free`: a body per solid placement, and the assembly tree
/// they sat in. A STEP file holds any number of solids and nobody knows how
/// many until it has been read, so this is the one call that answers with a
/// list — and, since the tree's interior nodes carry no solid, with two.
///
/// Mirrors `W3dOcctImport` in `native/w3d_occt.h`, field for field.
#[repr(C)]
struct RawImport {
    ids: *const u32,
    names: *const *const c_char,
    parents: *const u32,
    len: u32,
    assembly_names: *const *const c_char,
    assembly_parents: *const u32,
    assemblies: u32,
    owner: *mut core::ffi::c_void,
}

/// A profile and the plane it sits on, as the shim takes it.
///
/// Mirrors `W3dOcctProfile` in `native/w3d_occt.h`, field for field. The
/// `vertices` pointer borrows a buffer the caller keeps alive for the length of
/// the call and no longer, which is why [`RawProfile::new`] returns it beside
/// the struct rather than hiding it.
#[repr(C)]
struct RawProfile {
    kind: i32,
    p1: f64,
    p2: f64,
    vertices: *const f64,
    vertex_count: u32,
    origin: [f64; 3],
    x_axis: [f64; 3],
    y_axis: [f64; 3],
}

impl RawProfile {
    /// The descriptor, and the vertex buffer it points into.
    ///
    /// Validated first, in `w3d-kernel`, so that a sketch that is not a region
    /// — fewer than three corners, no area, an outline that crosses itself — is
    /// refused by the same rule in every backend rather than by whatever each
    /// one's sweeper happens to do with it.
    fn new(profile: &Profile, plane: &SketchPlane) -> Result<(Self, Vec<f64>)> {
        profile.validate()?;
        let (kind, p1, p2, flat) = match profile {
            Profile::Rectangle { width, height } => (0, *width, *height, Vec::new()),
            Profile::Circle { radius } => (1, *radius, 0.0, Vec::new()),
            Profile::Polygon { vertices } => (
                2,
                0.0,
                0.0,
                vertices.iter().flat_map(|(u, v)| [*u, *v]).collect(),
            ),
        };
        let raw = Self {
            kind,
            p1,
            p2,
            vertices: if flat.is_empty() {
                core::ptr::null()
            } else {
                flat.as_ptr()
            },
            vertex_count: u32::try_from(flat.len() / 2).unwrap_or(0),
            origin: [plane.origin.x, plane.origin.y, plane.origin.z],
            x_axis: [plane.x_axis.x, plane.x_axis.y, plane.x_axis.z],
            y_axis: [plane.y_axis.x, plane.y_axis.y, plane.y_axis.z],
        };
        Ok((raw, flat))
    }
}

/// The shim's `W3D_OCCT_NO_PARENT`: a solid at the file's root, or an assembly
/// nobody contains.
const NO_PARENT: u32 = u32::MAX;

impl RawImport {
    const fn empty() -> Self {
        Self {
            ids: core::ptr::null(),
            names: core::ptr::null(),
            parents: core::ptr::null(),
            len: 0,
            assembly_names: core::ptr::null(),
            assembly_parents: core::ptr::null(),
            assemblies: 0,
            owner: core::ptr::null_mut(),
        }
    }
}

#[repr(C)]
struct RawMesh {
    positions: *const f32,
    normals: *const f32,
    indices: *const u32,
    face_of_triangle: *const u32,
    line_positions: *const f32,
    line_indices: *const u32,
    vertex_count: u32,
    triangle_count: u32,
    line_vertex_count: u32,
    line_segment_count: u32,
    owner: *mut core::ffi::c_void,
}

impl RawMesh {
    const fn empty() -> Self {
        Self {
            positions: core::ptr::null(),
            normals: core::ptr::null(),
            indices: core::ptr::null(),
            face_of_triangle: core::ptr::null(),
            line_positions: core::ptr::null(),
            line_indices: core::ptr::null(),
            vertex_count: 0,
            triangle_count: 0,
            line_vertex_count: 0,
            line_segment_count: 0,
            owner: core::ptr::null_mut(),
        }
    }
}

enum Context {}

unsafe extern "C" {
    fn w3d_occt_context_new() -> *mut Context;
    fn w3d_occt_context_free(ctx: *mut Context);
    fn w3d_occt_make_box(ctx: *mut Context, sx: f64, sy: f64, sz: f64, out: *mut u32) -> i32;
    fn w3d_occt_make_sphere(ctx: *mut Context, radius: f64, out: *mut u32) -> i32;
    fn w3d_occt_make_cylinder(ctx: *mut Context, radius: f64, height: f64, out: *mut u32) -> i32;
    fn w3d_occt_boolean(
        ctx: *mut Context,
        op: i32,
        a: u32,
        b: u32,
        fuzzy: f64,
        out: *mut u32,
    ) -> i32;
    fn w3d_occt_transform(ctx: *mut Context, body: u32, m34: *const f64, out: *mut u32) -> i32;
    fn w3d_occt_copy(ctx: *mut Context, body: u32, out: *mut u32) -> i32;
    fn w3d_occt_delete(ctx: *mut Context, body: u32) -> i32;
    fn w3d_occt_fillet(ctx: *mut Context, body: u32, radius: f64, out: *mut u32) -> i32;
    fn w3d_occt_chamfer(ctx: *mut Context, body: u32, distance: f64, out: *mut u32) -> i32;
    fn w3d_occt_shell(
        ctx: *mut Context,
        body: u32,
        face_id: u32,
        thickness: f64,
        out: *mut u32,
    ) -> i32;
    fn w3d_occt_extrude(
        ctx: *mut Context,
        profile: *const RawProfile,
        distance: f64,
        out: *mut u32,
    ) -> i32;
    fn w3d_occt_revolve(
        ctx: *mut Context,
        profile: *const RawProfile,
        axis_origin: *const f64,
        axis_dir: *const f64,
        angle_rad: f64,
        out: *mut u32,
    ) -> i32;
    fn w3d_occt_sweep(
        ctx: *mut Context,
        profile: *const RawProfile,
        pts: *const f64,
        pt_count: u32,
        out: *mut u32,
    ) -> i32;
    fn w3d_occt_loft(
        ctx: *mut Context,
        profiles: *const RawProfile,
        profile_count: u32,
        out: *mut u32,
    ) -> i32;
    fn w3d_occt_topology(ctx: *mut Context, body: u32, out4: *mut u32) -> i32;
    fn w3d_occt_bounds(ctx: *mut Context, body: u32, out6: *mut f64) -> i32;
    fn w3d_occt_tessellate(
        ctx: *mut Context,
        body: u32,
        sag: f64,
        angle: f64,
        out: *mut RawMesh,
    ) -> i32;
    fn w3d_occt_mesh_free(mesh: *mut RawMesh);
    fn w3d_occt_save_body(ctx: *mut Context, body: u32, out: *mut RawBytes) -> i32;
    fn w3d_occt_load_body(ctx: *mut Context, data: *const u8, len: u32, out: *mut u32) -> i32;
    fn w3d_occt_bytes_free(bytes: *mut RawBytes);
    fn w3d_occt_export_step(
        ctx: *mut Context,
        bodies: *const u32,
        count: u32,
        out: *mut RawBytes,
    ) -> i32;
    fn w3d_occt_import_step(
        ctx: *mut Context,
        data: *const u8,
        len: u32,
        out: *mut RawImport,
    ) -> i32;
    fn w3d_occt_import_free(imported: *mut RawImport);
    fn w3d_occt_last_error() -> *const c_char;
    fn w3d_occt_live_bodies(ctx: *const Context) -> u32;
}

/// Turns a code from the shim into the contract's error. The message behind a
/// `Failed` is fetched here and nowhere else.
fn check(code: i32, body: Body) -> Result<()> {
    match code {
        OK => Ok(()),
        ERR_UNKNOWN_BODY => Err(KernelError::UnknownBody(body)),
        ERR_DEGENERATE => Err(KernelError::Degenerate("rejected by OpenCASCADE")),
        ERR_UNSUPPORTED => Err(KernelError::Unsupported("not implemented by this backend")),
        _ => {
            // SAFETY: the shim returns a pointer to a thread-local std::string
            // that lives until the next call on this thread, and we copy here.
            let msg = unsafe { CStr::from_ptr(w3d_occt_last_error()) };
            Err(KernelError::Failed(msg.to_string_lossy().into_owned()))
        }
    }
}

/// No handle is at fault — for constructors, where there is no operand to name.
fn check_new(code: i32) -> Result<()> {
    check(code, Body::from_raw(u32::MAX))
}

/// Owns a shape registry on the C++ side and frees it on drop.
///
/// Neither `Send` nor `Sync`, and that is correct rather than conservative:
/// tessellation mutates the shape it meshes, because OCCT attaches the
/// triangulation to the face. So even `&self` methods write, and sharing one
/// kernel across threads would be a race. Parallel meshing means one
/// `OcctKernel` per worker.
pub struct OcctKernel {
    ctx: *mut Context,
}

impl Default for OcctKernel {
    fn default() -> Self {
        Self::new()
    }
}

impl OcctKernel {
    pub fn new() -> Self {
        let ctx = unsafe { w3d_occt_context_new() };
        assert!(!ctx.is_null(), "OpenCASCADE context allocation failed");
        Self { ctx }
    }

    /// How many shapes this kernel's registry holds. For tests that assert the
    /// document is not leaking kernel storage.
    pub fn live_bodies(&self) -> usize {
        unsafe { w3d_occt_live_bodies(self.ctx) as usize }
    }
}

impl Drop for OcctKernel {
    fn drop(&mut self) {
        // Frees every shape this kernel still holds, so a dropped document does
        // not leak OCCT storage even when nothing called collect_garbage.
        unsafe { w3d_occt_context_free(self.ctx) };
    }
}

/// The top three rows of the 4x4, row-major, which is what the shim expects.
fn rows(m: &Mat4) -> [f64; 12] {
    let mut out = [0.0; 12];
    for row in 0..3 {
        out[row * 4..row * 4 + 4].copy_from_slice(&m.0[row]);
    }
    out
}

impl GeometryKernel for OcctKernel {
    fn name(&self) -> &'static str {
        "opencascade"
    }

    /// OpenCASCADE, exactly.
    fn does_geometry(&self) -> bool {
        true
    }

    fn create_box(&mut self, size: Vec3) -> Result<Body> {
        let mut id = 0;
        check_new(unsafe { w3d_occt_make_box(self.ctx, size.x, size.y, size.z, &mut id) })?;
        Ok(Body::from_raw(id))
    }

    fn create_sphere(&mut self, radius: f64) -> Result<Body> {
        let mut id = 0;
        check_new(unsafe { w3d_occt_make_sphere(self.ctx, radius, &mut id) })?;
        Ok(Body::from_raw(id))
    }

    fn create_cylinder(&mut self, radius: f64, height: f64) -> Result<Body> {
        let mut id = 0;
        check_new(unsafe { w3d_occt_make_cylinder(self.ctx, radius, height, &mut id) })?;
        Ok(Body::from_raw(id))
    }

    fn boolean(&mut self, op: BooleanOp, a: Body, b: Body, tol: Tolerance) -> Result<Body> {
        let code = match op {
            BooleanOp::Union => 0,
            BooleanOp::Difference => 1,
            BooleanOp::Intersection => 2,
        };
        let mut id = 0;
        // The document's linear tolerance becomes OCCT's fuzzy value: the
        // distance below which it treats two entities as coincident.
        check(
            unsafe { w3d_occt_boolean(self.ctx, code, a.raw(), b.raw(), tol.linear, &mut id) },
            a,
        )?;
        Ok(Body::from_raw(id))
    }

    fn transform(&mut self, body: Body, m: &Mat4) -> Result<Body> {
        let m34 = rows(m);
        let mut id = 0;
        check(
            unsafe { w3d_occt_transform(self.ctx, body.raw(), m34.as_ptr(), &mut id) },
            body,
        )?;
        Ok(Body::from_raw(id))
    }

    fn copy(&mut self, body: Body) -> Result<Body> {
        let mut id = 0;
        check(
            unsafe { w3d_occt_copy(self.ctx, body.raw(), &mut id) },
            body,
        )?;
        Ok(Body::from_raw(id))
    }

    fn delete(&mut self, body: Body) -> Result<()> {
        check(unsafe { w3d_occt_delete(self.ctx, body.raw()) }, body)
    }

    fn fillet(&mut self, body: Body, radius: f64) -> Result<Body> {
        let mut id = 0u32;
        check(
            unsafe { w3d_occt_fillet(self.ctx, body.raw(), radius, &mut id) },
            body,
        )?;
        Ok(Body::from_raw(id))
    }

    fn chamfer(&mut self, body: Body, distance: f64) -> Result<Body> {
        let mut id = 0u32;
        check(
            unsafe { w3d_occt_chamfer(self.ctx, body.raw(), distance, &mut id) },
            body,
        )?;
        Ok(Body::from_raw(id))
    }

    /// A prism over the profile's own outline, standing on the plane it was
    /// drawn on — which is what the trait says and what this did not do. It
    /// called `create_box` or `create_cylinder` with the profile's two numbers,
    /// so a sketched polygon became a 20 x 20 slab, centred on the plane
    /// instead of standing on it.
    fn extrude(&mut self, profile: &Profile, distance: f64) -> Result<Body> {
        if distance <= 0.0 {
            return Err(KernelError::Degenerate("extrude distance must be positive"));
        }
        let (raw, _vertices) = RawProfile::new(profile, &SketchPlane::default())?;
        let mut id = 0u32;
        // SAFETY: `raw` borrows `_vertices`, which outlives the call.
        check_new(unsafe { w3d_occt_extrude(self.ctx, &raw, distance, &mut id) })?;
        Ok(Body::from_raw(id))
    }

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
        let (raw, _vertices) = RawProfile::new(profile, &SketchPlane::default())?;
        let origin = [axis_origin.x, axis_origin.y, axis_origin.z];
        let dir = [axis_dir.x, axis_dir.y, axis_dir.z];
        let mut id = 0u32;
        // SAFETY: `raw` borrows `_vertices`; all three outlive the call.
        check_new(unsafe {
            w3d_occt_revolve(
                self.ctx,
                &raw,
                origin.as_ptr(),
                dir.as_ptr(),
                angle_rad,
                &mut id,
            )
        })?;
        Ok(Body::from_raw(id))
    }

    fn sweep(&mut self, profile: &Profile, path_points: &[Vec3]) -> Result<Body> {
        if path_points.len() < 2 {
            return Err(KernelError::Degenerate(
                "sweep path requires at least 2 points",
            ));
        }
        // The profile is taken to sit at the path's first point, which is where
        // a sketch plane's origin is when a user draws one there.
        let plane = SketchPlane {
            origin: path_points[0],
            ..SketchPlane::default()
        };
        let (raw, _vertices) = RawProfile::new(profile, &plane)?;
        let flat_pts: Vec<f64> = path_points.iter().flat_map(|p| [p.x, p.y, p.z]).collect();
        let mut id = 0u32;
        // SAFETY: `raw` borrows `_vertices`; both, and `flat_pts`, outlive the call.
        check_new(unsafe {
            w3d_occt_sweep(
                self.ctx,
                &raw,
                flat_pts.as_ptr(),
                u32::try_from(path_points.len()).unwrap_or(u32::MAX),
                &mut id,
            )
        })?;
        Ok(Body::from_raw(id))
    }

    /// Through the sections, each on the plane it was given. Both the profiles
    /// and the planes were thrown away until 2026-09-14 — a loft of anything was
    /// a box 20 tall — so a cone between two discs came out a cylinder.
    fn loft(&mut self, profiles: &[Profile], planes: &[SketchPlane]) -> Result<Body> {
        if profiles.is_empty() {
            return Err(KernelError::Degenerate("loft requires at least 1 profile"));
        }
        if profiles.len() < 2 {
            return Err(KernelError::Degenerate(
                "a loft needs at least two sections",
            ));
        }
        if planes.len() < profiles.len() {
            return Err(KernelError::Degenerate("a loft needs a plane per profile"));
        }
        // The vertex buffers are collected first and kept until the call
        // returns: each descriptor borrows one.
        let mut buffers = Vec::with_capacity(profiles.len());
        let mut raws = Vec::with_capacity(profiles.len());
        for (profile, plane) in profiles.iter().zip(planes) {
            let (raw, vertices) = RawProfile::new(profile, plane)?;
            buffers.push(vertices);
            raws.push(raw);
        }
        // The pointers were taken before the buffers moved into `buffers`, so
        // they are rebuilt here against their final addresses.
        for (raw, buffer) in raws.iter_mut().zip(&buffers) {
            raw.vertices = if buffer.is_empty() {
                core::ptr::null()
            } else {
                buffer.as_ptr()
            };
        }
        let mut id = 0u32;
        // SAFETY: every descriptor's `vertices` points into `buffers`, which
        // outlives the call.
        check_new(unsafe {
            w3d_occt_loft(
                self.ctx,
                raws.as_ptr(),
                u32::try_from(raws.len()).unwrap_or(u32::MAX),
                &mut id,
            )
        })?;
        Ok(Body::from_raw(id))
    }

    fn shell(&mut self, body: Body, face_id: u32, thickness: f64) -> Result<Body> {
        if thickness <= 0.0 {
            return Err(KernelError::Degenerate("shell thickness must be positive"));
        }
        let mut id = 0u32;
        check(
            unsafe { w3d_occt_shell(self.ctx, body.raw(), face_id, thickness, &mut id) },
            body,
        )?;
        Ok(Body::from_raw(id))
    }

    fn topology(&self, body: Body) -> Result<Topology> {
        let mut out = [0u32; 4];
        check(
            unsafe { w3d_occt_topology(self.ctx, body.raw(), out.as_mut_ptr()) },
            body,
        )?;
        Ok(Topology {
            solids: out[0],
            faces: out[1],
            edges: out[2],
            vertices: out[3],
        })
    }

    fn bounds(&self, body: Body) -> Result<Aabb> {
        let mut out = [0.0f64; 6];
        check(
            unsafe { w3d_occt_bounds(self.ctx, body.raw(), out.as_mut_ptr()) },
            body,
        )?;
        Ok(Aabb::new(
            Vec3::new(out[0], out[1], out[2]),
            Vec3::new(out[3], out[4], out[5]),
        ))
    }

    fn tessellate(&self, body: Body, quality: Quality) -> Result<Mesh> {
        let mut raw = RawMesh::empty();
        check(
            unsafe {
                w3d_occt_tessellate(
                    self.ctx,
                    body.raw(),
                    quality.sag,
                    quality.max_angle,
                    &mut raw,
                )
            },
            body,
        )?;

        // SAFETY: on OK the shim guarantees each pointer is non-null and valid
        // for the length it reported, until `w3d_occt_mesh_free`. Everything is
        // copied before that call, so no borrowed memory escapes this function.
        let mesh = unsafe {
            let verts = raw.vertex_count as usize;
            let tris = raw.triangle_count as usize;
            let line_verts = raw.line_vertex_count as usize;
            let line_segs = raw.line_segment_count as usize;
            Mesh {
                positions: chunks3(raw.positions, verts),
                normals: chunks3(raw.normals, verts),
                indices: core::slice::from_raw_parts(raw.indices, tris * 3).to_vec(),
                face_of_triangle: core::slice::from_raw_parts(raw.face_of_triangle, tris).to_vec(),
                line_positions: chunks3(raw.line_positions, line_verts),
                line_indices: if line_segs > 0 && !raw.line_indices.is_null() {
                    core::slice::from_raw_parts(raw.line_indices, line_segs * 2).to_vec()
                } else {
                    Vec::new()
                },
            }
        };
        unsafe { w3d_occt_mesh_free(&mut raw) };
        Ok(mesh)
    }

    fn geometry_format(&self) -> &'static str {
        // Versioned, because it is a promise about bytes on somebody's disk.
        // The `1` moves if what BRepTools::Write produces here ever changes
        // shape — an OCCT major version is the likely cause.
        "occt-brep-1"
    }

    fn save_body(&self, body: Body) -> Result<Vec<u8>> {
        let mut raw = RawBytes::empty();
        // SAFETY: the shim writes `raw` only on OK, and the buffer it points
        // at lives until `w3d_occt_bytes_free`. Everything is copied first, so
        // no borrowed memory escapes.
        let code = unsafe { w3d_occt_save_body(self.ctx, body.raw(), &mut raw) };
        check(code, body)?;
        let bytes = unsafe { core::slice::from_raw_parts(raw.data, raw.len as usize) }.to_vec();
        unsafe { w3d_occt_bytes_free(&mut raw) };
        Ok(bytes)
    }

    fn load_body(&mut self, bytes: &[u8]) -> Result<Body> {
        let mut out = 0u32;
        // SAFETY: the pointer and length describe `bytes`, which outlives the
        // call; the shim copies before returning.
        let code = unsafe {
            w3d_occt_load_body(
                self.ctx,
                bytes.as_ptr(),
                u32::try_from(bytes.len()).unwrap_or(u32::MAX),
                &mut out,
            )
        };
        match code {
            ERR_UNSUPPORTED => Err(KernelError::Unsupported(
                "these bytes are not OpenCASCADE BREP",
            )),
            other => {
                check_new(other)?;
                Ok(Body::from_raw(out))
            }
        }
    }

    fn export_step(&self, bodies: &[Body]) -> Result<Vec<u8>> {
        // `Body` is a `#[repr(transparent)]`-shaped newtype only by
        // convention, so the ids are collected rather than the slice
        // transmuted. It is a handful of `u32`s next to writing a STEP file.
        let ids: Vec<u32> = bodies.iter().map(|b| b.raw()).collect();
        let mut raw = RawBytes::empty();
        // SAFETY: `ids` outlives the call and the shim writes `raw` only on
        // OK, pointing at memory that lives until `w3d_occt_bytes_free`.
        let code = unsafe {
            w3d_occt_export_step(
                self.ctx,
                ids.as_ptr(),
                u32::try_from(ids.len()).unwrap_or(u32::MAX),
                &mut raw,
            )
        };
        // `UnknownBody` needs a handle to name and the shim does not say which
        // one, so the first is named. There is one in the common case, and the
        // alternative is a widened entry point for a message.
        check(
            code,
            bodies.first().copied().unwrap_or(Body::from_raw(u32::MAX)),
        )?;
        let bytes = unsafe { core::slice::from_raw_parts(raw.data, raw.len as usize) }.to_vec();
        unsafe { w3d_occt_bytes_free(&mut raw) };
        Ok(bytes)
    }

    fn import_step(&mut self, bytes: &[u8]) -> Result<w3d_kernel::Import> {
        let mut raw = RawImport::empty();
        // SAFETY: the pointer and length describe `bytes`, which outlives the
        // call; the ids, names and parents are copied out before they are
        // freed.
        let code = unsafe {
            w3d_occt_import_step(
                self.ctx,
                bytes.as_ptr(),
                u32::try_from(bytes.len()).unwrap_or(u32::MAX),
                &mut raw,
            )
        };
        // Never `Unsupported`: this build reads STEP. Bytes that are not STEP
        // are `Failed`, with OCCT's own parse error in the message — which is
        // the distinction the contract asks for, and the reason the shim
        // collects OCCT's diagnostics instead of letting them print.
        check_new(code)?;
        let ids = unsafe { core::slice::from_raw_parts(raw.ids, raw.len as usize) };
        let names = unsafe { strings(raw.names, raw.len) };
        let parents = unsafe { indices(raw.parents, raw.len) };
        let assembly_names = unsafe { strings(raw.assembly_names, raw.assemblies) };
        let assembly_parents = unsafe { indices(raw.assembly_parents, raw.assemblies) };

        let assemblies = (0..raw.assemblies as usize)
            .map(|i| w3d_kernel::ImportedAssembly {
                name: assembly_names.get(i).cloned().flatten(),
                parent: assembly_parents.get(i).copied().flatten(),
            })
            .collect();
        let bodies = (0..raw.len as usize)
            .map(|i| w3d_kernel::ImportedBody {
                body: Body::from_raw(ids[i]),
                name: names.get(i).cloned().flatten(),
                parent: parents.get(i).copied().flatten(),
            })
            .collect();

        unsafe { w3d_occt_import_free(&mut raw) };
        let imported = w3d_kernel::Import { bodies, assemblies };
        // The contract's structural rule, checked on this side of the boundary
        // rather than trusted across it: the walk in the shim emits a parent
        // before its children, and a C++ index that stopped being one would
        // otherwise reach the document as a tree that is not a tree.
        if let Err(why) = imported.validate() {
            return Err(KernelError::Failed(format!(
                "the STEP reader produced an assembly tree that is not one: {why}"
            )));
        }
        Ok(imported)
    }
}

/// Copies `count` nullable C strings out of the shim's arrays. A null pointer,
/// and a string that is nothing but whitespace, both become `None`: OCCT
/// answers with an empty name for a label that has none, and a document does
/// not want a node called `" "`.
///
/// SAFETY: `array` must be null, or valid for `count` pointers, each null or a
/// null-terminated string that outlives the call.
unsafe fn strings(array: *const *const c_char, count: u32) -> Vec<Option<String>> {
    if array.is_null() {
        return Vec::new();
    }
    let ptrs = unsafe { core::slice::from_raw_parts(array, count as usize) };
    ptrs.iter()
        .map(|&p| {
            if p.is_null() {
                return None;
            }
            let s = unsafe { CStr::from_ptr(p) }
                .to_string_lossy()
                .trim()
                .to_string();
            if s.is_empty() { None } else { Some(s) }
        })
        .collect()
}

/// Copies `count` parent indices out, turning the shim's `NO_PARENT` sentinel
/// into `None` here so that nothing above this file has to know the sentinel
/// exists.
///
/// SAFETY: `array` must be null, or valid for `count` `u32`s.
unsafe fn indices(array: *const u32, count: u32) -> Vec<Option<usize>> {
    if array.is_null() {
        return Vec::new();
    }
    unsafe { core::slice::from_raw_parts(array, count as usize) }
        .iter()
        .map(|&i| {
            if i == NO_PARENT {
                None
            } else {
                Some(i as usize)
            }
        })
        .collect()
}

/// SAFETY: `ptr` must be valid for `count * 3` floats.
unsafe fn chunks3(ptr: *const f32, count: usize) -> Vec<[f32; 3]> {
    if count == 0 {
        return Vec::new();
    }
    let flat = unsafe { core::slice::from_raw_parts(ptr, count * 3) };
    // `as_chunks`, not `chunks_exact`: the length is a multiple of three by
    // construction and this says so in the type. The remainder is empty and
    // dropped.
    flat.as_chunks::<3>().0.to_vec()
}
