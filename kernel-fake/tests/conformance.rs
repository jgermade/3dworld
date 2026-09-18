//! The fake is held to exactly the same suite as any real backend. That is the
//! point of the suite: "a backend is one that passes this" has to be true of
//! the first one too, or it will not be true of the fourth.
//!
//! Since 2026-09-05 "the same suite" has two halves, and this backend says it
//! does not do geometry, so it is held to one of them. The second test below
//! is what keeps that from being an excuse: the half it is let off **must
//! fail** here, and must fail on the boolean.

use w3d_kernel::{
    Aabb, Body, BooleanOp, Capability, GeometryKernel, Import, Mat4, Mesh, Profile, Quality,
    Result, SketchPlane, Tolerance, Topology, Vec3, conformance,
};
use w3d_kernel_fake::FakeKernel;

#[test]
fn the_fake_kernel_conforms() {
    let mut k = FakeKernel::new();
    let report = conformance::run(
        &mut k,
        Tolerance::document_default(),
        Quality::display_default(),
    );
    report.assert_passed();
}

/// The negative control for the geometry half of the suite.
///
/// `FakeKernel`'s boolean keeps a tree and answers from bounding boxes, which
/// is a fair description of what a stub does and an exact description of what
/// `TruckKernel` did until the record file for 2026-09-05. If the geometry
/// half cannot tell that from a boolean, it is not worth running against the
/// backends that do claim geometry — so it is pointed at the one backend that
/// is honest about not doing any, and required to say no.
#[test]
fn the_geometry_half_fails_the_backend_that_does_not_do_geometry() {
    let mut k = FakeKernel::new();
    let report = conformance::geometry(
        &mut k,
        Tolerance::document_default(),
        Quality::display_default(),
    );
    assert!(
        !report.passed(),
        "the fake kernel passed the geometry half, so the geometry half proves \
         nothing about the backends that do claim geometry"
    );

    let failed: Vec<&str> = report.failures().map(|c| c.name).collect();
    for expected in [
        "a difference removes exactly what the tool covers",
        "a union of two boxes is both, counted once",
    ] {
        assert!(
            failed.contains(&expected),
            "{expected:?} passed against bounding boxes. The failures were {failed:?}"
        );
    }
}

/// The negative control for the capability query.
///
/// `supports` is a second description of the backend, written by hand beside
/// the one the code gives, and the only thing that keeps the two from drifting
/// is the conformance probe. So the probe is pointed at a backend whose two
/// descriptions are *known* to disagree, and required to say so — in both
/// directions, because they fail differently and a check that caught only one
/// would be worse than none: it would pass the migration where a busy author
/// answers `false` to everything.
///
/// A backend that lies this way is not hypothetical. It is what every backend
/// becomes the day an operation learns or forgets something and the match arm
/// three hundred lines away is not touched, and the symptom is a button in the
/// wrong state — which the user who needed it reports as the feature being
/// missing, if they report it at all.
#[test]
fn the_capability_check_fails_a_backend_whose_answers_and_behaviour_disagree() {
    const NAME: &str = "what a backend says it can do is what it does, in both directions";

    // `FakeKernel` blends (as bookkeeping) and does not do STEP, so flipping
    // those two answers produces one lie of each kind against a backend that
    // otherwise passes the whole suite.
    for (lie, direction) in [
        // Fake blends, so inverting its answer *denies* a capability it has.
        (Capability::Blend, "denied a capability it performs"),
        // Fake writes no STEP, so inverting it *claims* one it has not.
        (Capability::StepExport, "claimed a capability it declines"),
    ] {
        let mut k = Liar {
            inner: FakeKernel::new(),
            lie,
        };
        let report = conformance::run(
            &mut k,
            Tolerance::document_default(),
            Quality::display_default(),
        );
        let failed: Vec<&str> = report.failures().map(|c| c.name).collect();
        assert_eq!(
            failed,
            vec![NAME],
            "a backend that {direction} was not caught by the capability check alone; \
             the failures were {failed:?}"
        );
    }
}

