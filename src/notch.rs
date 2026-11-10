//! The notch on the display under the pointer, in points.
//!
//! macOS reports a notch through `NSScreen`: `safeAreaInsets.top` is its height and the gap
//! between `auxiliaryTopLeftArea` and `auxiliaryTopRightArea` is its width. A display
//! without one (an external monitor, or a MacBook with its lid closed) gets a "virtual"
//! notch as tall as its menu bar, so the island has somewhere to grow from everywhere.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NotchGeo {
    pub width: f32,
    pub height: f32,
    /// True for a hardware notch, false for the virtual one.
    pub real: bool,
}

/// The virtual notch's width: a MacBook Pro notch at the default scaled resolution.
const VIRTUAL_WIDTH: f32 = 185.0;

impl Default for NotchGeo {
    fn default() -> Self { Self { width: VIRTUAL_WIDTH, height: 32.0, real: false } }
}

/// The notch of the display the pointer is on. Main thread only (it is where `poll` runs);
/// elsewhere, or off macOS, the default.
#[cfg(target_os = "macos")]
pub fn under_pointer() -> NotchGeo {
    use objc2_app_kit::{NSEvent, NSScreen};
    use objc2_foundation::MainThreadMarker;

    let Some(mtm) = MainThreadMarker::new() else { return NotchGeo::default() };
    // Both in Cocoa's global space (origin bottom-left of the primary display).
    let p = unsafe { NSEvent::mouseLocation() };
    let screens = NSScreen::screens(mtm);
    let screen = screens.iter().find(|s| {
        let f = s.frame();
        p.x >= f.origin.x && p.x < f.origin.x + f.size.width && p.y >= f.origin.y && p.y <= f.origin.y + f.size.height
    });
    let Some(screen) = screen else { return NotchGeo::default() };
    unsafe {
        let frame = screen.frame();
        let top = screen.safeAreaInsets().top;
        if top > 0.0 {
            let width = frame.size.width - screen.auxiliaryTopLeftArea().size.width - screen.auxiliaryTopRightArea().size.width;
            return NotchGeo { width: (width as f32).max(120.0), height: top as f32, real: true };
        }
        // No notch: match the menu bar (0 when it auto-hides, so keep a sensible minimum).
        let visible = screen.visibleFrame();
        let menu_bar = (frame.origin.y + frame.size.height) - (visible.origin.y + visible.size.height);
        NotchGeo { width: VIRTUAL_WIDTH, height: (menu_bar as f32).clamp(24.0, 40.0), real: false }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn under_pointer() -> NotchGeo { NotchGeo::default() }

/// The built-in display's CGDirectDisplayID (the one with the notch), if it's on.
#[cfg(target_os = "macos")]
pub fn builtin_display() -> Option<u32> {
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
        fn CGDisplayIsBuiltin(display: u32) -> u32;
    }
    let (mut ids, mut count) = ([0u32; 16], 0u32);
    unsafe {
        if CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) != 0 { return None; }
        ids[..count as usize].iter().copied().find(|&id| CGDisplayIsBuiltin(id) != 0)
    }
}

#[cfg(not(target_os = "macos"))]
pub fn builtin_display() -> Option<u32> { None }
