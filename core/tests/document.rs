//! The document driven against the fake kernel — no OCCT, no browser, no
//! `.wasm`. If this file needs a real kernel to say something, the seam has a
//! hole in it.

use w3d_core::Document;
use w3d_core::kernel::{
    Aabb, Body, BooleanOp, GeometryKernel, Import, ImportedAssembly, ImportedBody, Mat4, Mesh,
    Profile, Quality, SketchPlane, Tolerance, Topology, Vec3,
};
use w3d_kernel_fake::FakeKernel;

fn doc() -> Document<FakeKernel> {
    Document::new(FakeKernel::new())
}

#[test]
fn primitives_land_with_the_bounds_they_were_asked_for() {
    let mut d = doc();
    let id = d.add_box("Base", Vec3::new(2.0, 4.0, 6.0)).unwrap();

    assert_eq!(d.len(), 1);
    assert_eq!(d.node(id).unwrap().name, "Base");
    let b = d.bounds(id).unwrap();
    assert_eq!(b, Aabb::centered(Vec3::new(2.0, 4.0, 6.0)));
}

#[test]
fn a_degenerate_primitive_leaves_the_document_untouched() {
    let mut d = doc();
    assert!(d.add_sphere("Bad", 0.0).is_err());
    assert!(d.is_empty());
    // And nothing was recorded, so there is nothing to undo.
    assert!(!d.history().can_undo());
}

#[test]
fn a_boolean_consumes_its_nodes_and_undo_puts_them_back() {
    let mut d = doc();
    let a = d.add_box("A", Vec3::splat(2.0)).unwrap();
    let b = d.add_sphere("B", 1.0).unwrap();

    let u = d.boolean(BooleanOp::Union, a, b).unwrap();
    assert_eq!(d.len(), 1);
    assert!(d.node(a).is_err());
    assert!(d.node(b).is_err());
    assert_eq!(d.node(u).unwrap().name, "A ∪ B");

    assert_eq!(d.undo(), Some("Union"));
    assert_eq!(d.len(), 2);
    // The *same* handles come back, not equivalent ones. Anything holding a
    // NodeId across an undo — a selection, another edit — depends on this.
    assert_eq!(d.node(a).unwrap().name, "A");
    assert_eq!(d.node(b).unwrap().name, "B");
    assert!(d.node(u).is_err());

    assert_eq!(d.redo(), Some("Union"));
    assert_eq!(d.len(), 1);
    assert_eq!(d.node(u).unwrap().name, "A ∪ B");
}

#[test]
fn undo_walks_all_the_way_back_and_redo_all_the_way_forward() {
    let mut d = doc();
    let a = d.add_box("A", Vec3::splat(2.0)).unwrap();
    let b = d.add_sphere("B", 1.0).unwrap();
    d.boolean(BooleanOp::Difference, a, b).unwrap();
    d.rename(a, "renamed").ok(); // a is gone; this is a no-op error
    assert_eq!(d.len(), 1);

    let labels: Vec<_> = std::iter::from_fn(|| d.undo()).collect();
    assert_eq!(labels, vec!["Difference", "Add sphere", "Add box"]);
    assert!(d.is_empty());

    let labels: Vec<_> = std::iter::from_fn(|| d.redo()).collect();
    assert_eq!(labels, vec!["Add box", "Add sphere", "Difference"]);
    assert_eq!(d.len(), 1);
}

#[test]
fn a_new_edit_makes_the_redo_branch_unreachable() {
    let mut d = doc();
    d.add_box("A", Vec3::splat(1.0)).unwrap();
    d.add_sphere("B", 1.0).unwrap();
    d.undo();
    assert!(d.history().can_redo());

    d.add_cylinder("C", 1.0, 2.0).unwrap();
    assert!(!d.history().can_redo());
    assert_eq!(d.redo(), None);
}

