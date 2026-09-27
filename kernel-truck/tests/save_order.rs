//! `save_body` writes one file per solid, whatever order the topology holds it
//! in.
//!
//! The failure this is about cannot be reproduced inside one process: a
//! boolean's faces come back in the order of a map keyed by address, so two
//! saves in the *same* process always agreed, and two processes did not. A test
//! that waits for the allocator to shuffle something is a test that passes
//! whenever the allocator is kind. So the shuffle is done here, on purpose: the
//! saved cut is taken apart, every order in it that is not a fact about the
//! solid is changed, and it is loaded again. A writer that depends on any of
//! those orders writes a different file.

mod common;

use common::{cut, scramble};
use w3d_kernel::GeometryKernel;
use w3d_kernel_truck::TruckKernel;

#[test]
fn a_scrambled_solid_saves_to_the_same_bytes() {
    let mut k = TruckKernel::new();
    let body = cut(&mut k);
    let saved = k.save_body(body).expect("save");

    let scrambled = scramble(&saved);
    // The control: without it, a `scramble` that changed nothing would make
    // this test pass on any writer at all.
    assert_ne!(scrambled, saved, "the scramble changed nothing");

    let reloaded = k.load_body(&scrambled).expect("the scrambled solid loads");
    assert_eq!(
        String::from_utf8(k.save_body(reloaded).expect("save again")).unwrap(),
        String::from_utf8(saved).unwrap(),
        "the file depends on the order the topology was held in"
    );
}

#[test]
fn a_save_of_a_loaded_solid_is_the_file_it_was_loaded_from() {
    let mut k = TruckKernel::new();
    let body = cut(&mut k);
    let saved = k.save_body(body).expect("save");
    let reloaded = k.load_body(&saved).expect("load");
    assert_eq!(k.save_body(reloaded).expect("save again"), saved);
}

#[test]
fn a_scrambled_solid_is_still_the_solid() {
    // Canonical order is only worth having if it is the *same* solid: nine
    // faces, and a mesh of the same size, before and after.
    let mut k = TruckKernel::new();
    let body = cut(&mut k);
    let reloaded = k.load_body(&scramble(&k.save_body(body).unwrap())).unwrap();
    let (a, b) = (k.topology(body).unwrap(), k.topology(reloaded).unwrap());
    assert_eq!(
        (a.solids, a.faces, a.edges, a.vertices),
        (b.solids, b.faces, b.edges, b.vertices)
    );
    let q = w3d_kernel::Quality::display_default();
    assert_eq!(
        k.tessellate(body, q).unwrap().triangle_count(),
        k.tessellate(reloaded, q).unwrap().triangle_count()
    );
}
