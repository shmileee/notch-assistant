//! Calendar client: the meetings in every calendar of the Calendar app, read through EventKit.
//! Read-only, and nothing leaves the Mac.
//!
//! The accounts are whatever System Settings › Internet Accounts has with Calendars turned on
//! (Exchange, Google, iCloud), so there's no key: only the Calendars permission, which macOS asks
//! for on the app's behalf. Only the app bundle can be asked; a binary run from a terminal reads
//! only if the terminal already has access.
//!
//! Settings (`INTEGRATION`): that permission. Once it's granted, `calendar.enabled` in
//! settings.json remembers it, so that losing access later shows as an error rather than as
//! the mock.

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, Once};
use std::time::Duration;

use block2::RcBlock;
use chrono::{DateTime, Local, Utc};
use crate::time::{self, Time};
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{Bool, NSObjectProtocol};
use objc2::{msg_send_id, sel};
use objc2_app_kit::{NSColor, NSColorSpace, NSWorkspace};
use objc2_event_kit::{EKAuthorizationStatus, EKEntityType, EKEvent, EKEventAvailability, EKEventStatus, EKEventStore,
    EKEventStoreChangedNotification, EKParticipantStatus};
use objc2_foundation::{NSDate, NSError, NSNotification, NSNotificationCenter, NSString, NSURL};

use crate::agenda::{self, Agenda, Event, Focus, Reply};
use crate::feed::Fetched;
use crate::integrations::{Access, Field, FieldKind, Integration, Permission};
use crate::oncall::Origin;
use crate::store;

const ID: &str = "calendar";

pub const INTEGRATION: Integration = Integration {
    id: ID,
    name: "Calendar",
    summary: "Your meetings today and tomorrow, from the Calendar app.",
    fields: &[
        Field { key: "access", label: "Calendar access", kind: FieldKind::Permission(&Permission { status, request, open_settings }), env: "",
            help: "Reads every account in System Settings › Internet Accounts that has Calendars turned on (Exchange, Google, iCloud). Nothing leaves your Mac.",
            placeholder: String::new },
    ],
    required: "access",
    test,
    legacy: &[],
};

pub const EVERY: Duration = Duration::from_secs(60);

const PRIVACY_PANE: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_Calendars";

/// Set when the panel's own request is answered, for it to reconnect.
static ANSWERED: AtomicBool = AtomicBool::new(false);
static ASKED_AGAIN: Once = Once::new();
static REMEMBERED: AtomicBool = AtomicBool::new(false);
static SEEN: Mutex<Option<Access>> = Mutex::new(None);
static CHANGED: AtomicBool = AtomicBool::new(false);
static WATCHING: Once = Once::new();

thread_local! {
    /// The store of the thread that fetches, kept while access lasts: EventKit is slow to set
    /// one up, and only a store that's alive posts `EKEventStoreChangedNotification`.
    static STORE: RefCell<Option<Retained<EKEventStore>>> = const { RefCell::new(None) };
}

fn status() -> Access {
    if crate::sandboxed() {
        return if enabled() { Access::Granted } else { Access::NotAsked };
    }
    match unsafe { EKEventStore::authorizationStatusForEntityType(EKEntityType::Event) } {
        EKAuthorizationStatus::FullAccess => { remember(); Access::Granted }
        EKAuthorizationStatus::NotDetermined if crate::login::bundled() => Access::NotAsked,
        EKAuthorizationStatus::NotDetermined => Access::Unavailable,
        EKAuthorizationStatus::Restricted => Access::Restricted,
        // Denied, or write-only: allowed to add events but not to read them.
        _ => Access::Denied,
    }
}

/// Whether access was granted once, whatever macOS says now.
pub fn enabled() -> bool { store::setting(ID, "enabled").is_some() }

/// Remembers the grant once per launch, also when it was given in System Settings rather than
/// in the app's prompt. Only the app bundle does: the flag makes the installed app ask at
/// launch, which `cargo run` from a terminal with access mustn't cause.
fn remember() {
    if REMEMBERED.swap(true, Ordering::SeqCst) || !crate::login::bundled() || enabled() { return; }
    if let Err(e) = store::set_setting(ID, "enabled", "yes") { eprintln!("calendar: {e}"); }
}