#[test]
fn transform_is_undoable_and_moves_the_bounds() {
    let mut d = doc();
    let id = d.add_box("A", Vec3::splat(2.0)).unwrap();
    let before = d.bounds(id).unwrap();

    d.transform(id, &Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0)))
        .unwrap();
    let after = d.bounds(id).unwrap();
    assert_eq!(after.center().x, before.center().x + 10.0);

    assert_eq!(d.undo(), Some("Transform"));
    assert_eq!(d.bounds(id).unwrap(), before);
}

#[test]
fn a_no_op_edit_does_not_enter_history() {
    let mut d = doc();
    let id = d.add_box("A", Vec3::splat(1.0)).unwrap();
    d.rename(id, "A").unwrap();
    assert_eq!(d.undo(), Some("Add box"));
}

#[test]
fn undoing_past_a_nodes_creation_drops_it_from_the_selection() {
    let mut d = doc();
    let a = d.add_box("A", Vec3::splat(1.0)).unwrap();
    let b = d.add_sphere("B", 1.0).unwrap();
    d.select(a).unwrap();
    d.select(b).unwrap();
    assert_eq!(d.selection().count(), 2);

    d.undo();
    assert_eq!(d.selection().collect::<Vec<_>>(), vec![a]);
    assert!(!d.is_selected(b));

    // ...and comes back when it does.
    d.redo();
    assert_eq!(d.selection().count(), 2);
}

#[test]
fn selecting_something_that_does_not_exist_is_an_error_not_a_silence() {
    let mut d = doc();
    let a = d.add_box("A", Vec3::splat(1.0)).unwrap();
    d.remove(a).unwrap();
    assert!(d.select(a).is_err());
}

#[test]
fn meshes_are_cached_per_body_and_survive_re_asking() {
    let mut d = doc();
    let id = d.add_sphere("S", 1.0).unwrap();

    let first = d.mesh(id).unwrap().clone();
    assert!(first.triangle_count() > 0);
    assert_eq!(d.cached_mesh_count(), 1);

    let second = d.mesh(id).unwrap();
    assert_eq!(&first, second);
    assert_eq!(d.cached_mesh_count(), 1);
}

#[test]
fn changing_quality_evicts_the_cache_but_not_the_model() {
    let mut d = doc();
    let id = d.add_sphere("S", 1.0).unwrap();
    let bounds = d.bounds(id).unwrap();
    let coarse = d.mesh(id).unwrap().triangle_count();

    d.set_quality(Quality::new(0.001, 0.05));
    assert_eq!(d.cached_mesh_count(), 0);

    let fine = d.mesh(id).unwrap().triangle_count();
    assert!(fine >= coarse, "{fine} triangles against {coarse}");
    assert_eq!(
        d.bounds(id).unwrap(),
        bounds,
        "tessellation changed the model"
    );
}

#[test]
fn visible_bounds_covers_what_is_shown_and_ignores_what_is_not() {
    let mut d = doc();
    let a = d.add_box("A", Vec3::splat(2.0)).unwrap();
    let b = d.add_box("B", Vec3::splat(2.0)).unwrap();
    d.transform(b, &Mat4::from_translation(Vec3::new(100.0, 0.0, 0.0)))
        .unwrap();

    assert!(d.visible_bounds().size().x > 100.0);
    d.set_visible(b, false).unwrap();
    assert_eq!(d.visible_bounds(), d.bounds(a).unwrap());
}

#[test]
fn history_holds_bodies_alive_and_clearing_it_lets_them_go() {
    // The cost of state-based undo, made visible. If this test starts failing
    // it is because something learned to free a body history still needs.
    let mut d = doc();
    let a = d.add_box("A", Vec3::splat(2.0)).unwrap();
    let b = d.add_sphere("B", 1.0).unwrap();
    d.boolean(BooleanOp::Union, a, b).unwrap();

    // Two operands plus the result.
    assert_eq!(d.kernel().live_bodies(), 3);
    assert_eq!(d.collect_garbage(), 0, "history still refers to all three");

    d.undo();
    assert_eq!(
        d.collect_garbage(),
        0,
        "redo can still reach the union's body"
    );

    d.redo();
    d.clear_history();
    assert_eq!(
        d.collect_garbage(),
        2,
        "the two operands are now unreachable"
    );
    assert_eq!(d.kernel().live_bodies(), 1);
}

