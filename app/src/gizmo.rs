//! The manipulators: what a handle is, where it is on screen, and what a drag
//! on it means.
//!
//! None of this draws anything and none of it touches the document. That is the
//! point: the gizmos used to be nine hundred lines inside the frame function,
//! where the only way to find out whether a drag measured what it claimed was
//! to run the modeller and look. The arithmetic a handle needs — a 3D ray's
//! direction on screen, the distance a cursor has travelled *along* it, the
//! angle it has swept around it, and what the modifier keys do to both — is
//! here, where a test can ask.
//!
//! Two rules the old code broke, and that the types here are shaped to keep:
//!
//! - **A drag reads the cursor; it does not integrate frame deltas.** The value
//!   is always `f(start, now)`, so a frame dropped or a modifier pressed
//!   mid-drag changes nothing about where the handle ends up.
//! - **A drag does not touch geometry until it is let go.** The value is a
//!   number until then, and the document sees one operation and one undo step —
//!   not one kernel call per frame, which with a boolean underneath is the
//!   difference between a modeller and a slideshow.

use egui::{Pos2, Vec2};
use w3d_core::NodeId;
use w3d_core::kernel::Vec3;

/// Distance snapping, in document units, while `Shift` is held.
pub const SNAP_DISTANCE: f64 = 5.0;
/// The finer step the blend handles snap to: a 5 mm fillet is a big fillet.
pub const SNAP_DISTANCE_BLEND: f64 = 1.0;
/// Angle snapping, in degrees, while `Shift` is held.
pub const SNAP_ANGLE: f64 = 15.0;
/// What `Ctrl` does to the cursor's reach.
pub const FINE_SCALE: f64 = 0.1;
/// How close to a handle the cursor has to be, in points.
pub const GRAB_TOLERANCE: f32 = 10.0;

/// The modifier keys, as a drag reads them. Sampled every frame: pressing
/// `Shift` halfway through a drag snaps from there on, and releasing it goes
/// back to a free value with no jump, because neither one changes the anchor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub snap: bool,
    pub fine: bool,
}

/// A 3D ray, as it lands on the screen.
///
/// Built by projecting two points of the ray, which is what makes a drag track
/// the cursor under perspective: `points_per_unit` is measured at the handle
/// rather than assumed, so an axis pointing away from the camera moves slowly
/// and one across it moves fast, exactly as the projected arrow does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenAxis {
    pub origin: Pos2,
    pub dir: Vec2,
    pub points_per_unit: f64,
}

impl ScreenAxis {
    /// From the projection of `origin` and of the point `world_len` along the
    /// ray. `None` when the ray is end-on — there is no direction to drag in,
    /// and pretending otherwise is where the old code's jitter came from.
    #[must_use]
    pub fn new(origin: Pos2, ahead: Pos2, world_len: f64) -> Option<Self> {
        let delta = ahead - origin;
        let len = delta.length();
        if !(len.is_finite() && world_len > 0.0) || len < 1.0 {
            return None;
        }
        Some(Self {
            origin,
            dir: delta / len,
            points_per_unit: f64::from(len) / world_len,
        })
    }

    /// Where a point `units` along the ray lands, by the same linear map. Good
    /// enough for a handle a few tens of units long; anything that must be
    /// exact under perspective projects the 3D point instead.
    #[must_use]
    pub fn at(&self, units: f64) -> Pos2 {
        self.origin + self.dir * (units * self.points_per_unit) as f32
    }

    /// How far along the ray the cursor has moved, in document units.
    #[must_use]
    pub fn distance(&self, start: Pos2, now: Pos2, mods: Mods, blend: bool) -> f64 {
        let delta = now - start;
        let along = f64::from(delta.x * self.dir.x + delta.y * self.dir.y);
        let mut value = along / self.points_per_unit;
        if mods.fine {
            value *= FINE_SCALE;
        }
        if mods.snap {
            let step = if blend {
                SNAP_DISTANCE_BLEND
            } else {
                SNAP_DISTANCE
            };
            value = (value / step).round() * step;
        }
        value
    }
}