/// Whether calendar access changed since the last call, as when it's turned on in System
/// Settings, which tells the app nothing. Cheap enough for every poll.
pub fn access_changed() -> bool {
    if crate::sandboxed() { return false; }
    changed(&mut SEEN.lock().unwrap(), status())
}

/// Keeps `now` as `seen` and says whether it differs; the first look isn't a change.
fn changed(seen: &mut Option<Access>, now: Access) -> bool { seen.replace(now).is_some_and(|before| before != now) }

/// Starts noting changes to the Calendar database, by any app or account, for `take_changed`.
/// Call it once, on the main thread.
pub fn watch_changes() {
    WATCHING.call_once(|| unsafe {
        let noted = RcBlock::new(|_: NonNull<NSNotification>| CHANGED.store(true, Ordering::SeqCst));
        // The center keeps the observer until it's removed, and it never is.
        let _ = NSNotificationCenter::defaultCenter()
            .addObserverForName_object_queue_usingBlock(Some(EKEventStoreChangedNotification), None, None, &noted);
    });
}

/// Whether the Calendar database changed since the last call.
pub fn take_changed() -> bool { CHANGED.swap(false, Ordering::SeqCst) }

fn request(done: Box<dyn FnOnce() + Send>) {
    let answered = move |granted: bool| {
        if granted && let Err(e) = store::set_setting(ID, "enabled", "yes") { eprintln!("calendar: {e}"); }
        done();
    };
    if crate::sandboxed() {
        std::thread::spawn(move || { std::thread::sleep(Duration::from_millis(1200)); answered(true) });
        return;
    }
    if status() != Access::NotAsked { return answered(status() == Access::Granted); }
    let store = unsafe { EKEventStore::new() };
    // The store has to outlive the prompt, so the block holds on to it.
    let (keep, answered) = (store.clone(), Mutex::new(Some(answered)));
    let block = RcBlock::new(move |granted: Bool, _: *mut NSError| {
        let _ = &keep;
        if let Some(answered) = answered.lock().unwrap().take() { answered(granted.as_bool()) }
    });
    unsafe {
        if store.respondsToSelector(sel!(requestFullAccessToEventsWithCompletion:)) {
            store.requestFullAccessToEventsWithCompletion(&*block as *const _ as *mut _);
        } else {
            // macOS 13 has only the older call.
            #[allow(deprecated)]
            store.requestAccessToEntityType_completion(EKEntityType::Event, &*block as *const _ as *mut _);
        }
    }
}

/// Asks again when access was granted once but macOS no longer knows it. macOS ties the answer
/// to the app's signature, and an ad-hoc signature changes with every build, upgrades included.
pub fn ask_again() {
    if enabled() && status() == Access::NotAsked {
        ASKED_AGAIN.call_once(|| request(Box::new(|| ANSWERED.store(true, Ordering::SeqCst))));
    }
}

/// Whether `ask_again`'s request was answered since the last call.
pub fn answered() -> bool { ANSWERED.swap(false, Ordering::SeqCst) }

fn open_settings() { open(PRIVACY_PANE) }

/// Opens a link with the app that handles it, for the schemes Rune's `open_url` refuses.
fn open(url: &str) {
    if crate::sandboxed() { return; }
    let opened = unsafe { NSURL::URLWithString(&NSString::from_str(url)).is_some_and(|u| NSWorkspace::sharedWorkspace().openURL(&u)) };
    if !opened { eprintln!("calendar: can't open {url}"); }
}

/// Shows a meeting in the Calendar app. The `ical:` link isn't documented, and when it doesn't
/// find the meeting Calendar still opens.
pub fn show(e: &Event) {
    if e.id.is_empty() { return; }
    open(&format!("ical://ekevent/{}/{}?method=show&options=more", e.start.with_timezone(&Utc).format("%Y%m%dT%H%M%SZ"), e.id));
}