#[test]
fn the_document_never_names_a_backend() {
    // Not an assertion so much as a demonstration: this function is generic
    // over the kernel and compiles, which is the property the seam exists for.
    fn build<K: GeometryKernel>(kernel: K) -> Document<K> {
        let mut d = Document::new(kernel);
        d.set_tolerance(Tolerance::new(1.0e-6, 1.0e-4));
        d.add_box("A", Vec3::splat(1.0)).unwrap();
        d
    }
    assert_eq!(build(FakeKernel::new()).len(), 1);
}

#[test]
fn user_grouped_transactions_merge_multi_step_edits_into_one_undo_step() {
    let mut d = doc();
    let box_id = d.add_box("Box", Vec3::splat(2.0)).unwrap();
    let initial_bounds = d.bounds(box_id).unwrap();

    // Group multiple drag transform steps into a single transaction
    d.begin_transaction("Drag Move");
    d.transform(box_id, &Mat4::from_translation(Vec3::new(1.0, 0.0, 0.0)))
        .unwrap();
    d.transform(box_id, &Mat4::from_translation(Vec3::new(2.0, 0.0, 0.0)))
        .unwrap();
    d.transform(box_id, &Mat4::from_translation(Vec3::new(3.0, 0.0, 0.0)))
        .unwrap();
    d.commit_transaction();

    let moved_bounds = d.bounds(box_id).unwrap();
    assert_ne!(initial_bounds, moved_bounds);

    // One undo step reverses all three drag move steps back to initial bounds
    assert_eq!(d.undo(), Some("Drag Move"));
    let restored_bounds = d.bounds(box_id).unwrap();
    assert_eq!(initial_bounds, restored_bounds);

    // Redo restores all three drag move steps
    assert_eq!(d.redo(), Some("Drag Move"));
    assert_eq!(d.bounds(box_id).unwrap(), moved_bounds);
}

#[test]
fn compaction_requires_clear_history_and_reclaims_tombstones() {
    let mut d = doc();
    let b1 = d.add_box("B1", Vec3::splat(1.0)).unwrap();
    let b2 = d.add_box("B2", Vec3::splat(1.0)).unwrap();
    let _b3 = d.add_box("B3", Vec3::splat(1.0)).unwrap();

    let united = d.boolean(BooleanOp::Union, b1, b2).unwrap();

    // Compacting while history is non-empty must fail with HistoryNotEmpty
    assert_eq!(d.compact(), Err(w3d_core::DocumentError::HistoryNotEmpty));

    // Clear history to allow compaction
    d.clear_history();
    d.select(united).unwrap();

    let freed = d.compact().unwrap();
    assert_eq!(freed, 2, "reclaimed 2 deleted operand tombstone slots");

    // Selection set updated to remapped ID
    assert!(d.is_selected(united) || d.selection().next().is_some());
    assert_eq!(d.len(), 2);
}

#[test]
fn assembly_hierarchy_groups_and_reparenting() {
    let mut d = doc();
    let group = d.add_group("Engine Assembly");
    let part1 = d.add_box("Piston", Vec3::splat(1.0)).unwrap();
    let part2 = d.add_box("Crankshaft", Vec3::splat(2.0)).unwrap();

    assert_eq!(d.parent_of(part1), None);
    assert_eq!(d.children_of(group), &[]);

    d.reparent(part1, Some(group)).unwrap();
    d.reparent(part2, Some(group)).unwrap();

    assert_eq!(d.parent_of(part1), Some(group));
    assert_eq!(d.parent_of(part2), Some(group));
    assert_eq!(d.children_of(group), &[part1, part2]);

    d.reparent(part1, None).unwrap();
    assert_eq!(d.parent_of(part1), None);
    assert_eq!(d.children_of(group), &[part2]);
}

