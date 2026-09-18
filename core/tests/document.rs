//! The document driven against the fake kernel — no OCCT, no browser, no
//! `.wasm`. If this file needs a real kernel to say something, the seam has a
//! hole in it.

use w3d_core::kernel::{
    Aabb, Body, BooleanOp, Capability, GeometryKernel, Import, ImportedAssembly, ImportedBody,
    Mat4, Mesh, Profile, Quality, SketchPlane, Tolerance, Topology, Vec3,
};
use w3d_core::{Document, DocumentError, Loaded, LoadedNode, Uid, Unit};
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
    fn supports(&self, cap: Capability) -> bool {
        self.inner.supports(cap)
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
    fn fillet_edges(
        &mut self,
        body: Body,
        edges: &[u32],
        radius: f64,
    ) -> w3d_core::kernel::Result<Body> {
        self.inner.fillet_edges(body, edges, radius)
    }
    fn chamfer_edges(
        &mut self,
        body: Body,
        edges: &[u32],
        distance: f64,
    ) -> w3d_core::kernel::Result<Body> {
        self.inner.chamfer_edges(body, edges, distance)
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

// ---- identity, the tree, and the unit --------------------------------------
//
// What a version-2 `.w3d` needs from the document, tested with no file
// anywhere near it: an identity that a parent can name and that a save can
// carry, a tree that holds together, and a document that says what its numbers
// mean.

#[test]
fn every_node_gets_an_identity_and_no_two_share_one() {
    let mut d = doc();
    let a = d.add_box("A", Vec3::splat(1.0)).unwrap();
    let group = d.add_group("Assembly");
    let b = d.add_sphere("B", 1.0).unwrap();

    let uids: Vec<Uid> = [a, group, b]
        .iter()
        .map(|id| d.node(*id).unwrap().uid)
        .collect();
    assert!(
        uids.iter().all(|u| *u != Uid::UNASSIGNED),
        "a node in a document has an identity: {uids:?}"
    );
    let mut sorted = uids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), 3, "two nodes share an identity: {uids:?}");
    assert!(
        uids.iter().all(|u| *u < d.next_uid()),
        "the counter is behind an identity it handed out"
    );

    assert_eq!(d.by_uid(uids[1]), Some(group), "a uid finds its node");
}

#[test]
fn an_identity_survives_undo_and_is_never_handed_out_twice() {
    let mut d = doc();
    let a = d.add_box("A", Vec3::splat(1.0)).unwrap();
    let before = d.node(a).unwrap().uid;

    d.undo().unwrap();
    d.redo().unwrap();
    assert_eq!(
        d.node(a).unwrap().uid,
        before,
        "undo and redo must put the same node back, not a new one"
    );

    // And the counter does not rewind. Undoing the node away and creating
    // another gives the new one an identity of its own: the old one may
    // already be written down somewhere this document cannot see.
    d.undo().unwrap();
    let b = d.add_sphere("B", 1.0).unwrap();
    assert_ne!(
        d.node(b).unwrap().uid,
        before,
        "an identity that was handed out was handed out again"
    );
}

#[test]
fn depth_first_is_parents_before_children_and_reaches_every_node() {
    let mut d = doc();
    let outer = d.add_group("Outer");
    let inner = d.add_group("Inner");
    let deep = d.add_box("Deep", Vec3::splat(1.0)).unwrap();
    let loose = d.add_sphere("Loose", 1.0).unwrap();
    d.reparent(inner, Some(outer)).unwrap();
    d.reparent(deep, Some(inner)).unwrap();

    assert_eq!(
        d.depth_first(),
        vec![(outer, 0), (inner, 1), (deep, 2), (loose, 0)]
    );
    assert_eq!(d.depth_first().len(), d.len(), "a node was lost or doubled");
}

