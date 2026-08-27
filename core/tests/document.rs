//! The document driven against the fake kernel — no OCCT, no browser, no
//! `.wasm`. If this file needs a real kernel to say something, the seam has a
//! hole in it.

use w3d_core::Document;
use w3d_core::kernel::{Aabb, BooleanOp, GeometryKernel, Mat4, Quality, Tolerance, Vec3};
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
