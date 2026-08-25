//! The OpenCASCADE backend.
//!
//! The only crate in this workspace that contains `unsafe`, and it contains it
//! for one reason: everything here is a call across `native/w3d_occt.h`. That
//! header is the specification of what an OCCT build must keep exported, and it
//! is the same thirteen entry points as [`GeometryKernel`] — see its comments.
//!
//! Nothing OCCT-shaped reaches Rust. Error codes become [`KernelError`],
//! meshes are copied out and freed on the C++ side, and `TopoDS_Shape` never
//! appears at all.

use std::ffi::{CStr, c_char};
use w3d_kernel::{
    Aabb, Body, BooleanOp, GeometryKernel, KernelError, Mat4, Mesh, Quality, Result, Tolerance,
    Topology, Vec3,
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

#[repr(C)]
struct RawMesh {
    positions: *const f32,
    normals: *const f32,
    indices: *const u32,
    face_of_triangle: *const u32,
    vertex_count: u32,
    triangle_count: u32,
    owner: *mut core::ffi::c_void,
}

impl RawMesh {
    const fn empty() -> Self {
        Self {
            positions: core::ptr::null(),
            normals: core::ptr::null(),
            indices: core::ptr::null(),
            face_of_triangle: core::ptr::null(),
            vertex_count: 0,
            triangle_count: 0,
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
            Mesh {
                positions: chunks3(raw.positions, verts),
                normals: chunks3(raw.normals, verts),
                indices: core::slice::from_raw_parts(raw.indices, tris * 3).to_vec(),
                face_of_triangle: core::slice::from_raw_parts(raw.face_of_triangle, tris).to_vec(),
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
}

/// SAFETY: `ptr` must be valid for `count * 3` floats.
unsafe fn chunks3(ptr: *const f32, count: usize) -> Vec<[f32; 3]> {
    if count == 0 {
        return Vec::new();
    }
    let flat = unsafe { core::slice::from_raw_parts(ptr, count * 3) };
    flat.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect()
}