fn test() -> Result<String, String> {
    if crate::sandboxed() {
        std::thread::sleep(Duration::from_millis(1200));
        return Ok("Reading 3 calendars: 4 meetings left today, next 1:1 with ZQ at 15:00".into());
    }
    let now = Local::now();
    let found = fetch(now)?;
    let day = agenda::day(&found.events, now);
    let left = day.later.len() + day.focus.iter().count();
    let next = day.later.iter().copied().chain(day.focus.map(Focus::event)).filter(|e| e.start > now && e.busy()).min_by_key(|e| e.start);
    let next = next.map(|e| format!(", next {} at {}", e.title, e.start.format("%H:%M"))).unwrap_or_default();
    Ok(format!("Reading {} calendars: {left} meetings left today{next}", found.calendars))
}

impl Fetched for Agenda { fn origin(&mut self) -> &mut Origin { &mut self.origin } }

pub fn fetcher() -> impl FnMut() -> Result<Agenda, String> + Send + 'static {
    || fetch(Local::now())
}

/// Today's and tomorrow's meetings from every calendar. Never asks for access, and never reads
/// in demo mode or headless, where access is only pretended.
pub fn fetch(now: Time) -> Result<Agenda, String> {
    if crate::sandboxed() { return Err("Calendar isn't read in demo mode".into()); }
    let access = status();
    if access != Access::Granted { STORE.set(None); }
    match access {
        Access::Granted => {}
        Access::Restricted => return Err("Calendar access is blocked by whoever manages this Mac".into()),
        Access::Unavailable => return Err("Calendar access needs the installed app, or a terminal that has access to Calendars".into()),
        Access::NotAsked | Access::Denied => return Err("Calendar access is off: allow it in Settings › Calendar".into()),
    }
    let today = now.date_naive();
    let (from, to) = (time::midnight(today), time::midnight(today + chrono::Duration::days(2)));
    // A surprise from EventKit must show as an error, not end the thread that refreshes.
    catch_unwind(AssertUnwindSafe(|| autoreleasepool(|_| STORE.with_borrow_mut(|store| unsafe {
        let store = store.get_or_insert_with(|| EKEventStore::new());
        let calendars = store.calendarsForEntityType(EKEntityType::Event).len();
        let predicate = store.predicateForEventsWithStartDate_endDate_calendars(&date(from), &date(to), None);
        let mut events = one_by_one(store.eventsMatchingPredicate(&predicate).to_vec(), event);
        events.sort_by_key(|e| (e.start, e.end));
        Agenda { calendars, events, origin: Origin::Live { at: now } }
    })))).map_err(|_| {
        STORE.set(None);
        "Reading the calendars failed".to_string()
    })
}

/// Converts each item on its own, so one that panics is skipped instead of failing the batch.
fn one_by_one<T, U>(items: impl IntoIterator<Item = T>, convert: impl Fn(T) -> Option<U>) -> Vec<U> {
    items.into_iter().filter_map(|item| catch_unwind(AssertUnwindSafe(|| convert(item))).unwrap_or_else(|_| {
        eprintln!("calendar: skipped an event that couldn't be read");
        None
    })).collect()
}

/// An event as the panel keeps it; `None` for one that was cancelled, that you declined, or
/// that has no dates.
fn event(e: &EKEvent) -> Option<Event> {
    unsafe {
        if e.status() == EKEventStatus::Canceled { return None; }
        let list = e.attendees();
        let attendees = list.as_deref().map(|a| a.to_vec()).unwrap_or_default();
        let me = attendees.iter().find(|p| p.isCurrentUser());
        let reply = reply(me.map(|p| p.participantStatus()))?;
        let mine = mine(!attendees.is_empty(), me.is_some(), e.organizer().is_some_and(|o| o.isCurrentUser()));
        // The headers allow nil for these, which the bindings' plain getters would panic on.
        let title: Option<Retained<NSString>> = msg_send_id![e, title];
        let start: Option<Retained<NSDate>> = msg_send_id![e, startDate];
        let end: Option<Retained<NSDate>> = msg_send_id![e, endDate];
        let text = |s: Option<Retained<NSString>>| s.map(|s| s.to_string()).unwrap_or_default();
        let (calendar, color) = e.calendar().map(|c| {
            let color: Option<Retained<NSColor>> = msg_send_id![&c, color];
            (c.title().to_string(), color.map_or(GREY, |c| rgb(&c)))
        }).unwrap_or_default();
        let (link, location, notes) = (text(e.URL().and_then(|u| u.absoluteString())), text(e.location()), text(e.notes()));
        Some(Event {
            id: e.calendarItemIdentifier().to_string(),
            title: text(title),
            start: local(&*start?),
            end: local(&*end?),
            all_day: e.isAllDay(),
            calendar,
            color,
            join: agenda::join_link([link.as_str(), location.as_str(), notes.as_str()]),
            location,
            reply,
            free: e.availability() == EKEventAvailability::Free,
            mine,
        })
    }
}