/// `FakeKernel` with exactly one capability answer inverted, and everything
/// else delegated untouched.
///
/// The delegation is verbose on purpose: a wrapper with a blanket
/// implementation could not be held to the rest of the suite, and being held
/// to the rest of the suite is what makes the assertion above exact rather
/// than approximate. The test requires the capability check to be the **only**
/// failure, which is a claim about what the probe does *not* disturb as much
/// as about what it catches.
struct Liar {
    inner: FakeKernel,
    lie: Capability,
}

impl GeometryKernel for Liar {
    fn supports(&self, cap: Capability) -> bool {
        if cap == self.lie {
            !self.inner.supports(cap)
        } else {
            self.inner.supports(cap)
        }
    }

    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn does_geometry(&self) -> bool {
        self.inner.does_geometry()
    }
    fn create_box(&mut self, size: Vec3) -> Result<Body> {
        self.inner.create_box(size)
    }
    fn create_sphere(&mut self, radius: f64) -> Result<Body> {
        self.inner.create_sphere(radius)
    }
    fn create_cylinder(&mut self, radius: f64, height: f64) -> Result<Body> {
        self.inner.create_cylinder(radius, height)
    }
    fn boolean(&mut self, op: BooleanOp, a: Body, b: Body, tol: Tolerance) -> Result<Body> {
        self.inner.boolean(op, a, b, tol)
    }
    fn transform(&mut self, body: Body, m: &Mat4) -> Result<Body> {
        self.inner.transform(body, m)
    }
    fn copy(&mut self, body: Body) -> Result<Body> {
        self.inner.copy(body)
    }
    fn delete(&mut self, body: Body) -> Result<()> {
        self.inner.delete(body)
    }
    fn fillet(&mut self, body: Body, radius: f64) -> Result<Body> {
        self.inner.fillet(body, radius)
    }
    fn chamfer(&mut self, body: Body, distance: f64) -> Result<Body> {
        self.inner.chamfer(body, distance)
    }
    fn fillet_edges(&mut self, body: Body, edges: &[u32], radius: f64) -> Result<Body> {
        self.inner.fillet_edges(body, edges, radius)
    }
    fn chamfer_edges(&mut self, body: Body, edges: &[u32], distance: f64) -> Result<Body> {
        self.inner.chamfer_edges(body, edges, distance)
    }
    fn extrude(&mut self, profile: &Profile, distance: f64) -> Result<Body> {
        self.inner.extrude(profile, distance)
    }
    fn revolve(
        &mut self,
        profile: &Profile,
        axis_origin: Vec3,
        axis_dir: Vec3,
        angle_rad: f64,
    ) -> Result<Body> {
        self.inner
            .revolve(profile, axis_origin, axis_dir, angle_rad)
    }
    fn sweep(&mut self, profile: &Profile, path_points: &[Vec3]) -> Result<Body> {
        self.inner.sweep(profile, path_points)
    }
    fn loft(&mut self, profiles: &[Profile], planes: &[SketchPlane]) -> Result<Body> {
        self.inner.loft(profiles, planes)
    }
    fn shell(&mut self, body: Body, face_id: u32, thickness: f64) -> Result<Body> {
        self.inner.shell(body, face_id, thickness)
    }
    fn topology(&self, body: Body) -> Result<Topology> {
        self.inner.topology(body)
    }
    fn bounds(&self, body: Body) -> Result<Aabb> {
        self.inner.bounds(body)
    }
    fn tessellate(&self, body: Body, quality: Quality) -> Result<Mesh> {
        self.inner.tessellate(body, quality)
    }
    fn geometry_format(&self) -> &'static str {
        self.inner.geometry_format()
    }
    fn save_body(&self, body: Body) -> Result<Vec<u8>> {
        self.inner.save_body(body)
    }
    fn load_body(&mut self, bytes: &[u8]) -> Result<Body> {
        self.inner.load_body(bytes)
    }
    fn export_step(&self, bodies: &[Body]) -> Result<Vec<u8>> {
        self.inner.export_step(bodies)
    }
    fn import_step(&mut self, bytes: &[u8]) -> Result<Import> {
        self.inner.import_step(bytes)
    }
}