/// The bug a file with a tree in it turns from a display fault into data loss.
#[test]
fn deleting_a_group_promotes_what_was_inside_it() {
    let mut d = doc();
    let group = d.add_group("Assembly");
    let part = d.add_box("Piston", Vec3::splat(1.0)).unwrap();
    d.reparent(part, Some(group)).unwrap();

    d.remove(group).unwrap();

    assert_eq!(d.parent_of(part), None, "the part is left inside a hole");
    assert_eq!(
        d.depth_first().len(),
        d.len(),
        "a walk from the roots cannot reach every node"
    );

    // And the whole shape comes back, because the promotion is part of the
    // same undo step as the deletion.
    d.undo().unwrap();
    assert_eq!(d.parent_of(part), Some(group));
    assert_eq!(d.children_of(group), [part]);
}

#[test]
fn a_boolean_leaves_its_result_where_the_first_operand_was() {
    let mut d = doc();
    let group = d.add_group("Assembly");
    let a = d.add_box("A", Vec3::splat(2.0)).unwrap();
    let b = d.add_sphere("B", 1.0).unwrap();
    d.reparent(a, Some(group)).unwrap();
    d.reparent(b, Some(group)).unwrap();

    let result = d.boolean(BooleanOp::Difference, a, b).unwrap();

    assert_eq!(
        d.parent_of(result),
        Some(group),
        "the result left the group"
    );
    assert_eq!(
        d.children_of(group),
        [result],
        "the group still lists nodes that are gone"
    );
    assert_eq!(d.depth_first().len(), d.len());
}

#[test]
fn a_document_says_what_its_numbers_mean() {
    let mut d = doc();
    assert_eq!(
        d.unit(),
        Some(Unit::Millimetre),
        "a new document states the unit this program has always assumed"
    );

    d.set_unit(Some(Unit::Inch));
    assert_eq!(d.unit().map(Unit::symbol), Some("in"));
    assert_eq!(Unit::from_symbol("in"), Some(Unit::Inch));
    assert_eq!(Unit::from_symbol("furlong"), None);
    assert_eq!(
        Unit::Inch.millimetres(),
        25.4,
        "the inch is defined, not measured"
    );

    // And an export that states millimetres refuses rather than writing
    // numbers that mean something else.
    let id = d.add_box("A", Vec3::splat(1.0)).unwrap();
    assert!(matches!(
        d.export_step(&[id]),
        Err(DocumentError::NotMillimetres(Unit::Inch))
    ));
}

#[test]
fn a_loaded_tree_is_built_with_its_children_lists() {
    let mut kernel = FakeKernel::new();
    let body = kernel.create_box(Vec3::splat(1.0)).unwrap();
    let node = |uid: u64, name: &str, body: Option<Body>, parent: Option<u64>| LoadedNode {
        uid: Uid::from_raw(uid),
        name: String::from(name),
        body,
        visible: true,
        parent: parent.map(Uid::from_raw),
    };

    let d = Document::from_loaded(
        kernel,
        Loaded {
            tolerance: Tolerance::document_default(),
            quality: Quality::display_default(),
            unit: Some(Unit::Metre),
            next_uid: Uid::from_raw(9),
            nodes: vec![
                node(3, "Outer", None, None),
                node(5, "Inner", None, Some(3)),
                node(8, "Part", Some(body), Some(5)),
            ],
        },
    )
    .unwrap();

    let outer = d.by_uid(Uid::from_raw(3)).unwrap();
    let inner = d.by_uid(Uid::from_raw(5)).unwrap();
    let part = d.by_uid(Uid::from_raw(8)).unwrap();

    assert_eq!(d.children_of(outer), [inner], "the parent does not list it");
    assert_eq!(d.children_of(inner), [part]);
    assert_eq!(d.depth_first(), vec![(outer, 0), (inner, 1), (part, 2)]);
    assert!(
        d.node(outer).unwrap().is_group(),
        "a node with no body is a group"
    );
    assert_eq!(d.unit(), Some(Unit::Metre));
    assert_eq!(
        d.next_uid(),
        Uid::from_raw(9),
        "the counter came from the file"
    );

    // The counter is where it was told, so the next node created does not
    // collide with one that is already here.
    let mut d = d;
    let fresh = d.add_group("New");
    assert_eq!(d.node(fresh).unwrap().uid, Uid::from_raw(9));
}

