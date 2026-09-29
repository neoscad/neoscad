//! Pointer and wheel events turned into camera moves, with no DOM types:
//! the browser glue (`web.rs`) feeds in what each event says, and the
//! tests here feed in the same numbers.
//!
//! The mapping follows the macOS app's `MetalView` where a browser can
//! tell the same things apart:
//!
//! - a primary-button drag orbits; with Alt (Option) held it pans;
//! - a secondary- or middle-button drag pans;
//! - one finger orbits; two fingers pan with their midpoint and zoom with
//!   their spread (a pinch);
//! - the wheel zooms, OpenSCAD's tenth of the distance a notch; a
//!   trackpad pinch arrives as a wheel event with Ctrl set (Chrome,
//!   Firefox and Safari all send it so) and zooms by its own factor;
//!   Shift + wheel pans.
//!
//! The app pans on a two-finger trackpad scroll, because AppKit says
//! whether a scroll came from a trackpad. A browser does not: a mouse
//! wheel and a trackpad both send pixel deltas, and guessing from their
//! size misfires on one or the other. Zooming on every plain wheel event
//! is what web 3D viewers generally do, so that is the rule here.
//!
//! A press released within [`CLICK_SLOP`] points of where it started, with
//! no second finger, is a click (for picking), not a drag: the app allows
//! the same wobble so that a click does not orbit.

/// How far a press may wander, in points, and still be a click.
pub const CLICK_SLOP: f64 = 4.0;

/// Wheel pixels that make one notch: Chrome and Safari report a mouse
/// wheel's notch as 100 pixels; Firefox reports lines, 3 a notch.
const PIXELS_PER_NOTCH: f64 = 100.0;
const LINES_PER_NOTCH: f64 = 3.0;

/// `WheelEvent.deltaMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaMode {
    Pixel,
    Line,
    Page,
}

impl DeltaMode {
    pub fn from_dom(mode: u32) -> DeltaMode {
        match mode {
            1 => DeltaMode::Line,
            2 => DeltaMode::Page,
            _ => DeltaMode::Pixel,
        }
    }
}

/// What an event asks of the camera. Distances are in points (CSS
/// pixels), y down, as the viewport's `orbit` and `pan` take them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    Orbit(f64, f64),
    Pan(f64, f64),
    /// Wheel notches, positive closer (`Camera::zoom`, 120 a notch).
    Zoom(f64),
    /// Divide the distance by this factor (`Camera::zoom_by`).
    Magnify(f64),
    /// A click at this point, from the top left.
    Click(f64, f64),
}

/// A pointer that is down: where it started and where it was last.
#[derive(Debug, Clone, Copy)]
struct Down {
    id: i32,
    pan: bool,
    start: (f64, f64),
    last: (f64, f64),
    /// It went further than [`CLICK_SLOP`], or another pointer joined.
    dragged: bool,
}

/// The pointers that are down (two at most are tracked; a third finger is
/// ignored until one lifts).
#[derive(Debug, Default)]
pub struct Gestures {
    down: Vec<Down>,
}

impl Gestures {
    /// A pointer pressed at `(x, y)`: `button` as `PointerEvent.button`
    /// (0 primary, 1 middle, 2 secondary), with Alt held or not. Whether
    /// the pointer is tracked (the glue captures it if so).
    pub fn press(&mut self, id: i32, button: i16, alt: bool, x: f64, y: f64) -> bool {
        if self.down.len() >= 2 || self.down.iter().any(|d| d.id == id) {
            return false;
        }
        let pan = button == 1 || button == 2 || (button == 0 && alt);
        let second = !self.down.is_empty();
        for d in &mut self.down {
            d.dragged = true;
        }
        self.down.push(Down {
            id,
            pan,
            start: (x, y),
            last: (x, y),
            dragged: second,
        });
        true
    }

    /// A tracked pointer moved to `(x, y)`: what the camera should do
    /// (nothing, one move, or a pan and a pinch together).
    pub fn motion(&mut self, id: i32, x: f64, y: f64) -> Vec<Action> {
        let Some(i) = self.down.iter().position(|d| d.id == id) else {
            return Vec::new();
        };
        if self.down.len() == 2 {
            let other = self.down[1 - i].last;
            let before = self.down[i].last;
            self.down[i].last = (x, y);
            self.down[i].dragged = true;
            let spread = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).hypot(a.1 - b.1);
            let (s0, s1) = (spread(before, other), spread((x, y), other));
            // Each finger moves in its own event, so each carries half of
            // the midpoint's travel, and its own share of the pinch.
            let mut out = vec![Action::Pan((x - before.0) / 2.0, (y - before.1) / 2.0)];
            if s0 > 1.0 && s1 > 1.0 && s1 != s0 {
                out.push(Action::Magnify(s1 / s0));
            }
            return out;
        }
        let d = &mut self.down[i];
        let (mut dx, mut dy) = (x - d.last.0, y - d.last.1);
        d.last = (x, y);
        if !d.dragged {
            if (x - d.start.0).hypot(y - d.start.1) <= CLICK_SLOP {
                return Vec::new();
            }
            d.dragged = true;
            // The wobble allowed before the drag began counts too, so the
            // model stays under the pointer.
            (dx, dy) = (x - d.start.0, y - d.start.1);
        }
        vec![if d.pan {
            Action::Pan(dx, dy)
        } else {
            Action::Orbit(dx, dy)
        }]
    }

    /// A tracked pointer lifted (or was cancelled, `cancel`): a click if
    /// it never became a drag.
    pub fn release(&mut self, id: i32, cancel: bool) -> Option<Action> {
        let i = self.down.iter().position(|d| d.id == id)?;
        let d = self.down.remove(i);
        // The finger left behind must not turn into a click either.
        for o in &mut self.down {
            o.dragged = true;
        }
        (!d.dragged && !cancel).then_some(Action::Click(d.start.0, d.start.1))
    }

    /// Whether any pointer is down.
    pub fn active(&self) -> bool {
        !self.down.is_empty()
    }
}

