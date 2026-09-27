//! `native/w3d_occt.h`, on wasm32: the same functions with the same signatures,
//! forwarding to the OpenCASCADE module `make occt-wasm` builds.
//!
//! That module cannot be linked into this one — see `tools/occt_wasm.py` — so
//! it is a second wasm instance with a linear memory of its own, and the only
//! way across is through JS. Five primitives are asked of JS, on a global
//! `w3dOcct` object the page (or a test) installs before the first kernel is
//! made: `call` an entry point by name with numeric arguments, `malloc` and
//! `free` in the module's heap, and `write` and `read` bytes between the two
//! memories. Everything else is done here, in Rust, so the JS side stays a
//! handful of lines that know nothing about the header.
//!
//! Every function below does one thing three times: copy what the caller
//! pointed at into the module's memory, call, and copy what came back into
//! this memory — rewriting each pointer on the way, because a pointer into one
//! memory means nothing in the other. The structs are laid out identically on
//! both sides because both are wasm32 C ABI; the assertions at the bottom hold
//! that, and a struct that changed shape in the header fails them before it
//! can scramble a mesh.
//!
//! Ownership follows the header's rule unchanged: what the shim hands back is
//! freed by its own `_free` call. Here that means the module's copy is freed
//! as soon as it has been copied out, and the local copy is freed when the
//! caller calls the matching free — so `lib.rs` cannot tell which side it is
//! talking to, which is the point.

use super::{Context, RawBytes, RawImport, RawMesh, RawProfile};
use std::cell::RefCell;
use std::ffi::{CString, c_char, c_void};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = w3dOcct, js_name = call)]
    fn js_call(name: &str, args: &[f64]) -> f64;
    #[wasm_bindgen(js_namespace = w3dOcct, js_name = malloc)]
    fn js_malloc(len: u32) -> u32;
    #[wasm_bindgen(js_namespace = w3dOcct, js_name = free)]
    fn js_free(ptr: u32);
    #[wasm_bindgen(js_namespace = w3dOcct, js_name = write)]
    fn js_write(ptr: u32, bytes: &[u8]);
    #[wasm_bindgen(js_namespace = w3dOcct, js_name = read)]
    fn js_read(ptr: u32, out: &mut [u8]);
}

/// A block of the module's heap, freed when dropped.
struct Remote(u32);

impl Remote {
    fn alloc(len: usize) -> Self {
        Self(js_malloc(len.max(1) as u32))
    }

    fn bytes(bytes: &[u8]) -> Self {
        let r = Self::alloc(bytes.len());
        js_write(r.0, bytes);
        r
    }

    fn of<T: Copy>(values: &[T]) -> Self {
        // SAFETY: `T` is a plain number type or a `#[repr(C)]` struct of them,
        // read as the bytes it is made of.
        let bytes = unsafe {
            core::slice::from_raw_parts(values.as_ptr().cast::<u8>(), size_of_val(values))
        };
        Self::bytes(bytes)
    }

    fn arg(&self) -> f64 {
        f64::from(self.0)
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        js_free(self.0);
    }
}

fn read_bytes(ptr: u32, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    if len > 0 {
        js_read(ptr, &mut out);
    }
    out
}

fn read_one<T: Copy>(ptr: u32) -> T {
    let bytes = read_bytes(ptr, size_of::<T>());
    // SAFETY: `T` is a plain number type or a `#[repr(C)]` struct of them, and
    // any bit pattern of the right length is one of its values.
    unsafe { core::ptr::read_unaligned(bytes.as_ptr().cast::<T>()) }
}

// `as_chunks` wants the chunk size as a const argument, and `size_of::<T>()`
// of a generic `T` cannot be one.
#[allow(clippy::chunks_exact_to_as_chunks)]
fn read_vec<T: Copy>(ptr: u32, count: usize) -> Vec<T> {
    if ptr == 0 || count == 0 {
        return Vec::new();
    }
    let bytes = read_bytes(ptr, count * size_of::<T>());
    bytes
        .chunks_exact(size_of::<T>())
        // SAFETY: as in `read_one`.
        .map(|c| unsafe { core::ptr::read_unaligned(c.as_ptr().cast::<T>()) })
        .collect()
}

