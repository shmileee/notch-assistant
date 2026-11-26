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

/// Whether a point is on a display, by the rule Rune uses to pick the panel's display: the top
/// row belongs to the display, the bottom row to the one below it.
pub fn contains(frame: [f64; 4], x: f64, y: f64) -> bool {
    x >= frame[0] && x < frame[0] + frame[2] && y > frame[1] && y <= frame[1] + frame[3]
}

#[cfg(target_os = "macos")]
fn screen_under(mtm: objc2_foundation::MainThreadMarker, x: f64, y: f64) -> Option<objc2::rc::Retained<objc2_app_kit::NSScreen>> {
    objc2_app_kit::NSScreen::screens(mtm).into_iter().find(|s| {
        let f = s.frame();
        contains([f.origin.x, f.origin.y, f.size.width, f.size.height], x, y)
    })
}

/// The frame of the display holding a point in Cocoa's global space. Main thread only.
#[cfg(target_os = "macos")]
pub fn screen_at(x: f64, y: f64) -> Option<[f64; 4]> {
    let f = screen_under(objc2_foundation::MainThreadMarker::new()?, x, y)?.frame();
    Some([f.origin.x, f.origin.y, f.size.width, f.size.height])
}

/// The notch of the display the pointer is on. Main thread only (it is where `poll` runs);
/// elsewhere, or off macOS, the default.
#[cfg(target_os = "macos")]
pub fn under_pointer() -> NotchGeo {
    use objc2_app_kit::NSEvent;
    use objc2_foundation::MainThreadMarker;

    let Some(mtm) = MainThreadMarker::new() else { return NotchGeo::default() };
    // In Cocoa's global space (origin bottom-left of the primary display).
    let p = unsafe { NSEvent::mouseLocation() };
    let Some(screen) = screen_under(mtm, p.x, p.y) else { return NotchGeo::default() };
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

/// A window as the window server lists it: owner pid, layer, alpha and bounds (top-down points).
pub type Listed = (i32, i64, f64, [f64; 4]);

/// Whether a window of another app fills `display`, as a slideshow or a borderless full-screen
/// player does without a full-screen Space. The menu bar's layers never fill one.
pub fn covered(display: [f64; 4], me: i32, windows: &[Listed]) -> bool {
    windows.iter().any(|(pid, layer, alpha, b)| *pid != me && *layer >= 0 && *alpha > 0.0 && (0..4).all(|i| (b[i] - display[i]).abs() < 1.0))
}

/// Whether the current Space on the display with `uuid` is a full-screen one. `spaces` is each
/// display's identifier and current Space type; "Main" stands for every display when they
/// share their Spaces.
pub fn full_screen_space(uuid: &str, spaces: &[(String, i64)]) -> bool {
    const FULL_SCREEN: i64 = 4;
    spaces.iter().any(|(display, kind)| (display == uuid || display == "Main") && *kind == FULL_SCREEN)
}

/// Whether another app is full screen on the display under the pointer. Main thread only.
///
/// On a notched display a full-screen window sits below the notch, exactly where a maximised
/// window does, so the window list alone can't tell them apart: the display's Space can.
#[cfg(target_os = "macos")]
pub fn full_screen_under_pointer() -> bool {
    use objc2_app_kit::{NSEvent, NSScreen};
    let Some(mtm) = objc2_foundation::MainThreadMarker::new() else { return false };
    let p = unsafe { NSEvent::mouseLocation() };
    let (Some(screen), Some(primary)) = (screen_under(mtm, p.x, p.y), NSScreen::screens(mtm).into_iter().next()) else { return false };
    let (f, top) = (screen.frame(), primary.frame().size.height);
    let display = [f.origin.x, top - f.origin.y - f.size.height, f.size.width, f.size.height];
    screen_number(&screen).and_then(display_uuid).is_some_and(|uuid| full_screen_space(&uuid, &current_spaces()))
        || covered(display, std::process::id() as i32, &window_list())
}

#[cfg(not(target_os = "macos"))]
pub fn full_screen_under_pointer() -> bool { false }

#[cfg(target_os = "macos")]
fn screen_number(screen: &objc2_app_kit::NSScreen) -> Option<u32> {
    use objc2::runtime::AnyObject;
    use objc2::msg_send;
    // SAFETY: an AppKit read on the main thread.
    unsafe {
        let description: *mut AnyObject = msg_send![screen, deviceDescription];
        let number: *mut AnyObject = msg_send![description, objectForKey: &*objc2_foundation::NSString::from_str("NSScreenNumber")];
        (!number.is_null()).then(|| msg_send![number, unsignedIntValue])
    }
}

#[cfg(target_os = "macos")]
fn display_uuid(id: u32) -> Option<String> {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_foundation::NSString;
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" { fn CGDisplayCreateUUIDFromDisplayID(display: u32) -> *mut AnyObject; }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" { fn CFUUIDCreateString(allocator: *const std::ffi::c_void, uuid: *mut AnyObject) -> *mut NSString; }
    // SAFETY: CoreGraphics reads, thread-safe; each created object is released by `Retained`.
    unsafe {
        let uuid = Retained::from_raw(CGDisplayCreateUUIDFromDisplayID(id))?;
        let text = Retained::from_raw(CFUUIDCreateString(std::ptr::null(), Retained::as_ptr(&uuid).cast_mut()))?;
        Some(text.to_string())
    }
}

/// Each display's identifier and the type of its current Space, from SkyLight's private
/// `CGSCopyManagedDisplaySpaces`: macOS has no public way to tell a full-screen Space. Window
/// managers have read it the same way for years; if it ever goes, this is empty and only
/// `covered` decides.
#[cfg(target_os = "macos")]
fn current_spaces() -> Vec<(String, i64)> {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::msg_send;
    use objc2_foundation::NSString;
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGSMainConnectionID() -> i32;
        fn CGSCopyManagedDisplaySpaces(connection: i32) -> *mut AnyObject;
    }
    // SAFETY: the copied CFArray of CFDictionaries is toll-free bridged and released by
    // `Retained`; every value's class is checked before it's read.
    unsafe {
        let Some(list) = Retained::from_raw(CGSCopyManagedDisplaySpaces(CGSMainConnectionID())) else { return vec![] };
        let get = |d: *mut AnyObject, key: &str| -> *mut AnyObject { msg_send![d, objectForKey: &*NSString::from_str(key)] };
        let is = |o: *mut AnyObject, class: &str| -> bool {
            let Some(c) = objc2::runtime::AnyClass::get(class) else { return false };
            !o.is_null() && msg_send![o, isKindOfClass: c]
        };
        let count: usize = msg_send![&list, count];
        (0..count).filter_map(|i| {
            let d: *mut AnyObject = msg_send![&list, objectAtIndex: i];
            let (id, current) = (get(d, "Display Identifier"), get(d, "Current Space"));
            if !is(id, "NSString") || !is(current, "NSDictionary") { return None; }
            let kind = get(current, "type");
            if !is(kind, "NSNumber") { return None; }
            let id: &NSString = &*id.cast();
            Some((id.to_string(), msg_send![kind, longLongValue]))
        }).collect()
    }
}

