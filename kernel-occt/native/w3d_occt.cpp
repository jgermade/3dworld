// The OpenCASCADE side of the seam.
//
// Everything OCCT-shaped is confined to this file: its exceptions are caught
// here and become error codes, its cylinder's base-at-origin convention is
// corrected here, its per-face triangulations are stitched here. Nothing
// leaves through w3d_occt.h that OCCT would recognise as its own.
//
// SPDX-License-Identifier: GPL-3.0-or-later

#include "w3d_occt.h"

#include <cmath>
#include <cstring>
#include <mutex>
#include <sstream>
#include <string>
#include <unordered_map>
#include <vector>

#include <BRepAlgoAPI_Common.hxx>
#include <BRepAlgoAPI_Cut.hxx>
#include <BRepAlgoAPI_Fuse.hxx>
#include <BRepBndLib.hxx>
#include <BRepBuilderAPI_GTransform.hxx>
#include <BRepBuilderAPI_Transform.hxx>
#include <BRepFilletAPI_MakeChamfer.hxx>
#include <BRepFilletAPI_MakeFillet.hxx>
#include <BRepOffsetAPI_MakeThickSolid.hxx>
#include <BRepMesh_IncrementalMesh.hxx>
#include <BRepBuilderAPI_MakeEdge.hxx>
#include <BRepBuilderAPI_MakeFace.hxx>
#include <BRepBuilderAPI_MakePolygon.hxx>
#include <BRepBuilderAPI_MakeWire.hxx>
#include <BRepOffsetAPI_MakePipe.hxx>
#include <BRepOffsetAPI_ThruSections.hxx>
#include <BRepPrimAPI_MakeBox.hxx>
#include <BRepPrimAPI_MakePrism.hxx>
#include <BRepPrimAPI_MakeRevol.hxx>
#include <gp_Circ.hxx>
#include <gp_Pln.hxx>
#include <BRepPrimAPI_MakeCylinder.hxx>
#include <BRepPrimAPI_MakeSphere.hxx>
#include <BRep_Tool.hxx>
#include <Bnd_Box.hxx>
#include <Poly_Triangulation.hxx>
#include <BRepTools.hxx>
#include <BRep_Builder.hxx>
#include <Standard_Failure.hxx>

// STEP, and the machinery for keeping it quiet. See the block above
// w3d_occt_export_step.
#include <APIHeaderSection_MakeHeader.hxx>
#include <Interface_Static.hxx>
#include <Message.hxx>
#include <Message_Messenger.hxx>
#include <Message_Printer.hxx>
#include <STEPControl_Reader.hxx>
#include <STEPControl_Writer.hxx>
#include <STEPCAFControl_Reader.hxx>
#include <StepAP214.hxx>
#include <StepAP214_Protocol.hxx>
#include <StepData_StepModel.hxx>
#include <StepData_StepWriter.hxx>
#include <TCollection_AsciiString.hxx>
#include <TCollection_HAsciiString.hxx>
#include <TDF_Label.hxx>
#include <TDF_LabelSequence.hxx>
#include <TDataStd_Name.hxx>
#include <TDocStd_Document.hxx>
#include <XCAFDoc_DocumentTool.hxx>
#include <XCAFDoc_ShapeTool.hxx>

#include <TopAbs_Orientation.hxx>
#include <TopExp.hxx>
#include <TopExp_Explorer.hxx>
#include <TopTools_IndexedMapOfShape.hxx>
#include <TopoDS.hxx>
#include <TopoDS_Face.hxx>
#include <TopoDS_Shape.hxx>
#include <gp_Ax2.hxx>
#include <gp_GTrsf.hxx>
#include <gp_Pnt.hxx>
#include <gp_Trsf.hxx>

// Ids are never reused within a context. Same rule as the document's arena and
// for the same reason: a handle that outlives its body must stay an error
// rather than quietly name the next occupant.
struct W3dOcctContext {
  std::unordered_map<uint32_t, TopoDS_Shape> bodies;
  uint32_t next_id = 0;

  uint32_t store(const TopoDS_Shape &shape) {
    const uint32_t id = next_id++;
    bodies.emplace(id, shape);
    return id;
  }

  const TopoDS_Shape *find(uint32_t id) const {
    auto it = bodies.find(id);
    return it == bodies.end() ? nullptr : &it->second;
  }
};

namespace {

// Per-thread, not per-context: a message is read immediately by whoever made
// the failing call, and keeping it off the context means the reading entry
// points do not write through a shared pointer.
thread_local std::string g_last_error;

int32_t fail(const char *what) {
  g_last_error = what;
  return W3D_OCCT_ERR_FAILED;
}

int32_t fail(const Standard_Failure &e) {
  g_last_error = e.GetMessageString() ? e.GetMessageString() : "OCCT failure";
  return W3D_OCCT_ERR_FAILED;
}

// Every entry point is wrapped: an OCCT exception crossing into Rust would be
// undefined behaviour, and OCCT throws for input it dislikes rather than
// returning.
template <typename F> int32_t guarded(F &&f) {
  try {
    return f();
  } catch (const Standard_Failure &e) {
    return fail(e);
  } catch (const std::exception &e) {
    return fail(e.what());
  } catch (...) {
    return fail("unknown C++ exception");
  }
}

} // namespace

