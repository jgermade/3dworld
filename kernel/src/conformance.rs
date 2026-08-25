//! One suite, run against every backend.
//!
//! A backend is only a backend if it passes this. That is what keeps the
//! kernel decision reversible: swapping OCCT for something of our own is a
//! question with an answer, and the answer is this report.
//!
//! Every assertion here has to be true of *any* correct kernel, which rules
//! out most of the interesting ones. Nothing checks that a boolean is
//! geometrically right — that needs fixtures with golden topology, and it is
//! the next suite, not this one. What is checked is the contract: handles
//! stay valid when the contract says they do, bounds relate to their operands
//! the way set operations require, tessellation is well-formed and lies inside
//! the body it came from, and errors happen where they are promised.

use crate::{Aabb, BooleanOp, GeometryKernel, Mat4, Mesh, Quality, Tolerance, Vec3};

pub struct Check {
    pub name: &'static str,
    pub outcome: core::result::Result<(), String>,
}

pub struct Report {
    pub kernel: &'static str,
    pub checks: Vec<Check>,
}

impl Report {
    pub fn failures(&self) -> impl Iterator<Item = &Check> {
        self.checks.iter().filter(|c| c.outcome.is_err())
    }

    pub fn passed(&self) -> bool {
        self.failures().next().is_none()
    }

    /// Panics with every failure, not just the first. A backend under
    /// development fails several checks for one reason, and seeing them
    /// together is what tells you it is one reason.
    #[track_caller]
    pub fn assert_passed(&self) {
        if self.passed() {
            return;
        }
        let mut msg = format!(
            "{} failed {} of {} conformance checks:\n",
            self.kernel,
            self.failures().count(),
            self.checks.len()
        );
        for c in self.failures() {
            let reason = c.outcome.as_ref().err().map_or("", |e| e.as_str());
            msg.push_str(&format!("  - {}: {reason}\n", c.name));
        }
        panic!("{msg}");
    }
}

macro_rules! check {
    ($checks:expr, $name:literal, $body:block) => {
        let outcome: core::result::Result<(), String> = (|| $body)();
        $checks.push(Check {
            name: $name,
            outcome,
        });
    };
}

fn require(cond: bool, msg: impl Into<String>) -> core::result::Result<(), String> {
    if cond { Ok(()) } else { Err(msg.into()) }
}

