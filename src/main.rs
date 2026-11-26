//! Notch assistant: a panel that springs out of the MacBook notch when you hover it.
//!
//! `cargo run --release` puts it at the top of whichever display the pointer is on. Headless stills, with no window:
//! `cargo run -- --scene open --snap snaps/open.png` (scenes: collapsed, open, incidents,
//! offhours, pulls, reviews, linear, calendar, peek, soon).
//!
//! Open, the panel has a sidebar of integrations (FireHydrant, GitHub, Linear, Calendar) and
//! shows the selected one's data beside it.

mod agenda;
mod anchor;
mod calendar;
mod demo;
mod feed;
mod firehydrant;
mod github;
mod installed;
mod integrations;
mod island;
mod issues;
mod linear;
mod login;
mod net;
mod notch;
mod oncall;
mod pulls;
mod read;
mod settings;
mod store;
mod time;
mod view;
mod wake;

use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Duration;

use chrono::Local;
use crate::time::Time;
use rune::machine::Seq;
use rune::{App, AppHandle, AppSpec, DisplayTarget, OverlayHandle, OverlayInput, OverlayLevel, OverlayOptions, OverlayPlacement, OverlaySpec, RunOptions, Tx, WindowSpec};

use agenda::Agenda;
use island::{Event, Island};
use notch::NotchGeo;
use oncall::{Data, Origin, Snapshot};
use feed::Feed;
use issues::Issues;
use pulls::Pulls;
use read::ReadState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section { FireHydrant, GitHub, Linear, Calendar }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FireView { OnCall, Incidents }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullView { Yours, Reviews }

/// Something new, shown under the collapsed island for a few seconds. Opening the panel from
/// it goes to its section.
#[derive(Clone, Debug, PartialEq)]
pub struct Peek {
    pub section: Section,
    /// For GitHub, which list.
    pub pulls: Option<PullView>,
    pub mark: Mark,
    pub headline: String,
    pub detail: String,
    pub until: f64,
    /// For a meeting, its `agenda::Event::key`, so a peek that waited can update its countdown.
    pub meeting: Option<String>,
}

/// What a peek shows beside its text.
#[derive(Clone, Debug, PartialEq)]
pub enum Mark {
    /// Who did it, as an avatar.
    Person(String),
    /// The time to a meeting, on its calendar's colour.
    Countdown { text: String, color: u32 },
}

const PEEK_SECONDS: f64 = 6.0;
/// Peeks waiting for the one on show, at most.
const QUEUED: usize = 4;

pub struct State {
    pub geo: NotchGeo,
    pub island: Island,
    pub seq: Seq,
    pub section: Section,
    pub fire: FireView,
    pub pulls: PullView,
    /// The latest fetch (or the mock), and what the view shows from it at `snap.now`.
    pub data: Data,
    pub snap: Snapshot,
    /// Results from the FireHydrant thread, when there is a token.
    pub results: Option<Receiver<Result<Data, String>>>,
    /// Keeps that thread alive; dropping it stops the thread.
    pub control: Option<Sender<()>>,
    /// Settings being read for each integration's next connection (see `read_settings`).
    pub reading: Reading,
    pub github: Feed<Pulls>,
    /// The review requests already announced, kept apart from your own pull requests' so it
    /// starts counting only once the requests have loaded: counting from a list that failed to
    /// load would announce every old request.
    pub reviews_announced: read::Announced,
    pub linear: Feed<Issues>,
    pub calendar: Feed<Agenda>,
    /// The meetings already peeked at as starting, by `agenda::Event::key`.
    pub reminded: HashSet<String>,
    pub peek: Option<Peek>,
    /// Peeks waiting their turn: behind the one on show, or until no app is full screen.
    pub queued: VecDeque<Peek>,
    pub waker: Option<rune::Waker>,
    /// Scenes pin the clock and the notch so snapshots are reproducible.
    pub frozen: bool,
    /// The installed app's executable, to restart on a new version or quit once removed.
    pub installed: Option<installed::Watch>,
    pub anchor: anchor::Anchor,
}

/// Each integration's settings on their way from `read_settings`.
#[derive(Default)]
pub struct Reading {
    firehydrant: Option<Receiver<Option<firehydrant::Client>>>,
    github: Option<Receiver<Option<github::Client>>>,
    linear: Option<Receiver<Option<linear::Client>>>,
}

/// Reads an integration's saved settings on a thread and wakes the panel with them. Secrets come
/// from the Keychain, which blocks for as long as its unlock prompt is up, and the panel must
/// keep drawing meanwhile.
fn read_settings<C: Send + 'static>(read: fn() -> Option<C>, wake: rune::Waker) -> Receiver<Option<C>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || { let _ = tx.send(read()); wake(); });
    rx
}

/// Connects each integration whose settings have arrived. True when any did.
fn take_settings(s: &mut State) -> bool {
    fn take<C>(rx: &mut Option<Receiver<Option<C>>>) -> Option<Option<C>> {
        let got = rx.as_ref()?.try_recv().ok()?;
        *rx = None;
        Some(got)
    }
    let mut changed = false;
    if let Some(client) = take(&mut s.reading.firehydrant) { start_firehydrant(s, client); changed = true; }
    if let Some(client) = take(&mut s.reading.github) { start_github(s, client); changed = true; }
    if let Some(client) = take(&mut s.reading.linear) { start_linear(s, client); changed = true; }
    changed
}

