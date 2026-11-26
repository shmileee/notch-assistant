//! Notices the Mac waking from sleep, so the panel refreshes at once instead of showing data
//! from before the night. The refresh threads can't tell by themselves: their waits count
//! awake time only.

use std::sync::atomic::{AtomicBool, Ordering};

static WOKE: AtomicBool = AtomicBool::new(false);

/// Starts listening. Main thread, once.
#[cfg(target_os = "macos")]
pub fn watch() {
    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send, msg_send_id};
    use objc2_foundation::NSString;

    let block = RcBlock::new(|_: *mut AnyObject| WOKE.store(true, Ordering::SeqCst));
    // SAFETY: AppKit calls on the main thread with a valid block; the centre copies the block.
    unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let centre: *mut AnyObject = msg_send![workspace, notificationCenter];
        let name = NSString::from_str("NSWorkspaceDidWakeNotification");
        let observer: Option<Retained<AnyObject>> = msg_send_id![centre, addObserverForName: &*name, object: std::ptr::null::<AnyObject>(), queue: std::ptr::null::<AnyObject>(), usingBlock: &*block];
        // The observer lives as long as the app.
        std::mem::forget(observer);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn watch() {}

/// Whether the Mac woke since the last call.
pub fn take() -> bool { WOKE.swap(false, Ordering::SeqCst) }
