//! Open at login: the app as a macOS login item (`SMAppService.mainAppService`, macOS 13+),
//! listed in System Settings › General › Login Items.
//!
//! Only an app bundle can register itself, so a bare binary (`cargo run`) reports
//! `Unavailable`; `make install` or the Homebrew cask gives you the bundle.

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{msg_send, msg_send_id};
use objc2_foundation::{NSError, NSString};

#[link(name = "ServiceManagement", kind = "framework")]
unsafe extern "C" {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Login {
    /// Not running from an app bundle.
    Unavailable,
    Off,
    On,
    /// Registered, waiting for the user to allow it in System Settings.
    NeedsApproval,
}

/// Whether this process is an app bundle's executable.
pub fn bundled() -> bool {
    std::env::current_exe().ok().and_then(|p| p.canonicalize().ok())
        .is_some_and(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"))
}

fn service() -> Option<Retained<AnyObject>> {
    if !bundled() { return None; }
    let class = AnyClass::get("SMAppService")?;
    // SAFETY: a class method returning the app's shared service object (never nil on 13+).
    unsafe { msg_send_id![class, mainAppService] }
}

pub fn status() -> Login {
    let Some(service) = service() else { return Login::Unavailable };
    // SMAppServiceStatus: 0 not registered, 1 enabled, 2 requires approval, 3 not found.
    let status: isize = unsafe { msg_send![&service, status] };
    match status {
        1 => Login::On,
        2 => Login::NeedsApproval,
        _ => Login::Off,
    }
}

/// Adds or removes the login item; returns the new status.
pub fn set(on: bool) -> Result<Login, String> {
    let service = service().ok_or("Open at login needs the installed app (make install or brew)")?;
    let result: Result<(), Retained<NSError>> = unsafe {
        if on { msg_send![&service, registerAndReturnError: _] } else { msg_send![&service, unregisterAndReturnError: _] }
    };
    match result {
        Ok(()) => Ok(status()),
        // Unregistering something that isn't registered is fine.
        Err(_) if !on && status() == Login::Off => Ok(Login::Off),
        Err(e) => Err(describe(&e)),
    }
}

fn describe(e: &NSError) -> String {
    let text: Retained<NSString> = e.localizedDescription();
    text.to_string()
}

/// `--login on|off|status`, for `make install` and scripts. Prints the status.
pub fn cli(arg: Option<&str>) -> i32 {
    let result = match arg {
        Some("on") => set(true),
        Some("off") => set(false),
        Some("status") | None => Ok(status()),
        Some(other) => Err(format!("unknown --login value {other:?}: use on, off or status")),
    };
    match result {
        Ok(login) => {
            println!("{}", match login {
                Login::On => "on",
                Login::Off => "off",
                Login::NeedsApproval => "needs approval: allow Notch Assistant in System Settings › General › Login Items",
                Login::Unavailable => "unavailable: not running from the app bundle",
            });
            0
        }
        Err(e) => { eprintln!("error: {e}"); 1 }
    }
}
