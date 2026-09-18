//! What the build says it can do, and what the controls above it then offer.
//!
//! `w3d-kernel`'s conformance suite already holds `supports` to the *kernel* —
//! every capability probed, in both directions, on every backend. This file
//! asks the question one layer up, which that suite cannot: does the answer
//! reach the thing a user touches? A query nothing consults is a query, and
//! register item 8 was about a Ribbon that offered three operations the default
//! build has never had.
//!
//! Like `blend_edges.rs` and `push_pull.rs` it runs against whichever backend
//! the build has, and pins the two apart only where they genuinely differ.
#![cfg(any(feature = "truck", feature = "occt"))]

use w3d_app::editor::{Command, Editor};
use w3d_core::kernel::Capability;

#[cfg(feature = "occt")]
use w3d_kernel_occt::OcctKernel as Kernel;
#[cfg(all(feature = "truck", not(feature = "occt")))]
use w3d_kernel_truck::TruckKernel as Kernel;

/// An editor with one box in it, and that box selected.
fn with_a_box() -> Editor<Kernel> {
    let mut editor = Editor::new(Kernel::default());
    editor
        .try_run(Command::AddBox)
        .expect("a box every backend can make");
    editor
}

/// The two backends' capability answers, written out rather than derived.
///
/// A test that computed the expected answers from `supports` would assert
/// nothing. These are the table from the record, and a build that changes one
/// of them has to come here and say so — which is the point: the day `truck`
/// grows a rolling-ball surface, this test fails, and that failure is the
/// reminder to go and re-enable the Ribbon's buttons.
#[test]
fn the_backend_answers_for_itself_and_the_answers_are_the_ones_recorded() {
    let editor = with_a_box();

    let blends = cfg!(feature = "occt");
    assert_eq!(editor.supports(Capability::Blend), blends);
    assert_eq!(editor.supports(Capability::Shell), blends);
    assert_eq!(editor.supports(Capability::BentSweep), blends);
    assert_eq!(editor.supports(Capability::MultiSectionLoft), blends);
    assert_eq!(editor.supports(Capability::StepImport), blends);
    assert_eq!(editor.supports(Capability::StepExport), blends);

    // The one both real backends have, and the reason it is a capability of
    // its own: `truck` cannot blend an edge and can still name one.
    assert!(
        editor.supports(Capability::EdgeIdentity),
        "both real backends number their edges; only the fake does not"
    );
}

/// The claim the Ribbon rests on: a `false` here is an operation that refuses,
/// every time, on any solid.
///
/// This is the assertion that makes greying a button out honest rather than
/// merely cautious. The conformance probe made it about a box it built itself;
/// this makes it about the document's own solid, through the editor's own
/// commands — the exact path the button takes.
#[test]
fn an_operation_the_build_denies_is_one_the_editor_refuses() {
    for (cap, command) in [
        (Capability::Blend, Command::Fillet),
        (Capability::Blend, Command::Chamfer),
        (Capability::Shell, Command::Shell(1.5)),
        (Capability::BentSweep, Command::AddSweep),
    ] {
        // A fresh document per case, and the first draft of this test did not
        // have one. Sharing it ran `Chamfer` on the solid `Fillet` had just
        // rounded, and OpenCASCADE answered "there are no suitable edges" —
        // which is true, and is the operation reporting on *that solid*.
        // Nothing here is about the previous command's output, so each case
        // gets the box the Ribbon's button would be pressed on.
        let mut editor = with_a_box();
        let outcome = editor.try_run(command.clone());
        if editor.supports(cap) {
            assert!(
                outcome.is_ok(),
                "the build says it does {cap} and {command:?} failed: {outcome:?}"
            );
        } else {
            assert!(
                outcome.is_err(),
                "the build says it does not do {cap} and {command:?} succeeded anyway; \
                 the Ribbon greys that button out, so a user cannot reach a path that works"
            );
        }
    }
}

/// A loft of two sections is offered on both builds, and works on both.
///
/// The negative control for the *naming* of the capabilities. `truck` declines
/// a loft of three sections, so a query called "Loft" would be `false` for it
/// and the Ribbon's button — which lofts two — would be greyed out although it
/// works. Capabilities are named for the case that is actually declined, and
/// this is what that is worth.
#[test]
fn a_two_section_loft_is_not_gated_by_the_multi_section_capability() {
    let mut editor = with_a_box();
    let outcome = editor.try_run(Command::AddLoft);
    assert!(
        outcome.is_ok(),
        "the Ribbon's Loft joins two profiles, which every backend does: {outcome:?}"
    );
    assert_eq!(
        editor.supports(Capability::MultiSectionLoft),
        cfg!(feature = "occt"),
        "and the capability is still false on the backend that declines a third section"
    );
}

/// The refusal names the build, not the model.
///
/// Two different mistakes, two different sentences — the rule the seam states
/// for `Unsupported` against `Degenerate`, checked where a user actually reads
/// it. "That radius is too big for that solid" sends someone back to the model;
/// "this build cannot round an edge" sends them to another build, and a user
/// given the first when the second is true will spend the afternoon on their
/// geometry.
#[test]
fn the_refusal_sends_a_user_to_the_right_place() {
    let mut editor = with_a_box();
    if editor.supports(Capability::Blend) {
        return;
    }
    let message = editor
        .try_run(Command::Fillet)
        .expect_err("a build with no rolling-ball surface refuses");
    let message = message.to_lowercase();
    assert!(
        message.contains("motor") || message.contains("backend") || message.contains("build"),
        "the refusal should name the build; it said {message:?}"
    );
}