/// A wheel event: its deltas and mode, with Ctrl (a trackpad pinch) and
/// Shift held or not.
pub fn wheel(dx: f64, dy: f64, mode: DeltaMode, ctrl: bool, shift: bool) -> Option<Action> {
    let unit = match mode {
        DeltaMode::Pixel => 1.0,
        DeltaMode::Line => PIXELS_PER_NOTCH / LINES_PER_NOTCH,
        // A page is rare (a setting some systems have): call it a notch.
        DeltaMode::Page => PIXELS_PER_NOTCH,
    };
    let (dx, dy) = (dx * unit, dy * unit);
    if ctrl {
        // A pinch's `deltaY` grows with how far the fingers spread in the
        // event (negative spreading); an exponential makes the zoom the
        // same however the gesture is split into events.
        let factor = (-dy / PIXELS_PER_NOTCH).exp();
        return (factor.is_finite() && factor != 1.0).then_some(Action::Magnify(factor));
    }
    if shift {
        // Some browsers turn Shift + wheel into a horizontal scroll, so
        // either axis pans.
        return (dx != 0.0 || dy != 0.0).then_some(Action::Pan(-dx, -dy));
    }
    // Scrolling down (positive) moves away, as OpenSCAD's wheel does.
    (dy != 0.0).then_some(Action::Zoom(-dy / PIXELS_PER_NOTCH))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_press_is_a_click_and_a_large_one_a_drag() {
        let mut g = Gestures::default();
        assert!(g.press(1, 0, false, 10.0, 10.0));
        assert_eq!(g.motion(1, 12.0, 11.0), Vec::new());
        assert_eq!(g.release(1, false), Some(Action::Click(10.0, 10.0)));

        assert!(g.press(1, 0, false, 10.0, 10.0));
        assert_eq!(g.motion(1, 20.0, 10.0), vec![Action::Orbit(10.0, 0.0)]);
        assert_eq!(g.motion(1, 25.0, 12.0), vec![Action::Orbit(5.0, 2.0)]);
        assert_eq!(g.release(1, false), None);
        assert!(!g.active());

        // A cancelled press is not a click.
        g.press(1, 0, false, 0.0, 0.0);
        assert_eq!(g.release(1, true), None);
    }

    #[test]
    fn buttons_and_alt_choose_pan() {
        let mut g = Gestures::default();
        for (button, alt) in [(2, false), (1, false), (0, true)] {
            g.press(7, button, alt, 0.0, 0.0);
            assert_eq!(g.motion(7, 10.0, 0.0), vec![Action::Pan(10.0, 0.0)]);
            g.release(7, false);
        }
    }

    #[test]
    fn two_fingers_pinch_and_pan() {
        let mut g = Gestures::default();
        g.press(1, 0, false, 0.0, 0.0);
        g.press(2, 0, false, 100.0, 0.0);
        assert!(
            !g.press(3, 0, false, 50.0, 50.0),
            "a third finger is ignored"
        );
        // Spreading from 100 to 200 apart: the midpoint moves by half.
        assert_eq!(
            g.motion(2, 200.0, 0.0),
            vec![Action::Pan(50.0, 0.0), Action::Magnify(2.0)]
        );
        // Lifting either finger is never a click.
        assert_eq!(g.release(2, false), None);
        assert_eq!(g.release(1, false), None);
    }

    #[test]
    fn wheel_zooms_pinch_magnifies_shift_pans() {
        assert_eq!(
            wheel(0.0, 100.0, DeltaMode::Pixel, false, false),
            Some(Action::Zoom(-1.0))
        );
        assert_eq!(
            wheel(0.0, -3.0, DeltaMode::Line, false, false),
            Some(Action::Zoom(1.0))
        );
        let Some(Action::Magnify(f)) = wheel(0.0, -10.0, DeltaMode::Pixel, true, false) else {
            panic!("a pinch magnifies")
        };
        assert!(f > 1.0, "spreading the fingers brings the model closer");
        assert_eq!(
            wheel(5.0, 0.0, DeltaMode::Pixel, false, true),
            Some(Action::Pan(-5.0, 0.0))
        );
        assert_eq!(wheel(0.0, 0.0, DeltaMode::Pixel, false, false), None);
    }
}
