//! One suite, run against every backend.
//!
//! A backend is only a backend if it passes this. That is what keeps the
//! kernel decision reversible: swapping OCCT for something of our own is a
//! question with an answer, and the answer is this report.
//!
//! Every assertion here has to be true of *any* correct kernel, which rules
//! out most of the interesting ones. What is checked is the contract: handles
//! stay valid when the contract says they do, bounds relate to their operands
//! the way set operations require, tessellation is well-formed and lies inside
//! the body it came from, and errors happen where they are promised.
//!
//! # The two halves, and why there are two
//!
//! Until 2026-09-05 the whole suite was the paragraph above, and a backend
//! whose `boolean` returned *the bounding box of its operands* passed it. That
//! is not a hypothetical: `TruckKernel` did exactly that, in the browser, for
//! nine days. Every assertion the suite made about a boolean was about its
//! bounds, and the bounds of a bounding box are correct.
//!
//! So there is now a second half, run only against a backend whose
//! [`GeometryKernel::does_geometry`] says `true`, and it asks the question
//! bounds cannot: **how much material is there?** The volume a tessellation
//! encloses is cheap to compute, is true of any correct kernel to within its
//! sag, and is not something bookkeeping can fake — a bounding box has a
//! volume and it is the wrong one. `FakeKernel` is excused from this half by
//! saying what it is, not by being special-cased here.
//!
//! # A backend may decline; it may not lie
//!
//! The other half of the same decision. `truck`'s boolean is real and narrow:
//! it will subtract one box from another exactly, drill a plate exactly, and
//! it cannot touch a sphere — where it panics rather than answering, so the
//! backend refuses the call before it gets there. The suite therefore accepts
//! `Err` from a boolean on the harder fixtures, and accepts nothing else: an
//! answer, once given, is held to the set-theoretic relation it claims.
//!
//! There is a floor under that, or "decline everything" would pass. Three
//! fixtures are **mandatory** — the union, difference and intersection of two
//! overlapping axis-aligned boxes, whose volumes are arithmetic — because a
//! kernel that cannot subtract one box from another is not a kernel.

use crate::{
    Aabb, Body, BooleanOp, GeometryKernel, KernelError, Mat4, Mesh, Profile, Quality, Tolerance,
    Vec3,
};

pub struct Check {
    pub name: &'static str,
    pub outcome: core::result::Result<(), String>,
}