extern "C" {

W3dOcctContext *w3d_occt_context_new(void) { return new W3dOcctContext(); }

void w3d_occt_context_free(W3dOcctContext *ctx) { delete ctx; }

int32_t w3d_occt_make_box(W3dOcctContext *ctx, double sx, double sy, double sz,
                          uint32_t *out) {
  if (!(sx > 0.0) || !(sy > 0.0) || !(sz > 0.0)) {
    return W3D_OCCT_ERR_DEGENERATE;
  }
  return guarded([&] {
    // OCCT builds from a corner; the contract says origin-centred.
    const gp_Pnt corner(-sx / 2.0, -sy / 2.0, -sz / 2.0);
    *out = ctx->store(BRepPrimAPI_MakeBox(corner, sx, sy, sz).Shape());
    return W3D_OCCT_OK;
  });
}

int32_t w3d_occt_make_sphere(W3dOcctContext *ctx, double radius, uint32_t *out) {
  if (!(radius > 0.0)) {
    return W3D_OCCT_ERR_DEGENERATE;
  }
  return guarded([&] {
    *out = ctx->store(BRepPrimAPI_MakeSphere(radius).Shape());
    return W3D_OCCT_OK;
  });
}

int32_t w3d_occt_make_cylinder(W3dOcctContext *ctx, double radius, double height,
                               uint32_t *out) {
  if (!(radius > 0.0) || !(height > 0.0)) {
    return W3D_OCCT_ERR_DEGENERATE;
  }
  return guarded([&] {
    // OCCT's cylinder sits on its base. Ours is centred, so the axis starts
    // half a height below the origin.
    const gp_Ax2 axis(gp_Pnt(0.0, 0.0, -height / 2.0), gp_Dir(0.0, 0.0, 1.0));
    *out = ctx->store(BRepPrimAPI_MakeCylinder(axis, radius, height).Shape());
    return W3D_OCCT_OK;
  });
}

int32_t w3d_occt_boolean(W3dOcctContext *ctx, int32_t op, uint32_t a, uint32_t b,
                         double fuzzy, uint32_t *out) {
  const TopoDS_Shape *sa = ctx->find(a);
  const TopoDS_Shape *sb = ctx->find(b);
  if (!sa || !sb) {
    return W3D_OCCT_ERR_UNKNOWN_BODY;
  }
  return guarded([&] {
    // Copies, because `store` may rehash the map and invalidate sa/sb, and
    // because the contract requires the operands to survive untouched.
    const TopoDS_Shape shape_a = *sa;
    const TopoDS_Shape shape_b = *sb;

    BRepAlgoAPI_BooleanOperation *algo = nullptr;
    BRepAlgoAPI_Fuse fuse;
    BRepAlgoAPI_Cut cut;
    BRepAlgoAPI_Common common;
    switch (op) {
    case W3D_OCCT_OP_UNION:
      algo = &fuse;
      break;
    case W3D_OCCT_OP_DIFFERENCE:
      algo = &cut;
      break;
    case W3D_OCCT_OP_INTERSECTION:
      algo = &common;
      break;
    default:
      return W3D_OCCT_ERR_UNSUPPORTED;
    }

    TopTools_ListOfShape args, tools;
    args.Append(shape_a);
    tools.Append(shape_b);
    algo->SetArguments(args);
    algo->SetTools(tools);
    if (fuzzy > 0.0) {
      algo->SetFuzzyValue(fuzzy);
    }
    algo->Build();
    if (!algo->IsDone()) {
      return fail("boolean did not complete");
    }
    *out = ctx->store(algo->Shape());
    return W3D_OCCT_OK;
  });
}

static bool is_rigid_plus_uniform_scale(const double *m) {
  // m is 12 doubles: m[0..3] is row 0, m[4..7] is row 1, m[8..11] is row 2
  double c0[3] = {m[0], m[4], m[8]};
  double c1[3] = {m[1], m[5], m[9]};
  double c2[3] = {m[2], m[6], m[10]};

  double l0_sq = c0[0] * c0[0] + c0[1] * c0[1] + c0[2] * c0[2];
  double l1_sq = c1[0] * c1[0] + c1[1] * c1[1] + c1[2] * c1[2];
  double l2_sq = c2[0] * c2[0] + c2[1] * c2[1] + c2[2] * c2[2];

  constexpr double tol = 1e-5;
  if (std::abs(l0_sq - l1_sq) > tol || std::abs(l1_sq - l2_sq) > tol) {
    return false;
  }
  double dot01 = c0[0] * c1[0] + c0[1] * c1[1] + c0[2] * c1[2];
  double dot12 = c1[0] * c2[0] + c1[1] * c2[1] + c1[2] * c2[2];
  double dot20 = c2[0] * c0[0] + c2[1] * c0[1] + c2[2] * c0[2];

  return std::abs(dot01) <= tol && std::abs(dot12) <= tol && std::abs(dot20) <= tol;
}

int32_t w3d_occt_transform(W3dOcctContext *ctx, uint32_t body, const double *m34,
                           uint32_t *out) {
  const TopoDS_Shape *s = ctx->find(body);
  if (!s) {
    return W3D_OCCT_ERR_UNKNOWN_BODY;
  }
  return guarded([&] {
    const TopoDS_Shape shape = *s;
    // gp_Trsf is rigid-plus-uniform-scale and silently normalises non-uniform
    // scales without throwing. Detect rigid-plus-uniform scale explicitly first;
    // fall back to gp_GTrsf for non-uniform scaling or shear.
    if (is_rigid_plus_uniform_scale(m34)) {
      try {
        gp_Trsf t;
        t.SetValues(m34[0], m34[1], m34[2], m34[3], m34[4], m34[5], m34[6],
                    m34[7], m34[8], m34[9], m34[10], m34[11]);
        *out = ctx->store(BRepBuilderAPI_Transform(shape, t, Standard_True).Shape());
        return W3D_OCCT_OK;
      } catch (const Standard_Failure &) {
        // Fall through to GTransform if gp_Trsf rejected construction
      }
    }
    gp_GTrsf g;
    for (int row = 0; row < 3; ++row) {
      for (int col = 0; col < 4; ++col) {
        g.SetValue(row + 1, col + 1, m34[row * 4 + col]);
      }
    }
    *out = ctx->store(BRepBuilderAPI_GTransform(shape, g, Standard_True).Shape());
    return W3D_OCCT_OK;
  });
}

int32_t w3d_occt_copy(W3dOcctContext *ctx, uint32_t body, uint32_t *out) {
  const TopoDS_Shape *s = ctx->find(body);
  if (!s) {
    return W3D_OCCT_ERR_UNKNOWN_BODY;
  }
  // A shallow TopoDS_Shape copy shares the underlying TShape by reference
  // count, so it outlives the original's deletion. That is enough *because
  // bodies are immutable*: sharing is unobservable when nothing mutates. A
  // BRepBuilderAPI_Copy would be a deep copy nobody can tell apart, paid for.
  const TopoDS_Shape shape = *s;
  *out = ctx->store(shape);
  return W3D_OCCT_OK;
}

int32_t w3d_occt_delete(W3dOcctContext *ctx, uint32_t body) {
  return ctx->bodies.erase(body) == 1 ? W3D_OCCT_OK : W3D_OCCT_ERR_UNKNOWN_BODY;
}

int32_t w3d_occt_fillet(W3dOcctContext *ctx, uint32_t body, double radius, uint32_t *out) {
  if (radius <= 0.0) {
    return W3D_OCCT_ERR_DEGENERATE;
  }
  const TopoDS_Shape *s = ctx->find(body);
  if (!s) {
    return W3D_OCCT_ERR_UNKNOWN_BODY;
  }
  return guarded([&] {
    BRepFilletAPI_MakeFillet maker(*s);
    uint32_t edge_count = 0;
    for (TopExp_Explorer e(*s, TopAbs_EDGE); e.More(); e.Next()) {
      maker.Add(radius, TopoDS::Edge(e.Current()));
      edge_count++;
    }
    if (edge_count == 0) {
      return fail("the body has no edges to fillet");
    }
    maker.Build();
    if (!maker.IsDone()) {
      return fail("fillet operation failed: OpenCASCADE could not construct blend surfaces");
    }
    *out = ctx->store(maker.Shape());
    return W3D_OCCT_OK;
  });
}

int32_t w3d_occt_chamfer(W3dOcctContext *ctx, uint32_t body, double distance, uint32_t *out) {
  if (distance <= 0.0) {
    return W3D_OCCT_ERR_DEGENERATE;
  }
  const TopoDS_Shape *s = ctx->find(body);
  if (!s) {
    return W3D_OCCT_ERR_UNKNOWN_BODY;
  }
  return guarded([&] {
    BRepFilletAPI_MakeChamfer maker(*s);
    uint32_t edge_count = 0;
    for (TopExp_Explorer e(*s, TopAbs_EDGE); e.More(); e.Next()) {
      maker.Add(distance, TopoDS::Edge(e.Current()));
      edge_count++;
    }
    if (edge_count == 0) {
      return fail("the body has no edges to chamfer");
    }
    maker.Build();
    if (!maker.IsDone()) {
      return fail("chamfer operation failed: OpenCASCADE could not construct bevel surfaces");
    }
    *out = ctx->store(maker.Shape());
    return W3D_OCCT_OK;
  });
}

int32_t w3d_occt_shell(W3dOcctContext *ctx, uint32_t body, uint32_t face_id, double thickness, uint32_t *out) {
  if (thickness <= 0.0) {
    return W3D_OCCT_ERR_DEGENERATE;
  }
  const TopoDS_Shape *s = ctx->find(body);
  if (!s) {
    return W3D_OCCT_ERR_UNKNOWN_BODY;
  }
  return guarded([&] {
    TopTools_ListOfShape closing_faces;
    uint32_t current_face = 0;
    for (TopExp_Explorer e(*s, TopAbs_FACE); e.More(); e.Next()) {
      if (current_face == face_id) {
        closing_faces.Append(e.Current());
      }
      current_face++;
    }
    BRepOffsetAPI_MakeThickSolid maker;
    maker.MakeThickSolidByJoin(*s, closing_faces, -thickness, 1e-3);
    if (!maker.IsDone()) {
      return fail("shell operation failed: OpenCASCADE could not construct hollow solid");
    }
    *out = ctx->store(maker.Shape());
    return W3D_OCCT_OK;
  });
}

namespace {

/// The plane a profile is drawn on, as OpenCASCADE's own axis placement.
gp_Ax2 profile_plane(const W3dOcctProfile &p) {
  const gp_Pnt origin(p.origin[0], p.origin[1], p.origin[2]);
  const gp_Dir x(p.x_axis[0], p.x_axis[1], p.x_axis[2]);
  const gp_Dir y(p.y_axis[0], p.y_axis[1], p.y_axis[2]);
  return gp_Ax2(origin, x.Crossed(y), x);
}

gp_Pnt profile_point(const W3dOcctProfile &p, double u, double v) {
  return gp_Pnt(p.origin[0] + p.x_axis[0] * u + p.y_axis[0] * v,
                p.origin[1] + p.x_axis[1] * u + p.y_axis[1] * v,
                p.origin[2] + p.x_axis[2] * u + p.y_axis[2] * v);
}

/// The closed outline a profile describes, in three dimensions.
///
/// A rectangle and a circle are centred on the plane's origin, as the contract
/// says a profile is; a polygon's vertices say where it is. The caller has
/// already refused a profile that is not a region — fewer than three corners, no
/// area, an outline that crosses itself — in `Profile::validate`, on the Rust
/// side, so that both backends refuse the same sketches for the same reasons.
TopoDS_Wire profile_wire(const W3dOcctProfile &p) {
  if (p.kind == 1) { // a disc
    const gp_Circ circle(profile_plane(p), p.p1);
    return BRepBuilderAPI_MakeWire(BRepBuilderAPI_MakeEdge(circle).Edge()).Wire();
  }
  BRepBuilderAPI_MakePolygon poly;
  if (p.kind == 2 && p.vertices && p.vertex_count >= 3) {
    for (uint32_t i = 0; i < p.vertex_count; ++i) {
      poly.Add(profile_point(p, p.vertices[i * 2], p.vertices[i * 2 + 1]));
    }
  } else { // a rectangle, centred
    const double w = p.p1 / 2.0;
    const double h = (p.p2 > 0.0 ? p.p2 : p.p1) / 2.0;
    poly.Add(profile_point(p, -w, -h));
    poly.Add(profile_point(p, w, -h));
    poly.Add(profile_point(p, w, h));
    poly.Add(profile_point(p, -w, h));
  }
  poly.Close();
  return poly.Wire();
}

/// The planar face that outline bounds — what every one of these operations
/// sweeps.
TopoDS_Face profile_face(const W3dOcctProfile &p) {
  const TopoDS_Wire wire = profile_wire(p);
  return BRepBuilderAPI_MakeFace(gp_Pln(profile_plane(p)), wire, Standard_True).Face();
}

gp_Vec profile_normal(const W3dOcctProfile &p) { return gp_Vec(profile_plane(p).Direction()); }

} // namespace

int32_t w3d_occt_extrude(W3dOcctContext *ctx, const W3dOcctProfile *profile, double distance,
                         uint32_t *out) {
  if (!profile || distance <= 0.0) {
    return W3D_OCCT_ERR_DEGENERATE;
  }
  return guarded([&] {
    // A real prism over the profile's own outline. This function did not exist
    // before 2026-09-14: the Rust side turned an extrusion into `create_box` or
    // `create_cylinder` with the profile's two numbers, which is why a sketched
    // polygon came out a 20 x 20 slab that straddled the plane it was drawn on.
    const TopoDS_Face face = profile_face(*profile);
    BRepPrimAPI_MakePrism maker(face, profile_normal(*profile) * distance);
    if (!maker.IsDone()) {
      return fail("the profile could not be extruded");
    }
    *out = ctx->store(maker.Shape());
    return W3D_OCCT_OK;
  });
}

int32_t w3d_occt_revolve(W3dOcctContext *ctx, const W3dOcctProfile *profile,
                         const double *axis_origin, const double *axis_dir, double angle_rad,
                         uint32_t *out) {
  if (!profile || !axis_origin || !axis_dir || angle_rad <= 0.0) {
    return W3D_OCCT_ERR_DEGENERATE;
  }
  return guarded([&] {
    // The profile turned about the axis, rather than a sphere or a cylinder
    // chosen from the profile's kind and placed at the axis origin. The old
    // version was right for exactly one case — a disc centred on the axis is a
    // sphere — and wrong for every other, including a rectangle, which it made
    // a cylinder of the rectangle's own width.
    const TopoDS_Face face = profile_face(*profile);
    const gp_Ax1 axis(gp_Pnt(axis_origin[0], axis_origin[1], axis_origin[2]),
                      gp_Dir(axis_dir[0], axis_dir[1], axis_dir[2]));
    BRepPrimAPI_MakeRevol maker(face, axis, angle_rad);
    if (!maker.IsDone()) {
      return fail("the profile could not be revolved about that axis");
    }
    *out = ctx->store(maker.Shape());
    return W3D_OCCT_OK;
  });
}

int32_t w3d_occt_sweep(W3dOcctContext *ctx, const W3dOcctProfile *profile, const double *pts,
                       uint32_t pt_count, uint32_t *out) {
  if (!profile || !pts || pt_count < 2) {
    return W3D_OCCT_ERR_DEGENERATE;
  }
  return guarded([&] {
    const auto point = [&](uint32_t i) {
      return gp_Pnt(pts[i * 3], pts[i * 3 + 1], pts[i * 3 + 2]);
    };
    const TopoDS_Face face = profile_face(*profile);
    if (pt_count == 2) {
      // A straight path is a prism, which is both cheaper and more robust than
      // a pipe over a two-point spine.
      const gp_Vec along(point(0), point(1));
      if (along.Magnitude() <= 0.0) {
        return W3D_OCCT_ERR_DEGENERATE;
      }
      BRepPrimAPI_MakePrism maker(face, along);
      if (!maker.IsDone()) {
        return fail("the profile could not be swept along that path");
      }
      *out = ctx->store(maker.Shape());
      return W3D_OCCT_OK;
    }
    // A bent path: a polyline spine, and the profile run along it. The path was
    // ignored entirely before 2026-09-14 — every sweep was a box 30 long.
    BRepBuilderAPI_MakePolygon spine;
    for (uint32_t i = 0; i < pt_count; ++i) {
      spine.Add(point(i));
    }
    BRepOffsetAPI_MakePipe maker(spine.Wire(), face);
    maker.Build();
    if (!maker.IsDone()) {
      return fail("the profile could not be piped along that path");
    }
    *out = ctx->store(maker.Shape());
    return W3D_OCCT_OK;
  });
}

int32_t w3d_occt_loft(W3dOcctContext *ctx, const W3dOcctProfile *profiles, uint32_t profile_count,
                      uint32_t *out) {
  if (!profiles || profile_count < 2) {
    return W3D_OCCT_ERR_DEGENERATE;
  }
  return guarded([&] {
    // Through the sections, in order, each on its own plane. Both the profiles
    // and the planes were thrown away before 2026-09-14: a loft of anything was
    // a box 20 tall, so a cone came out a cylinder and nothing said so.
    BRepOffsetAPI_ThruSections maker(Standard_True /* build a solid */);
    for (uint32_t i = 0; i < profile_count; ++i) {
      maker.AddWire(profile_wire(profiles[i]));
    }
    maker.Build();
    if (!maker.IsDone()) {
      return fail("the sections could not be lofted");
    }
    *out = ctx->store(maker.Shape());
    return W3D_OCCT_OK;
  });
}

int32_t w3d_occt_topology(W3dOcctContext *ctx, uint32_t body, uint32_t *out4) {
  const TopoDS_Shape *s = ctx->find(body);
  if (!s) {
    return W3D_OCCT_ERR_UNKNOWN_BODY;
  }
  return guarded([&] {
    // Mapped, not explored: an explorer visits an edge once per face that
    // shares it, and reporting twelve edges as twenty-four is the sort of
    // wrong that nothing downstream would notice for months.
    const TopAbs_ShapeEnum kinds[4] = {TopAbs_SOLID, TopAbs_FACE, TopAbs_EDGE,
                                       TopAbs_VERTEX};
    for (int i = 0; i < 4; ++i) {
      TopTools_IndexedMapOfShape map;
      TopExp::MapShapes(*s, kinds[i], map);
      out4[i] = static_cast<uint32_t>(map.Extent());
    }
    return W3D_OCCT_OK;
  });
}

int32_t w3d_occt_bounds(W3dOcctContext *ctx, uint32_t body, double *out6) {
  const TopoDS_Shape *s = ctx->find(body);
  if (!s) {
    return W3D_OCCT_ERR_UNKNOWN_BODY;
  }
  return guarded([&] {
    Bnd_Box box;
    // AddOptimal, not Add: the plain version bounds curved surfaces by their
    // control polygon, which on a sphere is visibly larger than the sphere.
    // The contract says a sphere's bounds are 2r across and means it.
    //
    // useShapeTolerance is Standard_False, and that argument cost a conformance
    // failure to get right. Passing True inflates the box by each vertex's and
    // edge's own tolerance — 1e-7 by default — so a 2x4x6 box reported bounds
    // of 2.0000002 across. That is a defensible number for culling, where a
    // conservative box is the safe one, and the wrong answer to "what are this
    // solid's bounds". The contract asks for the geometry; a caller that wants
    // slack can add its own, and one that cannot tell the difference cannot
    // implement snapping.
    BRepBndLib::AddOptimal(*s, box, Standard_False, Standard_False);
    if (box.IsVoid()) {
      return fail("empty bounding box");
    }
    box.SetGap(0.0);
    box.Get(out6[0], out6[1], out6[2], out6[3], out6[4], out6[5]);
    return W3D_OCCT_OK;
  });
}

namespace {

struct MeshBuffers {
  std::vector<float> positions;
  std::vector<float> normals;
  std::vector<uint32_t> indices;
  std::vector<uint32_t> face_of_triangle;
  std::vector<float> line_positions;
  std::vector<uint32_t> line_indices;
};

} // namespace

int32_t w3d_occt_tessellate(W3dOcctContext *ctx, uint32_t body, double sag,
                            double angle, W3dOcctMesh *out) {
  const TopoDS_Shape *s = ctx->find(body);
  if (!s) {
    return W3D_OCCT_ERR_UNKNOWN_BODY;
  }
  return guarded([&] {
    TopoDS_Shape shape = *s;
    // Not parallel. Determinism is a product property here — the same document
    // must tessellate identically on every machine — and it is worth more than
    // the wall-clock of a display mesh.
    BRepMesh_IncrementalMesh mesher(shape, sag, Standard_False, angle,
                                    Standard_False);
    if (!mesher.IsDone()) {
      return fail("meshing did not complete");
    }

    auto *buf = new MeshBuffers();
    uint32_t face_index = 0;
    for (TopExp_Explorer exp(shape, TopAbs_FACE); exp.More();
         exp.Next(), ++face_index) {
      const TopoDS_Face face = TopoDS::Face(exp.Current());
      TopLoc_Location loc;
      const Handle(Poly_Triangulation) tri = BRep_Tool::Triangulation(face, loc);
      if (tri.IsNull()) {
        continue;
      }
      const gp_Trsf placement = loc.Transformation();
      const bool reversed = face.Orientation() == TopAbs_REVERSED;
      const uint32_t base = static_cast<uint32_t>(buf->positions.size() / 3);
      const int node_count = tri->NbNodes();

      for (int i = 1; i <= node_count; ++i) {
        gp_Pnt p = tri->Node(i);
        p.Transform(placement);
        buf->positions.push_back(static_cast<float>(p.X()));
        buf->positions.push_back(static_cast<float>(p.Y()));
        buf->positions.push_back(static_cast<float>(p.Z()));
        buf->normals.insert(buf->normals.end(), {0.0f, 0.0f, 0.0f});
      }

      for (int t = 1; t <= tri->NbTriangles(); ++t) {
        int a = 0, b = 0, c = 0;
        tri->Triangle(t).Get(a, b, c);
        if (reversed) {
          std::swap(b, c);
        }
        const uint32_t ia = base + static_cast<uint32_t>(a - 1);
        const uint32_t ib = base + static_cast<uint32_t>(b - 1);
        const uint32_t ic = base + static_cast<uint32_t>(c - 1);
        buf->indices.insert(buf->indices.end(), {ia, ib, ic});
        buf->face_of_triangle.push_back(face_index);

        // Accumulate the geometric normal onto each corner. Faces do not share
        // nodes in an OCCT triangulation, so this smooths within a face and
        // leaves a hard edge between faces — which is what a CAD model wants
        // without any crease-angle heuristic.
        const float *pa = &buf->positions[ia * 3];
        const float *pb = &buf->positions[ib * 3];
        const float *pc = &buf->positions[ic * 3];
        const float ux = pb[0] - pa[0], uy = pb[1] - pa[1], uz = pb[2] - pa[2];
        const float vx = pc[0] - pa[0], vy = pc[1] - pa[1], vz = pc[2] - pa[2];
        const float nx = uy * vz - uz * vy;
        const float ny = uz * vx - ux * vz;
        const float nz = ux * vy - uy * vx;
        for (uint32_t idx : {ia, ib, ic}) {
          buf->normals[idx * 3 + 0] += nx;
          buf->normals[idx * 3 + 1] += ny;
          buf->normals[idx * 3 + 2] += nz;
        }
      }
    }

    for (size_t i = 0; i + 2 < buf->normals.size(); i += 3) {
      const float len = std::sqrt(buf->normals[i] * buf->normals[i] +
                                  buf->normals[i + 1] * buf->normals[i + 1] +
                                  buf->normals[i + 2] * buf->normals[i + 2]);
      if (len > 0.0f) {
        buf->normals[i] /= len;
        buf->normals[i + 1] /= len;
        buf->normals[i + 2] /= len;
      } else {
        // A degenerate triangle fan gave no direction. Say so with a valid
        // unit vector rather than shipping NaNs into a vertex buffer.
        buf->normals[i + 2] = 1.0f;
      }
    }

    TopTools_IndexedMapOfShape edge_map;
    TopExp::MapShapes(shape, TopAbs_EDGE, edge_map);
    for (int e = 1; e <= edge_map.Extent(); ++e) {
      const TopoDS_Edge edge = TopoDS::Edge(edge_map(e));
      TopLoc_Location loc;
      const Handle(Poly_Polygon3D) poly = BRep_Tool::Polygon3D(edge, loc);
      if (!poly.IsNull()) {
        const gp_Trsf placement = loc.Transformation();
        const TColgp_Array1OfPnt &nodes = poly->Nodes();
        const uint32_t base = static_cast<uint32_t>(buf->line_positions.size() / 3);
        for (int i = nodes.Lower(); i <= nodes.Upper(); ++i) {
          gp_Pnt p = nodes.Value(i);
          p.Transform(placement);
          buf->line_positions.push_back(static_cast<float>(p.X()));
          buf->line_positions.push_back(static_cast<float>(p.Y()));
          buf->line_positions.push_back(static_cast<float>(p.Z()));
        }
        for (int i = 0; i < nodes.Length() - 1; ++i) {
          buf->line_indices.push_back(base + i);
          buf->line_indices.push_back(base + i + 1);
        }
      } else {
        Handle(Poly_Triangulation) tri;
        Handle(Poly_PolygonOnTriangulation) poly_tri;
        BRep_Tool::PolygonOnTriangulation(edge, poly_tri, tri, loc);
        if (!poly_tri.IsNull() && !tri.IsNull()) {
          const gp_Trsf placement = loc.Transformation();
          const TColStd_Array1OfInteger &nodes = poly_tri->Nodes();
          const uint32_t base = static_cast<uint32_t>(buf->line_positions.size() / 3);
          for (int i = nodes.Lower(); i <= nodes.Upper(); ++i) {
            gp_Pnt p = tri->Node(nodes.Value(i));
            p.Transform(placement);
            buf->line_positions.push_back(static_cast<float>(p.X()));
            buf->line_positions.push_back(static_cast<float>(p.Y()));
            buf->line_positions.push_back(static_cast<float>(p.Z()));
          }
          for (int i = 0; i < nodes.Length() - 1; ++i) {
            buf->line_indices.push_back(base + i);
            buf->line_indices.push_back(base + i + 1);
          }
        }
      }
    }

    out->positions = buf->positions.data();
    out->normals = buf->normals.data();
    out->indices = buf->indices.data();
    out->face_of_triangle = buf->face_of_triangle.data();
    out->line_positions = buf->line_positions.data();
    out->line_indices = buf->line_indices.data();
    out->vertex_count = static_cast<uint32_t>(buf->positions.size() / 3);
    out->triangle_count = static_cast<uint32_t>(buf->face_of_triangle.size());
    out->line_vertex_count = static_cast<uint32_t>(buf->line_positions.size() / 3);
    out->line_segment_count = static_cast<uint32_t>(buf->line_indices.size() / 2);
    out->owner = buf;
    return W3D_OCCT_OK;
  });
}

void w3d_occt_mesh_free(W3dOcctMesh *mesh) {
  if (mesh && mesh->owner) {
    delete static_cast<MeshBuffers *>(mesh->owner);
    std::memset(mesh, 0, sizeof(*mesh));
  }
}

int32_t w3d_occt_save_body(W3dOcctContext *ctx, uint32_t body,
                           W3dOcctBytes *out) {
  const TopoDS_Shape *shape = ctx->find(body);
  if (!shape) {
    return W3D_OCCT_ERR_UNKNOWN_BODY;
  }
  return guarded([&] {
    std::ostringstream stream;
    // OCCT's own BREP. Not an interchange format and not pretending to be:
    // the header says so, and a document records `occt-brep-1` beside it.
    BRepTools::Write(*shape, stream);
    auto *owned = new std::string(stream.str());
    if (owned->empty()) {
      delete owned;
      return fail("BRepTools::Write produced nothing");
    }
    out->data = reinterpret_cast<const uint8_t *>(owned->data());
    out->len = static_cast<uint32_t>(owned->size());
    out->owner = owned;
    return W3D_OCCT_OK;
  });
}

int32_t w3d_occt_load_body(W3dOcctContext *ctx, const uint8_t *data,
                           uint32_t len, uint32_t *out) {
  if (!data || len == 0) {
    return W3D_OCCT_ERR_UNSUPPORTED;
  }
  // BREP carries this signature near the start — "\nCASCADE Topology V3, (c)
  // Open Cascade" on 7.6, with the version digit varying. Searching a window
  // rather than matching at offset zero is deliberate: the leading newline and
  // the version are both things OCCT has changed before.
  //
  // Checking it here is what lets a caller tell "these are another kernel's
  // bytes" from "these are ours and broken", because BRepTools::Read reports
  // both the same way — it simply leaves the shape null.
  static const char kSignature[] = "CASCADE Topology";
  const size_t window = len < 64 ? len : 64;
  const char *begin = reinterpret_cast<const char *>(data);
  if (std::string(begin, window).find(kSignature) == std::string::npos) {
    return W3D_OCCT_ERR_UNSUPPORTED;
  }
  return guarded([&] {
    std::istringstream stream(
        std::string(reinterpret_cast<const char *>(data), len));
    TopoDS_Shape shape;
    BRep_Builder builder;
    BRepTools::Read(shape, stream, builder);
    if (shape.IsNull()) {
      return fail("the BREP data did not describe a shape");
    }
    *out = ctx->store(shape);
    return W3D_OCCT_OK;
  });
}

void w3d_occt_bytes_free(W3dOcctBytes *bytes) {
  if (bytes && bytes->owner) {
    delete static_cast<std::string *>(bytes->owner);
    std::memset(bytes, 0, sizeof(*bytes));
  }
}

namespace {

// Everything STEP needs that is not per-context, and all of it is global to
// the *process*: `Interface_Static` is one settings table for the whole
// program, and OCCT's diagnostics go to one default messenger. So both
// directions take this lock. It is the only lock in the shim, and the header
// says so where somebody parallelising tessellation will read it.
std::mutex g_step_lock;

/// Catches OCCT's own diagnostics instead of letting them reach stdout.
///
/// The STEP reader reports a syntax error by *printing* it — "Line 2:
/// Incorrect syntax: unexpected TYPE" — and returning a status code with no
/// detail in it. So the choice is between a library writing to somebody's
/// terminal and catching what it writes. Caught, it becomes the message behind
/// W3D_OCCT_ERR_FAILED, which is where a user can actually read it; and the
/// transfer statistics OCCT prints on every export stop existing, which they
/// should, because a browser console is not a log file.
class Collector : public Message_Printer {
public:
  mutable std::string text;

protected:
  void send(const TCollection_AsciiString &message,
            const Message_Gravity gravity) const override {
    if (gravity < Message_Warning) {
      return; // statistics and progress: noise, and not ours to print
    }
    if (!text.empty()) {
      text += "; ";
    }
    text += message.ToCString();
  }
};

// Installs a Collector for the duration of a scope and puts the real printers
// back afterwards, including when an OCCT exception unwinds through it.
struct Diagnostics {
  Handle(Collector) collector;
  Message_SequenceOfPrinters saved;

  Diagnostics() : collector(new Collector) {
    Message_SequenceOfPrinters &printers =
        Message::DefaultMessenger()->ChangePrinters();
    saved = printers;
    printers.Clear();
    printers.Append(collector);
  }

  ~Diagnostics() {
    Message::DefaultMessenger()->ChangePrinters() = saved;
  }

  // What OCCT said, or `fallback` when it said nothing. An error with no
  // sentence in it is the failure mode this whole class exists to avoid.
  std::string say(const char *fallback) const {
    return collector->text.empty() ? std::string(fallback) : collector->text;
  }
};

} // namespace

int32_t w3d_occt_export_step(W3dOcctContext *ctx, const uint32_t *bodies,
                             uint32_t count, W3dOcctBytes *out) {
  if (!bodies || count == 0) {
    return W3D_OCCT_ERR_DEGENERATE;
  }
  // Every handle resolved before a byte is written, so a stale one is a
  // refusal rather than a file with half a document in it.
  std::vector<TopoDS_Shape> shapes;
  shapes.reserve(count);
  for (uint32_t i = 0; i < count; ++i) {
    const TopoDS_Shape *shape = ctx->find(bodies[i]);
    if (!shape) {
      return W3D_OCCT_ERR_UNKNOWN_BODY;
    }
    shapes.push_back(*shape);
  }

  return guarded([&] {
    const std::lock_guard<std::mutex> lock(g_step_lock);
    const Diagnostics diagnostics;

    // Millimetres. The document above has no units at all — a number in it is
    // just a number — so exporting is the moment somebody has to decide what
    // those numbers meant, and this is that decision, stated in the file where
    // the receiving program will read it. The import side states the same one.
    Interface_Static::SetCVal("write.step.unit", "MM");
    // AP214 IS. AP242 is the newer schema and the one to move to when there is
    // anything in a document worth carrying that AP214 cannot hold — colours,
    // assemblies, tolerances. Today there is not, and AP214 is what every
    // program in the list this format exists to reach has read for twenty
    // years.
    Interface_Static::SetIVal("write.step.schema", 4);

    STEPControl_Writer writer;
    for (const TopoDS_Shape &shape : shapes) {
      if (writer.Transfer(shape, STEPControl_AsIs) != IFSelect_RetDone) {
        return fail(diagnostics.say("OpenCASCADE would not transfer the shape "
                                    "to STEP").c_str());
      }
    }

    Handle(StepData_StepModel) model = writer.Model();
    // Who wrote it. Not decoration: the first question asked about a STEP file
    // that opens badly somewhere else is which program produced it, and the
    // default answer here is "Open CASCADE Shape Model", which is a true
    // statement about the library and a useless one about the program.
    APIHeaderSection_MakeHeader header(model);
    header.SetName(new TCollection_HAsciiString("3dworld"));
    header.SetOriginatingSystem(new TCollection_HAsciiString("3dworld"));
    header.Apply(model);

    // Written to a stream rather than through STEPControl_Writer::Write, which
    // only takes a filename. The trait returns bytes because the browser needs
    // bytes — there is no filesystem to write to there — and a temporary file
    // on the way out would be a filesystem dependency inside the kernel.
    StepData_StepWriter step(model);
    step.SendModel(StepAP214::Protocol());
    std::ostringstream stream;
    if (!step.Print(stream)) {
      return fail(diagnostics.say("the STEP writer produced nothing").c_str());
    }
    auto *owned = new std::string(stream.str());
    if (owned->empty()) {
      delete owned;
      return fail("the STEP writer produced nothing");
    }
    out->data = reinterpret_cast<const uint8_t *>(owned->data());
    out->len = static_cast<uint32_t>(owned->size());
    out->owner = owned;
    return W3D_OCCT_OK;
  });
}

namespace {

struct OcctImportOwner {
  std::vector<uint32_t> ids;
  std::vector<std::string> names_str;
  std::vector<const char *> names_ptr;
  std::vector<uint32_t> parents;
  std::vector<std::string> assembly_names_str;
  std::vector<const char *> assembly_names_ptr;
  std::vector<uint32_t> assembly_parents;
};

/// One solid placement found in the file, and where it sat.
struct FoundSolid {
  TopoDS_Shape shape;
  std::string name;
  uint32_t parent;
};

/// An interior node of the file's assembly tree.
struct FoundAssembly {
  std::string name;
  uint32_t parent;
};

/// The name a *file* gave a label, and nothing OpenCASCADE invented.
///
/// XDE names an instance label that the file left unnamed after the label it
/// refers to — `=>[0:1:1:11]` — which is a debugging aid and not a product
/// name. It reached the Outliner as a node called `=>[0:1:1:11]` until this
/// function started refusing it.
std::string label_name(const TDF_Label &label) {
  Handle(TDataStd_Name) attr;
  if (!label.IsNull() && label.FindAttribute(TDataStd_Name::GetID(), attr)) {
    std::string name = TCollection_AsciiString(attr->Get()).ToCString();
    if (name.rfind("=>", 0) == 0) {
      return {};
    }
    return name;
  }
  return {};
}

/// Whether a label's components are an *arrangement* — a subassembly holding
/// parts — or one product's several shape representations.
///
/// XDE marks both as assemblies, and this is the distinction it does not draw.
/// Pro/ENGINEER writes every part in the AS1 sample as a product holding two
/// representations, a `SOLID` and a `COMPOUND`, so `BOLT` arrives as an
/// "assembly" of two unnamed instances. Taken at face value that puts a group
/// node around every single part, names the part's own body after nothing, and
/// turns a tree of ten assemblies into one of twenty-eight.
///
/// The signal is the *names*: a file names the components of a real assembly,
/// because that is how a user refers to them, and leaves the representations of
/// one product unnamed. So a label holds structure when at least one component
/// carries a name the file wrote.
///
/// It is a judgement about how STEP is written rather than a rule STEP states,
/// and what it costs when it is wrong is bounded and worth knowing: an assembly
/// whose components a file left *entirely* unnamed collapses into one node.
/// Every solid under it still arrives, still in the right place — only the one
/// level of structure is lost, and there was no name in the file to label it
/// with anyway.
bool holds_structure(const TDF_LabelSequence &components) {
  for (int c = 1; c <= components.Length(); ++c) {
    if (!label_name(components.Value(c)).empty()) {
      return true;
    }
  }
  return false;
}

} // namespace

int32_t w3d_occt_import_step(W3dOcctContext *ctx, const uint8_t *data,
                             uint32_t len, W3dOcctImport *out) {
  if (!data || len == 0) {
    return fail("no bytes: not a STEP file");
  }
  return guarded([&] {
    const std::lock_guard<std::mutex> lock(g_step_lock);
    const Diagnostics diagnostics;

    // The unit STEP is converted *to*, and the same one export states. A file
    // in inches arrives scaled, which is the whole point of a file carrying
    // its unit.
    Interface_Static::SetCVal("xstep.cascade.unit", "MM");

    std::vector<FoundSolid> solids;
    std::vector<FoundAssembly> assemblies;
    uint32_t faces = 0;

    // Try XDE / CAF reader first to extract product names and assembly hierarchy
    Handle(TDocStd_Document) doc = new TDocStd_Document("XmlXCAF");
    STEPCAFControl_Reader caf_reader;
    caf_reader.SetColorMode(Standard_True);
    caf_reader.SetNameMode(Standard_True);
    caf_reader.SetLayerMode(Standard_True);

    std::istringstream stream(
        std::string(reinterpret_cast<const char *>(data), len));

    if (caf_reader.ChangeReader().ReadStream("w3d", stream) == IFSelect_RetDone &&
        caf_reader.Transfer(doc)) {
      Handle(XCAFDoc_ShapeTool) shape_tool =
          XCAFDoc_DocumentTool::ShapeTool(doc->Main());
      TDF_LabelSequence free_shapes;
      shape_tool->GetFreeShapes(free_shapes);

      // Walks the XDE tree, emitting a solid where the tree has a part and an
      // assembly entry where it has structure — and emitting each solid
      // **once**.
      //
      // The walk this replaced did neither. It took the solids of every label
      // it met, and an assembly's label holds a compound of its whole subtree,
      // so every solid was emitted again for each ancestor above it: the AS1
      // assembly's eighteen placements arrived as thirty-six bodies, eighteen
      // of them named after the root product. And it recursed with
      // `GetComponents` on a component, which is a *reference* and has no
      // components of its own — the structure lives on the label it refers to
      // — so the walk stopped one level down and the names of the parts
      // themselves (L_BRACKET, NUT, BOLT) never left the file.
      //
      // `location` is why this cannot just take the top compound and be done.
      // A component's placement is relative to the assembly holding it, so the
      // global position of a bolt is the product of every location on the way
      // down. The old walk got that for free by reading solids out of the root
      // compound, where OCCT had already applied them; a walk that visits
      // parts has to carry it.
      auto walk = [&](auto self, const TDF_Label &label, uint32_t parent,
                      const TopLoc_Location &location) -> void {
        std::string name = label_name(label);
        TDF_Label target = label;
        TopLoc_Location here = location;

        if (XCAFDoc_ShapeTool::IsReference(label)) {
          here = location * XCAFDoc_ShapeTool::GetLocation(label);
          TDF_Label referred;
          if (XCAFDoc_ShapeTool::GetReferredShape(label, referred)) {
            target = referred;
            // The instance's name first, because that is what a file calls
            // this placement of the part, and the part's own name when the
            // placement has none.
            if (name.empty()) {
              name = label_name(referred);
            }
          }
        }

        TDF_LabelSequence components;
        const bool interior = XCAFDoc_ShapeTool::GetComponents(target, components) &&
                              components.Length() > 0 && holds_structure(components);

        if (interior) {
          const uint32_t me = static_cast<uint32_t>(assemblies.size());
          assemblies.push_back({name, parent});
          for (int c = 1; c <= components.Length(); ++c) {
            self(self, components.Value(c), me, here);
          }
          return;
        }

        // A leaf: this is a part, and its solids are the bodies. The shape
        // comes from the referred label in its own coordinates and is moved
        // into the world by the locations gathered on the way here.
        TopoDS_Shape shape = shape_tool->GetShape(target);
        if (shape.IsNull()) {
          return;
        }
        shape.Move(here);
        for (TopExp_Explorer e(shape, TopAbs_SOLID); e.More(); e.Next()) {
          solids.push_back({e.Current(), name, parent});
        }
        TopTools_IndexedMapOfShape map;
        TopExp::MapShapes(shape, TopAbs_FACE, map);
        faces += static_cast<uint32_t>(map.Extent());
      };

      for (int i = 1; i <= free_shapes.Length(); ++i) {
        walk(walk, free_shapes.Value(i), W3D_OCCT_NO_PARENT, TopLoc_Location());
      }
    }

    // Fallback to standard STEPControl_Reader if XDE yielded no solids
    if (solids.empty()) {
      STEPControl_Reader reader;
      std::istringstream stream_fallback(
          std::string(reinterpret_cast<const char *>(data), len));
      if (reader.ReadStream("w3d", stream_fallback) != IFSelect_RetDone) {
        return fail(diagnostics.say("not a STEP file").c_str());
      }
      reader.TransferRoots();
      for (int i = 1; i <= reader.NbShapes(); ++i) {
        const TopoDS_Shape shape = reader.Shape(i);
        for (TopExp_Explorer e(shape, TopAbs_SOLID); e.More(); e.Next()) {
          solids.push_back({e.Current(), "", W3D_OCCT_NO_PARENT});
        }
        TopTools_IndexedMapOfShape map;
        TopExp::MapShapes(shape, TopAbs_FACE, map);
        faces += static_cast<uint32_t>(map.Extent());
      }
    }

    if (solids.empty()) {
      std::ostringstream why;
      why << "the STEP file has no solids in it: " << faces
          << (faces == 1 ? " face, and no closed volume"
                         : " faces, and no closed volume");
      return fail(why.str().c_str());
    }

    auto *owner = new OcctImportOwner();
    owner->ids.reserve(solids.size());
    owner->names_str.reserve(solids.size());
    owner->names_ptr.reserve(solids.size());
    owner->parents.reserve(solids.size());

    for (const auto &item : solids) {
      owner->ids.push_back(ctx->store(item.shape));
      owner->names_str.push_back(item.name);
      owner->parents.push_back(item.parent);
    }
    for (const auto &s : owner->names_str) {
      owner->names_ptr.push_back(s.empty() ? nullptr : s.c_str());
    }
    owner->assembly_names_str.reserve(assemblies.size());
    owner->assembly_names_ptr.reserve(assemblies.size());
    owner->assembly_parents.reserve(assemblies.size());
    for (const auto &item : assemblies) {
      owner->assembly_names_str.push_back(item.name);
      owner->assembly_parents.push_back(item.parent);
    }
    for (const auto &s : owner->assembly_names_str) {
      owner->assembly_names_ptr.push_back(s.empty() ? nullptr : s.c_str());
    }

    out->ids = owner->ids.data();
    out->names = owner->names_ptr.data();
    out->parents = owner->parents.data();
    out->len = static_cast<uint32_t>(owner->ids.size());
    out->assembly_names = owner->assembly_names_ptr.data();
    out->assembly_parents = owner->assembly_parents.data();
    out->assemblies = static_cast<uint32_t>(owner->assembly_names_ptr.size());
    out->owner = owner;
    return W3D_OCCT_OK;
  });
}

void w3d_occt_import_free(W3dOcctImport *imported) {
  if (imported && imported->owner) {
    delete static_cast<OcctImportOwner *>(imported->owner);
    std::memset(imported, 0, sizeof(*imported));
  }
}

const char *w3d_occt_last_error(void) { return g_last_error.c_str(); }

uint32_t w3d_occt_live_bodies(const W3dOcctContext *ctx) {
  return static_cast<uint32_t>(ctx->bodies.size());
}

} // extern "C"
