//! The outline of one tessellated face, recovered from the triangles.
//!
//! A kernel's trait has no local face operation — no "pull this face" — and
//! the only face a document can name is the one the *mesh* labelled, through
//! `Mesh::face_of_triangle`. So push/pull is built from what is there: take the
//! triangles of a face, chain their boundary edges into a loop, and hand that
//! loop back as a profile the kernel can extrude. The result is swept into the
//! solid with a boolean, which every backend already has.
//!
//! **This only works on a planar face**, and says so rather than guessing. A
//! cylinder's side is one face id whose triangles are a fan of chords: an
//! outline pulled off it would be a prism over a polygon that is not the
//! surface, and the union of that with the body is a shape nobody asked for. A
//! curved face is [`FaceError::NotPlanar`], which the UI shows; it is not a
//! silent fallback to moving the whole body, which is what this code replaced.

use w3d_kernel::{KernelError, Mesh, Vec3};

/// A vertex welded onto a grid: what makes two coincident corners one corner.
type Welded = (i64, i64, i64);

/// Why a face's outline could not be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FaceError {
    /// No triangle carries this face id.
    NoSuchFace(u32),
    /// The face's triangles do not lie in one plane, so an outline pulled off
    /// it would not describe the surface.
    NotPlanar,
    /// The boundary edges do not chain into exactly one closed loop — a face
    /// with a hole in it, or a tessellation with a crack in it.
    NotOneLoop,
    /// A loop with fewer than three distinct corners, or with no area.
    Degenerate,
}

impl FaceError {
    /// The same refusal, in the vocabulary the document and the kernel share,
    /// so that a UI shows one sentence whether the face or the backend said no.
    #[must_use]
    pub fn kernel_error(&self) -> KernelError {
        match self {
            Self::NoSuchFace(id) => KernelError::Failed(format!("face #{id} is not on this body")),
            Self::NotPlanar => {
                KernelError::Degenerate("that face is curved, and push/pull needs a flat one")
            }
            Self::NotOneLoop => KernelError::Degenerate(
                "that face's outline is not one closed loop, so it cannot be pulled",
            ),
            Self::Degenerate => KernelError::Degenerate("that face's outline encloses no area"),
        }
    }
}

impl core::fmt::Display for FaceError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoSuchFace(id) => write!(f, "face #{id} is not on this body"),
            Self::NotPlanar => write!(
                f,
                "that face is curved, and this operation needs a flat one"
            ),
            Self::NotOneLoop => write!(
                f,
                "that face's outline is not a single closed loop — a hole, or a gap in the mesh"
            ),
            Self::Degenerate => write!(f, "that face's outline encloses no area"),
        }
    }
}

/// One planar face's outline, in world coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceLoop {
    /// The corners, in order, wound counter-clockwise seen from outside the
    /// body — the first point is not repeated at the end.
    pub points: Vec<Vec3>,
    /// A point on the face's plane: the area-weighted centroid.
    pub origin: Vec3,
    /// The outward unit normal.
    pub normal: Vec3,
    /// The largest side of the loop's bounding box.
    pub extent: f64,
}