pub struct Report {
    pub kernel: &'static str,
    pub checks: Vec<Check>,
    /// Whether the geometry half ran, which is the backend's own answer to
    /// [`GeometryKernel::does_geometry`]. A report that says `false` is a
    /// report about bookkeeping: it is evidence the contract is satisfied and
    /// no evidence at all that anything was modelled.
    pub geometry: bool,
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
        let half = if self.geometry {
            "contract and geometry"
        } else {
            "contract only"
        };
        let mut msg = format!(
            "{} failed {} of {} conformance checks ({half}):\n",
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
    require(
        mesh.line_indices.len().is_multiple_of(2),
        format!(
            "line index count {} is not a multiple of 2",
            mesh.line_indices.len()
        ),
    )?;
    if let Some(&bad) = mesh
        .line_indices
        .iter()
        .find(|&&i| i as usize >= mesh.line_positions.len())
    {
        return Err(format!(
            "line index {bad} out of range for {} line positions",
            mesh.line_positions.len()
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
        let u = match k.boolean(BooleanOp::Union, a, b, tol) {
            Ok(u) => u,
            // Declining is conforming — see the note at the top of this file.
            // A backend that declines *everything* is caught by the mandatory
            // box fixtures in the geometry half, not here.
            Err(KernelError::Unsupported(_)) => return Ok(()),
            Err(e) => return Err(format!("union failed: {e}")),
        };
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
        let d = match k.boolean(BooleanOp::Difference, a, b, tol) {
            Ok(d) => d,
            Err(KernelError::Unsupported(_)) => return Ok(()),
            Err(e) => return Err(format!("difference failed: {e}")),
        };
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
        let i = match k.boolean(BooleanOp::Intersection, a, b, tol) {
            Ok(i) => i,
            Err(KernelError::Unsupported(_)) => return Ok(()),
            Err(e) => return Err(format!("intersection failed: {e}")),
        };
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
        // Declined or done, the operands must survive: a backend that
        // consumed them and *then* failed has taken the document with it, and
        // that is the case a suite is least likely to think of.
        match k.boolean(BooleanOp::Union, a, b, tol) {
            Ok(_) | Err(KernelError::Unsupported(_)) => {}
            Err(e) => return Err(format!("union failed: {e}")),
        }
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

    check!(
        checks,
        "fillet creates a new body and leaves original untouched",
        {
            let a = k.create_box(Vec3::splat(4.0)).map_err(|e| e.to_string())?;
            let before = k.bounds(a).map_err(|e| e.to_string())?;
            let filleted = k.fillet(a, 0.5).map_err(|e| e.to_string())?;
            require(
                filleted != a,
                "fillet returned the same body handle".to_string(),
            )?;
            let after = k.bounds(a).map_err(|e| e.to_string())?;
            require(
                boxes_close(&before, &after, tol.linear),
                format!("original body mutated by fillet: {before:?} became {after:?}"),
            )
        }
    );

    check!(
        checks,
        "fillet with non-positive radius returns Degenerate",
        {
            let a = k.create_box(Vec3::splat(2.0)).map_err(|e| e.to_string())?;
            match k.fillet(a, 0.0) {
                Err(KernelError::Degenerate(_)) => Ok(()),
                other => Err(format!("fillet with zero radius returned {other:?}")),
            }
        }
    );

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

    check!(
        checks,
        "a face id names one connected region of the surface",
        {
            // Contract, not geometry: `face_of_triangle` is what a selection, an
            // ID-buffer pick and a per-face fillet are all stored against, so an id
            // that spans two separate patches means a click on one of them lands on
            // both. True of any backend that answers the question at all, including
            // one whose faces are the sides of a bounding box.
            for (what, body) in [
                ("box", k.create_box(Vec3::new(2.0, 3.0, 4.0))),
                ("cylinder", k.create_cylinder(1.0, 3.0)),
            ] {
                let body = body.map_err(|e| e.to_string())?;
                let mesh = k.tessellate(body, quality).map_err(|e| e.to_string())?;
                let bounds = k.bounds(body).map_err(|e| e.to_string())?;
                let extent = bounds
                    .size()
                    .axis(0)
                    .max(bounds.size().axis(1))
                    .max(bounds.size().axis(2));
                for f in census(&mesh, extent * 1.0e-5) {
                    require(
                        f.regions == 1,
                        format!(
                            "on the {what}, face {} covers {} disconnected regions \
                         of the surface",
                            f.id, f.regions
                        ),
                    )?;
                }
            }
            Ok(())
        }
    );

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

    check!(checks, "a saved body loads back the same", {
        let plate = k
            .create_box(Vec3::new(4.0, 4.0, 4.0))
            .map_err(|e| e.to_string())?;
        let drill = k.create_cylinder(1.0, 8.0).map_err(|e| e.to_string())?;
        // Something with a curved face and a seam, so a backend that only
        // round-trips planes does not pass.
        let a = k
            .boolean(BooleanOp::Difference, plate, drill, tol)
            .map_err(|e| e.to_string())?;
        let before = k.topology(a).map_err(|e| e.to_string())?;
        let bounds = k.bounds(a).map_err(|e| e.to_string())?;

        let bytes = k.save_body(a).map_err(|e| e.to_string())?;
        require(!bytes.is_empty(), "save_body produced nothing")?;

        let b = k.load_body(&bytes).map_err(|e| e.to_string())?;
        require(b != a, "load_body returned the body it was given")?;
        require(
            k.topology(b).map_err(|e| e.to_string())? == before,
            "the topology changed across a save and a load",
        )?;
        require(
            boxes_close(
                &k.bounds(b).map_err(|e| e.to_string())?,
                &bounds,
                tol.linear,
            ),
            "the bounds changed across a save and a load",
        )?;
        // The original must survive, like every other operation's operands.
        require(
            k.topology(a).map_err(|e| e.to_string())? == before,
            "saving consumed the body",
        )
    });

    check!(
        checks,
        "filleting and chamfering work on valid primitives and refuse zero or excessive distance",
        {
            let b = k
                .create_box(Vec3::new(10.0, 10.0, 10.0))
                .map_err(|e| e.to_string())?;
            require(k.fillet(b, 0.0).is_err(), "zero radius fillet accepted")?;
            require(k.chamfer(b, 0.0).is_err(), "zero distance chamfer accepted")?;
            require(
                k.fillet(b, 100.0).is_err(),
                "excessive radius fillet accepted",
            )?;
            require(
                k.chamfer(b, 100.0).is_err(),
                "excessive distance chamfer accepted",
            )?;

            let f = k.fillet(b, 1.0).map_err(|e| e.to_string())?;
            require(f != b, "fillet returned same handle")?;
            let c = k.chamfer(b, 1.0).map_err(|e| e.to_string())?;
            require(c != b, "chamfer returned same handle")
        }
    );

    check!(
        checks,
        "extruding 2D profiles produces valid solids and refuses zero or negative distances",
        {
            let rect = Profile::Rectangle {
                width: 10.0,
                height: 10.0,
            };
            require(
                k.extrude(&rect, 0.0).is_err(),
                "zero distance extrusion accepted",
            )?;
            require(
                k.extrude(&rect, -5.0).is_err(),
                "negative distance extrusion accepted",
            )?;

            let b = k.extrude(&rect, 20.0).map_err(|e| e.to_string())?;
            let bounds = k.bounds(b).map_err(|e| e.to_string())?;
            require(
                boxes_close(
                    &bounds,
                    &Aabb::centered(Vec3::new(10.0, 10.0, 20.0)),
                    tol.linear,
                ),
                format!("extrusion bounds mismatch: got {bounds:?}"),
            )
        }
    );

    check!(
        checks,
        "bytes from another kernel are refused, not guessed at",
        {
            // A file written by a different backend must fail loudly. The
            // alternative is a document that opens and is quietly wrong, which is
            // the worst outcome a file format has.
            let refused = k.load_body(b"this is not any kernel's geometry");
            require(refused.is_err(), "nonsense loaded as a body")?;
            match refused {
                Err(KernelError::Unsupported(_) | KernelError::Failed(_)) => Ok(()),
                Err(e) => Err(format!(
                    "refused, but as `{e}` — a caller cannot tell a foreign file \
                     from a damaged one"
                )),
                Ok(_) => unreachable!("checked above"),
            }
        }
    );

    check!(
        checks,
        "the geometry format is named and looks like a version",
        {
            let format = k.geometry_format();
            require(!format.is_empty(), "the geometry format has no name")?;
            require(
                format.chars().any(|c| c.is_ascii_digit()),
                format!("`{format}` carries no version, so it cannot be changed safely"),
            )
        }
    );

    // ---- interchange ---------------------------------------------------
    //
    // A backend may honestly not do STEP, so these four read as conditionals.
    // None of them *skips*: every branch asserts something, because a check
    // that quietly does nothing on one backend is a check that reads as a pass
    // on the backend it was written for — and this repository has already
    // written down once that a skipped test looks like a passed test.

    check!(
        checks,
        "STEP is offered in both directions, or in neither",
        {
            let a = k.create_box(Vec3::splat(2.0)).map_err(|e| e.to_string())?;
            let writes = !matches!(k.export_step(&[a]), Err(KernelError::Unsupported(_)));
            let reads = !matches!(k.import_step(NOT_STEP), Err(KernelError::Unsupported(_)));
            require(
                writes == reads,
                if writes {
                    "this kernel writes STEP and refuses to read it: a door that \
                 only opens outwards"
                } else {
                    "this kernel reads STEP and refuses to write it, so a user's \
                 work goes in and does not come out"
                },
            )
        }
    );

    check!(checks, "a solid survives a STEP export and import", {
        let plate = k
            .create_box(Vec3::new(4.0, 4.0, 4.0))
            .map_err(|e| e.to_string())?;
        let drill = k.create_cylinder(1.0, 8.0).map_err(|e| e.to_string())?;
        // A curved face and a seam, so a backend that only survives planes
        // does not pass — the same shape the save/load check uses.
        let a = k
            .boolean(BooleanOp::Difference, plate, drill, tol)
            .map_err(|e| e.to_string())?;
        let bounds = k.bounds(a).map_err(|e| e.to_string())?;
        let solids = k.topology(a).map_err(|e| e.to_string())?.solids;

        let bytes = match k.export_step(&[a]) {
            Ok(bytes) => bytes,
            // Refusing is conforming. The check above has already held this
            // kernel to refusing in the other direction too, so nothing is
            // being let through here.
            Err(KernelError::Unsupported(_)) => return Ok(()),
            Err(e) => return Err(format!("export failed: {e}")),
        };
        require(
            bytes.starts_with(b"ISO-10303-21"),
            "the bytes do not begin `ISO-10303-21`, which is the first thing \
             every STEP file says about itself",
        )?;

        let imported = k
            .import_step(&bytes)
            .map_err(|e| format!("this kernel could not read back what it just wrote: {e}"))?;
        require(
            imported.len() == 1,
            format!("one solid went out and {} came back", imported.len()),
        )?;
        let b = imported[0].body;

        // Bounds and the solid count, and deliberately **not** the face, edge
        // and vertex counts. STEP is a boundary description, and nothing in it
        // obliges a reader to split the faces the way the writer did: a
        // cylindrical face may arrive as two half-cylinders, a seam may move.
        // Asserting the topology here would be asserting that two
        // implementations of a 700-page standard agree on something the
        // standard does not require, and the first failure would be a correct
        // kernel failing a wrong check. What a caller may rely on is that a
        // solid comes back a solid, in the same place, at the same size.
        require(
            k.topology(b).map_err(|e| e.to_string())?.solids == solids,
            "the solid count changed across a STEP round-trip",
        )?;
        require(
            boxes_close(&k.bounds(b).map_err(|e| e.to_string())?, &bounds, slack),
            format!(
                "the bounds moved across a STEP round-trip: {bounds:?} became {:?}",
                k.bounds(b)
            ),
        )?;
        require(
            k.bounds(a).is_ok(),
            "exporting consumed the body it was given",
        )
    });

    check!(
        checks,
        "bytes that are not STEP are refused by the right name",
        {
            // Two different sentences to a user — "this build cannot read STEP"
            // and "this file is not STEP" — and they send them to two different
            // places. A backend that answers `Unsupported` for a corrupt file
            // sends somebody looking for a different build of the program.
            let a = k.create_box(Vec3::splat(2.0)).map_err(|e| e.to_string())?;
            let does_step = !matches!(k.export_step(&[a]), Err(KernelError::Unsupported(_)));
            match (does_step, k.import_step(NOT_STEP)) {
                (_, Ok(bodies)) => Err(format!(
                    "{} bodies came out of {} bytes of prose",
                    bodies.len(),
                    NOT_STEP.len()
                )),
                (true, Err(KernelError::Failed(_))) => Ok(()),
                (true, Err(e)) => Err(format!(
                    "a kernel that does STEP refused a non-STEP file as `{e}`, \
                     which says the build cannot read STEP at all"
                )),
                (false, Err(KernelError::Unsupported(_))) => Ok(()),
                (false, Err(e)) => Err(format!(
                    "a kernel that does not do STEP refused as `{e}`, which \
                     says the file was at fault"
                )),
            }
        }
    );

    check!(
        checks,
        "exporting nothing, or a body that is gone, is refused before anything is written",
        {
            match k.export_step(&[]) {
                Ok(bytes) => {
                    return Err(format!(
                        "{} bytes of STEP for no bodies at all",
                        bytes.len()
                    ));
                }
                Err(KernelError::Degenerate(_) | KernelError::Unsupported(_)) => {}
                Err(e) => return Err(format!("no bodies was refused, but as `{e}`")),
            }
            let gone = k.create_box(Vec3::splat(1.0)).map_err(|e| e.to_string())?;
            let alive = k.create_box(Vec3::splat(1.0)).map_err(|e| e.to_string())?;
            k.delete(gone).map_err(|e| e.to_string())?;
            // The live body first, so that a backend which writes as it walks
            // has already written something by the time it meets the stale
            // handle. A file with half a document in it is worse than no file.
            match k.export_step(&[alive, gone]) {
                Ok(bytes) => Err(format!(
                    "{} bytes of STEP written for a document containing a \
                     deleted body",
                    bytes.len()
                )),
                Err(KernelError::UnknownBody(_) | KernelError::Unsupported(_)) => Ok(()),
                Err(e) => Err(format!("a deleted body was refused, but as `{e}`")),
            }
        }
    );

    check!(
        checks,
        "revolve produces a non-empty body with valid bounds",
        {
            let prof = Profile::Rectangle {
                width: 10.0,
                height: 20.0,
            };
            let body = k
                .revolve(&prof, Vec3::ZERO, Vec3::Y, std::f64::consts::PI * 2.0)
                .map_err(|e| e.to_string())?;
            let bounds = k.bounds(body).map_err(|e| e.to_string())?;
            require(!bounds.is_empty(), "revolved body bounds are empty")?;
            let mesh = k
                .tessellate(body, Quality::display_default())
                .map_err(|e| e.to_string())?;
            check_mesh(&mesh, &bounds, 1e-4)
        }
    );

    check!(
        checks,
        "sweep produces a non-empty body with valid bounds",
        {
            let prof = Profile::Circle { radius: 5.0 };
            let pts = [Vec3::ZERO, Vec3::new(0.0, 0.0, 10.0)];
            let body = k.sweep(&prof, &pts).map_err(|e| e.to_string())?;
            let bounds = k.bounds(body).map_err(|e| e.to_string())?;
            require(!bounds.is_empty(), "swept body bounds are empty")?;
            let mesh = k
                .tessellate(body, Quality::display_default())
                .map_err(|e| e.to_string())?;
            check_mesh(&mesh, &bounds, 1e-4)
        }
    );

    check!(
        checks,
        "loft produces a non-empty body with valid bounds",
        {
            let profiles = [
                Profile::Circle { radius: 10.0 },
                Profile::Circle { radius: 5.0 },
            ];
            let planes = [
                crate::SketchPlane::default(),
                crate::SketchPlane {
                    origin: Vec3::new(0.0, 0.0, 20.0),
                    x_axis: Vec3::X,
                    y_axis: Vec3::Y,
                },
            ];
            let body = k.loft(&profiles, &planes).map_err(|e| e.to_string())?;
            let bounds = k.bounds(body).map_err(|e| e.to_string())?;
            require(!bounds.is_empty(), "lofted body bounds are empty")?;
            let mesh = k
                .tessellate(body, Quality::display_default())
                .map_err(|e| e.to_string())?;
            check_mesh(&mesh, &bounds, 1e-4)
        }
    );

    if k.does_geometry() {
        geometry_checks(k, tol, quality, &mut checks);
    }

    Report {
        kernel: k.name(),
        checks,
        geometry: k.does_geometry(),
    }
}

/// The volume these triangles enclose, by the divergence theorem.
///
/// Positive when the mesh winds counter-clockwise seen from the outside, which
/// is the convention [`Mesh`] states — so the sign of this number is the
/// orientation check, and its magnitude is the geometry one. It is the
/// cheapest question that tells modelling from bookkeeping: a bounding box has
/// a volume too, and it is not the same volume.
///
/// It assumes a closed mesh, which every check that calls it has already
/// asserted the well-formedness of.
fn enclosed_volume(mesh: &Mesh) -> f64 {
    let point = |i: u32| {
        let p = mesh.positions[i as usize];
        Vec3::new(p[0] as f64, p[1] as f64, p[2] as f64)
    };
    let mut total = 0.0;
    for t in mesh.indices.as_chunks::<3>().0 {
        total += point(t[0]).dot(point(t[1]).cross(point(t[2])));
    }
    total / 6.0
}

/// Two overlapping axis-aligned boxes, whose union, difference and
/// intersection are arithmetic rather than opinion.
///
/// A 4-cube at the origin and a 2-cube whose own corner sits at the first's,
/// so they share exactly a 1×1×1 corner: 64, 8, and 1. Every number the
/// mandatory checks assert comes from those three.
struct Corners {
    big: Body,
    small: Body,
}

const BIG: f64 = 64.0;
const SMALL: f64 = 8.0;
const OVERLAP: f64 = 1.0;

fn corners<K: GeometryKernel>(k: &mut K) -> core::result::Result<Corners, String> {
    let big = k.create_box(Vec3::splat(4.0)).map_err(|e| e.to_string())?;
    let small = k.create_box(Vec3::splat(2.0)).map_err(|e| e.to_string())?;
    // Centred bodies, so moving the small one by half of each extent puts its
    // corner on the big one's: the overlap is the unit cube at (1.5, 1.5, 1.5).
    let small = k
        .transform(small, &Mat4::from_translation(Vec3::splat(2.0)))
        .map_err(|e| e.to_string())?;
    Ok(Corners { big, small })
}

/// What a body's mesh says its volume is, with the mesh checked first.
fn volume_of<K: GeometryKernel>(
    k: &K,
    body: Body,
    quality: Quality,
) -> core::result::Result<f64, String> {
    let bounds = k.bounds(body).map_err(|e| e.to_string())?;
    let mesh = k
        .tessellate(body, quality)
        .map_err(|e| format!("tessellate: {e}"))?;
    check_mesh(&mesh, &bounds, quality.sag.max(1e-4) * 4.0)?;
    Ok(enclosed_volume(&mesh))
}

fn within(got: f64, expected: f64, fraction: f64) -> bool {
    (got - expected).abs() <= expected.abs() * fraction
}

/// The geometry half on its own, whatever the backend claims.
///
/// [`run`] decides from [`GeometryKernel::does_geometry`] whether to include
/// these, which is the right thing for a backend and the wrong thing for
/// testing the split itself. This is how the negative control in
/// `kernel-fake/tests/conformance.rs` works: it holds `FakeKernel` — the one
/// backend that says `false` — to the half it is excused from, and requires it
/// to *fail*. A tier nothing fails is a tier that excuses everybody.
pub fn geometry<K: GeometryKernel>(k: &mut K, tol: Tolerance, quality: Quality) -> Report {
    let mut checks = Vec::new();
    geometry_checks(k, tol, quality, &mut checks);
    Report {
        kernel: k.name(),
        checks,
        geometry: true,
    }
}

/// The half that only runs against a backend that says it does geometry.
///
/// Everything here is still true of *any* correct kernel — these are volumes a
/// child could compute — but none of it is true of a backend that answers with
/// bounding boxes, and that is the point. See the note at the top of the file.
fn geometry_checks<K: GeometryKernel>(
    k: &mut K,
    tol: Tolerance,
    quality: Quality,
    checks: &mut Vec<Check>,
) {
    check!(checks, "a box's mesh encloses the box's volume", {
        let size = Vec3::new(2.0, 4.0, 6.0);
        let b = k.create_box(size).map_err(|e| e.to_string())?;
        let got = volume_of(k, b, quality)?;
        // A box is planar, so its mesh is not an approximation of anything and
        // the only slack is `f32`'s. Anything looser here would let a mesh of
        // the *bounding* box through, which for a box is the same mesh — which
        // is why the checks below use shapes where it is not.
        require(
            within(got, 48.0, 1.0e-3),
            format!("a 2x4x6 box tessellated to a volume of {got}, not 48"),
        )
    });

    check!(checks, "a curved body is oriented outwards", {
        // The sign is the assertion. A mesh wound the other way encloses minus
        // its own volume, which is what a sphere and a cylinder did here for
        // nine days while every other check in this file passed.
        //
        // The magnitude is asserted loosely and one-sidedly: a tessellation
        // whose vertices lie on the surface is inscribed, so it may fall short
        // of the true volume by its sag and must not exceed it by more than
        // rounding.
        for (name, body, exact) in [
            (
                "sphere",
                k.create_sphere(1.0).map_err(|e| e.to_string())?,
                4.0 * core::f64::consts::PI / 3.0,
            ),
            (
                "cylinder",
                k.create_cylinder(1.0, 2.0).map_err(|e| e.to_string())?,
                2.0 * core::f64::consts::PI,
            ),
        ] {
            let got = volume_of(k, body, quality)?;
            require(
                got > 0.0,
                format!(
                    "the {name}'s mesh encloses {got}, a negative volume: it is \
                     wound inside out, and every normal on it points at the \
                     body's centre"
                ),
            )?;
            require(
                got <= exact * 1.01 && got >= exact * 0.7,
                format!("the {name}'s mesh encloses {got}, and it should be near {exact}"),
            )?;
        }
        Ok(())
    });

    // The three below are the floor: no `Unsupported` is accepted, because a
    // kernel that cannot subtract one box from another is not a kernel, and
    // without them "decline everything" would pass this file.
    check!(checks, "a union of two boxes is both, counted once", {
        let c = corners(k)?;
        let u = k
            .boolean(BooleanOp::Union, c.big, c.small, tol)
            .map_err(|e| format!("a union of two boxes is mandatory, and this one {e}"))?;
        let got = volume_of(k, u, quality)?;
        require(
            within(got, BIG + SMALL - OVERLAP, 1.0e-3),
            format!(
                "the union encloses {got}, not {}. A result of {} would be the \
                 bounding box of the operands rather than their union",
                BIG + SMALL - OVERLAP,
                5.0 * 5.0 * 5.0
            ),
        )
    });

    check!(
        checks,
        "a difference removes exactly what the tool covers",
        {
            let c = corners(k)?;
            let d = k
                .boolean(BooleanOp::Difference, c.big, c.small, tol)
                .map_err(|e| format!("a difference of two boxes is mandatory, and this one {e}"))?;
            let got = volume_of(k, d, quality)?;
            require(
                within(got, BIG - OVERLAP, 1.0e-3),
                format!(
                    "the difference encloses {got}, not {}. A result of {BIG} would \
                 be a copy of the minuend, which is a boolean that ignored its \
                 second operand",
                    BIG - OVERLAP
                ),
            )
        }
    );

    check!(checks, "an intersection is only what both contain", {
        let c = corners(k)?;
        let i = k
            .boolean(BooleanOp::Intersection, c.big, c.small, tol)
            .map_err(|e| format!("an intersection of two boxes is mandatory, and this one {e}"))?;
        let got = volume_of(k, i, quality)?;
        require(
            within(got, OVERLAP, 1.0e-3),
            format!("the intersection encloses {got}, not {OVERLAP}"),
        )
    });

    check!(
        checks,
        "a box's boundary is six planes, each holding its own area at its own centre",
        {
            // The fixture a census can be read on. A box has no curved surface
            // at all, so `curved` is 0 and asserted rather than skipped: a
            // backend whose face ids do not partition the boundary leaves area
            // over here, and there is nowhere for it to go.
            let b = k
                .create_box(Vec3::new(2.0, 4.0, 6.0))
                .map_err(|e| e.to_string())?;
            let mesh = k
                .tessellate(b, quality)
                .map_err(|e| format!("tessellate: {e}"))?;
            let planes = [
                (Vec3::X, 1.0, 24.0),
                (Vec3::new(-1.0, 0.0, 0.0), 1.0, 24.0),
                (Vec3::Y, 2.0, 12.0),
                (Vec3::new(0.0, -1.0, 0.0), 2.0, 12.0),
                (Vec3::Z, 3.0, 8.0),
                (Vec3::new(0.0, 0.0, -1.0), 3.0, 8.0),
            ]
            .map(|(normal, offset, area)| GoldenPlane {
                normal,
                offset,
                area,
                // A box's face is centred on its own plane, so the centroid is
                // the normal scaled out to the plane.
                centroid: normal * offset,
            });
            check_census(&mesh, &planes, 0.0, &slack_for(6.0))
        }
    );

    check!(
        checks,
        "a drilled plate has the hole where it was asked for",
        {
            // The 40x40x10 plate and the 6 mm drill that `make freecad-check`
            // weighs against 16000 - pi*6^2*10, and that `w3d-kernel-truck`'s
            // tessellation fingerprints are recorded on. The volume is already
            // checked in three other places; what is new here is *where*.
            //
            // The hole is off-centre on purpose. Centred, the plate's top face
            // has its centre of area at the origin whether the hole is there
            // or not, and the assertion would be about nothing. At x = 8 the
            // material left in that plane is lopsided, and its centre of area
            // moves to a number arithmetic can state — which no volume, no
            // bounding box and no face count can.
            const HALF: f64 = 20.0;
            const THICK: f64 = 10.0;
            const R: f64 = 6.0;
            const AT: f64 = 8.0;

            let plate = k
                .create_box(Vec3::new(2.0 * HALF, 2.0 * HALF, THICK))
                .map_err(|e| e.to_string())?;
            let drill = k
                .create_cylinder(R, 2.0 * THICK)
                .map_err(|e| e.to_string())?;
            let drill = k
                .transform(drill, &Mat4::from_translation(Vec3::new(AT, 0.0, 0.0)))
                .map_err(|e| e.to_string())?;

            // Declinable, like every fixture here that is not one of the three
            // mandatory boxes: a backend that will not drill says so and is
            // held to nothing further. It may not answer wrongly.
            let Ok(drilled) = k.boolean(BooleanOp::Difference, plate, drill, tol) else {
                return Ok(());
            };
            let mesh = k
                .tessellate(drilled, quality)
                .map_err(|e| format!("tessellate: {e}"))?;

            let face = 2.0 * HALF * 2.0 * HALF;
            let hole = core::f64::consts::PI * R * R;
            // The centre of area of a square with a disc taken out of it: the
            // square's first moment is zero, the disc's is its area times where
            // it sits, and what is left is the difference over the difference.
            let shift = -AT * hole / (face - hole);
            let side = 2.0 * HALF * THICK;

            let planes = [
                GoldenPlane {
                    normal: Vec3::Z,
                    offset: THICK / 2.0,
                    area: face - hole,
                    centroid: Vec3::new(shift, 0.0, THICK / 2.0),
                },
                GoldenPlane {
                    normal: Vec3::new(0.0, 0.0, -1.0),
                    offset: THICK / 2.0,
                    area: face - hole,
                    centroid: Vec3::new(shift, 0.0, -THICK / 2.0),
                },
                GoldenPlane {
                    normal: Vec3::X,
                    offset: HALF,
                    area: side,
                    centroid: Vec3::new(HALF, 0.0, 0.0),
                },
                GoldenPlane {
                    normal: Vec3::new(-1.0, 0.0, 0.0),
                    offset: HALF,
                    area: side,
                    centroid: Vec3::new(-HALF, 0.0, 0.0),
                },
                GoldenPlane {
                    normal: Vec3::Y,
                    offset: HALF,
                    area: side,
                    centroid: Vec3::new(0.0, HALF, 0.0),
                },
                GoldenPlane {
                    normal: Vec3::new(0.0, -1.0, 0.0),
                    offset: HALF,
                    area: side,
                    centroid: Vec3::new(0.0, -HALF, 0.0),
                },
            ];
            // Everything not in one of those six planes is the wall of the
            // hole, and a cylinder's wall is its circumference times the
            // plate's thickness.
            let wall = 2.0 * core::f64::consts::PI * R * THICK;
            check_census(&mesh, &planes, wall, &slack_for(2.0 * HALF))
        }
    );

    check!(
        checks,
        "a difference of a body with a copy of itself is empty or refused",
        {
            // There is no empty `Body` in this contract, so a kernel is
            // entitled to refuse this outright — and a kernel that answers has
            // said something about nothing, which had better weigh nothing.
            let a = k.create_box(Vec3::splat(2.0)).map_err(|e| e.to_string())?;
            let b = k.copy(a).map_err(|e| e.to_string())?;
            match k.boolean(BooleanOp::Difference, a, b, tol) {
                Err(_) => Ok(()),
                Ok(d) => match volume_of(k, d, quality) {
                    // An empty result may not tessellate at all, which is not a
                    // failure of this check.
                    Err(_) => Ok(()),
                    Ok(got) => require(
                        got.abs() <= SMALL * 1.0e-3,
                        format!(
                            "subtracting a body from itself left {got} of \
                             material behind"
                        ),
                    ),
                },
            }
        }
    );
}

/// Prose, and not a STEP file by any reading. Used in both directions of the
/// STEP checks so that "not supported" and "not a STEP file" can be told
/// apart.
const NOT_STEP: &[u8] = b"this is not a STEP file, and never was one";

// ---------------------------------------------------------------------------
// The golden census: comparing a result face by face, not weighing it
// ---------------------------------------------------------------------------
//
// The geometry half above weighs a body: it asks how much material a mesh
// encloses. That catches a bounding box pretending to be a boolean, which is
// what it was written for, and it is blind to everything that does not change
// a volume. A hole drilled 8 mm from where it was asked for encloses exactly
// the volume of a hole drilled in the right place.
//
// What follows compares the boundary instead. Three things are asserted about
// it, and each is chosen because it is true of *any* correct kernel:
//
//   - **Every face id names one connected region of the surface.** A face is
//     one region. An id spanning two means two faces share it, and since
//     `face_of_triangle` is what a selection and a per-face fillet are stored
//     against, a click on one of them lands on the other.
//   - **The material in each plane of the boundary, and where its centre of
//     area is.** Not per face: which planar faces a kernel splits a coplanar
//     region into is its own business — a union may leave an L-shaped side as
//     one face or as two, and both are correct — so faces are summed into the
//     plane they lie in first. The area and the area-weighted centroid of a
//     plane's material are then arithmetic, and no partition changes them.
//   - **The total area that is not planar.** A cylindrical wall may be one
//     face or four; its area is 2*pi*r*h either way.
//
// The centroid is the half that volume cannot reach, and it is why this is
// worth its length: it is what says the hole is *where it was asked for*.

/// Union-find over triangle indices, for counting a face's connected regions.
struct Dsu(Vec<usize>);

impl Dsu {
    fn new(n: usize) -> Self {
        Self((0..n).collect())
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.0[x] != x {
            self.0[x] = self.0[self.0[x]];
            x = self.0[x];
        }
        x
    }

    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[a] = b;
        }
    }
}

/// Coincident vertices, as clusters of position indices.
///
/// A tessellator is free to emit a shared corner once or once per face, so
/// "these two triangles touch" cannot be read off the index buffer. Positions
/// decide it instead.
///
/// The method is a sort and a split on gaps, once per axis: everything starts
/// in one bucket, and a bucket is cut wherever consecutive coordinates differ
/// by more than `tol`. That chains — three points at 0, `tol` and `2*tol` come
/// out as one cluster — so it can **over**-merge, and cannot under-merge: two
/// points within `tol` of each other are always in the same cluster.
///
/// Over-merging is the safe direction and is why it is worth having a method
/// with a known bias rather than an exact one. Merging too much can make a
/// face that really is in two pieces look connected — a missed defect. Merging
/// too little would make a connected face look broken, which is a conformance
/// suite failing a correct kernel, and the more expensive mistake by far.
fn weld_clusters(positions: &[[f32; 3]], tol: f64) -> Vec<usize> {
    let n = positions.len();
    let coord = |i: usize, ax: usize| f64::from(positions[i][ax]);

    let mut buckets: Vec<Vec<usize>> = vec![(0..n).collect()];
    for ax in 0..3 {
        let mut next: Vec<Vec<usize>> = Vec::new();
        for mut bucket in buckets {
            bucket.sort_unstable_by(|&i, &j| {
                coord(i, ax)
                    .partial_cmp(&coord(j, ax))
                    .unwrap_or(core::cmp::Ordering::Equal)
            });
            let mut run: Vec<usize> = Vec::new();
            for i in bucket {
                if let Some(&last) = run.last()
                    && coord(i, ax) - coord(last, ax) > tol
                {
                    next.push(core::mem::take(&mut run));
                }
                run.push(i);
            }
            if !run.is_empty() {
                next.push(run);
            }
        }
        buckets = next;
    }

    let mut cluster = vec![0usize; n];
    for (c, bucket) in buckets.iter().enumerate() {
        for &i in bucket {
            cluster[i] = c;
        }
    }
    cluster
}

/// One face id's worth of a tessellation, reduced to what is true of the
/// *face* rather than of the mesh that happens to represent it.
struct Face {
    id: u32,
    area: f64,
    /// Area-weighted mean of the triangle normals, normalised. `None` when the
    /// face curves far enough that they cancel — a whole cylinder in one face
    /// — which is a face no plane describes, and is treated as one.
    normal: Option<Vec3>,
    /// Centre of area.
    centroid: Vec3,
    /// How far the furthest of the face's vertices lies from the plane through
    /// `centroid` with normal `normal`.
    ///
    /// A distance, deliberately, and not an angle between normals: a
    /// constrained triangulation leaves slivers along a trimming curve, a
    /// sliver's normal is numerical noise, and its distance from the plane is
    /// not. Planarity tested by normals fails on the one fixture that matters
    /// here — the face with a hole in it.
    flatness: f64,
    /// How many connected regions of surface carry this id. One, in a correct
    /// kernel.
    regions: usize,
}

/// Every face of a mesh, by [`Mesh::face_of_triangle`].
fn census(mesh: &Mesh, weld: f64) -> Vec<Face> {
    let cluster = weld_clusters(&mesh.positions, weld);
    let point = |i: u32| {
        let p = mesh.positions[i as usize];
        Vec3::new(f64::from(p[0]), f64::from(p[1]), f64::from(p[2]))
    };

    let mut ids: Vec<u32> = mesh.face_of_triangle.clone();
    ids.sort_unstable();
    ids.dedup();

    let mut faces = Vec::with_capacity(ids.len());
    for id in ids {
        let tris: Vec<usize> = (0..mesh.triangle_count())
            .filter(|&t| mesh.face_of_triangle[t] == id)
            .collect();

        let mut area = 0.0;
        let mut normal_sum = Vec3::ZERO;
        let mut centroid_sum = Vec3::ZERO;
        for &t in &tris {
            let a = point(mesh.indices[3 * t]);
            let b = point(mesh.indices[3 * t + 1]);
            let c = point(mesh.indices[3 * t + 2]);
            let cross = (b - a).cross(c - a);
            let tri_area = 0.5 * cross.length();
            area += tri_area;
            normal_sum = normal_sum + cross * 0.5;
            centroid_sum = centroid_sum + (a + b + c) * (tri_area / 3.0);
        }
        let centroid = if area > 0.0 {
            centroid_sum * (1.0 / area)
        } else {
            Vec3::ZERO
        };

        // Relative to the face's own area, so a sliver cannot decide this.
        let normal = normal_sum.normalize(area * 1.0e-6);
        let flatness = match normal {
            None => f64::INFINITY,
            Some(n) => tris
                .iter()
                .flat_map(|&t| (0..3).map(move |k| point(mesh.indices[3 * t + k])))
                .fold(0.0f64, |worst, v| worst.max((v - centroid).dot(n).abs())),
        };

        let mut dsu = Dsu::new(tris.len());
        let mut seen: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
        for (slot, &t) in tris.iter().enumerate() {
            for k in 0..3 {
                let c = cluster[mesh.indices[3 * t + k] as usize];
                match seen.entry(c) {
                    std::collections::hash_map::Entry::Occupied(e) => dsu.union(slot, *e.get()),
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(slot);
                    }
                }
            }
        }
        let mut roots: Vec<usize> = (0..tris.len()).map(|s| dsu.find(s)).collect();
        roots.sort_unstable();
        roots.dedup();

        faces.push(Face {
            id,
            area,
            normal,
            centroid,
            flatness,
            regions: roots.len(),
        });
    }
    faces
}

/// A plane of a body's boundary, and what a fixture asserts about it.
///
/// `offset` is the signed distance from the origin along `normal`, so a plane
/// is named by where it is rather than by which face id happened to land on
/// it — face ids are a backend's own numbering and this suite never compares
/// them between kernels.
struct GoldenPlane {
    normal: Vec3,
    offset: f64,
    area: f64,
    centroid: Vec3,
}

/// How much a golden fixture is willing to be wrong by, and about what.
///
/// Three numbers rather than one because they answer to three different
/// things: `flat` to `f32` and to the sag a curved neighbour is allowed,
/// `area` to a tessellation being inscribed, and `centroid` to a position,
/// which is the number this whole census exists to assert and the one that has
/// no reason to drift at all.
struct Slack {
    /// Coincident within this, for connectivity.
    weld: f64,
    /// A vertex this far off its face's own plane is still on it.
    flat: f64,
    /// Relative, on the area in one plane.
    area: f64,
    /// Relative, on the *total* curved area, and one-sided: a tessellation
    /// whose vertices lie on the surface is inscribed, and a polygon inscribed
    /// in a circle is shorter than the circle. So a curved area may fall short
    /// by the sag and may not exceed by more than rounding, and this is the
    /// looser of the two directions. It is looser than `area` for the same
    /// reason: a wall's area is a perimeter times a height and loses the
    /// polygon's whole shortfall, where a face with a hole in it loses only
    /// the difference between two areas.
    curved: f64,
    /// Absolute, on a centre of area.
    centroid: f64,
}

/// A body's boundary against a fixture that names every plane of it.
///
/// `curved` is the total area of everything not lying in one of `planes` —
/// 2*pi*r*h for the wall of a hole, and zero for a body with no curved surface
/// at all, which is asserted rather than skipped.
fn check_census(
    mesh: &Mesh,
    planes: &[GoldenPlane],
    curved: f64,
    slack: &Slack,
) -> core::result::Result<(), String> {
    let faces = census(mesh, slack.weld);

    for f in &faces {
        require(
            f.regions == 1,
            format!(
                "face {} covers {} disconnected regions of the surface. A face \
                 is one region: two of them under one id means a click on \
                 either selects both, and a fillet asked for one rounds the \
                 other",
                f.id, f.regions
            ),
        )?;
    }

    // A face lies in a golden plane when it is flat, points the same way, and
    // sits at the same distance. All three, because two of them are satisfied
    // by the face on the opposite side of a plate.
    let mut claimed = vec![false; faces.len()];
    for plane in planes {
        let mut area = 0.0;
        let mut centroid_sum = Vec3::ZERO;
        for (i, f) in faces.iter().enumerate() {
            let Some(n) = f.normal else { continue };
            if f.flatness > slack.flat
                || n.dot(plane.normal) < 1.0 - 1.0e-6
                || (f.centroid.dot(plane.normal) - plane.offset).abs() > slack.flat
            {
                continue;
            }
            claimed[i] = true;
            area += f.area;
            centroid_sum = centroid_sum + f.centroid * f.area;
        }

        require(
            area > 0.0,
            format!(
                "nothing in this body lies in the plane {:?} at {}, where the \
                 fixture says {} of material is",
                plane.normal, plane.offset, plane.area
            ),
        )?;
        require(
            within(area, plane.area, slack.area),
            format!(
                "the plane {:?} at {} holds {area} of material, not {}",
                plane.normal, plane.offset, plane.area
            ),
        )?;

        let centroid = centroid_sum * (1.0 / area);
        let off = (centroid - plane.centroid).length();
        require(
            off <= slack.centroid,
            format!(
                "the material in the plane {:?} at {} has its centre of area \
                 at {:?}, and the fixture puts it at {:?} — {off} away. The \
                 area is right and the position is not, which is what a \
                 feature in the wrong place looks like",
                plane.normal, plane.offset, centroid, plane.centroid
            ),
        )?;
    }

    let unclaimed: f64 = faces
        .iter()
        .zip(&claimed)
        .filter(|&(_, &c)| !c)
        .map(|(f, _)| f.area)
        .sum();
    require(
        unclaimed <= curved * (1.0 + 1.0e-3) && unclaimed >= curved * (1.0 - slack.curved),
        format!(
            "{unclaimed} of this body's boundary lies in no plane the fixture \
             names, and it says there should be {curved}"
        ),
    )
}

/// Slack proportional to the fixture, so one set of numbers describes a 6 mm
/// box and a 40 mm plate alike. `size` is the body's largest extent.
fn slack_for(size: f64) -> Slack {
    Slack {
        weld: size * 1.0e-5,
        // A planar face's vertices are off its own plane by `f32` rounding and
        // nothing else. The sag belongs to the curved faces and is deliberately
        // *not* in this number: a tolerance loose enough to admit a curved face
        // as flat would let the wall of a hole be counted as part of the plate.
        flat: size * 1.0e-4,
        // Measured rather than guessed, against `TruckKernel` at
        // `Quality::display_default()` on the drilled plate: the plane holding
        // the face with the hole in it came out 1.15e-4 over, the wall 1.8e-4
        // short, and the centre of area 0.001 from where the arithmetic puts
        // it. Every number below is that measurement with an order of
        // magnitude on it — loose enough that a finer or coarser tessellator
        // does not fail the suite, and tight enough that the defects these
        // exist to catch miss by a wide margin: a hole 0.3 mm out of place
        // moves the centroid past `centroid`, and a wall that is not there at
        // all misses `curved` by the whole of it.
        //
        // `OcctKernel`'s margins are **not** measured here. OCCT is not
        // installed in the environment this was written in, so what holds
        // these numbers against the other backend that does geometry is CI.
        area: 2.0e-3,
        curved: 2.0e-2,
        centroid: size * 5.0e-4,
    }
}

#[cfg(test)]
mod census_controls {
    //! Negative controls for the golden census.
    //!
    //! `make licences` runs its negative controls first because a checker that
    //! cannot fail is a checker that says yes, and the same argument applies
    //! with more force here: [`check_census`] passes against both backends in
    //! the tree, and that is equally consistent with it asserting nothing.
    //!
    //! So the census is run against a mesh built here, by hand, with no kernel
    //! anywhere — a 40x40x10 plate with a *square* hole through it, which
    //! exercises every path the real fixture does and whose every number is an
    //! integer. Then the same mesh is broken in the three specific ways the
    //! census claims to catch, and each break has to produce a failure.
    //!
    //! The second control is the one worth reading. It moves the hole and
    //! changes nothing else: the volume is identical, the bounding box is
    //! identical, the face count is identical, and every area in the census is
    //! identical. Only the centre of area moves. That is the defect this whole
    //! file was written for, and it is invisible to every other check in this
    //! suite.

