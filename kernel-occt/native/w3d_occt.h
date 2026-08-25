/* The seam, in C.
 *
 * This header is the specification of what an OpenCASCADE build must keep
 * exported, and it is deliberately the same shape as `GeometryKernel`:
 * sixteen entry points, one per trait method, plus two for moving a mesh
 * across, one for moving serialised geometry across, and one for the error
 * text. It is not bindings for OCCT — nothing
 * here exposes a TopoDS_Shape, a Handle or a Standard_Real, so nothing above
 * it can start depending on OCCT's vocabulary.
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
  uint32_t vertex_count;
  uint32_t triangle_count;
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

/* The message behind the last W3D_OCCT_ERR_FAILED, for a log. Never for a
 * caller to match on. Valid until the next call on this thread. */
const char *w3d_occt_last_error(void);

/* How many bodies this context holds. For tests that assert nothing leaks. */
uint32_t w3d_occt_live_bodies(const W3dOcctContext *ctx);

#ifdef __cplusplus
}
#endif

#endif /* W3D_OCCT_H */