/// Mock data only: headless tooling, or `NOTCH_MOCK`. Set once in `main`.
static MOCK_ONLY: OnceLock<bool> = OnceLock::new();
/// Headless tooling (snapshots, scripts). Set once in `main`.
static HEADLESS: OnceLock<bool> = OnceLock::new();

/// Demo mode or headless tooling: settings in memory and no personal details (your name,
/// Keychain, login items), so recordings and snapshots show nothing of yours.
pub fn sandboxed() -> bool { demo::on() || *HEADLESS.get().unwrap_or(&false) }

thread_local! {
    /// The Settings window, opened from the panel (handlers run on the main thread).
    static SETTINGS: RefCell<Option<AppHandle<settings::Settings>>> = const { RefCell::new(None) };
    /// Demo mode: where Settings opens (a display and a rect in its points).
    static SETTINGS_PLACE: RefCell<Option<(DisplayTarget, OverlayPlacement)>> = const { RefCell::new(None) };
    /// The panel, for `anchor` to put back where it belongs.
    static OVERLAY: RefCell<Option<OverlayHandle<State>>> = const { RefCell::new(None) };
}

pub fn open_settings() {
    SETTINGS.with(|s| if let Some(handle) = s.borrow().as_ref() {
        handle.update(|s, _| s.load());
        if let Some((display, rect)) = SETTINGS_PLACE.with(|p| p.borrow().clone()) { handle.place(display, rect); }
        handle.open();
        handle.focus();
    });
}

pub fn open_settings_for(id: &'static str) {
    SETTINGS.with(|s| if let Some(handle) = s.borrow().as_ref() { handle.update(move |s, _| s.select_id(id)) });
    open_settings();
}

/// Quits the app. The panel's × can't use `Tx::quit`: an overlay has no window, and Rune drops
/// window requests (close, quit) made from one, so the quit goes through the Settings handle.
pub fn quit() {
    SETTINGS.with(|s| if let Some(handle) = s.borrow().as_ref() { handle.quit() });
}

/// Quits and starts the app again from its bundle (a new version installed over it).
fn relaunch() {
    SETTINGS.with(|s| if let Some(handle) = s.borrow().as_ref() { handle.relaunch() });
}

/// The time the panel shows: pinned in demo mode.
fn now() -> Time { if demo::on() { demo::time() } else { Local::now() } }

/// Who "you" are in the mock rotation (`NOTCH_ME`, default OP).
fn mock_me() -> String { std::env::var("NOTCH_ME").unwrap_or_else(|_| "OP".into()) }

fn init() -> State {
    let data = oncall::mock(&mock_me(), now());
    State {
        geo: notch::under_pointer(),
        island: Island::Collapsed,
        seq: Seq::default(),
        section: Section::FireHydrant,
        fire: FireView::OnCall,
        pulls: PullView::Yours,
        snap: Snapshot::build(&data, now()),
        data,
        results: None,
        control: None,
        reading: Reading::default(),
        github: Feed::new(pulls::mock(now()), mock_read()),
        reviews_announced: read::Announced::default(),
        linear: Feed::new(issues::mock(now()), mock_read()),
        calendar: Feed::new(agenda::mock(now()), mock_read()),
        reminded: HashSet::new(),
        peek: None,
        queued: VecDeque::new(),
        waker: None,
        frozen: false,
        installed: installed::Watch::new(),
        anchor: anchor::Anchor::default(),
    }
}

/// The mock's read state: counting from long before its events, so some are unread.
fn mock_read() -> ReadState { ReadState::memory(now() - chrono::Duration::days(30)) }

fn mock_only() -> bool { demo::on() || *MOCK_ONLY.get().unwrap_or(&false) }

/// The overlay's setup: keep the waker and connect.
fn start_fetching(s: &mut State, wake: rune::Waker) {
    s.waker = Some(wake);
    wake::watch();
    calendar::watch_changes();
    connect_firehydrant(s);
    connect_github(s);
    connect_linear(s);
    connect_calendar(s);
}

/// Reconnects one integration after Settings saved it.
pub fn reconnect(s: &mut State, id: &str) {
    match id {
        "github" => connect_github(s),
        "linear" => connect_linear(s),
        "calendar" => connect_calendar(s),
        _ => connect_firehydrant(s),
    }
}

/// (Re)connects to FireHydrant with the saved settings, stopping any earlier connection;
/// without an API key, back to mock data.
fn connect_firehydrant(s: &mut State) {
    s.control = None; // stops the old thread
    s.results = None;
    if demo::on() {
        // Demo: the mock, shown as live once a key is "saved" in Settings.
        s.data = oncall::mock("OP", now());
        if firehydrant::INTEGRATION.configured() { s.data.origin = Origin::Live { at: now() }; }
        s.snap = Snapshot::build(&s.data, now());
        return;
    }
    match s.waker.clone().filter(|_| !*MOCK_ONLY.get().unwrap_or(&false)) {
        Some(wake) => {
            s.reading.firehydrant = Some(read_settings(firehydrant::Client::from_settings, wake));
            s.data = Data::loading();
        }
        None => s.data = oncall::mock(&mock_me(), now()),
    }
    s.snap = Snapshot::build(&s.data, now());
}