/// A NUL-terminated string in the module's memory, read in chunks until the
/// NUL, because nothing says how long it is.
fn read_cstring(ptr: u32) -> CString {
    let mut out = Vec::new();
    let mut at = ptr;
    loop {
        let chunk = read_bytes(at, 64);
        match chunk.iter().position(|&b| b == 0) {
            Some(end) => {
                out.extend_from_slice(&chunk[..end]);
                break;
            }
            None => {
                out.extend_from_slice(&chunk);
                at += 64;
            }
        }
    }
    CString::new(out).unwrap_or_default()
}

fn ctx_arg(ctx: *const Context) -> f64 {
    ctx as usize as f64
}

/// Calls an entry point whose last argument is a `uint32_t *out`, and writes
/// what it wrote there into `out`.
///
/// # Safety
/// `out` must be valid for a write.
unsafe fn with_out_u32(out: *mut u32, call: impl FnOnce(f64) -> f64) -> i32 {
    let slot = Remote::alloc(4);
    let code = call(slot.arg()) as i32;
    // SAFETY: the caller's contract.
    unsafe { *out = read_one::<u32>(slot.0) };
    code
}

// --- The remote layouts: the same structs with every pointer a u32 offset into
// the module's memory. Identical in size and field order to `Raw*`, checked at
// the bottom of this file.