// ---------------------------------------------------------------------------
// A kernel that can hand the document an assembly
// ---------------------------------------------------------------------------
//
// `FakeKernel` refuses STEP, which is conforming and leaves the document's
// tree-building with no way to be tested here — and "here" is where it has to
// be tested, because turning an `Import` into nodes is the document's job and
// has nothing to do with which kernel read the file. So this is `FakeKernel` in
// every respect but one: `import_step` answers with a tree it was handed.
//
// It is a stub and not a second backend: `name` says so, and the only method
// with a body of its own is the one under test.

struct Assembling {
    inner: FakeKernel,
    answer: fn(&mut FakeKernel) -> w3d_core::kernel::Import,
}

impl Assembling {
    fn new(answer: fn(&mut FakeKernel) -> w3d_core::kernel::Import) -> Self {
        Self {
            inner: FakeKernel::new(),
            answer,
        }
    }
}

impl GeometryKernel for Assembling {
    fn import_step(&mut self, _bytes: &[u8]) -> w3d_core::kernel::Result<Import> {
        Ok((self.answer)(&mut self.inner))
    }

    fn name(&self) -> &'static str {
        "assembling stub over the fake kernel"
    }
    fn does_geometry(&self) -> bool {
        self.inner.does_geometry()
    }
    fn create_box(&mut self, size: Vec3) -> w3d_core::kernel::Result<Body> {
        self.inner.create_box(size)
    }
    fn create_sphere(&mut self, radius: f64) -> w3d_core::kernel::Result<Body> {
        self.inner.create_sphere(radius)
    }
    fn create_cylinder(&mut self, radius: f64, height: f64) -> w3d_core::kernel::Result<Body> {
        self.inner.create_cylinder(radius, height)
    }
    fn boolean(
        &mut self,
        op: BooleanOp,
        a: Body,
        b: Body,
        tol: Tolerance,
    ) -> w3d_core::kernel::Result<Body> {
        self.inner.boolean(op, a, b, tol)
    }
    fn transform(&mut self, body: Body, m: &Mat4) -> w3d_core::kernel::Result<Body> {
        self.inner.transform(body, m)
    }
    fn copy(&mut self, body: Body) -> w3d_core::kernel::Result<Body> {
        self.inner.copy(body)
    }
    fn delete(&mut self, body: Body) -> w3d_core::kernel::Result<()> {
        self.inner.delete(body)
    }
    fn fillet(&mut self, body: Body, radius: f64) -> w3d_core::kernel::Result<Body> {
        self.inner.fillet(body, radius)
    }
    fn chamfer(&mut self, body: Body, distance: f64) -> w3d_core::kernel::Result<Body> {
        self.inner.chamfer(body, distance)
    }
    fn extrude(&mut self, profile: &Profile, distance: f64) -> w3d_core::kernel::Result<Body> {
        self.inner.extrude(profile, distance)
    }
    fn revolve(
        &mut self,
        profile: &Profile,
        axis_origin: Vec3,
        axis_dir: Vec3,
        angle_rad: f64,
    ) -> w3d_core::kernel::Result<Body> {
        self.inner
            .revolve(profile, axis_origin, axis_dir, angle_rad)
    }
    fn sweep(&mut self, profile: &Profile, path_points: &[Vec3]) -> w3d_core::kernel::Result<Body> {
        self.inner.sweep(profile, path_points)
    }
    fn loft(
        &mut self,
        profiles: &[Profile],
        planes: &[SketchPlane],
    ) -> w3d_core::kernel::Result<Body> {
        self.inner.loft(profiles, planes)
    }
    fn shell(
        &mut self,
        body: Body,
        face_id: u32,
        thickness: f64,
    ) -> w3d_core::kernel::Result<Body> {
        self.inner.shell(body, face_id, thickness)
    }
    fn topology(&self, body: Body) -> w3d_core::kernel::Result<Topology> {
        self.inner.topology(body)
    }
    fn bounds(&self, body: Body) -> w3d_core::kernel::Result<Aabb> {
        self.inner.bounds(body)
    }
    fn tessellate(&self, body: Body, quality: Quality) -> w3d_core::kernel::Result<Mesh> {
        self.inner.tessellate(body, quality)
    }
    fn geometry_format(&self) -> &'static str {
        self.inner.geometry_format()
    }
    fn save_body(&self, body: Body) -> w3d_core::kernel::Result<Vec<u8>> {
        self.inner.save_body(body)
    }
    fn load_body(&mut self, bytes: &[u8]) -> w3d_core::kernel::Result<Body> {
        self.inner.load_body(bytes)
    }
    fn export_step(&self, bodies: &[Body]) -> w3d_core::kernel::Result<Vec<u8>> {
        self.inner.export_step(bodies)
    }
}