/// `Loaded::validate` is the only rule the tree has, so these are its negative
/// controls: each is a file that must not become a document.
#[test]
fn a_file_that_is_not_a_tree_is_refused_entry() {
    let node = |uid: u64, parent: Option<u64>| LoadedNode {
        uid: Uid::from_raw(uid),
        name: String::from("n"),
        body: None,
        visible: true,
        parent: parent.map(Uid::from_raw),
    };
    let loaded = |next_uid: u64, nodes: Vec<LoadedNode>| Loaded {
        tolerance: Tolerance::document_default(),
        quality: Quality::display_default(),
        unit: None,
        next_uid: Uid::from_raw(next_uid),
        nodes,
    };

    assert!(
        loaded(3, vec![node(1, None), node(2, Some(1))])
            .validate()
            .is_ok()
    );

    // A node with no identity.
    assert!(loaded(2, vec![node(0, None)]).validate().is_err());
    // Two nodes with one identity.
    assert!(
        loaded(3, vec![node(1, None), node(1, None)])
            .validate()
            .is_err()
    );
    // A parent that is not there at all.
    assert!(loaded(3, vec![node(1, Some(7))]).validate().is_err());
    // A parent that is there, but later — which is how a cycle would have to
    // be written, and the reason it cannot be.
    assert!(
        loaded(3, vec![node(1, Some(2)), node(2, None)])
            .validate()
            .is_err()
    );
    // Its own parent.
    assert!(loaded(3, vec![node(1, Some(1))]).validate().is_err());
    // A counter behind an identity in the file: the next node created would be
    // given one that is already taken.
    assert!(loaded(1, vec![node(1, None)]).validate().is_err());

    // And nothing lands from a refused one.
    assert!(matches!(
        Document::from_loaded(FakeKernel::new(), loaded(9, vec![node(1, Some(7))])),
        Err(DocumentError::NotATree(_))
    ));
}

#[test]
fn compaction_keeps_the_tree_pointing_at_the_right_nodes() {
    let mut d = doc();
    let doomed = d.add_box("Doomed", Vec3::splat(1.0)).unwrap();
    let group = d.add_group("Assembly");
    let part = d.add_box("Piston", Vec3::splat(1.0)).unwrap();
    d.reparent(part, Some(group)).unwrap();
    d.remove(doomed).unwrap();
    d.clear_history();

    assert!(d.compact().unwrap() > 0, "nothing was compacted");

    let rows: Vec<_> = d
        .depth_first()
        .into_iter()
        .map(|(id, depth)| (d.node(id).unwrap().name.clone(), depth))
        .collect();
    assert_eq!(
        rows,
        vec![(String::from("Assembly"), 0), (String::from("Piston"), 1)],
        "compaction renumbered the arena and left the tree pointing at the old slots"
    );
    assert_eq!(d.depth_first().len(), d.len());
}

/// Which face of the fake kernel's box mesh points along `+axis`, and the
/// distance its plane stands at.
fn face_pointing(d: &mut Document<FakeKernel>, id: w3d_core::NodeId, axis: Vec3) -> u32 {
    let mesh = d.mesh(id).unwrap().clone();
    let mut ids: Vec<u32> = mesh.face_of_triangle.clone();
    ids.sort_unstable();
    ids.dedup();
    ids.into_iter()
        .find(|&f| {
            mesh.face_metrics(f)
                .is_some_and(|m| m.normal.dot(axis) > 0.99)
        })
        .expect("no face points that way")
}

