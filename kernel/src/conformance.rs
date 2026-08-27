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

use crate::{
    Aabb, BooleanOp, GeometryKernel, KernelError, Mat4, Mesh, Profile, Quality, Tolerance, Vec3,
};

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

    Report {
        kernel: k.name(),
        checks,
    }
}

/// Prose, and not a STEP file by any reading. Used in both directions of the
/// STEP checks so that "not supported" and "not a STEP file" can be told
/// apart.
const NOT_STEP: &[u8] = b"this is not a STEP file, and never was one";