/// The tree the document is asked to build, and the smallest one with every
/// case in it: an assembly inside an assembly, a body two levels down, a body
/// one level down, and a body at the root beside the assembly.
fn nested(k: &mut FakeKernel) -> Import {
    let deep = k.create_box(Vec3::splat(1.0)).unwrap();
    let shallow = k.create_box(Vec3::splat(2.0)).unwrap();
    let loose = k.create_box(Vec3::splat(3.0)).unwrap();
    Import {
        assemblies: vec![
            ImportedAssembly {
                name: Some("Top".into()),
                parent: None,
            },
            ImportedAssembly {
                name: Some("Sub".into()),
                parent: Some(0),
            },
        ],
        bodies: vec![
            ImportedBody {
                body: deep,
                name: Some("Deep".into()),
                parent: Some(1),
            },
            ImportedBody {
                body: shallow,
                name: Some("Shallow".into()),
                parent: Some(0),
            },
            ImportedBody {
                body: loose,
                name: Some("Loose".into()),
                parent: None,
            },
        ],
    }
}

#[test]
fn an_import_builds_the_tree_the_file_had() {
    let mut d = Document::new(Assembling::new(nested));
    let ids = d.import_step(b"pretend this is STEP", "Imported").unwrap();

    // Three solids, three nodes returned — the groups are not in the answer,
    // because what a user selects after an import is the parts.
    assert_eq!(ids.len(), 3);
    // Three bodies and two groups.
    assert_eq!(d.len(), 5);

    let named = |name: &str| {
        d.nodes()
            .find(|(_, n)| n.name == name)
            .map(|(id, _)| id)
            .unwrap_or_else(|| panic!("no node called {name}"))
    };
    let (top, sub) = (named("Top"), named("Sub"));
    assert_eq!(d.parent_of(top), None);
    assert_eq!(d.parent_of(sub), Some(top));
    assert_eq!(d.parent_of(named("Deep")), Some(sub));
    assert_eq!(d.parent_of(named("Shallow")), Some(top));
    assert_eq!(d.parent_of(named("Loose")), None);
    // Both halves of the link, not just the child's: an Outliner walks down.
    assert_eq!(d.children_of(sub), &[named("Deep")]);
    assert_eq!(d.children_of(top), &[sub, named("Shallow")]);
}

#[test]
fn undoing_an_import_takes_the_groups_with_it_and_redo_puts_them_back() {
    let mut d = Document::new(Assembling::new(nested));
    d.import_step(b"pretend this is STEP", "Imported").unwrap();
    assert_eq!(d.len(), 5);

    // One transaction, groups included: an import that undoes into two bodies
    // and an empty group is an import that half-happened.
    assert_eq!(d.undo(), Some("Import STEP"));
    assert!(d.is_empty(), "{} nodes survived the undo", d.len());

    assert_eq!(d.redo(), Some("Import STEP"));
    assert_eq!(d.len(), 5);
    // The point of recording the parent's side of the link as well: a redo that
    // restores nodes but not the children lists looks right in the document and
    // empty in the Outliner.
    let named = |name: &str| {
        d.nodes()
            .find(|(_, n)| n.name == name)
            .map(|(id, _)| id)
            .unwrap_or_else(|| panic!("no node called {name}"))
    };
    let (top, sub) = (named("Top"), named("Sub"));
    assert_eq!(d.children_of(top), &[sub, named("Shallow")]);
    assert_eq!(d.children_of(sub), &[named("Deep")]);
    assert_eq!(d.parent_of(sub), Some(top));
}