#[repr(C)]
#[derive(Clone, Copy)]
struct RMesh {
    positions: u32,
    normals: u32,
    indices: u32,
    face_of_triangle: u32,
    line_positions: u32,
    line_indices: u32,
    edge_of_line: u32,
    vertex_count: u32,
    triangle_count: u32,
    line_vertex_count: u32,
    line_segment_count: u32,
    owner: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RBytes {
    data: u32,
    len: u32,
    owner: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RImport {
    ids: u32,
    names: u32,
    parents: u32,
    len: u32,
    assembly_names: u32,
    assembly_parents: u32,
    assemblies: u32,
    owner: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RProfile {
    kind: i32,
    p1: f64,
    p2: f64,
    vertices: u32,
    vertex_count: u32,
    origin: [f64; 3],
    x_axis: [f64; 3],
    y_axis: [f64; 3],
}

const _: () = assert!(size_of::<RMesh>() == size_of::<RawMesh>());
const _: () = assert!(size_of::<RBytes>() == size_of::<RawBytes>());
const _: () = assert!(size_of::<RImport>() == size_of::<RawImport>());
const _: () = assert!(size_of::<RProfile>() == size_of::<RawProfile>());
// And the sizes the header gives on wasm32, where the module's C compiler lays
// them out: a disagreement here is a struct the two sides read differently.
const _: () = assert!(size_of::<RawMesh>() == 48);
const _: () = assert!(size_of::<RawBytes>() == 12);
const _: () = assert!(size_of::<RawImport>() == 32);
const _: () = assert!(size_of::<RawProfile>() == 104);

/// A profile copied across, with its vertex buffer, both freed together.
struct RemoteProfile {
    _vertices: Option<Remote>,
    rp: RProfile,
}

/// # Safety
/// `p` must point at a valid profile whose `vertices` hold `2 * vertex_count`
/// doubles when it is not null.
unsafe fn remote_profile(p: *const RawProfile) -> RemoteProfile {
    // SAFETY: the caller's contract.
    let p = unsafe { &*p };
    let vertices = if p.vertices.is_null() || p.vertex_count == 0 {
        None
    } else {
        // SAFETY: the caller's contract.
        let v = unsafe { core::slice::from_raw_parts(p.vertices, p.vertex_count as usize * 2) };
        Some(Remote::of(v))
    };
    let rp = RProfile {
        kind: p.kind,
        p1: p.p1,
        p2: p.p2,
        vertices: vertices.as_ref().map_or(0, |r| r.0),
        vertex_count: p.vertex_count,
        origin: p.origin,
        x_axis: p.x_axis,
        y_axis: p.y_axis,
    };
    RemoteProfile {
        _vertices: vertices,
        rp,
    }
}

// --- The header's entry points ------------------------------------------------

pub(super) unsafe fn w3d_occt_context_new() -> *mut Context {
    js_call("context_new", &[]) as u32 as usize as *mut Context
}

pub(super) unsafe fn w3d_occt_context_free(ctx: *mut Context) {
    js_call("context_free", &[ctx_arg(ctx)]);
}

pub(super) unsafe fn w3d_occt_make_box(
    ctx: *mut Context,
    sx: f64,
    sy: f64,
    sz: f64,
    out: *mut u32,
) -> i32 {
    unsafe { with_out_u32(out, |o| js_call("make_box", &[ctx_arg(ctx), sx, sy, sz, o])) }
}

pub(super) unsafe fn w3d_occt_make_sphere(ctx: *mut Context, radius: f64, out: *mut u32) -> i32 {
    unsafe { with_out_u32(out, |o| js_call("make_sphere", &[ctx_arg(ctx), radius, o])) }
}

pub(super) unsafe fn w3d_occt_make_cylinder(
    ctx: *mut Context,
    radius: f64,
    height: f64,
    out: *mut u32,
) -> i32 {
    unsafe {
        with_out_u32(out, |o| {
            js_call("make_cylinder", &[ctx_arg(ctx), radius, height, o])
        })
    }
}

pub(super) unsafe fn w3d_occt_boolean(
    ctx: *mut Context,
    op: i32,
    a: u32,
    b: u32,
    fuzzy: f64,
    out: *mut u32,
) -> i32 {
    let args = |o| {
        [
            ctx_arg(ctx),
            f64::from(op),
            f64::from(a),
            f64::from(b),
            fuzzy,
            o,
        ]
    };
    unsafe { with_out_u32(out, |o| js_call("boolean", &args(o))) }
}

pub(super) unsafe fn w3d_occt_transform(
    ctx: *mut Context,
    body: u32,
    m34: *const f64,
    out: *mut u32,
) -> i32 {
    // SAFETY: the header's contract — twelve doubles.
    let m = Remote::of(unsafe { core::slice::from_raw_parts(m34, 12) });
    unsafe {
        with_out_u32(out, |o| {
            js_call("transform", &[ctx_arg(ctx), f64::from(body), m.arg(), o])
        })
    }
}

pub(super) unsafe fn w3d_occt_copy(ctx: *mut Context, body: u32, out: *mut u32) -> i32 {
    unsafe {
        with_out_u32(out, |o| {
            js_call("copy", &[ctx_arg(ctx), f64::from(body), o])
        })
    }
}

pub(super) unsafe fn w3d_occt_delete(ctx: *mut Context, body: u32) -> i32 {
    js_call("delete", &[ctx_arg(ctx), f64::from(body)]) as i32
}

pub(super) unsafe fn w3d_occt_fillet(
    ctx: *mut Context,
    body: u32,
    radius: f64,
    out: *mut u32,
) -> i32 {
    unsafe {
        with_out_u32(out, |o| {
            js_call("fillet", &[ctx_arg(ctx), f64::from(body), radius, o])
        })
    }
}

pub(super) unsafe fn w3d_occt_chamfer(
    ctx: *mut Context,
    body: u32,
    distance: f64,
    out: *mut u32,
) -> i32 {
    unsafe {
        with_out_u32(out, |o| {
            js_call("chamfer", &[ctx_arg(ctx), f64::from(body), distance, o])
        })
    }
}

unsafe fn blend_edges(
    name: &str,
    ctx: *mut Context,
    body: u32,
    edges: *const u32,
    edge_count: u32,
    size: f64,
    out: *mut u32,
) -> i32 {
    let list = if edges.is_null() || edge_count == 0 {
        Remote::alloc(4)
    } else {
        // SAFETY: the header's contract — `edge_count` ids.
        Remote::of(unsafe { core::slice::from_raw_parts(edges, edge_count as usize) })
    };
    let args = |o| {
        [
            ctx_arg(ctx),
            f64::from(body),
            list.arg(),
            f64::from(edge_count),
            size,
            o,
        ]
    };
    unsafe { with_out_u32(out, |o| js_call(name, &args(o))) }
}

pub(super) unsafe fn w3d_occt_fillet_edges(
    ctx: *mut Context,
    body: u32,
    edges: *const u32,
    edge_count: u32,
    radius: f64,
    out: *mut u32,
) -> i32 {
    unsafe { blend_edges("fillet_edges", ctx, body, edges, edge_count, radius, out) }
}

pub(super) unsafe fn w3d_occt_chamfer_edges(
    ctx: *mut Context,
    body: u32,
    edges: *const u32,
    edge_count: u32,
    distance: f64,
    out: *mut u32,
) -> i32 {
    unsafe { blend_edges("chamfer_edges", ctx, body, edges, edge_count, distance, out) }
}

pub(super) unsafe fn w3d_occt_shell(
    ctx: *mut Context,
    body: u32,
    face_id: u32,
    thickness: f64,
    out: *mut u32,
) -> i32 {
    let args = |o| {
        [
            ctx_arg(ctx),
            f64::from(body),
            f64::from(face_id),
            thickness,
            o,
        ]
    };
    unsafe { with_out_u32(out, |o| js_call("shell", &args(o))) }
}

pub(super) unsafe fn w3d_occt_extrude(
    ctx: *mut Context,
    profile: *const RawProfile,
    distance: f64,
    out: *mut u32,
) -> i32 {
    let p = unsafe { remote_profile(profile) };
    let rp = Remote::of(core::slice::from_ref(&p.rp));
    unsafe {
        with_out_u32(out, |o| {
            js_call("extrude", &[ctx_arg(ctx), rp.arg(), distance, o])
        })
    }
}

pub(super) unsafe fn w3d_occt_revolve(
    ctx: *mut Context,
    profile: *const RawProfile,
    axis_origin: *const f64,
    axis_dir: *const f64,
    angle_rad: f64,
    out: *mut u32,
) -> i32 {
    let p = unsafe { remote_profile(profile) };
    let rp = Remote::of(core::slice::from_ref(&p.rp));
    // SAFETY: the header's contract — three doubles each.
    let origin = Remote::of(unsafe { core::slice::from_raw_parts(axis_origin, 3) });
    let dir = Remote::of(unsafe { core::slice::from_raw_parts(axis_dir, 3) });
    let args = |o| {
        [
            ctx_arg(ctx),
            rp.arg(),
            origin.arg(),
            dir.arg(),
            angle_rad,
            o,
        ]
    };
    unsafe { with_out_u32(out, |o| js_call("revolve", &args(o))) }
}

pub(super) unsafe fn w3d_occt_sweep(
    ctx: *mut Context,
    profile: *const RawProfile,
    pts: *const f64,
    pt_count: u32,
    out: *mut u32,
) -> i32 {
    let p = unsafe { remote_profile(profile) };
    let rp = Remote::of(core::slice::from_ref(&p.rp));
    let path = if pts.is_null() || pt_count == 0 {
        Remote::alloc(8)
    } else {
        // SAFETY: the header's contract — three doubles per point.
        Remote::of(unsafe { core::slice::from_raw_parts(pts, pt_count as usize * 3) })
    };
    let args = |o| [ctx_arg(ctx), rp.arg(), path.arg(), f64::from(pt_count), o];
    unsafe { with_out_u32(out, |o| js_call("sweep", &args(o))) }
}

pub(super) unsafe fn w3d_occt_loft(
    ctx: *mut Context,
    profiles: *const RawProfile,
    profile_count: u32,
    out: *mut u32,
) -> i32 {
    let copied: Vec<RemoteProfile> = (0..profile_count as usize)
        // SAFETY: the header's contract — `profile_count` profiles.
        .map(|i| unsafe { remote_profile(profiles.add(i)) })
        .collect();
    let structs: Vec<RProfile> = copied.iter().map(|p| p.rp).collect();
    let list = Remote::of(&structs);
    let args = |o| [ctx_arg(ctx), list.arg(), f64::from(profile_count), o];
    unsafe { with_out_u32(out, |o| js_call("loft", &args(o))) }
}

pub(super) unsafe fn w3d_occt_topology(ctx: *mut Context, body: u32, out4: *mut u32) -> i32 {
    let slot = Remote::alloc(16);
    let code = js_call("topology", &[ctx_arg(ctx), f64::from(body), slot.arg()]) as i32;
    let values: Vec<u32> = read_vec(slot.0, 4);
    // SAFETY: the header's contract — four u32 slots.
    unsafe { core::ptr::copy_nonoverlapping(values.as_ptr(), out4, 4) };
    code
}

pub(super) unsafe fn w3d_occt_bounds(ctx: *mut Context, body: u32, out6: *mut f64) -> i32 {
    let slot = Remote::alloc(48);
    let code = js_call("bounds", &[ctx_arg(ctx), f64::from(body), slot.arg()]) as i32;
    let values: Vec<f64> = read_vec(slot.0, 6);
    // SAFETY: the header's contract — six f64 slots.
    unsafe { core::ptr::copy_nonoverlapping(values.as_ptr(), out6, 6) };
    code
}

/// What a local `RawMesh` points into, owned through its `owner` field.
struct LocalMesh {
    positions: Vec<f32>,
    normals: Vec<f32>,
    indices: Vec<u32>,
    face_of_triangle: Vec<u32>,
    line_positions: Vec<f32>,
    line_indices: Vec<u32>,
    edge_of_line: Vec<u32>,
}

pub(super) unsafe fn w3d_occt_tessellate(
    ctx: *mut Context,
    body: u32,
    sag: f64,
    angle: f64,
    out: *mut RawMesh,
) -> i32 {
    let slot = Remote::alloc(size_of::<RMesh>());
    let code = js_call(
        "tessellate",
        &[ctx_arg(ctx), f64::from(body), sag, angle, slot.arg()],
    ) as i32;
    if code != 0 {
        return code;
    }
    let r: RMesh = read_one(slot.0);
    let (v, t) = (r.vertex_count as usize, r.triangle_count as usize);
    let (lv, ls) = (r.line_vertex_count as usize, r.line_segment_count as usize);
    let local = Box::new(LocalMesh {
        positions: read_vec(r.positions, 3 * v),
        normals: read_vec(r.normals, 3 * v),
        indices: read_vec(r.indices, 3 * t),
        face_of_triangle: read_vec(r.face_of_triangle, t),
        line_positions: read_vec(r.line_positions, 3 * lv),
        line_indices: read_vec(r.line_indices, 2 * ls),
        edge_of_line: read_vec(r.edge_of_line, ls),
    });
    // The module's copy has been read; it is freed now, not when the caller
    // frees the local one.
    js_call("mesh_free", &[slot.arg()]);
    let ptr_or_null = |v: &Vec<u32>| {
        if v.is_empty() {
            core::ptr::null()
        } else {
            v.as_ptr()
        }
    };
    // SAFETY: the caller's contract — `out` is a valid mesh to fill.
    unsafe {
        *out = RawMesh {
            positions: local.positions.as_ptr(),
            normals: local.normals.as_ptr(),
            indices: local.indices.as_ptr(),
            face_of_triangle: local.face_of_triangle.as_ptr(),
            line_positions: local.line_positions.as_ptr(),
            line_indices: local.line_indices.as_ptr(),
            edge_of_line: ptr_or_null(&local.edge_of_line),
            vertex_count: r.vertex_count,
            triangle_count: r.triangle_count,
            line_vertex_count: r.line_vertex_count,
            line_segment_count: r.line_segment_count,
            owner: Box::into_raw(local).cast::<c_void>(),
        };
    }
    code
}

pub(super) unsafe fn w3d_occt_mesh_free(mesh: *mut RawMesh) {
    // SAFETY: `owner` is null or the box `w3d_occt_tessellate` leaked.
    unsafe {
        let m = &mut *mesh;
        if !m.owner.is_null() {
            drop(Box::from_raw(m.owner.cast::<LocalMesh>()));
        }
        *m = RawMesh::empty();
    }
}

/// Copies an `RBytes` out of the module into a local `RawBytes`, and frees the
/// module's copy.
unsafe fn bytes_out(slot: &Remote, out: *mut RawBytes) {
    let r: RBytes = read_one(slot.0);
    let local = Box::new(read_bytes(r.data, r.len as usize));
    js_call("bytes_free", &[slot.arg()]);
    // SAFETY: the caller's contract.
    unsafe {
        *out = RawBytes {
            data: local.as_ptr(),
            len: r.len,
            owner: Box::into_raw(local).cast::<c_void>(),
        };
    }
}

pub(super) unsafe fn w3d_occt_save_body(ctx: *mut Context, body: u32, out: *mut RawBytes) -> i32 {
    let slot = Remote::alloc(size_of::<RBytes>());
    let code = js_call("save_body", &[ctx_arg(ctx), f64::from(body), slot.arg()]) as i32;
    if code == 0 {
        unsafe { bytes_out(&slot, out) };
    }
    code
}

pub(super) unsafe fn w3d_occt_load_body(
    ctx: *mut Context,
    data: *const u8,
    len: u32,
    out: *mut u32,
) -> i32 {
    // SAFETY: the header's contract — `len` bytes.
    let bytes = Remote::bytes(unsafe { core::slice::from_raw_parts(data, len as usize) });
    let args = |o| [ctx_arg(ctx), bytes.arg(), f64::from(len), o];
    unsafe { with_out_u32(out, |o| js_call("load_body", &args(o))) }
}

pub(super) unsafe fn w3d_occt_bytes_free(bytes: *mut RawBytes) {
    // SAFETY: `owner` is null or the box `bytes_out` leaked.
    unsafe {
        let b = &mut *bytes;
        if !b.owner.is_null() {
            drop(Box::from_raw(b.owner.cast::<Vec<u8>>()));
        }
        *b = RawBytes::empty();
    }
}

pub(super) unsafe fn w3d_occt_export_step(
    ctx: *mut Context,
    bodies: *const u32,
    count: u32,
    out: *mut RawBytes,
) -> i32 {
    let list = if bodies.is_null() || count == 0 {
        Remote::alloc(4)
    } else {
        // SAFETY: the header's contract — `count` handles.
        Remote::of(unsafe { core::slice::from_raw_parts(bodies, count as usize) })
    };
    let slot = Remote::alloc(size_of::<RBytes>());
    let code = js_call(
        "export_step",
        &[ctx_arg(ctx), list.arg(), f64::from(count), slot.arg()],
    ) as i32;
    if code == 0 {
        unsafe { bytes_out(&slot, out) };
    }
    code
}

/// What a local `RawImport` points into.
struct LocalImport {
    ids: Vec<u32>,
    parents: Vec<u32>,
    names: Vec<Option<CString>>,
    name_ptrs: Vec<*const c_char>,
    assembly_parents: Vec<u32>,
    assembly_names: Vec<Option<CString>>,
    assembly_name_ptrs: Vec<*const c_char>,
}

/// A nullable array of nullable C strings in the module, copied out.
fn read_names(array: u32, count: usize) -> Vec<Option<CString>> {
    if array == 0 {
        return Vec::new();
    }
    read_vec::<u32>(array, count)
        .into_iter()
        .map(|p| (p != 0).then(|| read_cstring(p)))
        .collect()
}

fn pointers(names: &[Option<CString>]) -> Vec<*const c_char> {
    names
        .iter()
        .map(|n| n.as_ref().map_or(core::ptr::null(), |s| s.as_ptr()))
        .collect()
}

pub(super) unsafe fn w3d_occt_import_step(
    ctx: *mut Context,
    data: *const u8,
    len: u32,
    out: *mut RawImport,
) -> i32 {
    // SAFETY: the header's contract — `len` bytes.
    let bytes = Remote::bytes(unsafe { core::slice::from_raw_parts(data, len as usize) });
    let slot = Remote::alloc(size_of::<RImport>());
    let code = js_call(
        "import_step",
        &[ctx_arg(ctx), bytes.arg(), f64::from(len), slot.arg()],
    ) as i32;
    if code != 0 {
        return code;
    }
    let r: RImport = read_one(slot.0);
    let (n, a) = (r.len as usize, r.assemblies as usize);
    let mut local = Box::new(LocalImport {
        ids: read_vec(r.ids, n),
        parents: read_vec(r.parents, n),
        names: read_names(r.names, n),
        name_ptrs: Vec::new(),
        assembly_parents: read_vec(r.assembly_parents, a),
        assembly_names: read_names(r.assembly_names, a),
        assembly_name_ptrs: Vec::new(),
    });
    js_call("import_free", &[slot.arg()]);
    local.name_ptrs = pointers(&local.names);
    local.assembly_name_ptrs = pointers(&local.assembly_names);
    let array_or_null = |v: &Vec<*const c_char>| {
        if v.is_empty() {
            core::ptr::null()
        } else {
            v.as_ptr()
        }
    };
    // SAFETY: the caller's contract.
    unsafe {
        *out = RawImport {
            ids: local.ids.as_ptr(),
            names: array_or_null(&local.name_ptrs),
            parents: local.parents.as_ptr(),
            len: r.len,
            assembly_names: array_or_null(&local.assembly_name_ptrs),
            assembly_parents: local.assembly_parents.as_ptr(),
            assemblies: r.assemblies,
            owner: Box::into_raw(local).cast::<c_void>(),
        };
    }
    code
}

pub(super) unsafe fn w3d_occt_import_free(imported: *mut RawImport) {
    // SAFETY: `owner` is null or the box `w3d_occt_import_step` leaked.
    unsafe {
        let i = &mut *imported;
        if !i.owner.is_null() {
            drop(Box::from_raw(i.owner.cast::<LocalImport>()));
        }
        *i = RawImport::empty();
    }
}

thread_local! {
    /// The last error, copied out of the module. The header promises the
    /// pointer lives until the next call on this thread, and so does this.
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

pub(super) unsafe fn w3d_occt_last_error() -> *const c_char {
    let message = read_cstring(js_call("last_error", &[]) as u32);
    LAST_ERROR.with(|e| {
        *e.borrow_mut() = message;
        e.borrow().as_ptr()
    })
}

pub(super) unsafe fn w3d_occt_live_bodies(ctx: *const Context) -> u32 {
    js_call("live_bodies", &[ctx_arg(ctx)]) as u32
}