/// Recovers the outline of `face_id`.
///
/// `weld` is the distance below which two vertices are one vertex: the
/// tessellator is free to emit a face's triangles with unshared vertices, and
/// an edge is only "used twice" once they are welded. It doubles as the
/// flatness tolerance.
///
/// # Errors
/// [`FaceError`], one per way the triangles can fail to describe a flat region.
pub fn face_loop(mesh: &Mesh, face_id: u32, weld: f64) -> Result<FaceLoop, FaceError> {
    let metrics = mesh
        .face_metrics(face_id)
        .ok_or(FaceError::NoSuchFace(face_id))?;
    let normal = metrics.normal;
    let origin = metrics.centroid;

    let triangles_of_face: Vec<[usize; 3]> = (0..mesh.triangle_count())
        .filter(|t| mesh.face_of_triangle.get(*t).copied() == Some(face_id))
        .map(|t| {
            [
                mesh.indices[3 * t] as usize,
                mesh.indices[3 * t + 1] as usize,
                mesh.indices[3 * t + 2] as usize,
            ]
        })
        .filter(|idx| idx.iter().all(|&i| i < mesh.positions.len()))
        .collect();
    if triangles_of_face.is_empty() {
        return Err(FaceError::NoSuchFace(face_id));
    }

    // Positions are `f32`: a face at x = 1000 carries about 6e-5 of noise in
    // its coordinates, and a weld distance finer than that would split one
    // corner into two and call a flat face curved. So the caller's tolerance is
    // a floor, and the face's own distance from the origin sets the rest.
    let far = triangles_of_face
        .iter()
        .flatten()
        .flat_map(|&i| mesh.positions[i])
        .fold(0.0f32, |acc, v| acc.max(v.abs()));
    let weld = weld.max(f64::from(far) * 1.0e-5).max(1.0e-12);

    // Every vertex of the face, welded onto a grid, so that the edge each
    // triangle shares with its neighbour is one edge and not two.
    let key = |p: Vec3| -> Welded {
        let q = |v: f64| (v / weld).round() as i64;
        (q(p.x), q(p.y), q(p.z))
    };
    let point = |i: usize| -> Vec3 {
        let p = mesh.positions[i];
        Vec3::new(f64::from(p[0]), f64::from(p[1]), f64::from(p[2]))
    };

    let mut corner = std::collections::HashMap::new();
    let mut directed: std::collections::HashMap<(Welded, Welded), i32> =
        std::collections::HashMap::new();
    let mut flatness: f64 = 0.0;

    for idx in &triangles_of_face {
        let p = idx.map(point);
        for v in p {
            flatness = flatness.max((v - origin).dot(normal).abs());
            corner.insert(key(v), v);
        }
        for (a, b) in [(p[0], p[1]), (p[1], p[2]), (p[2], p[0])] {
            let (ka, kb) = (key(a), key(b));
            if ka == kb {
                continue;
            }
            // One counter per undirected edge, signed by the direction it was
            // walked. An interior edge is walked once each way and cancels; a
            // boundary edge keeps the winding of the triangle it came from,
            // which is the winding the loop needs.
            let (lo, hi, step) = if ka < kb { (ka, kb, 1) } else { (kb, ka, -1) };
            *directed.entry((lo, hi)).or_insert(0) += step;
        }
    }

    // A flat face's vertices sit on its plane, to within the same distance that
    // welds two of them into one.
    if flatness > weld {
        return Err(FaceError::NotPlanar);
    }

    let mut next: std::collections::HashMap<Welded, Welded> = std::collections::HashMap::new();
    for ((lo, hi), count) in directed {
        match count {
            0 => {}
            1 => {
                if next.insert(lo, hi).is_some() {
                    return Err(FaceError::NotOneLoop);
                }
            }
            -1 => {
                if next.insert(hi, lo).is_some() {
                    return Err(FaceError::NotOneLoop);
                }
            }
            _ => return Err(FaceError::NotOneLoop),
        }
    }
    if next.len() < 3 {
        return Err(FaceError::Degenerate);
    }

    let start = *next.keys().next().ok_or(FaceError::Degenerate)?;
    let mut order = Vec::with_capacity(next.len());
    let mut at = start;
    loop {
        order.push(at);
        let Some(&to) = next.get(&at) else {
            return Err(FaceError::NotOneLoop);
        };
        at = to;
        if at == start {
            break;
        }
        if order.len() > next.len() {
            return Err(FaceError::NotOneLoop);
        }
    }
    // Walking from one boundary edge must reach every boundary edge: a face
    // with a hole gives two loops, and a prism over the outer one alone would
    // fill the hole in.
    if order.len() != next.len() {
        return Err(FaceError::NotOneLoop);
    }

    let mut points: Vec<Vec3> = order
        .iter()
        .map(|k| corner.get(k).copied().ok_or(FaceError::Degenerate))
        .collect::<Result<_, _>>()?;
    drop_collinear(&mut points, weld);
    if points.len() < 3 {
        return Err(FaceError::Degenerate);
    }

    // Newell's area vector: its size is twice the loop's area, and its sign
    // against the outward normal is the winding.
    let area2 = points
        .iter()
        .enumerate()
        .fold(Vec3::ZERO, |acc, (i, &p)| {
            acc + p.cross(points[(i + 1) % points.len()])
        })
        .dot(normal);
    if area2.abs() <= weld * weld {
        return Err(FaceError::Degenerate);
    }
    if area2 < 0.0 {
        points.reverse();
    }

    let mut lo = points[0];
    let mut hi = points[0];
    for p in &points {
        lo = Vec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
        hi = Vec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
    }
    let size = hi - lo;
    let extent = size.x.max(size.y).max(size.z);

    Ok(FaceLoop {
        points,
        origin,
        normal,
        extent,
    })
}