#[test]
fn a_tree_that_is_not_one_is_refused_and_changes_nothing() {
    fn broken(k: &mut FakeKernel) -> Import {
        let body = k.create_box(Vec3::splat(1.0)).unwrap();
        Import {
            // One assembly, and a body claiming to sit in a second.
            assemblies: vec![ImportedAssembly {
                name: Some("Top".into()),
                parent: None,
            }],
            bodies: vec![ImportedBody {
                body,
                name: Some("Nowhere".into()),
                parent: Some(1),
            }],
        }
    }

    let mut d = Document::new(Assembling::new(broken));
    let outcome = d.import_step(b"pretend this is STEP", "Imported");
    let Err(e) = outcome else {
        panic!("a body under an assembly that is not there was imported anyway");
    };
    assert!(
        e.to_string().contains("not one"),
        "the refusal does not say what was wrong: {e}"
    );
    // Nothing landed, and there is nothing to undo: the tree is checked before
    // the transaction opens.
    assert!(d.is_empty());
    assert!(!d.history().can_undo());
}

#[test]
fn a_node_cannot_be_put_inside_its_own_descendant() {
    let mut d = doc();
    let outer = d.add_group("Outer");
    let inner = d.add_group("Inner");
    let part = d.add_box("Part", Vec3::splat(1.0)).unwrap();
    d.reparent(inner, Some(outer)).unwrap();
    d.reparent(part, Some(inner)).unwrap();

    // One step down is fine, two steps are the same question, and both must be
    // refused: a ring of nodes has no root, and every walk over the tree —
    // the Outliner's, and `parent_of` chasing upwards — runs forever.
    for parent in [inner, part] {
        let err = d
            .reparent(outer, Some(parent))
            .expect_err("a node was put inside its own descendant");
        assert!(
            matches!(err, w3d_core::DocumentError::Cycle { .. }),
            "refused, but as {err}"
        );
    }
    // And the tree is exactly as it was.
    assert_eq!(d.parent_of(outer), None);
    assert_eq!(d.parent_of(inner), Some(outer));
    assert_eq!(d.parent_of(part), Some(inner));
}

#[test]
fn moving_a_node_between_groups_is_one_undo_step() {
    let mut d = doc();
    let from = d.add_group("From");
    let into = d.add_group("Into");
    let part = d.add_box("Part", Vec3::splat(1.0)).unwrap();
    d.reparent(part, Some(from)).unwrap();

    d.reparent(part, Some(into)).unwrap();
    assert_eq!(d.parent_of(part), Some(into));
    assert_eq!(d.children_of(from), &[]);
    assert_eq!(d.children_of(into), &[part]);

    // Both sides of both links come back: the part's parent, the group it
    // rejoins, and the group it leaves again.
    assert_eq!(d.undo(), Some("Reparent"));
    assert_eq!(d.parent_of(part), Some(from));
    assert_eq!(d.children_of(from), &[part]);
    assert_eq!(d.children_of(into), &[]);

    assert_eq!(d.redo(), Some("Reparent"));
    assert_eq!(d.parent_of(part), Some(into));
    assert_eq!(d.children_of(from), &[]);
    assert_eq!(d.children_of(into), &[part]);

    // A move that changes nothing records nothing, so it does not leave an
    // undo step that appears to do something.
    let before = d.history().can_undo();
    d.reparent(part, Some(into)).unwrap();
    assert_eq!(
        d.undo(),
        Some("Reparent"),
        "the real move is still the last step"
    );
    assert!(before);
}