fn start_firehydrant(s: &mut State, client: Option<firehydrant::Client>) {
    match (client, s.waker.clone()) {
        (Some(client), Some(wake)) => {
            let (tx, rx) = std::sync::mpsc::channel();
            s.control = Some(feed::every("firehydrant", Duration::from_secs(300), move || client.fetch(now()), tx, wake));
            s.results = Some(rx);
            s.data = Data::loading();
        }
        _ => s.data = oncall::mock(&mock_me(), now()),
    }
    s.snap = Snapshot::build(&s.data, now());
}

/// (Re)connects to GitHub with the saved settings, as `connect_firehydrant` does.
fn connect_github(s: &mut State) {
    forget_peeks(s, Section::GitHub);
    s.reviews_announced = read::Announced::default();
    match s.waker.clone().filter(|_| !mock_only()) {
        Some(wake) => {
            s.reading.github = Some(read_settings(github::Client::from_settings, wake));
            s.github.show(Pulls::loading(), mock_read());
        }
        None => start_github(s, None),
    }
}

fn start_github(s: &mut State, client: Option<github::Client>) {
    match (client, s.waker.clone()) {
        (Some(client), Some(wake)) => s.github.start("github", github::EVERY, client.fetcher(), wake, Pulls::loading(),
            ReadState::load(pulls::READ_FILE, now())),
        _ => {
            let mut pulls = pulls::mock(now());
            if demo::on() && github::INTEGRATION.configured() { pulls.origin = Origin::Live { at: now() }; }
            s.github.show(pulls, mock_read());
        }
    }
}

/// Takes in finished GitHub fetches and peeks the newest new event, unless the panel is open.
/// Only good data counts toward what's new: after a failure the list is still empty, and
/// counting from it would make the next good fetch announce everything already unread.
fn take_pulls(s: &mut State, clock: f64) -> bool {
    let gh = &mut s.github;
    let took = gh.take(|read, pulls| read.sync(pulls.list.iter().chain(&pulls.reviews).map(|p| (p.id.as_str(), p.read_on_github)), now()));
    if !took.fresh { return took.changed; }
    let mut new = gh.announced.update(pulls::unread_events(&gh.data.list, &gh.read));
    if gh.data.reviews_loaded { new.extend(s.reviews_announced.update(pulls::unread_events(&gh.data.reviews, &gh.read))); }
    let peek = new.into_iter().max_by_key(|(_, a)| a.at).filter(|_| !s.island.is_open()).map(|(p, a)| Peek {
        section: Section::GitHub,
        pulls: Some(if p.request.is_some() { PullView::Reviews } else { PullView::Yours }),
        mark: Mark::Person(a.who.clone()),
        headline: format!("{} on #{}", a.headline(), p.number),
        detail: if a.excerpt.is_empty() { p.title.clone() } else { a.excerpt.clone() },
        until: 0.0,
        meeting: None,
    });
    if let Some(peek) = peek { offer(s, peek, clock); }
    true
}

/// (Re)connects to Linear with the saved settings, as `connect_firehydrant` does.
fn connect_linear(s: &mut State) {
    forget_peeks(s, Section::Linear);
    match s.waker.clone().filter(|_| !mock_only()) {
        Some(wake) => {
            s.reading.linear = Some(read_settings(linear::Client::from_settings, wake));
            s.linear.show(Issues::loading(), mock_read());
        }
        None => start_linear(s, None),
    }
}

fn start_linear(s: &mut State, client: Option<linear::Client>) {
    match (client, s.waker.clone()) {
        (Some(client), Some(wake)) => s.linear.start("linear", linear::EVERY, client.fetcher(), wake, Issues::loading(),
            ReadState::load(issues::READ_FILE, now() - issues::look_back())),
        _ => {
            let mut found = issues::mock(now());
            if demo::on() && linear::INTEGRATION.configured() { found.origin = Origin::Live { at: now() }; }
            s.linear.show(found, mock_read());
        }
    }
}

/// Takes in finished Linear fetches and peeks the newest new notification, as `take_pulls` does.
fn take_issues(s: &mut State, clock: f64) -> bool {
    let ln = &mut s.linear;
    let took = ln.take(|read, found| read.sync(found.list.iter().map(|t| (t.id.as_str(), None)), now()));
    if !took.fresh { return took.changed; }
    let new = ln.announced.update(issues::unread_events(&ln.data.list, &ln.read));
    let peek = new.into_iter().max_by_key(|(_, n)| n.at).filter(|_| !s.island.is_open()).map(|(t, n)| Peek {
        section: Section::Linear,
        pulls: None,
        mark: Mark::Person(if n.who.is_empty() { "Linear".into() } else { n.who.clone() }),
        headline: format!("{} on {}", n.headline, if t.key.is_empty() { &t.title } else { &t.key }),
        detail: if n.excerpt.is_empty() { t.title.clone() } else { n.excerpt.clone() },
        until: 0.0,
        meeting: None,
    });
    if let Some(peek) = peek { offer(s, peek, clock); }
    true
}

