//! Keeps the running app in step with its installed copy: when `brew upgrade` or
//! `make install` replaces the app bundle, the app restarts on the new one, and when
//! `brew uninstall` or `make uninstall` removes it, the app quits.
//!
//! Homebrew is meant to quit the app itself before an upgrade or uninstall, but it skips that
//! when `who` doesn't list the user's login name on the console, which is the case on Macs
//! whose login comes from single sign-on (`who` shows the account's email instead). So the app
//! watches its own executable: a new file at its path means a new version, and no file means
//! it's gone.

use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

/// How often to look, in seconds.
const EVERY: f64 = 2.0;
/// How long the bundle may be missing before the app takes it as uninstalled (an upgrade
/// removes the old bundle before it moves the new one in).
const GONE_AFTER: f64 = 10.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// A new executable is in place, and has stopped changing.
    Replaced,
    /// The executable has been gone for `GONE_AFTER`.
    Removed,
}

/// The file at a path: which file it is, and how big and how new.
type Stamp = (u64, u64, u64, i64);

fn stamp(path: &PathBuf) -> Option<Stamp> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.dev(), m.ino(), m.len(), m.mtime()))
}

pub struct Watch {
    exe: PathBuf,
    running: Stamp,
    /// A different file seen at the last look, waiting to be seen unchanged once more (a copy
    /// in progress keeps changing).
    pending: Option<Stamp>,
    missing_since: Option<f64>,
    next: f64,
}

impl Watch {
    /// Only for the installed app: a bare binary (`cargo run`) is rebuilt all the time.
    pub fn new() -> Option<Watch> {
        if !crate::login::bundled() || crate::sandboxed() { return None; }
        Watch::at(std::env::current_exe().ok()?.canonicalize().ok()?)
    }

    fn at(exe: PathBuf) -> Option<Watch> {
        let running = stamp(&exe)?;
        Some(Watch { exe, running, pending: None, missing_since: None, next: 0.0 })
    }

    /// Looks at most every `EVERY` seconds of `clock`.
    pub fn check(&mut self, clock: f64) -> Option<Change> {
        if clock < self.next { return None; }
        self.next = clock + EVERY;
        let Some(now) = stamp(&self.exe) else {
            self.pending = None;
            let since = *self.missing_since.get_or_insert(clock);
            return (clock - since >= GONE_AFTER).then_some(Change::Removed);
        };
        self.missing_since = None;
        if now == self.running {
            self.pending = None;
            return None;
        }
        if self.pending == Some(now) { return Some(Change::Replaced); }
        self.pending = Some(now);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("notch-installed-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("notch-assistant")
    }

    #[test]
    fn a_new_file_restarts_once_it_stops_changing() {
        let exe = temp("replaced");
        std::fs::write(&exe, b"v1").unwrap();
        let mut w = Watch::at(exe.clone()).unwrap();
        assert_eq!(w.check(0.0), None);
        assert_eq!(w.check(1.0), None, "between looks");
        // An upgrade: the old file goes, a new one (another inode) takes its place.
        std::fs::remove_file(&exe).unwrap();
        assert_eq!(w.check(2.0), None, "missing for a moment");
        std::fs::write(&exe, b"v2, longer").unwrap();
        assert_eq!(w.check(4.0), None, "seen once");
        assert_eq!(w.check(6.0), Some(Change::Replaced), "seen unchanged");
    }

    #[test]
    fn a_file_gone_for_ten_seconds_quits() {
        let exe = temp("removed");
        std::fs::write(&exe, b"v1").unwrap();
        let mut w = Watch::at(exe.clone()).unwrap();
        std::fs::remove_file(&exe).unwrap();
        assert_eq!(w.check(0.0), None);
        assert_eq!(w.check(8.0), None);
        assert_eq!(w.check(10.0), Some(Change::Removed));
    }
}