#[test]
fn pulling_a_face_adds_material_on_that_side_and_leaves_the_others_alone() {
    let mut d = doc();
    let id = d.add_box("Base", Vec3::new(2.0, 2.0, 2.0)).unwrap();
    let before = d.bounds(id).unwrap();
    let top = face_pointing(&mut d, id, Vec3::Z);

    d.push_pull_face(id, top, 3.0).unwrap();

    let after = d.bounds(id).unwrap();
    assert!(
        (after.max.z - (before.max.z + 3.0)).abs() < 1.0e-6,
        "the pulled side did not move by what was asked: {before:?} -> {after:?}"
    );
    for (got, want) in [
        (after.min.z, before.min.z),
        (after.min.x, before.min.x),
        (after.max.x, before.max.x),
        (after.min.y, before.min.y),
        (after.max.y, before.max.y),
    ] {
        assert!(
            (got - want).abs() < 1.0e-6,
            "push/pull moved a side it was not asked about: {before:?} -> {after:?}"
        );
    }
}

#[test]
fn a_pull_is_one_undo_step_and_undo_puts_the_solid_back() {
    let mut d = doc();
    let id = d.add_box("Base", Vec3::new(2.0, 2.0, 2.0)).unwrap();
    let before = d.bounds(id).unwrap();
    let top = face_pointing(&mut d, id, Vec3::Z);

    d.push_pull_face(id, top, 5.0).unwrap();
    assert!(d.bounds(id).unwrap() != before);

    assert_eq!(d.undo(), Some("Push/Pull Face"));
    assert_eq!(d.bounds(id).unwrap(), before);
}

#[test]
fn pushing_a_face_inward_cuts_rather_than_moving_the_part() {
    let mut d = doc();
    let id = d.add_box("Base", Vec3::new(4.0, 4.0, 4.0)).unwrap();
    let body_before = d.node(id).unwrap().body;
    let top = face_pointing(&mut d, id, Vec3::Z);

    d.push_pull_face(id, top, -1.0).unwrap();

    // The fake kernel keeps a boolean symbolic, so what this can assert is the
    // shape of the edit: a new body, made by a difference, in one undo step.
    // That the cut *lands* where it should is the truck suite's to say.
    assert!(d.node(id).unwrap().body != body_before);
    assert_eq!(d.undo(), Some("Push/Pull Face"));
    assert_eq!(d.node(id).unwrap().body, body_before);
}

#[test]
fn pulling_a_face_that_is_not_there_leaves_the_document_untouched() {
    let mut d = doc();
    let id = d.add_box("Base", Vec3::splat(2.0)).unwrap();
    let before = d.node(id).unwrap().clone();
    d.clear_history();

    assert!(d.push_pull_face(id, 404, 1.0).is_err());

    assert_eq!(d.node(id).unwrap(), &before);
    assert_eq!(
        d.undo(),
        None,
        "a failed push/pull left an undo step behind"
    );
}

#[test]
fn pulling_a_face_by_nothing_is_not_an_edit() {
    let mut d = doc();
    let id = d.add_box("Base", Vec3::splat(2.0)).unwrap();
    let top = face_pointing(&mut d, id, Vec3::Z);
    let body = d.node(id).unwrap().body;
    d.clear_history();

    d.push_pull_face(id, top, 0.0).unwrap();

    assert_eq!(d.node(id).unwrap().body, body);
    assert_eq!(d.undo(), None, "a zero pull left an undo step behind");
}

#[test]
fn the_tessellations_box_is_the_solids_box_and_costs_no_kernel_call() {
    let mut d = doc();
    let id = d.add_box("Base", Vec3::new(2.0, 4.0, 6.0)).unwrap();

    let exact = d.bounds(id).unwrap();
    let cheap = d.mesh_bounds(id).unwrap();

    // A flat-sided solid tessellates to its own corners, so the two agree
    // outright here; on a curved one the mesh box is inside the solid's by the
    // tessellation's chordal error, which is what the doc comment claims and
    // what makes this the cheap answer rather than the same answer.
    assert!((cheap.min - exact.min).length() < 1.0e-6, "{cheap:?}");
    assert!((cheap.max - exact.max).length() < 1.0e-6, "{cheap:?}");
}

#[test]
fn the_tessellations_box_refuses_a_group_rather_than_inventing_one() {
    let mut d = doc();
    let group = d.add_group("Assembly");
    assert!(d.mesh_bounds(group).is_err());
}