/// Well-formedness of a mesh, independent of what it is a mesh *of*.
fn check_mesh(mesh: &Mesh, bounds: &Aabb, slack: f64) -> core::result::Result<(), String> {
    require(mesh.triangle_count() > 0, "no triangles")?;
    require(
        mesh.indices.len().is_multiple_of(3),
        format!("index count {} is not a multiple of 3", mesh.indices.len()),
    )?;
    require(
        mesh.normals.len() == mesh.positions.len(),
        format!(
            "{} normals for {} positions",
            mesh.normals.len(),
            mesh.positions.len()
        ),
    )?;
    require(
        mesh.face_of_triangle.len() == mesh.triangle_count(),
        format!(
            "{} face ids for {} triangles",
            mesh.face_of_triangle.len(),
            mesh.triangle_count()
        ),
    )?;
    if let Some(&bad) = mesh
        .indices
        .iter()
        .find(|&&i| i as usize >= mesh.positions.len())
    {
        return Err(format!(
            "index {bad} out of range for {} positions",
            mesh.positions.len()
        ));
    }
    for (i, p) in mesh.positions.iter().enumerate() {
        let v = Vec3::new(p[0] as f64, p[1] as f64, p[2] as f64);
        if !v.x.is_finite() || !v.y.is_finite() || !v.z.is_finite() {
            return Err(format!("vertex {i} is not finite: {p:?}"));
        }
        if !bounds.contains(v, slack) {
            return Err(format!(
                "vertex {i} at {p:?} lies outside the body's bounds"
            ));
        }
    }
    Ok(())
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

fn boxes_close(a: &Aabb, b: &Aabb, tol: f64) -> bool {
    close(a.min.x, b.min.x, tol)
        && close(a.min.y, b.min.y, tol)
        && close(a.min.z, b.min.z, tol)
        && close(a.max.x, b.max.x, tol)
        && close(a.max.y, b.max.y, tol)
        && close(a.max.z, b.max.z, tol)
}

/// Run the suite. `tol` is the document tolerance the backend should be held
/// to; the geometric slack allowed on tessellation is derived from `quality`,
/// because a coarse mesh is *permitted* to miss the surface by its sag.
pub fn run<K: GeometryKernel>(k: &mut K, tol: Tolerance, quality: Quality) -> Report {
    let mut checks = Vec::new();
    let slack = quality.sag.max(tol.linear) * 2.0;

    check!(checks, "box bounds are exact and origin-centred", {
        let size = Vec3::new(2.0, 4.0, 6.0);
        let b = k.create_box(size).map_err(|e| e.to_string())?;
        let got = k.bounds(b).map_err(|e| e.to_string())?;
        require(
            boxes_close(&got, &Aabb::centered(size), tol.linear),
            format!("expected {:?}, got {got:?}", Aabb::centered(size)),
        )
    });

    check!(checks, "sphere bounds are symmetric about the origin", {
        let r = 3.0;
        let b = k.create_sphere(r).map_err(|e| e.to_string())?;
        let got = k.bounds(b).map_err(|e| e.to_string())?;
        let c = got.center();
        require(
            close(c.x, 0.0, slack) && close(c.y, 0.0, slack) && close(c.z, 0.0, slack),
            format!("centre {c:?} is not the origin"),
        )?;
        let s = got.size();
        require(
            close(s.x, 2.0 * r, slack) && close(s.y, 2.0 * r, slack) && close(s.z, 2.0 * r, slack),
            format!("size {s:?} does not match a radius of {r}"),
        )
    });

    check!(checks, "degenerate primitives are refused, not built", {
        require(
            k.create_sphere(0.0).is_err(),
            "a sphere of radius 0 was accepted",
        )?;
        require(
            k.create_box(Vec3::new(1.0, -1.0, 1.0)).is_err(),
            "a box with a negative extent was accepted",
        )?;
        require(
            k.create_cylinder(1.0, 0.0).is_err(),
            "a cylinder of height 0 was accepted",
        )
    });

    check!(checks, "union bounds contain both operands", {
        let a = k.create_box(Vec3::splat(2.0)).map_err(|e| e.to_string())?;
        let b = k.create_sphere(1.5).map_err(|e| e.to_string())?;
        let (ba, bb) = (
            k.bounds(a).map_err(|e| e.to_string())?,
            k.bounds(b).map_err(|e| e.to_string())?,
        );
        let u = k
            .boolean(BooleanOp::Union, a, b, tol)
            .map_err(|e| e.to_string())?;
        let bu = k.bounds(u).map_err(|e| e.to_string())?;
        require(
            bu.contains_box(&ba, slack) && bu.contains_box(&bb, slack),
            format!("union bounds {bu:?} do not contain {ba:?} and {bb:?}"),
        )
    });

    check!(checks, "difference bounds stay within the minuend", {
        let a = k.create_box(Vec3::splat(4.0)).map_err(|e| e.to_string())?;
        let b = k.create_sphere(1.0).map_err(|e| e.to_string())?;
        let ba = k.bounds(a).map_err(|e| e.to_string())?;
        let d = k
            .boolean(BooleanOp::Difference, a, b, tol)
            .map_err(|e| e.to_string())?;
        let bd = k.bounds(d).map_err(|e| e.to_string())?;
        require(
            ba.contains_box(&bd, slack),
            format!("difference bounds {bd:?} escape the minuend's {ba:?}"),
        )
    });

    check!(checks, "intersection bounds stay within both operands", {
        let a = k.create_box(Vec3::splat(4.0)).map_err(|e| e.to_string())?;
        let b = k.create_sphere(1.0).map_err(|e| e.to_string())?;
        let (ba, bb) = (
            k.bounds(a).map_err(|e| e.to_string())?,
            k.bounds(b).map_err(|e| e.to_string())?,
        );
        let i = k
            .boolean(BooleanOp::Intersection, a, b, tol)
            .map_err(|e| e.to_string())?;
        let bi = k.bounds(i).map_err(|e| e.to_string())?;
        require(
            ba.contains_box(&bi, slack) && bb.contains_box(&bi, slack),
            format!("intersection bounds {bi:?} escape {ba:?} or {bb:?}"),
        )
    });

    check!(checks, "a boolean leaves its operands valid", {
        // The whole of undo rests on this one. If a backend consumes its
        // operands, history cannot hold a previous state at all.
        let a = k.create_box(Vec3::splat(2.0)).map_err(|e| e.to_string())?;
        let b = k.create_sphere(1.0).map_err(|e| e.to_string())?;
        let before = k.bounds(a).map_err(|e| e.to_string())?;
        let _ = k
            .boolean(BooleanOp::Union, a, b, tol)
            .map_err(|e| e.to_string())?;
        let after = k
            .bounds(a)
            .map_err(|e| format!("operand invalidated by the boolean: {e}"))?;
        require(
            boxes_close(&before, &after, tol.linear),
            format!("operand mutated by the boolean: {before:?} became {after:?}"),
        )?;
        k.bounds(b)
            .map(|_| ())
            .map_err(|e| format!("second operand invalidated by the boolean: {e}"))
    });

    check!(checks, "a translation moves the bounds by exactly that", {
        let a = k.create_box(Vec3::splat(2.0)).map_err(|e| e.to_string())?;
        let before = k.bounds(a).map_err(|e| e.to_string())?;
        let t = Vec3::new(10.0, -5.0, 0.25);
        let moved = k
            .transform(a, &Mat4::from_translation(t))
            .map_err(|e| e.to_string())?;
        let after = k.bounds(moved).map_err(|e| e.to_string())?;
        let expected = Aabb::new(before.min + t, before.max + t);
        require(
            boxes_close(&after, &expected, tol.linear),
            format!("expected {expected:?}, got {after:?}"),
        )
    });

    check!(checks, "a copy is independent of its original", {
        let a = k.create_box(Vec3::splat(2.0)).map_err(|e| e.to_string())?;
        let c = k.copy(a).map_err(|e| e.to_string())?;
        require(c != a, "copy returned the same handle")?;
        k.delete(a).map_err(|e| e.to_string())?;
        k.bounds(c)
            .map(|_| ())
            .map_err(|e| format!("copy died with its original: {e}"))
    });

    check!(checks, "a deleted body is gone, and says so", {
        let a = k.create_box(Vec3::splat(1.0)).map_err(|e| e.to_string())?;
        k.delete(a).map_err(|e| e.to_string())?;
        require(k.bounds(a).is_err(), "bounds answered for a deleted body")?;
        require(
            k.tessellate(a, quality).is_err(),
            "tessellate answered for a deleted body",
        )?;
        require(
            k.delete(a).is_err(),
            "deleting twice was accepted, so handles are being reused unsafely",
        )
    });

    check!(checks, "topology is reported and is non-degenerate", {
        let a = k.create_box(Vec3::splat(2.0)).map_err(|e| e.to_string())?;
        let t = k.topology(a).map_err(|e| e.to_string())?;
        require(t.solids >= 1, format!("{} solids", t.solids))?;
        require(t.faces >= 4, format!("only {} faces", t.faces))?;
        require(t.vertices >= 4, format!("only {} vertices", t.vertices))
    });

    check!(checks, "tessellation is well-formed and inside its body", {
        for (what, body) in [
            ("box", k.create_box(Vec3::new(2.0, 3.0, 4.0))),
            ("sphere", k.create_sphere(1.5)),
            ("cylinder", k.create_cylinder(1.0, 3.0)),
        ] {
            let body = body.map_err(|e| e.to_string())?;
            let bounds = k.bounds(body).map_err(|e| e.to_string())?;
            let mesh = k.tessellate(body, quality).map_err(|e| e.to_string())?;
            check_mesh(&mesh, &bounds, slack).map_err(|e| format!("{what}: {e}"))?;
        }
        Ok(())
    });

    check!(checks, "tessellation is deterministic", {
        // Not a nicety: it is what makes a fixture suite mean anything, and it
        // is the property relaxed SIMD below the seam would quietly destroy.
        let a = k.create_cylinder(1.0, 2.0).map_err(|e| e.to_string())?;
        let first = k.tessellate(a, quality).map_err(|e| e.to_string())?;
        let second = k.tessellate(a, quality).map_err(|e| e.to_string())?;
        require(
            first == second,
            "the same body tessellated twice at the same quality gave two meshes",
        )
    });

    check!(checks, "quality changes the mesh, not the model", {
        let a = k.create_sphere(1.0).map_err(|e| e.to_string())?;
        let bounds = k.bounds(a).map_err(|e| e.to_string())?;
        let coarse = k
            .tessellate(a, Quality::new(0.5, 1.0))
            .map_err(|e| e.to_string())?;
        let fine = k
            .tessellate(a, Quality::new(0.001, 0.05))
            .map_err(|e| e.to_string())?;
        require(
            fine.triangle_count() >= coarse.triangle_count(),
            format!(
                "finer quality gave fewer triangles: {} against {}",
                fine.triangle_count(),
                coarse.triangle_count()
            ),
        )?;
        let after = k.bounds(a).map_err(|e| e.to_string())?;
        require(
            boxes_close(&bounds, &after, tol.linear),
            "tessellating changed the body",
        )
    });

    Report {
        kernel: k.name(),
        checks,
    }
}