/// (Re)connects to the Calendar app, as `connect_firehydrant` does. Once access was granted,
/// losing it shows as an error rather than as the mock.
fn connect_calendar(s: &mut State) {
    forget_peeks(s, Section::Calendar);
    let live = !mock_only() && (calendar::INTEGRATION.configured() || calendar::enabled());
    match (live, s.waker.clone()) {
        (true, Some(wake)) => {
            calendar::ask_again();
            s.calendar.start("calendar", calendar::EVERY, calendar::fetcher(), wake, Agenda::loading(), mock_read());
        }
        _ => {
            let mut found = agenda::mock(now());
            if demo::on() && calendar::INTEGRATION.configured() { found.origin = Origin::Live { at: now() }; }
            s.calendar.show(found, mock_read());
        }
    }
}

/// Takes in finished calendar reads, and peeks once at each meeting about to start. The peek
/// waits while the panel is open.
fn take_agenda(s: &mut State, clock: f64) -> bool {
    // Both are read every time: each remembers what it last saw.
    if calendar::answered() | calendar::access_changed() { connect_calendar(s); }
    if calendar::take_changed() { s.calendar.refresh(); }
    let arrived = s.calendar.take(|_, _| {}).changed;
    let events = &s.calendar.data.events;
    if arrived { s.reminded.retain(|key| events.iter().any(|e| e.key() == *key)); }
    if !matches!(s.calendar.data.origin, Origin::Live { .. }) || s.island.is_open() { return arrived; }
    let now = now();
    let Some(e) = agenda::starting(events, now, agenda::lead()).into_iter().find(|e| !s.reminded.contains(&e.key())) else { return arrived };
    s.reminded.insert(e.key());
    let peek = Peek {
        section: Section::Calendar,
        pulls: None,
        mark: Mark::Countdown { text: agenda::until(e.start - now), color: e.color },
        headline: e.title.clone(),
        detail: format!("Starts at {} · {}", e.start.format("%H:%M"), e.place()),
        until: 0.0,
        meeting: Some(e.key()),
    };
    offer(s, peek, clock);
    true
}

/// Drops the peeks, on show or waiting, of an integration that is reconnecting.
fn forget_peeks(s: &mut State, section: Section) {
    if s.peek.as_ref().is_some_and(|p| p.section == section) { s.peek = None; }
    s.queued.retain(|q| q.section != section);
}

/// Shows a peek now, or once the ones before it have had their turn. Meetings go before the
/// rest, and a newer peek for the same list takes the place of one still waiting. Nothing shows
/// while an app is full screen where the panel is, so a slideshow or a shared screen doesn't
/// get a comment or an incident dropped over it.
fn offer(s: &mut State, peek: Peek, clock: f64) {
    if s.peek.is_none() && s.queued.is_empty() && !notch::full_screen_under_pointer() {
        s.peek = Some(Peek { until: clock + PEEK_SECONDS, ..peek });
        return;
    }
    queue(&mut s.queued, peek);
}

fn queue(queued: &mut VecDeque<Peek>, peek: Peek) {
    if peek.meeting.is_none() { queued.retain(|q| q.meeting.is_some() || (q.section, q.pulls) != (peek.section, peek.pulls)); }
    let at = if peek.meeting.is_some() { queued.iter().take_while(|q| q.meeting.is_some()).count() } else { queued.len() };
    queued.insert(at, peek);
    while queued.len() > QUEUED {
        let oldest = queued.iter().position(|q| q.meeting.is_none()).unwrap_or(0);
        queued.remove(oldest);
    }
}

/// The next waiting peek that still means something: a meeting that hasn't started, with its
/// countdown brought up to now.
fn next_peek(s: &mut State, clock: f64) -> Option<Peek> {
    let now = now();
    while let Some(mut peek) = s.queued.pop_front() {
        if let Some(key) = &peek.meeting {
            let Some(e) = s.event(key).filter(|e| e.start > now) else { continue };
            peek.mark = Mark::Countdown { text: agenda::until(e.start - now), color: e.color };
        }
        peek.until = clock + PEEK_SECONDS;
        return Some(peek);
    }
    None
}

impl State {
    fn event(&self, key: &str) -> Option<&agenda::Event> { self.calendar.data.events.iter().find(|e| e.key() == key) }

    /// Shows a meeting in the Calendar app.
    pub fn show_event(&mut self, key: &str) {
        if let Some(e) = self.event(key) { calendar::show(e); }
    }

    /// Opens a meeting's call.
    pub fn join(&mut self, key: &str, tx: &mut Tx) {
        let Some(url) = self.event(key).and_then(|e| e.join.as_ref()).map(|j| j.url.clone()).filter(|u| !u.is_empty()) else { return };
        if let Err(e) = tx.open_url(&url) { eprintln!("calendar: can't open {url}: {e:?}"); }
    }

    /// Opens an issue or project where its oldest unread notification points, and marks it read.
    pub fn open_ticket(&mut self, id: &str, tx: &mut Tx) {
        let ln = &mut self.linear;
        let Some(t) = ln.data.list.iter().find(|t| t.id == id) else { return };
        let url = t.unread(&ln.read).first().map(|n| n.url.as_str()).filter(|u| !u.is_empty()).unwrap_or(&t.url).to_string();
        ln.read.mark(id, now());
        if !url.is_empty() && let Err(e) = tx.open_url(&url) { eprintln!("linear: can't open {url}: {e:?}"); }
    }

