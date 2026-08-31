/* The seam, in C.
 *
 * This header is the specification of what an OpenCASCADE build must keep
 * exported, and it is deliberately the same shape as `GeometryKernel`: one
 * entry point per trait method, plus the ones C requires and Rust does not —
 * a context, and a free for every buffer that crosses. It is not bindings for
 * OCCT — nothing here exposes a TopoDS_Shape, a Handle or a Standard_Real, so
 * nothing above it can start depending on OCCT's vocabulary.
 *
 * The count is deliberately not written down here any more. It was, twice,
 * and both numbers were wrong within a session of being right; what matters is
 * the rule, which is that this file is the list.
 *
 * When the modeller needs a new geometric capability it is declared HERE
 * first, then implemented in w3d_occt.cpp, then used from Rust. Every symbol
 * this header adds is a symbol an Emscripten build has to keep alive through
 * EMSCRIPTEN_KEEPALIVE, so a narrow header is a short list of things that can
 * break.
 *
 * Bodies are u32 handles into a registry owned by the C++ side, matching
 * w3d_kernel::Body. Ids are never reused, so a stale handle stays an error
 * rather than naming somebody else's solid.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */
#ifndef W3D_OCCT_H
#define W3D_OCCT_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define W3D_OCCT_OK 0
#define W3D_OCCT_ERR_UNKNOWN_BODY 1
#define W3D_OCCT_ERR_DEGENERATE 2
#define W3D_OCCT_ERR_UNSUPPORTED 3
#define W3D_OCCT_ERR_FAILED 4

#define W3D_OCCT_OP_UNION 0
#define W3D_OCCT_OP_DIFFERENCE 1
#define W3D_OCCT_OP_INTERSECTION 2

/* A kernel instance's shape registry.
 *
 * Not a global. The first version of this shim kept one static map and a
 * conformance run under `cargo test` — several tests, several threads —
 * mutated it concurrently, which is a data race whatever the map does about
 * it. A context per kernel makes each instance's storage its own, and lets
 * Rust's borrow rules serialise access for free: the mutating entry points
 * take &mut self on the other side, the reading ones take &self.
 *
 * It is not thread-safe *within* one context, and that is deliberate rather
 * than pending: tessellation mutates the shape it meshes (OCCT attaches the
 * triangulation to the face), so even the read-shaped calls write. Parallel
 * meshing means one context per worker, not a lock. */
typedef struct W3dOcctContext W3dOcctContext;

W3dOcctContext *w3d_occt_context_new(void);
void w3d_occt_context_free(W3dOcctContext *ctx);

/* Triangles, owned by the C++ side until w3d_occt_mesh_free. Positions and
 * normals are float because their destination is a GPU buffer; everything the
 * kernel computes with stays double on the other side of this struct. */
typedef struct {
  const float *positions;        /* 3 * vertex_count */
  const float *normals;          /* 3 * vertex_count */
  const uint32_t *indices;       /* 3 * triangle_count */
  const uint32_t *face_of_triangle; /* triangle_count */
  const float *line_positions;   /* 3 * line_vertex_count */
  const uint32_t *line_indices;  /* 2 * line_segment_count */
  uint32_t vertex_count;
  uint32_t triangle_count;
  uint32_t line_vertex_count;
  uint32_t line_segment_count;
  void *owner; /* opaque; pass the struct back to mesh_free */
} W3dOcctMesh;

/* Primitives. All origin-centred: the contract says so, and OCCT's own
 * cylinder is not, which is exactly the kind of difference that must be
 * absorbed here rather than leak upwards. */
int32_t w3d_occt_make_box(W3dOcctContext *ctx, double sx, double sy, double sz, uint32_t *out);
int32_t w3d_occt_make_sphere(W3dOcctContext *ctx, double radius, uint32_t *out);
int32_t w3d_occt_make_cylinder(W3dOcctContext *ctx, double radius, double height, uint32_t *out);

/* `fuzzy` is the document's linear tolerance, passed to OCCT as the boolean
 * fuzzy value. Operands remain valid: the contract's undo depends on it. */
int32_t w3d_occt_boolean(W3dOcctContext *ctx, int32_t op, uint32_t a, uint32_t b, double fuzzy,
                         uint32_t *out);

/* `m34` is 12 doubles, row-major, the top three rows of the 4x4. */
int32_t w3d_occt_transform(W3dOcctContext *ctx, uint32_t body, const double *m34, uint32_t *out);

