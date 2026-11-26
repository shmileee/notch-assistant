//! Keeps the panel at the top of the display under the pointer.
//!
//! Rune puts the panel back only when the pointer moves inside it or onto another display, so a
//! panel something else moved stays where it was put. Window managers such as Rectangle move it
//! through Accessibility (they fall back to the topmost window on screen when the front app has
//! none, and this panel sits above the menu bar), and macOS moves it when displays change.
//! So the panel leaves the Accessibility window list, and any drift is put right on the next poll.

use rune::{Align, OverlayHandle, OverlayPlacement, ScreenEdge};

pub const PLACEMENT: OverlayPlacement = OverlayPlacement::Edge { edge: ScreenEdge::Top, margin: 0.0, align: Align::Center, inset: 0.0 };

/// A rect as `[x, y, width, height]` in AppKit's points (origin bottom-left of the primary display).
pub type Rect = [f64; 4];

/// Whether a panel at `frame` is away from its place, centred at the top of `screen`.
pub fn drifted(frame: Rect, screen: Rect) -> bool {
    let x = screen[0] + (screen[2] - frame[2]) / 2.0;
    let y = screen[1] + screen[3] - frame[3];
    (frame[0] - x).abs() > 0.5 || (frame[1] - y).abs() > 0.5
}

#[derive(Debug, Default)]
pub struct Anchor {
    /// The panel window last taken out of the Accessibility list (Rune makes a new one if it
    /// has to recreate the panel).
    hidden: Option<u32>,
}

impl Anchor {
    /// Main thread only, which is where `poll` runs.
    #[cfg(target_os = "macos")]
    pub fn keep(&mut self, overlay: &OverlayHandle<crate::State>) {
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};
        use objc2_app_kit::NSEvent;
        use objc2_foundation::NSRect;

        let Some(number) = overlay.window_number() else { return };
        // SAFETY: AppKit calls on the main thread; the window is only read and flagged here.
        unsafe {
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let window: *mut AnyObject = msg_send![app, windowWithWindowNumber: number as isize];
            let Some(window) = window.as_ref() else { return };
            if self.hidden != Some(number) {
                let _: () = msg_send![window, setAccessibilityElement: false];
                self.hidden = Some(number);
            }
            let f: NSRect = msg_send![window, frame];
            let frame = [f.origin.x, f.origin.y, f.size.width, f.size.height];
            let p = NSEvent::mouseLocation();
            let Some(screen) = crate::notch::screen_at(p.x, p.y) else { return };
            if drifted(frame, screen) {
                eprintln!("anchor: the panel was moved to {frame:?}; putting it back on {screen:?}");
                overlay.set_placement(Some(PLACEMENT));
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    pub fn keep(&mut self, _: &OverlayHandle<crate::State>) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDE: Rect = [0.0, 0.0, 3440.0, 1440.0];
    const LAPTOP: Rect = [3440.0, -240.0, 1512.0, 982.0];

    #[test]
    fn a_panel_at_the_top_centre_stays() {
        assert!(!drifted([1290.0, 1070.0, 860.0, 370.0], WIDE));
        assert!(!drifted([3766.0, 372.0, 860.0, 370.0], LAPTOP));
        assert!(!drifted([1290.3, 1070.0, 860.0, 370.0], WIDE), "Rune doesn't round, so allow a fraction");
    }

    #[test]
    fn a_moved_panel_drifted() {
        assert!(drifted([1290.0, 535.0, 860.0, 370.0], WIDE), "centred on the screen, as Rectangle's maximize leaves it");
        assert!(drifted([0.0, 1070.0, 860.0, 370.0], WIDE), "moved sideways");
        assert!(drifted([3766.0, 372.0, 860.0, 370.0], WIDE), "on another display");
    }

    #[test]
    fn a_display_that_changed_size_moves_its_panel() {
        let more_space = [3440.0, -340.0, 1800.0, 1169.0];
        assert!(drifted([3766.0, 372.0, 860.0, 370.0], more_space));
    }
}