/// Your answer, from your own entry among the attendees; `None` when you declined.
fn reply(status: Option<EKParticipantStatus>) -> Option<Reply> {
    match status {
        Some(EKParticipantStatus::Declined) => None,
        Some(EKParticipantStatus::Tentative) => Some(Reply::Maybe),
        Some(EKParticipantStatus::Pending) => Some(Reply::NotYet),
        _ => Some(Reply::Yes),
    }
}

/// Whether a meeting is yours rather than a colleague's on a shared calendar.
fn mine(has_guests: bool, guest: bool, organizer: bool) -> bool { !has_guests || guest || organizer }

const GREY: u32 = 0x8e8e93;

fn rgb(color: &NSColor) -> u32 {
    let Some(c) = (unsafe { color.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace()) }) else { return GREY };
    let channel = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    unsafe { channel(c.redComponent()) << 16 | channel(c.greenComponent()) << 8 | channel(c.blueComponent()) }
}

fn local(d: &NSDate) -> Time {
    let secs = unsafe { d.timeIntervalSince1970() };
    DateTime::from_timestamp(secs.floor() as i64, 0).map(|t| t.with_timezone(&Local)).unwrap_or_default()
}

fn date(t: Time) -> Retained<NSDate> {
    unsafe { NSDate::dateWithTimeIntervalSince1970(t.timestamp() as f64) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declined_meetings_are_dropped_and_no_invitation_counts_as_yes() {
        assert_eq!(reply(Some(EKParticipantStatus::Declined)), None);
        assert_eq!(reply(Some(EKParticipantStatus::Tentative)), Some(Reply::Maybe));
        assert_eq!(reply(Some(EKParticipantStatus::Pending)), Some(Reply::NotYet));
        assert_eq!(reply(Some(EKParticipantStatus::Accepted)), Some(Reply::Yes));
        assert_eq!(reply(None), Some(Reply::Yes));
    }

    #[test]
    fn a_meeting_with_guests_is_yours_only_as_a_guest_or_the_organizer() {
        assert!(mine(false, false, false), "an event of your own, with no guests");
        assert!(mine(true, true, false));
        assert!(mine(true, false, true), "the organizer isn't always among the guests");
        assert!(!mine(true, false, false), "a colleague's meeting on a shared calendar");
    }

    #[test]
    fn an_event_that_panics_is_skipped_and_the_rest_are_kept() {
        let kept = one_by_one([1, 2, 3, 4], |n| if n == 3 { panic!("an odd event") } else { (n != 1).then_some(n) });
        assert_eq!(kept, [2, 4]);
    }

    #[test]
    fn access_counts_as_changed_only_after_a_first_look() {
        let mut seen = None;
        assert!(!changed(&mut seen, Access::NotAsked), "the first look");
        assert!(!changed(&mut seen, Access::NotAsked));
        assert!(changed(&mut seen, Access::Granted), "turned on in System Settings");
        assert!(!changed(&mut seen, Access::Granted));
        assert!(changed(&mut seen, Access::Denied));
    }

    #[test]
    fn dates_survive_the_trip_through_nsdate() {
        let t = time::at(2026, 11, 10, 14, 5);
        assert_eq!(local(&date(t)), t);
    }
}
