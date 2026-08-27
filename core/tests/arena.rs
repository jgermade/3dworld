//! The generation is the whole point; most of this file is about stale handles.

use w3d_core::Arena;

#[derive(Debug, PartialEq)]
struct Thing(u32);

#[test]
fn a_removed_handle_never_names_anything_again() {
    let mut arena = Arena::new();
    let first = arena.insert(Thing(1));
    arena.remove(first);

    // The slot is *not* reused: undo may still want to put the old occupant
    // back into it, and a free list is how that quietly stops working.
    let second = arena.insert(Thing(2));
    assert_ne!(first.index(), second.index(), "the slot must not be reused");
    assert_eq!(arena.get(first), None);
    assert_eq!(arena.get(second), Some(&Thing(2)));
}

#[test]
fn deleted_slots_are_kept_and_the_cost_is_visible() {
    let mut arena = Arena::new();
    for i in 0..4 {
        let id = arena.insert(Thing(i));
        arena.remove(id);
    }
    assert_eq!(arena.len(), 0);
    assert_eq!(
        arena.slot_count(),
        4,
        "this is the price, and it is on purpose"
    );
}

#[test]
fn removing_twice_is_not_an_accident_waiting_to_happen() {
    let mut arena = Arena::new();
    let id = arena.insert(Thing(1));
    assert_eq!(arena.remove(id), Some(Thing(1)));
    assert_eq!(arena.remove(id), None);
}

#[test]
fn iteration_is_in_index_order_and_skips_holes() {
    let mut arena = Arena::new();
    let ids: Vec<_> = (0..5).map(|i| arena.insert(Thing(i))).collect();
    arena.remove(ids[1]);
    arena.remove(ids[3]);

    let seen: Vec<u32> = arena.iter().map(|(_, t)| t.0).collect();
    assert_eq!(seen, vec![0, 2, 4]);
    assert_eq!(arena.len(), 3);
}

#[test]
fn compaction_removes_tombstones_and_reindexes_surviving_items() {
    let mut arena = Arena::new();
    let ids: Vec<_> = (0..5).map(|i| arena.insert(Thing(i))).collect();
    arena.remove(ids[1]);
    arena.remove(ids[3]);

    assert_eq!(arena.slot_count(), 5);
    assert_eq!(arena.len(), 3);

    let id_map = arena.compact();

    assert_eq!(arena.slot_count(), 3);
    assert_eq!(arena.len(), 3);

    // Old IDs [0, 2, 4] mapped to new contiguous indices [0, 1, 2]
    let new_id0 = id_map.get(&ids[0]).copied().unwrap();
    let new_id2 = id_map.get(&ids[2]).copied().unwrap();
    let new_id4 = id_map.get(&ids[4]).copied().unwrap();

    assert_eq!(new_id0.index(), 0);
    assert_eq!(new_id2.index(), 1);
    assert_eq!(new_id4.index(), 2);

    assert_eq!(arena.get(new_id0), Some(&Thing(0)));
    assert_eq!(arena.get(new_id2), Some(&Thing(2)));
    assert_eq!(arena.get(new_id4), Some(&Thing(4)));
}