    use super::*;

    const H: f64 = 20.0;
    const T: f64 = 10.0;
    const HOLE: f64 = 3.0;

    /// Two triangles, wound counter-clockwise seen from outside when `a`, `b`,
    /// `c`, `d` go round that way.
    fn quad(mesh: &mut Mesh, face: u32, corners: [[f64; 3]; 4]) {
        let base = mesh.positions.len() as u32;
        for c in corners {
            mesh.positions.push([c[0] as f32, c[1] as f32, c[2] as f32]);
            // Never read by the census, which takes its normals from the
            // triangles. Present so the mesh is well-formed.
            mesh.normals.push([0.0, 0.0, 0.0]);
        }
        mesh.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        mesh.face_of_triangle.push(face);
        mesh.face_of_triangle.push(face);
    }

    /// A 40x40x10 plate with a square hole of side `2 * HOLE` through it,
    /// centred at `(at, 0)`. Ten faces: two with the hole in them, four sides,
    /// and four walls.
    fn plate(at: f64) -> Mesh {
        let mut m = Mesh::default();
        let (lo, hi) = (at - HOLE, at + HOLE);
        let z = T / 2.0;

        // The face with the hole in it, top and bottom, as four rectangles
        // around the opening — one face id each, so the census has a face made
        // of pieces that are only connected through each other.
        for (id, sign) in [(0u32, 1.0), (1u32, -1.0)] {
            let s = sign * z;
            let rects = [
                [[-H, -H], [lo, -H], [lo, H], [-H, H]],
                [[hi, -H], [H, -H], [H, H], [hi, H]],
                [[lo, -H], [hi, -H], [hi, -HOLE], [lo, -HOLE]],
                [[lo, HOLE], [hi, HOLE], [hi, H], [lo, H]],
            ];
            for r in rects {
                let mut c = r.map(|p| [p[0], p[1], s]);
                if sign < 0.0 {
                    c.reverse();
                }
                quad(&mut m, id, c);
            }
        }

        // The four outer sides.
        quad(&mut m, 2, [[H, -H, -z], [H, H, -z], [H, H, z], [H, -H, z]]);
        quad(
            &mut m,
            3,
            [[-H, H, -z], [-H, -H, -z], [-H, -H, z], [-H, H, z]],
        );
        quad(&mut m, 4, [[H, H, -z], [-H, H, -z], [-H, H, z], [H, H, z]]);
        quad(
            &mut m,
            5,
            [[-H, -H, -z], [H, -H, -z], [H, -H, z], [-H, -H, z]],
        );

        // The four walls of the hole. Their outward normals point *into* the
        // hole, which is what puts them in planes no golden names.
        quad(
            &mut m,
            6,
            [
                [lo, -HOLE, -z],
                [lo, HOLE, -z],
                [lo, HOLE, z],
                [lo, -HOLE, z],
            ],
        );
        quad(
            &mut m,
            7,
            [
                [hi, HOLE, -z],
                [hi, -HOLE, -z],
                [hi, -HOLE, z],
                [hi, HOLE, z],
            ],
        );
        quad(
            &mut m,
            8,
            [
                [hi, -HOLE, -z],
                [lo, -HOLE, -z],
                [lo, -HOLE, z],
                [hi, -HOLE, z],
            ],
        );
        quad(
            &mut m,
            9,
            [[lo, HOLE, -z], [hi, HOLE, -z], [hi, HOLE, z], [lo, HOLE, z]],
        );
        m
    }