/// Whether the cursor is on the segment from `a` to `b`, rather than in the box
/// around it.
///
/// A diagonal arrow's bounding box is mostly not the arrow. Claiming that box
/// is what made the viewport go dead around a selected part: every press inside
/// it was a press on a handle, so it could not orbit and could not pick.
#[must_use]
pub fn hit_segment(p: Pos2, a: Pos2, b: Pos2, tolerance: f32) -> bool {
    let d = b - a;
    let len_sq = d.length_sq();
    if len_sq < 1.0 {
        return (p - a).length() <= tolerance;
    }
    let t = (((p - a).dot(d)) / len_sq).clamp(0.0, 1.0);
    (p - (a + d * t)).length() <= tolerance
}

/// The angle the cursor has swept around `centre`, in degrees, signed so that a
/// positive value turns the right way around `facing`: `+1` when the rotation
/// axis points towards the camera, `-1` when it points away.
#[must_use]
pub fn swept_angle(centre: Pos2, start: Pos2, now: Pos2, facing: f64, mods: Mods) -> f64 {
    let a = start - centre;
    let b = now - centre;
    if a.length() < 1.0 || b.length() < 1.0 {
        return 0.0;
    }
    let cross = f64::from(a.x * b.y - a.y * b.x);
    let dot = f64::from(a.x * b.x + a.y * b.y);
    // Screen y grows downward, so a positive screen cross product is a
    // clockwise turn as the user sees it; the sign here puts it back.
    let mut degrees = -cross.atan2(dot).to_degrees() * facing.signum();
    if mods.fine {
        degrees *= FINE_SCALE;
    }
    if mods.snap {
        degrees = (degrees / SNAP_ANGLE).round() * SNAP_ANGLE;
    }
    degrees
}

/// What a handle does when it is dragged.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Handle {
    /// Move the selection along a world axis.
    Translate { axis: Vec3 },
    /// Turn the selection about a world axis through its centre.
    Rotate { axis: Vec3 },
    /// Pull the selected face along its normal — outward adds material,
    /// inward cuts.
    PushPull {
        node: NodeId,
        face: u32,
        outward: bool,
    },
    /// Round the solid's edges. Not the selected edge alone: no backend here
    /// blends one edge, and the handle says so rather than implying otherwise.
    Fillet,
    /// Bevel the solid's edges, on the same terms.
    Chamfer,
}

impl Handle {
    /// Whether applying the handle twice adds up.
    ///
    /// A translation of 3 then 2 is a translation of 5, and a second push/pull
    /// pulls further. A fillet of 2 mm on a solid already filleted 1 mm is not a
    /// 3 mm fillet of the original and is not a 2 mm one either — so typing a
    /// new radius has to undo the first, not add to it.
    #[must_use]
    pub fn composes(self) -> bool {
        match self {
            Self::Translate { .. } | Self::Rotate { .. } | Self::PushPull { .. } => true,
            Self::Fillet | Self::Chamfer => false,
        }
    }

    /// Whether this handle measures an angle rather than a distance.
    #[must_use]
    pub fn is_angular(self) -> bool {
        matches!(self, Self::Rotate { .. })
    }

    /// Whether this handle snaps on the finer step.
    #[must_use]
    pub fn is_blend(self) -> bool {
        matches!(self, Self::Fillet | Self::Chamfer)
    }

    /// What the reading is called, and what unit it is in.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Translate { .. } => "Mover",
            Self::Rotate { .. } => "Girar",
            Self::PushPull { outward: true, .. } => "Extruir",
            Self::PushPull { outward: false, .. } => "Vaciar",
            Self::Fillet => "Redondear (todo el sólido)",
            Self::Chamfer => "Chaflán (todo el sólido)",
        }
    }

    #[must_use]
    pub fn unit(self) -> &'static str {
        if self.is_angular() { "°" } else { "mm" }
    }
}

/// A handle being dragged, or resting with its box open after one.
///
/// `applied` is what the document has already been told; `value` is what the
/// cursor or the text box says now. The difference between them is the whole
/// of the state machine: it is what gets applied, and when it is zero there is
/// nothing to undo — which is why cancelling this no longer reaches back and
/// undoes whatever the user did before it.
#[derive(Clone, Debug)]
pub struct Session {
    pub handle: Handle,
    /// Where the handle is anchored, in world units.
    pub anchor: Vec3,
    /// The direction it acts along: the axis, or the face normal.
    pub direction: Vec3,
    pub axis: ScreenAxis,
    /// The cursor position the drag started at.
    pub grabbed_at: Pos2,
    /// Where the readout sits. It follows the cursor while dragging and stops
    /// when the button comes up, so that it can be typed into.
    pub readout_at: Pos2,
    /// Sign of the rotation axis against the view direction.
    pub facing: f64,
    pub value: f64,
    pub applied: f64,
    pub text: String,
    pub dragging: bool,
    pub focus_wanted: bool,
}