/// The on-screen windows, front to back. Bounds, layers and owners need no Screen Recording permission.
#[cfg(target_os = "macos")]
fn window_list() -> Vec<Listed> {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::msg_send;
    use objc2_foundation::NSString;
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" { fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> *mut AnyObject; }
    const ON_SCREEN_ONLY: u32 = 1 << 0;
    const EXCLUDE_DESKTOP: u32 = 1 << 4;
    // SAFETY: the copied CFArray is toll-free bridged to NSArray and released by `Retained`; its
    // dictionaries and numbers are only read while it lives.
    unsafe {
        let Some(list) = Retained::from_raw(CGWindowListCopyWindowInfo(ON_SCREEN_ONLY | EXCLUDE_DESKTOP, 0)) else { return vec![] };
        let get = |d: *mut AnyObject, key: &str| -> *mut AnyObject { msg_send![d, objectForKey: &*NSString::from_str(key)] };
        let num = |d: *mut AnyObject, key: &str| -> Option<f64> { let v = get(d, key); (!v.is_null()).then(|| msg_send![v, doubleValue]) };
        let count: usize = msg_send![&list, count];
        (0..count).filter_map(|i| {
            let w: *mut AnyObject = msg_send![&list, objectAtIndex: i];
            let b = get(w, "kCGWindowBounds");
            if b.is_null() { return None; }
            let bounds = [num(b, "X")?, num(b, "Y")?, num(b, "Width")?, num(b, "Height")?];
            Some((num(w, "kCGWindowOwnerPID")? as i32, num(w, "kCGWindowLayer")? as i64, num(w, "kCGWindowAlpha").unwrap_or(1.0), bounds))
        }).collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shared_edge_of_stacked_displays_belongs_to_the_lower_one() {
        let (upper, lower) = ([0.0, 1080.0, 1920.0, 1080.0], [0.0, 0.0, 1920.0, 1080.0]);
        assert!(contains(lower, 100.0, 1080.0) && !contains(upper, 100.0, 1080.0));
        assert!(contains(upper, 100.0, 2160.0), "the top row");
        assert!(!contains(lower, 1920.0, 500.0), "the right edge is the next display's");
    }

    #[test]
    fn a_window_filling_the_display_is_full_screen() {
        let display = [4952.0, 240.0, 1920.0, 1080.0];
        let slideshow = (7, 1000, 1.0, display);
        let maximised = (8, 0, 1.0, [4952.0, 270.0, 1920.0, 1050.0]);
        let elsewhere = (9, 0, 1.0, [0.0, 0.0, 3440.0, 1440.0]);
        let menu_bar = (0, 24, 1.0, [4952.0, 240.0, 1920.0, 30.0]);
        assert!(covered(display, 1, &[menu_bar, maximised, slideshow]), "at any level, behind others or not");
        assert!(!covered(display, 1, &[menu_bar, maximised, elsewhere]), "a maximised window leaves the menu bar");
        assert!(!covered(display, 7, &[slideshow]), "our own windows don't count");
        assert!(!covered(display, 1, &[(7, 0, 0.0, display)]), "invisible windows don't count");
    }

    #[test]
    fn a_full_screen_space_is_known_by_its_display() {
        let laptop = "37D8832A-2D66-02CA-B9F7-8F30A301B230";
        let spaces = |kind| vec![(laptop.to_string(), kind), ("9A1F-EXTERNAL".to_string(), 0)];
        assert!(full_screen_space(laptop, &spaces(4)));
        assert!(!full_screen_space(laptop, &spaces(0)));
        assert!(!full_screen_space("9A1F-EXTERNAL", &spaces(4)), "another display's full screen");
        assert!(full_screen_space("9A1F-EXTERNAL", &[("Main".into(), 4)]), "displays sharing their Spaces");
        assert!(!full_screen_space(laptop, &[]), "nothing read");
    }

    /// Reads the real window list and displays: `cargo test -- --ignored --nocapture` prints, for
    /// each display, whether another app fills it.
    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn live_full_screen_check() {
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
            fn CGDisplayBounds(display: u32) -> objc2_foundation::NSRect;
        }
        let windows = window_list();
        assert!(!windows.is_empty(), "the window server lists windows");
        let (mut ids, mut count) = ([0u32; 16], 0u32);
        unsafe { CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut count) };
        for id in &ids[..count as usize] {
            let b = unsafe { CGDisplayBounds(*id) };
            let display = [b.origin.x, b.origin.y, b.size.width, b.size.height];
            let space = display_uuid(*id).is_some_and(|uuid| full_screen_space(&uuid, &current_spaces()));
            println!("display {id} {display:?}: full-screen Space {space}, filled {}", covered(display, std::process::id() as i32, &windows));
        }
    }
}