    /// The six planes of a plate whose hole is at `(at, 0)`, and the area of
    /// the walls that lie in none of them.
    fn golden(at: f64) -> ([GoldenPlane; 6], f64) {
        let face = 4.0 * H * H;
        let hole = 4.0 * HOLE * HOLE;
        let shift = -at * hole / (face - hole);
        let side = 2.0 * H * T;
        (
            [
                GoldenPlane {
                    normal: Vec3::Z,
                    offset: T / 2.0,
                    area: face - hole,
                    centroid: Vec3::new(shift, 0.0, T / 2.0),
                },
                GoldenPlane {
                    normal: Vec3::new(0.0, 0.0, -1.0),
                    offset: T / 2.0,
                    area: face - hole,
                    centroid: Vec3::new(shift, 0.0, -T / 2.0),
                },
                GoldenPlane {
                    normal: Vec3::X,
                    offset: H,
                    area: side,
                    centroid: Vec3::new(H, 0.0, 0.0),
                },
                GoldenPlane {
                    normal: Vec3::new(-1.0, 0.0, 0.0),
                    offset: H,
                    area: side,
                    centroid: Vec3::new(-H, 0.0, 0.0),
                },
                GoldenPlane {
                    normal: Vec3::Y,
                    offset: H,
                    area: side,
                    centroid: Vec3::new(0.0, H, 0.0),
                },
                GoldenPlane {
                    normal: Vec3::new(0.0, -1.0, 0.0),
                    offset: H,
                    area: side,
                    centroid: Vec3::new(0.0, -H, 0.0),
                },
            ],
            // Four walls, each `2 * HOLE` across and `T` deep.
            8.0 * HOLE * T,
        )
    }

