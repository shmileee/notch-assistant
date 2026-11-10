//! Demo mode (`NOTCH_DEMO=1`), for recording the app with `scripts/record-demo.sh`.
//!
//! - The clock is pinned to Tue 10 Nov 2026, 14:05 and the data is the mock, so every recording
//!   shows the same thing.
//! - Settings live in memory: nothing touches the Keychain or settings.json, and "Test
//!   connection" answers after a short pause instead of calling FireHydrant.
//! - A backdrop covers the notched display (menu bar included), so a recording shows the app
//!   over a clean wallpaper, not your desktop. The island sits above it, and the Settings window
//!   between the two.
//!
//! The driver (`scripts/demo/driver.swift`) clicks at positions worked out from the constants
//! below; keep the two in step.

use std::sync::OnceLock;

use chrono::{NaiveDate, NaiveDateTime};
use rune::tok::TextStyle;
use rune::{Color, Cx, El, col, el, text};

/// Settings window size and its top edge below the display's top, in points.
pub const SETTINGS_SIZE: (f32, f32) = (680.0, 500.0);
pub const SETTINGS_TOP: f32 = 380.0;

/// macOS window levels in demo mode: backdrop < Settings < island.
pub const BACKDROP_LEVEL: isize = 25;
pub const SETTINGS_LEVEL: isize = 26;

static DEMO: OnceLock<bool> = OnceLock::new();

pub fn on() -> bool { *DEMO.get_or_init(|| std::env::var_os("NOTCH_DEMO").is_some()) }

/// The pinned demo time: the user (OP) is primary this week.
pub fn time() -> NaiveDateTime { NaiveDate::from_ymd_opt(2026, 11, 10).unwrap().and_hms_opt(14, 5, 0).unwrap() }

/// The backdrop overlay's (empty) state.
pub struct Backdrop;

pub fn backdrop(_: &Backdrop, _: &mut Cx<Backdrop>) -> El {
    let rgb = |r: u8, g: u8, b: u8| Color { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a: 1.0 };
    let caption = col().gap(4.0).pin(Some(48.0), None, None, Some(44.0))
        .child(text("Notch assistant").font(TextStyle::new(22.0, 650.0)).color(Color::WHITE.alpha(0.9)))
        .child(text("Hover the notch. Built with Rune.").font(TextStyle::new(14.0, 500.0)).color(Color::WHITE.alpha(0.55)));
    el().w_full().h_full()
        .linear_gradient(2.4, [(0.0, rgb(0x1b, 0x23, 0x4a)), (0.55, rgb(0x3a, 0x2a, 0x6b)), (1.0, rgb(0x8a, 0x4b, 0x7d))])
        .child(caption)
}
