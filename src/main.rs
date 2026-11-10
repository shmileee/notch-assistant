//! Notch assistant: a panel that springs out of the MacBook notch when you hover it.
//!
//! `cargo run --release` puts it at the top of whichever display the pointer is on. Headless stills, with no window:
//! `cargo run -- --scene open --snap snaps/open.png` (scenes: collapsed, open, incidents,
//! offhours).

mod firehydrant;
mod integrations;
mod island;
mod notch;
mod oncall;
mod settings;
mod store;
mod view;

use std::cell::RefCell;
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use chrono::{Local, NaiveDate, NaiveDateTime};
use rune::machine::Seq;
use rune::{Align, App, AppHandle, AppSpec, DisplayTarget, OverlayInput, OverlayLevel, OverlayOptions, OverlayPlacement, OverlaySpec, RunOptions, ScreenEdge, Tx, WindowSpec};

use island::{Event, Island};
use notch::NotchGeo;
use oncall::{Data, Origin, Snapshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab { OnCall, Incidents }

pub struct State {
    pub geo: NotchGeo,
    pub island: Island,
    pub seq: Seq,
    pub tab: Tab,
    /// The latest fetch (or the mock), and what the view shows from it at `snap.now`.
    pub data: Data,
    pub snap: Snapshot,
    /// Results from the FireHydrant thread, when there is a token.
    pub results: Option<Receiver<Result<Data, String>>>,
    /// Keeps that thread alive; dropping it stops the thread.
    pub control: Option<Sender<()>>,
    pub waker: Option<rune::Waker>,
    /// Scenes pin the clock so snapshots are reproducible.
    pub frozen: bool,
}

/// Mock data only: headless tooling, or `NOTCH_MOCK`. Set once in `main`.
static MOCK_ONLY: OnceLock<bool> = OnceLock::new();

thread_local! {
    /// The Settings window, opened from the panel (handlers run on the main thread).
    static SETTINGS: RefCell<Option<AppHandle<settings::Settings>>> = const { RefCell::new(None) };
}

pub fn open_settings() {
    SETTINGS.with(|s| if let Some(handle) = s.borrow().as_ref() { handle.open(); handle.focus(); });
}

fn now() -> NaiveDateTime { Local::now().naive_local() }

/// Who "you" are in the mock rotation (`NOTCH_ME`, default OP).
fn mock_me() -> String { std::env::var("NOTCH_ME").unwrap_or_else(|_| "OP".into()) }

fn init() -> State {
    let data = oncall::mock(&mock_me(), now());
    State {
        geo: notch::under_pointer(),
        island: Island::Collapsed,
        seq: Seq::default(),
        tab: Tab::OnCall,
        snap: Snapshot::build(&data, now()),
        data,
        results: None,
        control: None,
        waker: None,
        frozen: false,
    }
}

/// The overlay's setup: keep the waker and connect.
fn start_fetching(s: &mut State, wake: rune::Waker) {
    s.waker = Some(wake);
    connect(s);
}

/// (Re)connects to FireHydrant with the saved settings, stopping any earlier connection;
/// without an API key, back to mock data. Called at startup and whenever Settings saves.
pub fn connect(s: &mut State) {
    s.control = None; // stops the old thread
    s.results = None;
    let client = if *MOCK_ONLY.get().unwrap_or(&false) { None } else { firehydrant::Client::from_settings() };
    match (client, s.waker.clone()) {
        (Some(client), Some(wake)) => {
            let (tx, rx) = std::sync::mpsc::channel();
            s.control = Some(firehydrant::spawn(client, tx, wake));
            s.results = Some(rx);
            s.data = Data::loading();
        }
        _ => s.data = oncall::mock(&mock_me(), now()),
    }
    s.snap = Snapshot::build(&s.data, now());
}

/// Takes in finished fetches. A failure keeps the last good data and says so.
fn take_results(s: &mut State) -> bool {
    let Some(rx) = &s.results else { return false };
    let mut changed = false;
    while let Ok(result) = rx.try_recv() {
        match result {
            Ok(data) => s.data = data,
            Err(message) => {
                let stale = match &s.data.origin { Origin::Live { at } => Some(*at), Origin::Error { stale, .. } => *stale, _ => None };
                s.data.origin = Origin::Error { message, stale };
            }
        }
        changed = true;
    }
    changed
}

/// Fires the island's timers and refreshes the data each minute. True only when something
/// the view shows changed, so an idle island draws nothing.
fn poll(s: &mut State, clock: f64, tx: &mut Tx) -> bool {
    let mut changed = false;
    // The overlay follows the pointer between displays; reshape the island to the notch (real
    // or virtual) of the display it's now on, without animating the jump.
    if !s.frozen {
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
        let t = now();
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
fn demo_time(day: u32, h: u32, m: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 11, day).unwrap().and_hms_opt(h, m, 0).unwrap()
}
fn freeze(s: &mut State, at: NaiveDateTime) {
    s.frozen = true;
    s.geo = NotchGeo { width: 185.0, height: 32.0, real: true };
    s.data = oncall::mock("OP", at);
    s.snap = Snapshot::build(&s.data, at);
}

fn spec() -> AppSpec<State> {
    AppSpec {
        scenes: &[
            ("collapsed", |s| freeze(s, demo_time(10, 14, 5))),
            ("open", |s| { freeze(s, demo_time(10, 14, 5)); s.island = Island::Open; }),
            ("incidents", |s| { freeze(s, demo_time(10, 14, 5)); s.island = Island::Open; s.tab = Tab::Incidents; }),
            ("offhours", |s| { freeze(s, demo_time(14, 11, 30)); s.island = Island::Open; }),
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--probe" || a == "--probe-raw") {
        return probe(args.iter().any(|a| a == "--probe-raw"));
    }
    // Headless tooling renders mock scenes; only a real launch reads the token.
    let headless = args.iter().any(|a| a.starts_with("--snap") || a.starts_with("--script") || a.starts_with("--tree") || a.starts_with("--list"));
    let _ = MOCK_ONLY.set(headless || std::env::var_os("NOTCH_MOCK").is_some());

    rune::prewarm_gpu();
    rune::run_windows_with(RunOptions { accessory: true }, move |_displays| {
        let mut app = App::new(spec()).windowless();
        let overlay = app.overlay(OverlaySpec::new("Notch", view::CANVAS, view::view).options(OverlayOptions {
            // On the display under the pointer, moving with it from screen to screen.
            display: DisplayTarget::UnderPointer,
            follow_pointer: true,
            placement: Some(OverlayPlacement::Edge { edge: ScreenEdge::Top, margin: 0.0, align: Align::Center, inset: 0.0 }),
            // Above the menu bar; only the island itself (`.hit()`) takes the pointer.
            level: OverlayLevel::AboveStatus,
            input: OverlayInput::Regions,
            prewarm: true,
            ..Default::default()
        }).setup(start_fetching), init());
        overlay.show();
        // Settings: an ordinary window, hidden until opened from the panel.
        let spec = AppSpec { setup: Some(settings::setup), poll: Some(settings::poll),
            ..AppSpec::new("Notch assistant settings", (680.0, 470.0), || settings::Settings::new(None), settings::view) };
        let (settings_app, _) = app.window(WindowSpec::new("settings", spec).state(settings::Settings::new(Some(overlay.clone())))
            .options(rune::WindowOptions { visible: false, min_size: Some((560.0, 420.0)), ..Default::default() }));
        SETTINGS.with(|s| *s.borrow_mut() = Some(settings_app));
        // The headless tools snapshot this (never shown) window with the same view and scenes.
        vec![WindowSpec::from_app("notch", app).options(rune::WindowOptions { transparent: true, ..Default::default() })]
    });
}