    #[test]
    fn the_fixture_describes_the_plate_it_was_written_for() {
        let (planes, walls) = golden(8.0);
        check_census(&plate(8.0), &planes, walls, &slack_for(2.0 * H))
            .expect("the plate this fixture describes");
    }

    #[test]
    fn a_hole_in_the_wrong_place_keeps_every_area_and_is_caught_anyway() {
        // Same plate, hole moved 2 mm. Nothing an area, a volume, a bounding
        // box or a face count can see.
        let moved = plate(6.0);
        let (planes, walls) = golden(8.0);

        let err = check_census(&moved, &planes, walls, &slack_for(2.0 * H))
            .expect_err("a hole 2 mm from where the fixture puts it must fail");
        assert!(
            err.contains("centre of area"),
            "the hole moved and the census failed for some other reason: {err}"
        );

        // And the claim that nothing else moved, asserted rather than assumed:
        // the area is still exactly right, so the centroid is carrying this
        // failure on its own.
        let faces = census(&moved, 2.0 * H * 1.0e-5);
        let by_plane: f64 = faces.iter().take(2).map(|f| f.area).sum();
        assert!(
            (by_plane - 2.0 * (4.0 * H * H - 4.0 * HOLE * HOLE)).abs() < 1.0e-9,
            "the two faces with the hole in them hold {by_plane}, and moving a \
             hole does not change that"
        );
    }