impl Session {
    #[must_use]
    pub fn new(handle: Handle, anchor: Vec3, direction: Vec3, axis: ScreenAxis, at: Pos2) -> Self {
        Self {
            handle,
            anchor,
            direction,
            axis,
            grabbed_at: at,
            readout_at: at + egui::vec2(18.0, -22.0),
            facing: 1.0,
            value: 0.0,
            applied: 0.0,
            text: String::from("0"),
            dragging: true,
            focus_wanted: false,
        }
    }

    /// Re-reads the cursor. Returns the value, which is a number and not yet an
    /// edit.
    pub fn track(&mut self, now: Pos2, mods: Mods) -> f64 {
        self.value = if self.handle.is_angular() {
            swept_angle(self.axis.origin, self.grabbed_at, now, self.facing, mods)
        } else {
            self.axis
                .distance(self.grabbed_at, now, mods, self.handle.is_blend())
        };
        self.readout_at = now + egui::vec2(18.0, -22.0);
        self.text = format!("{:.2}", self.value);
        self.value
    }

    /// What must be applied to the document to make it match `value`, and
    /// whether the previous application has to be undone first.
    #[must_use]
    pub fn pending(&self) -> Pending {
        if (self.value - self.applied).abs() <= 1.0e-9 {
            return Pending::Nothing;
        }
        if self.handle.composes() {
            Pending::Add(self.value - self.applied)
        } else {
            Pending::Replace {
                value: self.value,
                undo_first: self.applied != 0.0,
            }
        }
    }

    /// Records that the document now matches `value`.
    pub fn settled(&mut self) {
        self.applied = self.value;
    }

    /// Whether letting this go leaves the document changed.
    #[must_use]
    pub fn touched_the_document(&self) -> bool {
        self.applied != 0.0
    }
}