    pub fn mark_issues_read(&mut self) {
        let ln = &mut self.linear;
        ln.read.mark_all(ln.data.list.iter().filter_map(|t| Some((t.id.as_str(), t.notices.last()?.at))));
    }

    /// Opens a pull request where its oldest unread event is, and marks it read.
    pub fn open_pull(&mut self, id: &str, tx: &mut Tx) {
        let gh = &mut self.github;
        let Some(pull) = gh.data.list.iter().chain(&gh.data.reviews).find(|p| p.id == id) else { return };
        let url = pull.unread(&gh.read).first().map_or(pull.url.as_str(), |a| a.url.as_str()).to_string();
        gh.read.mark(id, now());
        if !url.is_empty() && let Err(e) = tx.open_url(&url) { eprintln!("github: can't open {url}: {e:?}"); }
    }

    /// Marks the list on show read.
    pub fn mark_pulls_read(&mut self) {
        let gh = &mut self.github;
        let list = if self.pulls == PullView::Reviews { &gh.data.reviews } else { &gh.data.list };
        gh.read.mark_all(list.iter().filter_map(|p| Some((p.id.as_str(), p.activity.last()?.at))));
    }

    /// Opening the panel from a peek goes to what it showed.
    pub fn follow_peek(&mut self) {
        let Some(peek) = &self.peek else { return };
        self.section = peek.section;
        if let Some(view) = peek.pulls { self.pulls = view; }
    }
}

/// Takes in finished fetches. A failure keeps the last good data and says so.
fn take_results(s: &mut State) -> bool {
    let Some(rx) = &s.results else { return false };
    let (mut changed, mut stopped) = (false, false);
    loop {
        match rx.try_recv() {
            Ok(Ok(data)) => s.data = data,
            Ok(Err(message)) => feed::fail(&mut s.data.origin, message),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => { stopped = true; break; }
        }
        changed = true;
    }
    if stopped {
        (s.results, s.control) = (None, None);
        feed::fail(&mut s.data.origin, "Refreshing stopped. Save FireHydrant in Settings to start it again".into());
        changed = true;
    }
    changed
}

/// Fires the island's timers and refreshes the data each minute. True only when something
/// the view shows changed, so an idle island draws nothing.
fn poll(s: &mut State, clock: f64, tx: &mut Tx) -> bool {
    let mut changed = false;
    match s.installed.as_mut().and_then(|w| w.check(clock)) {
        Some(installed::Change::Replaced) => { eprintln!("installed: a new version, restarting"); relaunch(); }
        Some(installed::Change::Removed) => { eprintln!("installed: the app was removed, quitting"); quit(); }
        None => {}
    }
    // The overlay follows the pointer between displays; reshape the island to the notch (real
    // or virtual) of the display it's now on, without animating the jump.
    if !s.frozen {
        OVERLAY.with(|o| if let Some(overlay) = o.borrow().as_ref() { s.anchor.keep(overlay) });
        let geo = notch::under_pointer();
        if geo != s.geo {
            eprintln!("notch: {geo:?}");
            s.geo = geo;
            tx.instant();
            changed = true;
        }
    }
    if let Some((_, due)) = s.island.due() {
        if clock >= due {
            let before = s.island.is_open();
            let _ = island::step(&mut s.seq, &mut s.island, Event::Due { now: clock });
            changed |= before != s.island.is_open() || s.island.due().is_none();
        } else {
            tx.wake_at(due);
        }
    }
    if !s.frozen {
        if s.island.is_open() {
            changed |= s.peek.take().is_some() || !s.queued.is_empty();
            // A meeting still waiting its turn wasn't seen, so it may peek once the panel closes.
            for waiting in s.queued.drain(..) {
                if let Some(key) = waiting.meeting { s.reminded.remove(&key); }
            }
        } else if s.peek.as_ref().is_some_and(|p| clock >= p.until) {
            s.peek = None;
            changed = true;
        }
        if s.peek.is_none() && !s.queued.is_empty() && !s.island.is_open() && !notch::full_screen_under_pointer() {
            s.peek = next_peek(s, clock);
            changed |= s.peek.is_some();
        }
        if let Some(peek) = &s.peek { tx.wake_at(peek.until); }
        if wake::take() {
            eprintln!("wake: refreshing everything");
            s.github.refresh();
            s.linear.refresh();
            s.calendar.refresh();
            if let Some(control) = &s.control { let _ = control.send(()); }
        }
    }
    if !s.frozen {
        let t = now();
        changed |= take_settings(s);
        changed |= take_pulls(s, clock);
        changed |= take_issues(s, clock);
        changed |= take_agenda(s, clock);
        let fetched = take_results(s);
        if fetched || t.format("%H:%M").to_string() != s.snap.now.format("%H:%M").to_string() {
            s.snap = Snapshot::build(&s.data, t);
            changed = true;
        }
        // Check again in a second: that catches the pointer reaching another display even if
        // nothing else wakes us, and the minute ticking over. A wake that changes nothing
        // returns false and draws nothing.
        tx.wake_after(Duration::from_secs(1));
    }
    changed
}