    #[test]
    fn one_id_over_two_separate_patches_is_caught() {
        // The two opposite sides of the plate, given the same face id. They
        // are 40 mm apart and nothing joins them, so this is two faces wearing
        // one name — and a click on either would select both.
        let mut broken = plate(8.0);
        for f in &mut broken.face_of_triangle {
            if *f == 5 {
                *f = 4;
            }
        }
        let (planes, walls) = golden(8.0);
        let err = check_census(&broken, &planes, walls, &slack_for(2.0 * H))
            .expect_err("one id over two patches must fail");
        assert!(
            err.contains("disconnected regions"),
            "two patches under one id failed for some other reason: {err}"
        );
    }

    #[test]
    fn a_wall_that_is_not_there_is_caught() {
        // The hole's walls deleted: the plate still has a hole in the face it
        // is drilled through, and nothing lining it.
        let mut hollow = plate(8.0);
        let keep: Vec<bool> = hollow.face_of_triangle.iter().map(|&f| f < 6).collect();
        let mut i = 0;
        hollow.indices = hollow
            .indices
            .chunks(3)
            .zip(&keep)
            .filter(|&(_, &k)| k)
            .flat_map(|(t, _)| t.to_vec())
            .collect();
        hollow.face_of_triangle.retain(|_| {
            i += 1;
            keep[i - 1]
        });

        let (planes, walls) = golden(8.0);
        let err = check_census(&hollow, &planes, walls, &slack_for(2.0 * H))
            .expect_err("a hole with no wall must fail");
        assert!(
            err.contains("lies in no plane"),
            "a missing wall failed for some other reason: {err}"
        );
    }
}