/// What a session still owes the document.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pending {
    Nothing,
    /// Apply this much more of the same operation.
    Add(f64),
    /// Apply this value instead of the one already applied.
    Replace {
        value: f64,
        undo_first: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    fn axis() -> ScreenAxis {
        // Ten units of world along +x map to a hundred points of screen.
        ScreenAxis::new(pos2(100.0, 100.0), pos2(200.0, 100.0), 10.0).unwrap()
    }

    #[test]
    fn a_drag_measures_where_the_cursor_is_not_how_it_got_there() {
        let a = axis();
        let straight = a.distance(
            pos2(100.0, 100.0),
            pos2(150.0, 100.0),
            Mods::default(),
            false,
        );
        assert!((straight - 5.0).abs() < 1.0e-9, "{straight}");

        // The same endpoint, reached across the axis: the crossways part does
        // not count, and no number of intermediate frames changes the answer.
        let wandered = a.distance(
            pos2(100.0, 100.0),
            pos2(150.0, 400.0),
            Mods::default(),
            false,
        );
        assert!((wandered - 5.0).abs() < 1.0e-9, "{wandered}");
    }

    #[test]
    fn an_end_on_axis_has_no_drag_direction_rather_than_a_wild_one() {
        assert!(ScreenAxis::new(pos2(10.0, 10.0), pos2(10.2, 10.1), 10.0).is_none());
    }

    #[test]
    fn shift_snaps_and_ctrl_slows_without_moving_the_anchor() {
        let a = axis();
        let free = a.distance(pos2(0.0, 0.0), pos2(63.0, 0.0), Mods::default(), false);
        assert!((free - 6.3).abs() < 1.0e-9, "{free}");

        let snapped = a.distance(
            pos2(0.0, 0.0),
            pos2(63.0, 0.0),
            Mods {
                snap: true,
                ..Mods::default()
            },
            false,
        );
        assert!((snapped - 5.0).abs() < 1.0e-9, "{snapped}");

        let blend = a.distance(
            pos2(0.0, 0.0),
            pos2(63.0, 0.0),
            Mods {
                snap: true,
                ..Mods::default()
            },
            true,
        );
        assert!((blend - 6.0).abs() < 1.0e-9, "{blend}");

        let fine = a.distance(
            pos2(0.0, 0.0),
            pos2(63.0, 0.0),
            Mods {
                fine: true,
                ..Mods::default()
            },
            false,
        );
        assert!((fine - 0.63).abs() < 1.0e-9, "{fine}");
    }

    #[test]
    fn the_handle_tip_lands_under_the_cursor_that_dragged_it() {
        let a = axis();
        let start = pos2(100.0, 100.0);
        for cursor in [pos2(140.0, 100.0), pos2(60.0, 130.0), pos2(300.0, 90.0)] {
            let d = a.distance(start, cursor, Mods::default(), false);
            let tip = a.at(d);
            // The tip tracks the cursor's travel *along* the axis, exactly.
            let expected = start + a.dir * ((cursor - start).dot(a.dir));
            assert!(
                (tip - expected).length() < 1.0e-3,
                "{tip:?} vs {expected:?}"
            );
        }
    }

    #[test]
    fn a_handle_is_grabbed_by_its_shaft_and_not_by_its_bounding_box() {
        let (a, b) = (pos2(0.0, 0.0), pos2(100.0, 100.0));
        assert!(hit_segment(pos2(50.0, 53.0), a, b, GRAB_TOLERANCE));
        // Inside the box the old code claimed, nowhere near the arrow.
        assert!(!hit_segment(pos2(95.0, 5.0), a, b, GRAB_TOLERANCE));
        assert!(!hit_segment(pos2(5.0, 95.0), a, b, GRAB_TOLERANCE));
    }

    #[test]
    fn a_turn_is_signed_by_which_way_the_axis_faces() {
        let centre = pos2(0.0, 0.0);
        let towards = swept_angle(
            centre,
            pos2(100.0, 0.0),
            pos2(0.0, -100.0),
            1.0,
            Mods::default(),
        );
        assert!((towards - 90.0).abs() < 1.0e-6, "{towards}");

        let away = swept_angle(
            centre,
            pos2(100.0, 0.0),
            pos2(0.0, -100.0),
            -1.0,
            Mods::default(),
        );
        assert!((away + 90.0).abs() < 1.0e-6, "{away}");
    }

    #[test]
    fn a_turn_snaps_to_fifteen_degrees() {
        let got = swept_angle(
            pos2(0.0, 0.0),
            pos2(100.0, 0.0),
            pos2(70.0, -70.0),
            1.0,
            Mods {
                snap: true,
                ..Mods::default()
            },
        );
        assert!((got - 45.0).abs() < 1.0e-6, "{got}");
    }

    fn session(handle: Handle) -> Session {
        Session::new(handle, Vec3::ZERO, Vec3::X, axis(), pos2(100.0, 100.0))
    }

    #[test]
    fn nothing_is_owed_until_the_value_moves() {
        let s = session(Handle::Translate { axis: Vec3::X });
        assert_eq!(s.pending(), Pending::Nothing);
    }

    #[test]
    fn an_operation_that_adds_up_is_applied_as_the_difference() {
        let mut s = session(Handle::Translate { axis: Vec3::X });
        s.track(pos2(150.0, 100.0), Mods::default());
        assert_eq!(s.pending(), Pending::Add(5.0));
        s.settled();

        s.track(pos2(170.0, 100.0), Mods::default());
        assert_eq!(s.pending(), Pending::Add(2.0));
    }

    #[test]
    fn a_blend_replaces_what_it_applied_rather_than_adding_to_it() {
        let mut s = session(Handle::Fillet);
        s.track(pos2(120.0, 100.0), Mods::default());
        assert_eq!(
            s.pending(),
            Pending::Replace {
                value: 2.0,
                undo_first: false
            }
        );
        s.settled();

        s.track(pos2(130.0, 100.0), Mods::default());
        assert_eq!(
            s.pending(),
            Pending::Replace {
                value: 3.0,
                undo_first: true
            }
        );
    }

    #[test]
    fn a_session_that_applied_nothing_has_nothing_to_undo() {
        let mut s = session(Handle::Fillet);
        assert!(!s.touched_the_document());
        s.track(pos2(140.0, 100.0), Mods::default());
        // Tracked, but never settled: the document has not been told.
        assert!(!s.touched_the_document());
        s.settled();
        assert!(s.touched_the_document());
    }
}