/// Drops the corners the tessellator left in the middle of a straight side.
///
/// A box's face comes back as two triangles and four corners, but a face split
/// by a neighbour's tessellation comes back with a vertex partway along a side.
/// They describe the same region; the shorter list is the one a kernel builds a
/// clean prism over.
fn drop_collinear(points: &mut Vec<Vec3>, weld: f64) {
    if points.len() < 3 {
        return;
    }
    let mut keep = Vec::with_capacity(points.len());
    for i in 0..points.len() {
        let prev = points[(i + points.len() - 1) % points.len()];
        let here = points[i];
        let next = points[(i + 1) % points.len()];
        let a = here - prev;
        let b = next - here;
        // Twice the area of the triangle the three corners make. Scaled by the
        // sides, so this is a distance: how far `here` stands off the line.
        let off = a.cross(b).length();
        let span = a.length().max(b.length());
        if span <= weld || off / span > weld {
            keep.push(here);
        }
    }
    if keep.len() >= 3 {
        *points = keep;
    }
}

impl FaceLoop {
    /// The loop as `(u, v)` pairs on a right-handed frame whose third axis is
    /// `along`, wound counter-clockwise seen from `+along`.
    ///
    /// This is what [`w3d_kernel::Profile::Polygon`] wants: the kernel extrudes
    /// a profile along its own `+Z` from `z = 0`, so the caller places the
    /// prism with the same frame.
    #[must_use]
    pub fn projected(&self, along: Vec3) -> (Vec<(f64, f64)>, Vec3, Vec3) {
        let w = along;
        // Any unit vector across `w`. Picking the world axis `w` leans on least
        // keeps the cross product well away from zero.
        let seed = if w.x.abs() <= w.y.abs() && w.x.abs() <= w.z.abs() {
            Vec3::X
        } else if w.y.abs() <= w.z.abs() {
            Vec3::Y
        } else {
            Vec3::Z
        };
        let x_axis = seed.cross(w).normalize(1.0e-12).unwrap_or(Vec3::X);
        let y_axis = w.cross(x_axis);

        let mut uv: Vec<(f64, f64)> = self
            .points
            .iter()
            .map(|&p| {
                let d = p - self.origin;
                (d.dot(x_axis), d.dot(y_axis))
            })
            .collect();
        let signed: f64 = uv
            .iter()
            .enumerate()
            .map(|(i, &(u, v))| {
                let (u2, v2) = uv[(i + 1) % uv.len()];
                u * v2 - u2 * v
            })
            .sum();
        if signed < 0.0 {
            uv.reverse();
        }
        (uv, x_axis, y_axis)
    }
}