/// Tuesday 10 November 2026, 14:05: the user is primary this week.
fn scene_time(day: u32, h: u32, m: u32) -> Time { time::at(2026, 11, day, h, m) }
fn freeze(s: &mut State, at: Time) {
    s.frozen = true;
    s.geo = NotchGeo { width: 185.0, height: 32.0, real: true };
    s.data = oncall::mock("OP", at);
    s.snap = Snapshot::build(&s.data, at);
    s.github.show(pulls::mock(at), ReadState::memory(at - chrono::Duration::days(30)));
    s.linear.show(issues::mock(at), ReadState::memory(at - issues::look_back()));
    s.calendar.show(agenda::mock(at), ReadState::memory(at));
}

fn spec() -> AppSpec<State> {
    AppSpec {
        scenes: &[
            ("collapsed", |s| freeze(s, scene_time(10, 14, 5))),
            ("open", |s| { freeze(s, scene_time(10, 14, 5)); s.island = Island::Open; }),
            ("incidents", |s| { freeze(s, scene_time(10, 14, 5)); s.island = Island::Open; s.fire = FireView::Incidents; }),
            ("offhours", |s| { freeze(s, scene_time(14, 11, 30)); s.island = Island::Open; }),
            ("pulls", |s| { freeze(s, scene_time(10, 14, 5)); s.island = Island::Open; s.section = Section::GitHub; }),
            ("reviews", |s| { freeze(s, scene_time(10, 14, 5)); s.island = Island::Open; s.section = Section::GitHub; s.pulls = PullView::Reviews; }),
            ("linear", |s| { freeze(s, scene_time(10, 14, 5)); s.island = Island::Open; s.section = Section::Linear; }),
            ("calendar", |s| { freeze(s, scene_time(10, 14, 5)); s.island = Island::Open; s.section = Section::Calendar; }),
            ("peek", |s| {
                freeze(s, scene_time(10, 14, 5));
                let (pull, a) = (&s.github.data.list[0], &s.github.data.list[0].activity[1]);
                s.peek = Some(Peek { section: Section::GitHub, pulls: Some(PullView::Yours), mark: Mark::Person(a.who.clone()),
                    headline: format!("{} on #{}", a.headline(), pull.number), detail: a.excerpt.clone(), until: 0.0, meeting: None });
            }),
            ("soon", |s| {
                let at = scene_time(10, 14, 56);
                freeze(s, at);
                let e = s.calendar.data.events.iter().find(|e| e.title == "1:1 with ZQ").unwrap();
                s.peek = Some(Peek { section: Section::Calendar, pulls: None, mark: Mark::Countdown { text: agenda::until(e.start - at), color: e.color },
                    headline: e.title.clone(), detail: format!("Starts at {} · {}", e.start.format("%H:%M"), e.place()), until: 0.0, meeting: Some(e.key()) });
            }),
        ],
        poll: Some(poll),
        ..AppSpec::new("Notch", view::CANVAS, init, view::view)
    }
}

/// `--probe` prints what the app would show from FireHydrant; `--probe-raw` the raw JSON.
fn probe(raw: bool) {
    let Some(client) = firehydrant::Client::from_settings() else {
        eprintln!("No FireHydrant API key: add one in Settings (the gear in the panel) or set FIREHYDRANT_API_TOKEN.");
        std::process::exit(1);
    };
    if raw {
        match client.raw(now()) {
            Ok(calls) => for (name, json) in calls { println!("=== {name}\n{}", serde_json::to_string_pretty(&json).unwrap()); },
            Err(e) => { eprintln!("error: {e}"); std::process::exit(1); }
        }
        return;
    }
    match client.fetch(now()) {
        Ok(data) => {
            let snap = Snapshot::build(&data, now());
            println!("team {} ({}), you are user {}", data.team, client.team_id, data.me);
            println!("{} shifts, {} active incidents", data.shifts.len(), data.incidents.len());
            for s in &data.shifts { println!("  {:9} {:3} {} → {}{}", s.role.label(), s.who, s.start, s.end, if s.user_id == data.me { "  (you)" } else { "" }); }
            println!("now: primary {:?}, secondary {:?}", snap.primary.map(|s| s.who), snap.secondary.map(|s| s.who));
            println!("you: now {:?}, next {:?}", snap.mine_now.map(|s| (s.role, s.end)), snap.mine_next.map(|s| (s.role, s.start)));
            for i in &data.incidents { println!("  [{}] {} · {} · {:?}", i.severity, i.title, i.status, i.commander.as_ref().map(|c| &c.0)); }
        }
        Err(e) => { eprintln!("error: {e}"); std::process::exit(1); }
    }
}

