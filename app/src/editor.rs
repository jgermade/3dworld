//! Everything the modeller does, with no window in it.
//!
//! The split is deliberate and it is what makes this crate testable: a winit
//! event loop cannot be driven from `cargo test`, and a state machine can. So
//! the rules that are easy to get wrong — a drag is not a click, a boolean
//! needs exactly two operands, undo must not leave a deleted node selected —
//! live here and are asserted here. The shell translates events into
//! [`Input`] and [`Command`] and does what [`Reaction`] tells it.

use std::path::{Path, PathBuf};

use w3d_core::kernel::{BooleanOp, GeometryKernel, Vec3};
use w3d_core::{Document, NodeId};
use w3d_render::{Camera, Pick};

/// A semantic action. The shell decides which key or button produces one; this
/// crate decides what it means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    AddBox,
    AddSphere,
    AddCylinder,
    Boolean(BooleanOp),
    DeleteSelected,
    Undo,
    Redo,
    ClearSelection,
    SelectAll,
    ZoomToFit,
    /// Writes to the path the document was last saved to or opened from.
    /// Without one, the shell has to ask — which it does by treating this as
    /// `SaveAs` with a default name.
    Save,
    /// Writes the selection — or everything, when nothing is selected — as a
    /// STEP file beside the document.
    ///
    /// There is deliberately no `ImportStep` here. Exporting can invent a
    /// name from the document's own; importing needs a file that already
    /// exists and there is no file dialogue to name one with, so import is a
    /// command-line option until there is. A menu item that cannot be
    /// clicked is worse than one that is not there.
    ExportStep,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    /// Orbits, and selects when it did not move.
    Left,
    /// Pans.
    Middle,
}

/// Pointer events, in physical pixels, before any interpretation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Input {
    Down {
        x: f64,
        y: f64,
        button: Button,
        additive: bool,
    },
    Move {
        x: f64,
        y: f64,
    },
    Up {
        button: Button,
    },
    Scroll(f64),
}

/// What the shell must do about an input. Everything that needs a GPU is a
/// [`Reaction::Pick`]; the editor cannot do it and does not pretend to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reaction {
    Nothing,
    Redraw,
    /// Read the ids at this pixel and hand the answer back to
    /// [`Editor::picked`].
    Pick {
        x: u32,
        y: u32,
        additive: bool,
    },
}

/// Below this, a press-and-release is a click. Above it, it was a drag and
/// selecting whatever the cursor happened to stop over would be wrong — which
/// is the single most annoying bug a modeller can have, because it fires on
/// every rotation.
const DRAG_SLOP: f64 = 3.0;

struct Drag {
    button: Button,
    start: (f64, f64),
    last: (f64, f64),
    moved: bool,
    additive: bool,
}

pub struct Editor<K: GeometryKernel> {
    doc: Document<K>,
    /// Where this document came from and where `Save` writes. `None` for one
    /// that has never been saved.
    path: Option<PathBuf>,
    camera: Camera,
    drag: Option<Drag>,
    viewport: (u32, u32),
    /// The last thing that happened, for the status line. A modeller that
    /// refuses an operation silently is a modeller people think is broken.
    status: String,
}

impl<K: GeometryKernel> Editor<K> {
    pub fn new(kernel: K) -> Self {
        Self {
            doc: Document::new(kernel),
            path: None,
            camera: Camera::default(),
            drag: None,
            viewport: (1, 1),
            status: String::from("ready"),
        }
    }

    pub fn document(&self) -> &Document<K> {
        &self.doc
    }

    pub fn document_mut(&mut self) -> &mut Document<K> {
        &mut self.doc
    }