int32_t w3d_occt_copy(W3dOcctContext *ctx, uint32_t body, uint32_t *out);
int32_t w3d_occt_delete(W3dOcctContext *ctx, uint32_t body);
int32_t w3d_occt_fillet(W3dOcctContext *ctx, uint32_t body, double radius, uint32_t *out);
int32_t w3d_occt_chamfer(W3dOcctContext *ctx, uint32_t body, double distance, uint32_t *out);
int32_t w3d_occt_shell(W3dOcctContext *ctx, uint32_t body, uint32_t face_id, double thickness, uint32_t *out);
int32_t w3d_occt_revolve(W3dOcctContext *ctx, int32_t profile_kind, double p1, double p2,
                         double ax_ox, double ax_oy, double ax_oz,
                         double ax_dx, double ax_dy, double ax_dz,
                         double angle_rad, uint32_t *out);

/* out4: solids, faces, edges, vertices — unique, not per-face duplicates. */
int32_t w3d_occt_topology(W3dOcctContext *ctx, uint32_t body, uint32_t *out4);

/* out6: xmin, ymin, zmin, xmax, ymax, zmax. */
int32_t w3d_occt_bounds(W3dOcctContext *ctx, uint32_t body, double *out6);

int32_t w3d_occt_tessellate(W3dOcctContext *ctx, uint32_t body, double sag,
                            double angle, W3dOcctMesh *out);
void w3d_occt_mesh_free(W3dOcctMesh *mesh);

/* Serialised geometry, owned by the C++ side until w3d_occt_bytes_free.
 *
 * These bytes are OCCT's BREP format and nothing above the seam interprets
 * them: a document records that they are `occt-brep-1` and a different backend
 * refuses them. Moving geometry *between* kernels is what STEP is for, and
 * STEP is not this. */
typedef struct {
  const uint8_t *data;
  uint32_t len;
  void *owner; /* opaque; pass the struct back to bytes_free */
} W3dOcctBytes;

int32_t w3d_occt_save_body(W3dOcctContext *ctx, uint32_t body, W3dOcctBytes *out);

/* Returns W3D_OCCT_ERR_UNSUPPORTED when the bytes are not BREP at all, and
 * W3D_OCCT_ERR_FAILED when they are BREP and damaged — the caller can tell
 * "wrong kernel" from "broken file", and a user deserves to know which. */
int32_t w3d_occt_load_body(W3dOcctContext *ctx, const uint8_t *data, uint32_t len, uint32_t *out);

void w3d_occt_bytes_free(W3dOcctBytes *bytes);

/* Several body ids, owned by the C++ side until w3d_occt_bodies_free. A STEP
 * file holds any number of solids and the caller cannot know how many before
 * reading it, so this is the one place a call answers with a list. */
typedef struct {
  const uint32_t *ids;
  const char *const *names; /* Nullable array of null-terminated strings for product names */
  uint32_t len;
  void *owner; /* opaque; pass the struct back to bodies_free */
} W3dOcctBodies;

/* ---- STEP -----------------------------------------------------------------
 *
 * The only way geometry crosses *between* kernels, and the only entry points
 * here that touch process-global state: STEP's unit, schema and reader
 * settings live in OCCT's `Interface_Static`, which is one table for the whole
 * program, and its diagnostics go to one global messenger. Both are therefore
 * taken under a **process-wide lock** — not a per-context one. It is the only
 * place in this shim where two contexts on two threads can wait on each other,
 * and it is worth knowing before somebody meshes in parallel and wonders why
 * an import serialises everything.
 *
 * Units: the document above has none, so a file has to state one and this one
 * states millimetres, in both directions. A STEP file in inches is converted
 * on the way in.
 */

/* Writes `count` bodies into one STEP file. Refuses `count == 0` as
 * degenerate, and a stale id as unknown, before writing anything: a file with
 * half a document in it is worse than no file. */
int32_t w3d_occt_export_step(W3dOcctContext *ctx, const uint32_t *bodies, uint32_t count,
                             W3dOcctBytes *out);

/* Reads a STEP file, one body per solid in it.
 *
 * Never answers with an empty list: a file that imports into nothing is
 * W3D_OCCT_ERR_FAILED with a message saying what was in it instead. Bytes that
 * are not STEP are W3D_OCCT_ERR_FAILED too — *not* UNSUPPORTED, which this
 * build reserves for "this program cannot read STEP at all" and never says,
 * because it can. */
int32_t w3d_occt_import_step(W3dOcctContext *ctx, const uint8_t *data, uint32_t len,
                             W3dOcctBodies *out);

void w3d_occt_bodies_free(W3dOcctBodies *bodies);

/* The message behind the last W3D_OCCT_ERR_FAILED, for a log. Never for a
 * caller to match on. Valid until the next call on this thread. */
const char *w3d_occt_last_error(void);

/* How many bodies this context holds. For tests that assert nothing leaks. */
uint32_t w3d_occt_live_bodies(const W3dOcctContext *ctx);

#ifdef __cplusplus
}
#endif

#endif /* W3D_OCCT_H */