/// `--probe-github` prints your pull requests and their activity, unread marked with *.
fn probe_github() {
    let Some(mut client) = github::Client::from_settings() else {
        eprintln!("No GitHub token: add one in Settings (the gear in the panel) or set NOTCH_GITHUB_TOKEN.");
        std::process::exit(1);
    };
    match client.fetch(now()) {
        Ok(found) => {
            let read = ReadState::load(pulls::READ_FILE, now());
            println!("you are {}: {} pull requests, {} with something unread; {} to review, {} new that count",
                found.login, found.list.len(), pulls::unread_count(&found.list, &read), found.reviews.len(), pulls::unread_count(&found.reviews, &read));
            for w in &found.trouble { println!("note: {w}"); }
            for (title, list) in [("yours", &found.list), ("to review", &found.reviews)] {
                println!("{title}:");
                for (p, _) in pulls::shown(list, &read) {
                    let last = read.last_read(&p.id, p.read_on_github);
                    let by = if p.author.is_empty() { String::new() } else { format!(" · by {} · for {:?}", p.author, p.request) };
                    println!("  {}#{} [{:?} · checks {:?} · review {:?} · merge {:?}{by}] {}", p.repo, p.number, p.status, p.checks, p.review, p.merge, p.title);
                    println!("    last read {}{}", last.format("%F %T"), p.read_on_github.map(|t| format!(" (on GitHub {})", t.format("%F %T"))).unwrap_or_default());
                    for a in &p.activity {
                        println!("    {} {} {}{}", if a.at > last { "*" } else { " " }, a.at, a.headline(), if a.excerpt.is_empty() { String::new() } else { format!(": {}", a.excerpt) });
                    }
                }
            }
        }
        Err(e) => { eprintln!("error: {e}"); std::process::exit(1); }
    }
}

/// `--probe-linear` prints what the Linear section would list, unread notifications marked with *.
fn probe_linear() {
    let Some(client) = linear::Client::from_settings() else {
        eprintln!("No Linear API key: add one in Settings (the gear in the panel) or set NOTCH_LINEAR_API_KEY.");
        std::process::exit(1);
    };
    match client.fetch(now()) {
        Ok(found) => {
            let read = ReadState::load(issues::READ_FILE, now() - issues::look_back());
            let shown = issues::shown(&found.list, &read);
            println!("you are {}: {} issues and projects, {} listed, {} with something unread", found.me, found.list.len(), shown.len(),
                issues::unread_count(&found.list, &read));
            for (t, _) in shown {
                println!("  {} [{}{}]", t.name(), t.state, if t.assigned { " · yours" } else { "" });
                let last = read.last_read(&t.id, None);
                for n in &t.notices {
                    let unread = !n.read && n.at > last;
                    println!("    {} {} {}{}", if unread { "*" } else { " " }, n.at.format("%F %T"), n.headline,
                        if n.excerpt.is_empty() { String::new() } else { format!(": {}", n.excerpt) });
                }
            }
        }
        Err(e) => { eprintln!("error: {e}"); std::process::exit(1); }
    }
}