    pub fn camera(&self) -> &Camera {
        &self.camera
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn set_viewport(&mut self, width: u32, height: u32) {
        self.viewport = (width.max(1), height.max(1));
    }

    pub fn selection(&self) -> Vec<NodeId> {
        self.doc.selection().collect()
    }

    // ---- input --------------------------------------------------------

    pub fn input(&mut self, event: Input) -> Reaction {
        match event {
            Input::Down {
                x,
                y,
                button,
                additive,
            } => {
                self.drag = Some(Drag {
                    button,
                    start: (x, y),
                    last: (x, y),
                    moved: false,
                    additive,
                });
                Reaction::Nothing
            }
            Input::Move { x, y } => {
                let height = f64::from(self.viewport.1);
                let Some(drag) = &mut self.drag else {
                    return Reaction::Nothing;
                };
                let (dx, dy) = (x - drag.last.0, y - drag.last.1);
                drag.last = (x, y);
                if (x - drag.start.0).abs() + (y - drag.start.1).abs() > DRAG_SLOP {
                    drag.moved = true;
                }
                match drag.button {
                    // Dragging right turns the model right, which means the
                    // camera goes the other way.
                    Button::Left => self.camera.orbit(-dx * 0.01, dy * 0.01),
                    Button::Middle => self.camera.pan(dx, dy, height),
                }
                Reaction::Redraw
            }
            Input::Up { button } => {
                let Some(drag) = self.drag.take() else {
                    return Reaction::Nothing;
                };
                if drag.button != button || drag.moved || button != Button::Left {
                    return Reaction::Nothing;
                }
                Reaction::Pick {
                    x: drag.start.0.max(0.0) as u32,
                    y: drag.start.1.max(0.0) as u32,
                    additive: drag.additive,
                }
            }
            Input::Scroll(delta) => {
                self.camera.dolly((delta * 0.1).exp());
                Reaction::Redraw
            }
        }
    }

    /// The answer to a [`Reaction::Pick`].
    ///
    /// `object` is the node's arena index, which is what the shell hands the
    /// renderer as an object id. Resolving it back can fail — a node can be
    /// deleted between the click and the readback, which is a frame or two —
    /// and that is a miss, not a panic.
    pub fn picked(&mut self, pick: Pick, additive: bool) {
        let Some((object, face)) = pick.hit() else {
            if !additive {
                self.doc.clear_selection();
                self.status = String::from("nothing");
            }
            return;
        };
        let Some(id) = self.node_at(object) else {
            self.status = String::from("clicked a body that is no longer there");
            return;
        };
        if !additive {
            self.doc.clear_selection();
        }
        if self.doc.is_selected(id) {
            self.doc.deselect(id);
            self.status = format!("deselected {}", self.name_of(id));
        } else {
            let _ = self.doc.select(id);
            self.status = format!("{} · face {face}", self.name_of(id));
        }
    }

    fn node_at(&self, index: u32) -> Option<NodeId> {
        self.doc
            .nodes()
            .map(|(id, _)| id)
            .find(|id| id.index() == index)
    }

    fn name_of(&self, id: NodeId) -> String {
        self.doc
            .node(id)
            .map(|n| n.name.clone())
            .unwrap_or_else(|_| String::from("?"))
    }

    // ---- commands -----------------------------------------------------

    pub fn run(&mut self, command: Command) {
        let outcome = match command {
            Command::AddBox => self.add("Box", |doc, name| {
                doc.add_box(name, Vec3::new(20.0, 20.0, 20.0))
            }),
            Command::AddSphere => self.add("Sphere", |doc, name| doc.add_sphere(name, 12.0)),
            Command::AddCylinder => {
                self.add("Cylinder", |doc, name| doc.add_cylinder(name, 8.0, 24.0))
            }
            Command::Boolean(op) => self.boolean(op),
            Command::DeleteSelected => self.delete_selected(),
            Command::Undo => Ok(match self.doc.undo() {
                Some(label) => format!("undid {label}"),
                None => String::from("nothing to undo"),
            }),
            Command::Redo => Ok(match self.doc.redo() {
                Some(label) => format!("redid {label}"),
                None => String::from("nothing to redo"),
            }),
            Command::ClearSelection => {
                self.doc.clear_selection();
                Ok(String::from("selection cleared"))
            }
            Command::SelectAll => {
                let ids: Vec<_> = self.doc.nodes().map(|(id, _)| id).collect();
                for id in &ids {
                    let _ = self.doc.select(*id);
                }
                Ok(format!("selected {}", ids.len()))
            }
            Command::Save => self.save(None),
            Command::ExportStep => self.export_step(None),
            Command::ZoomToFit => {
                let bounds = self.doc.visible_bounds();
                if bounds.is_empty() {
                    Ok(String::from("nothing to frame"))
                } else {
                    self.camera.fit(&bounds);
                    Ok(String::from("framed"))
                }
            }
        };
        self.status = match outcome {
            Ok(message) => message,
            Err(message) => message,
        };
    }

    // ---- files --------------------------------------------------------

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Saves to `path`, or to wherever this document already lives.
    ///
    /// The path is remembered on success only. A failed save must not make the
    /// next `Save` write somewhere the last one did not reach.
    pub fn save(&mut self, path: Option<PathBuf>) -> Result<String, String> {
        let target = match path.or_else(|| self.path.clone()) {
            Some(path) => path,
            None => PathBuf::from("untitled.w3d"),
        };
        let bytes = w3d_format::save(&self.doc).map_err(|e| format!("could not save: {e}"))?;
        std::fs::write(&target, bytes)
            .map_err(|e| format!("could not write {}: {e}", target.display()))?;
        let message = format!("saved {}", target.display());
        self.path = Some(target);
        self.status = message.clone();
        Ok(message)
    }

    /// Replaces the document with one read from `path`.
    ///
    /// Takes the kernel by argument because `w3d_format::load` builds a
    /// document *around* a kernel: the bodies in a file mean nothing to the
    /// kernel currently loaded, so opening is a replacement rather than a
    /// merge.
    pub fn open(&mut self, path: PathBuf, kernel: K) -> Result<String, String> {
        let bytes =
            std::fs::read(&path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
        let doc = w3d_format::load(kernel, &bytes).map_err(|e| e.to_string())?;
        self.doc = doc;
        self.drag = None;
        self.camera = Camera::default();
        self.camera.fit(&self.doc.visible_bounds());
        let bodies = self.doc.len();
        let message = format!(
            "opened {} — {bodies} {}",
            path.display(),
            if bodies == 1 { "body" } else { "bodies" }
        );
        self.path = Some(path);
        self.status = message.clone();
        Ok(message)
    }

    /// Writes STEP to `path`, or beside the document, or to `untitled.step`.
    ///
    /// The selection, or the whole document when nothing is selected — the
    /// same rule every other program in the list this is meant to reach uses,
    /// and the one a user has already learned.
    ///
    /// **The path is not remembered.** A `.w3d` is where this document lives
    /// and a `.step` is a copy that left; making `Save` follow an export
    /// would silently move the document into a format that cannot hold it.
    pub fn export_step(&mut self, path: Option<PathBuf>) -> Result<String, String> {
        let selected = self.selection();
        let ids = if selected.is_empty() {
            self.doc.nodes().map(|(id, _)| id).collect()
        } else {
            selected
        };
        if ids.is_empty() {
            return Err(String::from("nothing to export"));
        }
        let target = path.unwrap_or_else(|| match &self.path {
            Some(path) => path.with_extension("step"),
            None => PathBuf::from("untitled.step"),
        });
        let bytes = self
            .doc
            .export_step(&ids)
            .map_err(|e| format!("could not export STEP: {e}"))?;
        std::fs::write(&target, bytes)
            .map_err(|e| format!("could not write {}: {e}", target.display()))?;
        let message = format!(
            "exported {} {} to {}",
            ids.len(),
            if ids.len() == 1 { "body" } else { "bodies" },
            target.display()
        );
        self.status = message.clone();
        Ok(message)
    }

    /// Adds the solids in a STEP file to this document.
    ///
    /// Unlike [`Editor::open`], which replaces the document around a kernel,
    /// this appends — a STEP file carries solids, not a document, so there is
    /// nothing in it to replace a document with.
    pub fn import_step(&mut self, path: PathBuf) -> Result<String, String> {
        let bytes =
            std::fs::read(&path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
        let name = path.file_stem().map_or_else(
            || String::from("Imported"),
            |s| s.to_string_lossy().into_owned(),
        );
        let was_empty = self.doc.is_empty();
        let ids = self
            .doc
            .import_step(&bytes, &name)
            .map_err(|e| format!("could not import {}: {e}", path.display()))?;
        self.doc.clear_selection();
        for id in &ids {
            let _ = self.doc.select(*id);
        }
        // Same reason adding the first primitive frames it: geometry that
        // arrives outside the camera's view is indistinguishable from an
        // import that did nothing.
        if was_empty {
            self.camera.fit(&self.doc.visible_bounds());
        }
        let message = format!(
            "imported {} {} from {}",
            ids.len(),
            if ids.len() == 1 { "body" } else { "bodies" },
            path.display()
        );
        self.status = message.clone();
        Ok(message)
    }

    fn add(
        &mut self,
        kind: &str,
        make: impl Fn(&mut Document<K>, String) -> w3d_core::document::Result<NodeId>,
    ) -> Result<String, String> {
        let name = format!("{kind} {}", self.doc.len() + 1);
        match make(&mut self.doc, name.clone()) {
            Ok(id) => {
                self.doc.clear_selection();
                let _ = self.doc.select(id);
                // The first solid in an empty document is invisible until the
                // camera knows where it is, and "I added a box and nothing
                // happened" is not a bug report anyone should have to file.
                if self.doc.len() == 1 {
                    self.camera.fit(&self.doc.visible_bounds());
                }
                Ok(format!("added {name}"))
            }
            Err(e) => Err(format!("could not add a {kind}: {e}")),
        }
    }

    fn boolean(&mut self, op: BooleanOp) -> Result<String, String> {
        let selected = self.selection();
        let [a, b] = selected[..] else {
            return Err(format!(
                "a boolean needs exactly two bodies selected, not {}",
                selected.len()
            ));
        };
        match self.doc.boolean(op, a, b) {
            Ok(id) => {
                self.doc.clear_selection();
                let _ = self.doc.select(id);
                Ok(format!("{} → {}", label(op), self.name_of(id)))
            }
            // A boolean is the operation most likely to fail on real geometry,
            // and the kernel's message is the only useful thing to say.
            Err(e) => Err(format!("{} failed: {e}", label(op))),
        }
    }

    fn delete_selected(&mut self) -> Result<String, String> {
        let selected = self.selection();
        if selected.is_empty() {
            return Err(String::from("nothing selected"));
        }
        for id in &selected {
            if let Err(e) = self.doc.remove(*id) {
                return Err(format!("could not delete: {e}"));
            }
        }
        self.doc.clear_selection();
        Ok(format!("deleted {}", selected.len()))
    }
}

fn label(op: BooleanOp) -> &'static str {
    match op {
        BooleanOp::Union => "union",
        BooleanOp::Difference => "difference",
        BooleanOp::Intersection => "intersection",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use w3d_kernel_fake::FakeKernel;
    use w3d_render::NOTHING;

    fn editor() -> Editor<FakeKernel> {
        let mut e = Editor::new(FakeKernel::default());
        e.set_viewport(800, 600);
        e
    }

    fn hit(object: u32) -> Pick {
        Pick { object, face: 0 }
    }

    #[test]
    fn a_drag_is_not_a_click() {
        let mut e = editor();
        e.run(Command::AddBox);

        e.input(Input::Down {
            x: 100.0,
            y: 100.0,
            button: Button::Left,
            additive: false,
        });
        assert_eq!(
            e.input(Input::Move { x: 140.0, y: 130.0 }),
            Reaction::Redraw
        );
        assert_eq!(
            e.input(Input::Up {
                button: Button::Left
            }),
            Reaction::Nothing,
            "a rotation must not select whatever the cursor stopped over"
        );
    }

    #[test]
    fn a_press_that_barely_moves_is_still_a_click() {
        let mut e = editor();
        e.input(Input::Down {
            x: 100.0,
            y: 100.0,
            button: Button::Left,
            additive: false,
        });
        // Inside the slop: a hand shakes, and a click is still a click.
        e.input(Input::Move { x: 101.0, y: 101.0 });
        assert_eq!(
            e.input(Input::Up {
                button: Button::Left
            }),
            Reaction::Pick {
                x: 100,
                y: 100,
                additive: false
            },
            "the pick is at the *press*, not at the release"
        );
    }

    #[test]
    fn dragging_with_the_middle_button_pans_and_never_selects() {
        let mut e = editor();
        let before = *e.camera();
        e.input(Input::Down {
            x: 10.0,
            y: 10.0,
            button: Button::Middle,
            additive: false,
        });
        e.input(Input::Move { x: 60.0, y: 10.0 });
        assert_ne!(e.camera().target, before.target);
        assert_eq!(e.camera().yaw, before.yaw, "panning is not orbiting");
        assert_eq!(
            e.input(Input::Up {
                button: Button::Middle
            }),
            Reaction::Nothing
        );
    }

    /// A plain click *replaces* the selection; only shift toggles. Written the
    /// other way round first — plain click deselecting what it lands on — which
    /// no modeller does and which makes clicking the thing you just selected
    /// throw it away.
    #[test]
    fn a_plain_click_replaces_the_selection_and_shift_toggles() {
        let mut e = editor();
        e.run(Command::AddBox);
        e.run(Command::AddSphere);
        let ids: Vec<_> = e.document().nodes().map(|(id, _)| id).collect();
        let (first, second) = (ids[0], ids[1]);
        e.document_mut().clear_selection();

        e.picked(hit(first.index()), false);
        assert_eq!(e.selection(), vec![first]);
        // Clicking it again keeps it. Anything else loses a selection to a
        // stray second click.
        e.picked(hit(first.index()), false);
        assert_eq!(e.selection(), vec![first]);
        // A plain click elsewhere replaces rather than adds.
        e.picked(hit(second.index()), false);
        assert_eq!(e.selection(), vec![second]);

        // Shift adds, and shift on something selected takes it away.
        e.picked(hit(first.index()), true);
        assert_eq!(e.selection().len(), 2);
        e.picked(hit(first.index()), true);
        assert_eq!(e.selection(), vec![second]);
    }

    #[test]
    fn clicking_the_background_clears_the_selection_unless_shift_is_held() {
        let mut e = editor();
        e.run(Command::AddBox);
        assert_eq!(e.selection().len(), 1, "a new body is selected");

        e.picked(Pick::MISS, true);
        assert_eq!(e.selection().len(), 1, "shift-clicking away keeps it");
        e.picked(Pick::MISS, false);
        assert!(e.selection().is_empty());
    }

    #[test]
    fn a_pick_that_names_a_body_that_is_gone_is_a_miss_not_a_panic() {
        let mut e = editor();
        e.run(Command::AddBox);
        e.picked(hit(NOTHING - 1), false);
        assert!(e.status().contains("no longer there"), "{}", e.status());
    }

    #[test]
    fn a_boolean_needs_exactly_two_and_says_so_when_it_does_not_have_them() {
        let mut e = editor();
        e.run(Command::AddBox);
        e.run(Command::Boolean(BooleanOp::Union));
        assert!(e.status().contains("exactly two"), "{}", e.status());

        e.run(Command::AddSphere);
        let ids: Vec<_> = e.document().nodes().map(|(id, _)| id).collect();
        e.document_mut().clear_selection();
        for id in &ids {
            e.document_mut().select(*id).unwrap();
        }
        e.run(Command::Boolean(BooleanOp::Union));
        assert_eq!(e.document().len(), 1, "{}", e.status());
        assert_eq!(e.selection().len(), 1, "the result is what is selected");
    }

    #[test]
    fn undo_puts_the_operands_back_and_does_not_leave_a_ghost_selected() {
        let mut e = editor();
        e.run(Command::AddBox);
        e.run(Command::AddSphere);
        let ids: Vec<_> = e.document().nodes().map(|(id, _)| id).collect();
        e.document_mut().clear_selection();
        for id in &ids {
            e.document_mut().select(*id).unwrap();
        }
        e.run(Command::Boolean(BooleanOp::Difference));
        assert_eq!(e.document().len(), 1);

        e.run(Command::Undo);
        assert_eq!(e.document().len(), 2, "{}", e.status());
        // The result node is gone, so nothing that names it may survive in the
        // selection — `Document::selection` filters, and this is the assertion
        // that it actually does.
        assert!(
            e.selection()
                .iter()
                .all(|id| e.document().node(*id).is_ok()),
            "a deleted node is still selected"
        );
    }

    #[test]
    fn select_all_takes_everything_and_a_boolean_can_then_run() {
        let mut e = editor();
        e.run(Command::AddBox);
        e.run(Command::AddCylinder);
        e.run(Command::SelectAll);
        assert_eq!(e.selection().len(), 2);
        e.run(Command::Boolean(BooleanOp::Difference));
        assert_eq!(e.document().len(), 1, "{}", e.status());
    }

    #[test]
    fn saving_remembers_where_and_opening_replaces_everything() {
        let dir = std::env::temp_dir().join(format!("w3d-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.w3d");

        let mut e = editor();
        e.run(Command::AddBox);
        e.run(Command::AddSphere);
        assert!(e.path().is_none(), "nothing has been saved yet");
        e.save(Some(path.clone())).unwrap();
        assert_eq!(e.path(), Some(path.as_path()));

        // A second `Save` goes to the same place without being told.
        e.run(Command::AddCylinder);
        e.run(Command::Save);
        assert!(e.status().starts_with("saved"), "{}", e.status());

        let mut other = editor();
        other.run(Command::AddBox);
        other.open(path.clone(), FakeKernel::default()).unwrap();
        assert_eq!(
            other.document().len(),
            3,
            "the opened document replaced, not merged"
        );
        assert_eq!(other.path(), Some(path.as_path()));
        // Opening frames what was opened, or the file appears to be empty.
        assert!(other.camera().distance > 0.0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opening_something_that_is_not_a_document_leaves_the_document_alone() {
        let dir = std::env::temp_dir().join(format!("w3d-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nonsense.w3d");
        std::fs::write(&path, b"this is not a zip").unwrap();

        let mut e = editor();
        e.run(Command::AddBox);
        let err = e.open(path, FakeKernel::default()).unwrap_err();
        assert!(err.contains("zip"), "{err}");
        assert_eq!(
            e.document().len(),
            1,
            "a failed open destroyed the document"
        );
        assert!(e.path().is_none(), "a failed open claimed a path");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleting_nothing_says_so_rather_than_doing_nothing_quietly() {
        let mut e = editor();
        e.run(Command::DeleteSelected);
        assert_eq!(e.status(), "nothing selected");
    }

    #[test]
    fn the_first_body_is_framed_so_that_adding_one_is_visible() {
        let mut e = editor();
        let before = e.camera().distance;
        e.run(Command::AddBox);
        assert_ne!(e.camera().distance, before, "the camera never moved");
        // And the second is not, because reframing under someone mid-edit is
        // worse than leaving them where they were.
        let after_first = *e.camera();
        e.run(Command::AddSphere);
        assert_eq!(e.camera().distance, after_first.distance);
    }

    #[test]
    fn zoom_to_fit_on_an_empty_document_says_so_instead_of_dividing_by_nothing() {
        let mut e = editor();
        let before = *e.camera();
        e.run(Command::ZoomToFit);
        assert_eq!(e.status(), "nothing to frame");
        assert_eq!(e.camera().distance, before.distance);
    }

    #[test]
    fn scrolling_is_multiplicative_in_both_directions() {
        let mut e = editor();
        let start = e.camera().distance;
        e.input(Input::Scroll(1.0));
        let out = e.camera().distance;
        assert!(out > start);
        e.input(Input::Scroll(-1.0));
        assert!(
            (e.camera().distance - start).abs() < 1.0e-9,
            "not reversible"
        );
    }

    // The fake kernel does not do STEP, and a build of the modeller against it
    // is a build in which these two commands cannot work. What is asserted
    // here is the *refusal*: that it reaches the status line in words, and
    // that nothing is written. A backend that does STEP is exercised where
    // there is one — `kernel-occt/tests/step.rs`.

    #[test]
    fn exporting_step_without_a_kernel_that_does_it_says_so_and_writes_no_file() {
        let dir = std::env::temp_dir().join(format!("w3d-step-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("refused.step");
        let _ = std::fs::remove_file(&path);

        let mut e = editor();
        e.run(Command::AddBox);
        let err = e.export_step(Some(path.clone())).unwrap_err();
        assert!(
            err.contains("unsupported"),
            "an export that cannot happen must say why: {err}"
        );
        assert!(
            !path.exists(),
            "a refused export left a file behind, which is the one outcome \
             worse than refusing"
        );
        // And through the command, which is how the button reaches it: the
        // status line is the only place a modeller can say no.
        e.run(Command::ExportStep);
        assert!(
            e.status().contains("unsupported"),
            "the refusal never reached the status line: {}",
            e.status()
        );
    }

    #[test]
    fn exporting_an_empty_document_says_so_before_asking_the_kernel() {
        let mut e = editor();
        assert_eq!(e.export_step(None).unwrap_err(), "nothing to export");
    }

    #[test]
    fn importing_a_file_that_is_not_there_leaves_the_document_alone() {
        let mut e = editor();
        e.run(Command::AddBox);
        let err = e
            .import_step(std::env::temp_dir().join("w3d-no-such-file.step"))
            .unwrap_err();
        assert!(err.starts_with("could not read"), "{err}");
        assert_eq!(
            e.document().len(),
            1,
            "a failed import changed the document"
        );
    }
}