/// Moves every side of a closed, counter-clockwise polygon inward by `delta`
/// (outward, for a negative one).
///
/// Each corner slides along the bisector of the two sides that meet there, so
/// the sides stay parallel to where they were. It is exact for the offsets this
/// is used with — a fraction of a tolerance — and makes no attempt at the
/// self-intersections a large offset produces on a concave outline, because a
/// large offset is not a thing any caller here asks for.
#[must_use]
pub fn offset_polygon(uv: &[(f64, f64)], delta: f64) -> Vec<(f64, f64)> {
    if delta == 0.0 || uv.len() < 3 {
        return uv.to_vec();
    }
    let n = uv.len();
    (0..n)
        .map(|i| {
            let (px, py) = uv[(i + n - 1) % n];
            let (cx, cy) = uv[i];
            let (nx, ny) = uv[(i + 1) % n];
            // Counter-clockwise means the inside is to the left of each
            // directed side, so `(-dy, dx)` normalised points inward.
            let inward = |(ax, ay): (f64, f64), (bx, by): (f64, f64)| {
                let (dx, dy) = (bx - ax, by - ay);
                let len = dx.hypot(dy);
                if len <= 0.0 {
                    (0.0, 0.0)
                } else {
                    (-dy / len, dx / len)
                }
            };
            let (ax, ay) = inward((px, py), (cx, cy));
            let (bx, by) = inward((cx, cy), (nx, ny));
            // The new corner is where the two offset sides cross: solving
            // `(p - c)·a = (p - c)·b = delta` for `p = c + t(a + b)` gives
            // `t = delta / (1 + a·b)`, which is what keeps a sharp corner sharp
            // rather than cutting it off.
            let dot = ax * bx + ay * by;
            if (1.0 + dot).abs() <= 1.0e-9 {
                (cx, cy)
            } else {
                let t = delta / (1.0 + dot);
                (cx + (ax + bx) * t, cy + (ay + by) * t)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unit box's six faces, one face id each, vertices unshared between
    /// faces — the shape every backend's tessellator hands back.
    fn box_mesh() -> Mesh {
        let mut mesh = Mesh::default();
        let corners = [
            [-1.0f32, -1.0, -1.0],
            [1.0, -1.0, -1.0],
            [1.0, 1.0, -1.0],
            [-1.0, 1.0, -1.0],
            [-1.0, -1.0, 1.0],
            [1.0, -1.0, 1.0],
            [1.0, 1.0, 1.0],
            [-1.0, 1.0, 1.0],
        ];
        let faces: [[usize; 4]; 6] = [
            [4, 5, 6, 7], // +Z
            [1, 0, 3, 2], // -Z
            [0, 1, 5, 4], // -Y
            [2, 3, 7, 6], // +Y
            [1, 2, 6, 5], // +X
            [3, 0, 4, 7], // -X
        ];
        for (f, quad) in faces.iter().enumerate() {
            let base = mesh.positions.len() as u32;
            for &c in quad {
                mesh.positions.push(corners[c]);
                mesh.normals.push([0.0, 0.0, 0.0]);
            }
            mesh.indices
                .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
            mesh.face_of_triangle.push(f as u32);
            mesh.face_of_triangle.push(f as u32);
        }
        mesh
    }

    #[test]
    fn a_box_face_comes_back_as_its_four_corners() {
        let mesh = box_mesh();
        let loop_ = face_loop(&mesh, 0, 1.0e-6).unwrap();
        assert_eq!(loop_.points.len(), 4, "{:?}", loop_.points);
        assert!((loop_.normal.z - 1.0).abs() < 1.0e-9, "{:?}", loop_.normal);
        assert!((loop_.origin.z - 1.0).abs() < 1.0e-9);
        assert!((loop_.extent - 2.0).abs() < 1.0e-9);
    }

    #[test]
    fn the_loop_winds_counter_clockwise_seen_from_outside() {
        let mesh = box_mesh();
        for face in 0..6 {
            let loop_ = face_loop(&mesh, face, 1.0e-6).unwrap();
            let area2 = loop_
                .points
                .iter()
                .enumerate()
                .fold(Vec3::ZERO, |acc, (i, &p)| {
                    acc + p.cross(loop_.points[(i + 1) % loop_.points.len()])
                })
                .dot(loop_.normal);
            assert!(area2 > 0.0, "face {face} winds the wrong way: {area2}");
        }
    }

    #[test]
    fn a_vertex_left_partway_along_a_side_is_dropped() {
        let mut mesh = Mesh::default();
        // A square on z = 0, triangulated through a midpoint on one side.
        let pts: [[f32; 3]; 5] = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [2.0, 2.0, 0.0],
            [0.0, 2.0, 0.0],
        ];
        mesh.positions.extend_from_slice(&pts);
        mesh.normals = vec![[0.0, 0.0, 1.0]; 5];
        mesh.indices.extend_from_slice(&[0, 1, 4, 1, 3, 4, 1, 2, 3]);
        mesh.face_of_triangle = vec![7, 7, 7];

        let loop_ = face_loop(&mesh, 7, 1.0e-6).unwrap();
        assert_eq!(loop_.points.len(), 4, "{:?}", loop_.points);
    }

    #[test]
    fn a_curved_face_is_refused_rather_than_flattened() {
        let mut mesh = Mesh::default();
        // Three triangles fanning around a bend: not one plane.
        let pts: [[f32; 3]; 5] = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.3],
            [0.0, 1.0, 0.3],
            [0.5, 0.5, 0.6],
        ];
        mesh.positions.extend_from_slice(&pts);
        mesh.normals = vec![[0.0, 0.0, 1.0]; 5];
        mesh.indices
            .extend_from_slice(&[0, 1, 4, 1, 2, 4, 2, 3, 4, 3, 0, 4]);
        mesh.face_of_triangle = vec![1, 1, 1, 1];

        assert_eq!(face_loop(&mesh, 1, 1.0e-6), Err(FaceError::NotPlanar));
    }

    #[test]
    fn a_face_with_a_hole_is_refused_rather_than_filled_in() {
        // A square ring: an outer square and an inner square, triangulated as a
        // band, so the boundary chains into two loops.
        let mut mesh = Mesh::default();
        let outer = [
            [0.0f32, 0.0, 0.0],
            [4.0, 0.0, 0.0],
            [4.0, 4.0, 0.0],
            [0.0, 4.0, 0.0],
        ];
        let inner = [
            [1.0f32, 1.0, 0.0],
            [3.0, 1.0, 0.0],
            [3.0, 3.0, 0.0],
            [1.0, 3.0, 0.0],
        ];
        mesh.positions.extend_from_slice(&outer);
        mesh.positions.extend_from_slice(&inner);
        mesh.normals = vec![[0.0, 0.0, 1.0]; 8];
        for i in 0..4u32 {
            let j = (i + 1) % 4;
            mesh.indices.extend_from_slice(&[i, j, 4 + j]);
            mesh.indices.extend_from_slice(&[i, 4 + j, 4 + i]);
            mesh.face_of_triangle.push(3);
            mesh.face_of_triangle.push(3);
        }
        assert_eq!(face_loop(&mesh, 3, 1.0e-6), Err(FaceError::NotOneLoop));
    }

    #[test]
    fn a_face_id_no_triangle_carries_is_named_in_the_error() {
        let mesh = box_mesh();
        assert_eq!(face_loop(&mesh, 42, 1.0e-6), Err(FaceError::NoSuchFace(42)));
    }

    #[test]
    fn an_offset_square_is_a_smaller_square() {
        let square = [(-5.0, -5.0), (5.0, -5.0), (5.0, 5.0), (-5.0, 5.0)];
        let inset = offset_polygon(&square, 0.5);
        assert_eq!(
            inset,
            vec![(-4.5, -4.5), (4.5, -4.5), (4.5, 4.5), (-4.5, 4.5)]
        );
        let outset = offset_polygon(&square, -0.5);
        assert_eq!(
            outset,
            vec![(-5.5, -5.5), (5.5, -5.5), (5.5, 5.5), (-5.5, 5.5)]
        );
    }

    #[test]
    fn an_offset_keeps_a_concave_corner_on_its_own_sides() {
        // An L, wound counter-clockwise. The reflex corner must move outward
        // along its bisector, not inward with the rest.
        let l = [
            (0.0, 0.0),
            (4.0, 0.0),
            (4.0, 2.0),
            (2.0, 2.0),
            (2.0, 4.0),
            (0.0, 4.0),
        ];
        let inset = offset_polygon(&l, 0.25);
        assert!((inset[3].0 - 1.75).abs() < 1.0e-12, "{:?}", inset[3]);
        assert!((inset[3].1 - 1.75).abs() < 1.0e-12, "{:?}", inset[3]);
        assert!((inset[0].0 - 0.25).abs() < 1.0e-12, "{:?}", inset[0]);
        assert!((inset[0].1 - 0.25).abs() < 1.0e-12, "{:?}", inset[0]);
    }

    #[test]
    fn the_projection_is_a_right_handed_frame_around_the_pull_direction() {
        let mesh = box_mesh();
        let loop_ = face_loop(&mesh, 0, 1.0e-6).unwrap();
        for along in [loop_.normal, -loop_.normal] {
            let (uv, x_axis, y_axis) = loop_.projected(along);
            assert_eq!(uv.len(), 4);
            let cross = x_axis.cross(y_axis);
            assert!((cross - along).length() < 1.0e-9, "{cross:?} vs {along:?}");
            let signed: f64 = uv
                .iter()
                .enumerate()
                .map(|(i, &(u, v))| {
                    let (u2, v2) = uv[(i + 1) % uv.len()];
                    u * v2 - u2 * v
                })
                .sum();
            assert!(signed > 0.0, "the profile must wind CCW in its own frame");
        }
    }
}