/// `--probe-calendar` prints today's and tomorrow's meetings as the Calendar section reads them.
/// It never asks for access.
fn probe_calendar() {
    match calendar::fetch(now()) {
        Ok(found) => {
            println!("{} calendars, {} events today and tomorrow", found.calendars, found.events.len());
            for e in &found.events {
                let when = if e.all_day { format!("{} all day", e.start.format("%a %-d %b")) } else { format!("{} {}", e.start.format("%a %-d %b"), e.hours()) };
                let join = e.join.as_ref().map(|j| format!(" · join on {}", j.service)).unwrap_or_default();
                println!("  {when:22} {} [{} · {:?}{join}]", e.title, e.calendar, e.reply);
            }
        }
        Err(e) => { eprintln!("error: {e}"); std::process::exit(1); }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--login") {
        std::process::exit(login::cli(args.get(i + 1).map(String::as_str)));
    }
    if args.iter().any(|a| a == "--version") { return println!("notch-assistant {}", env!("CARGO_PKG_VERSION")); }
    if args.iter().any(|a| a == "--probe-github") { return probe_github(); }
    if args.iter().any(|a| a == "--probe-linear") { return probe_linear(); }
    if args.iter().any(|a| a == "--probe-calendar") { return probe_calendar(); }
    if args.iter().any(|a| a == "--probe" || a == "--probe-raw") {
        return probe(args.iter().any(|a| a == "--probe-raw"));
    }
    // Headless tooling renders mock scenes; only a real launch reads the token.
    let headless = args.iter().any(|a| a.starts_with("--snap") || a.starts_with("--script") || a.starts_with("--tree") || a.starts_with("--list"));
    let _ = MOCK_ONLY.set(headless || std::env::var_os("NOTCH_MOCK").is_some());
    let _ = HEADLESS.set(headless);
    if sandboxed() { store::use_memory(); }
    // Held until the process exits.
    let _lock = if sandboxed() { None } else {
        match store::single_instance() {
            Ok(lock) => lock,
            Err(store::Running) => { eprintln!("Notch Assistant is already running; quitting this copy."); return; }
        }
    };

    rune::prewarm_gpu();
    rune::run_windows_with(RunOptions { accessory: true }, move |displays| {
        let mut app = App::new(spec()).windowless();
        // Demo: a backdrop over the notched display, under everything the app shows.
        let builtin = notch::builtin_display()
            .and_then(|id| displays.iter().find(|d| d.native_id() == Some(rune::NativeDisplayId::MacOS(id))));
        if let (true, Some(display)) = (demo::on(), builtin) {
            let target = DisplayTarget::Native(display.native_id().unwrap());
            let backdrop = app.overlay(OverlaySpec::new("Backdrop", (1.0, 1.0), demo::backdrop).options(OverlayOptions {
                display: target, placement: Some(OverlayPlacement::FullScreen), input: OverlayInput::None, prewarm: true, ..Default::default()
            }).macos_level(demo::BACKDROP_LEVEL), demo::Backdrop);
            backdrop.show();
        }
        let overlay = app.overlay(OverlaySpec::new("Notch", view::CANVAS, view::view).options(OverlayOptions {
            // On the display under the pointer, moving with it from screen to screen.
            display: DisplayTarget::UnderPointer,
            follow_pointer: true,
            placement: Some(anchor::PLACEMENT),
            // Above the menu bar; only the island itself (`.hit()`) takes the pointer.
            level: if demo::on() { OverlayLevel::AboveOverlays } else { OverlayLevel::AboveStatus },
            input: OverlayInput::Regions,
            prewarm: true,
            exclude_from_capture: !settings::in_capture(),
            ..Default::default()
        }).setup(start_fetching), init());
        overlay.show();
        OVERLAY.with(|o| *o.borrow_mut() = Some(overlay.clone()));
        // Settings: an ordinary window, hidden until opened from the panel.
        let spec = AppSpec { setup: Some(settings::setup), poll: Some(settings::poll),
            ..AppSpec::new("Notch assistant settings", demo::SETTINGS_SIZE, || settings::Settings::new(None), settings::view) };
        let options = rune::WindowOptions { visible: false, min_size: Some((560.0, 420.0)), ..Default::default() };
        let options = match (demo::on(), builtin) {
            // Demo: centred on the notched display, below the panel, above the backdrop.
            (true, Some(d)) => {
                let (w, h) = demo::SETTINGS_SIZE;
                let width = (d.size.0 as f64 / d.scale) as f32;
                let rect = OverlayPlacement::Rect { x: (width - w) / 2.0, y: demo::SETTINGS_TOP, width: w, height: h };
                SETTINGS_PLACE.with(|p| *p.borrow_mut() = Some((DisplayTarget::Native(d.native_id().unwrap()), rect)));
                options.macos(rune::MacWindowOptions::default().level(demo::SETTINGS_LEVEL))
            }
            _ => options.into(),
        };
        let mut settings_spec = WindowSpec::new("settings", spec).state(settings::Settings::new(Some(overlay.clone())))
            .configured_options(options);
        // ⌘Q while Settings is in front (the panel never takes key focus, so it can't get ⌘Q).
        let quit_key = "cmd+q".parse().expect("a valid shortcut");
        settings_spec.app = settings_spec.app.menu_item(rune::MenuItem::new("Quit Notch Assistant", |_, tx: &mut Tx| tx.quit()).key_equivalent(quit_key));
        let (settings_app, _) = app.window(settings_spec);
        // NOTCH_OPEN_SETTINGS=1 opens Settings at launch (handy for checking its layout).
        SETTINGS.with(|s| *s.borrow_mut() = Some(settings_app));
        if std::env::var_os("NOTCH_OPEN_SETTINGS").is_some() { open_settings(); }
        // The headless tools snapshot this (never shown) window with the same view and scenes.
        vec![WindowSpec::from_app("notch", app).options(rune::WindowOptions { transparent: true, ..Default::default() })]
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peek(section: Section, headline: &str, meeting: bool) -> Peek {
        Peek { section, pulls: (section == Section::GitHub).then_some(PullView::Yours), mark: Mark::Person("ZQ".into()),
            headline: headline.into(), detail: String::new(), until: 0.0, meeting: meeting.then(|| headline.to_string()) }
    }

    fn headlines(q: &VecDeque<Peek>) -> Vec<&str> { q.iter().map(|p| p.headline.as_str()).collect() }

    #[test]
    fn meetings_wait_ahead_of_the_rest_and_two_at_once_both_show() {
        let mut q = VecDeque::new();
        queue(&mut q, peek(Section::GitHub, "comment", false));
        queue(&mut q, peek(Section::Calendar, "standup", true));
        queue(&mut q, peek(Section::Calendar, "1:1", true));
        assert_eq!(headlines(&q), ["standup", "1:1", "comment"]);
    }

    #[test]
    fn a_newer_peek_for_the_same_list_replaces_the_waiting_one() {
        let mut q = VecDeque::new();
        queue(&mut q, peek(Section::GitHub, "old comment", false));
        queue(&mut q, peek(Section::Linear, "mention", false));
        queue(&mut q, peek(Section::GitHub, "new comment", false));
        assert_eq!(headlines(&q), ["mention", "new comment"]);
    }

    #[test]
    fn the_queue_drops_the_oldest_non_meeting_when_full() {
        let mut q = VecDeque::new();
        queue(&mut q, peek(Section::GitHub, "comment", false));
        queue(&mut q, peek(Section::Linear, "mention", false));
        for m in ["a", "b", "c"] { queue(&mut q, peek(Section::Calendar, m, true)); }
        assert_eq!(headlines(&q), ["a", "b", "c", "mention"]);
    }
}
