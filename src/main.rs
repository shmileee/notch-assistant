//! Notch assistant: a panel that springs out of the MacBook notch when you hover it.
//!
//! `cargo run --release` puts it at the top of whichever display the pointer is on. Headless stills, with no window:
//! `cargo run -- --scene open --snap snaps/open.png` (scenes: collapsed, open, incidents,
//! offhours, pulls, linear, peek).
//!
//! Open, the panel has a sidebar of integrations (FireHydrant, GitHub, Linear) and shows the
//! selected one's data beside it.

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
mod notch;
mod oncall;
mod pulls;
mod read;
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
use feed::Feed;
use issues::Issues;
use pulls::Pulls;
use read::ReadState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section { FireHydrant, GitHub, Linear }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FireView { OnCall, Incidents }

/// Something new, shown under the collapsed island for a few seconds. Opening the panel from
/// it goes to its section.
#[derive(Clone, Debug, PartialEq)]
pub struct Peek {
    pub section: Section,
    /// Who did it, for the avatar.
    pub who: String,
    pub headline: String,
    pub detail: String,
    pub until: f64,
}

const PEEK_SECONDS: f64 = 6.0;

pub struct State {
    pub geo: NotchGeo,
    pub island: Island,
    pub seq: Seq,
    pub section: Section,
    pub fire: FireView,
    /// The latest fetch (or the mock), and what the view shows from it at `snap.now`.
    pub data: Data,
    pub snap: Snapshot,
    /// Results from the FireHydrant thread, when there is a token.
    pub results: Option<Receiver<Result<Data, String>>>,
    /// Keeps that thread alive; dropping it stops the thread.
    pub control: Option<Sender<()>>,
    pub github: Feed<Pulls>,
    pub linear: Feed<Issues>,
    pub peek: Option<Peek>,
    pub waker: Option<rune::Waker>,
    /// Scenes pin the clock and the notch so snapshots are reproducible.
    pub frozen: bool,
    /// The installed app's executable, to restart on a new version or quit once removed.
    pub installed: Option<installed::Watch>,
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
}

pub fn open_settings() {
    SETTINGS.with(|s| if let Some(handle) = s.borrow().as_ref() {
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
fn now() -> NaiveDateTime { if demo::on() { demo::time() } else { Local::now().naive_local() } }

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
        snap: Snapshot::build(&data, now()),
        data,
        results: None,
        control: None,
        github: Feed::new(pulls::mock(now()), mock_read()),
        linear: Feed::new(issues::mock(now()), mock_read()),
        peek: None,
        waker: None,
        frozen: false,
        installed: installed::Watch::new(),
    }
}

/// The mock's read state: counting from long before its events, so some are unread.
fn mock_read() -> ReadState { ReadState::memory(now() - chrono::Duration::days(30)) }

fn mock_only() -> bool { demo::on() || *MOCK_ONLY.get().unwrap_or(&false) }

/// The overlay's setup: keep the waker and connect.
fn start_fetching(s: &mut State, wake: rune::Waker) {
    s.waker = Some(wake);
    connect_firehydrant(s);
    connect_github(s);
    connect_linear(s);
}

/// Reconnects one integration after Settings saved it.
pub fn reconnect(s: &mut State, id: &str) {
    match id {
        "github" => connect_github(s),
        "linear" => connect_linear(s),
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

/// (Re)connects to GitHub with the saved settings, as `connect_firehydrant` does.
fn connect_github(s: &mut State) {
    if s.peek.as_ref().is_some_and(|p| p.section == Section::GitHub) { s.peek = None; }
    let client = if mock_only() { None } else { github::Client::from_settings() };
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
fn take_pulls(s: &mut State, clock: f64) -> bool {
    let gh = &mut s.github;
    let arrived = gh.take(|read, pulls| read.sync(pulls.list.iter().map(|p| (p.id.as_str(), p.read_on_github)), now()));
    if !arrived { return false; }
    let new = gh.announced.update(pulls::unread_events(&gh.data.list, &gh.read));
    if let (Some((p, a)), false) = (new.into_iter().max_by_key(|(_, a)| a.at), s.island.is_open()) {
        s.peek = Some(Peek {
            section: Section::GitHub,
            who: a.who.clone(),
            headline: format!("{} on #{}", a.headline(), p.number),
            detail: if a.excerpt.is_empty() { p.title.clone() } else { a.excerpt.clone() },
            until: clock + PEEK_SECONDS,
        });
    }
    true
}

/// (Re)connects to Linear with the saved settings, as `connect_firehydrant` does.
fn connect_linear(s: &mut State) {
    if s.peek.as_ref().is_some_and(|p| p.section == Section::Linear) { s.peek = None; }
    let client = if mock_only() { None } else { linear::Client::from_settings() };
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
    let arrived = ln.take(|read, found| read.sync(found.list.iter().map(|t| (t.id.as_str(), None)), now()));
    if !arrived { return false; }
    let new = ln.announced.update(issues::unread_events(&ln.data.list, &ln.read));
    if let (Some((t, n)), false) = (new.into_iter().max_by_key(|(_, n)| n.at), s.island.is_open()) {
        let on = if t.key.is_empty() { t.title.clone() } else { t.key.clone() };
        s.peek = Some(Peek {
            section: Section::Linear,
            who: if n.who.is_empty() { "Linear".into() } else { n.who.clone() },
            headline: format!("{} on {on}", n.headline),
            detail: if n.excerpt.is_empty() { t.title.clone() } else { n.excerpt.clone() },
            until: clock + PEEK_SECONDS,
        });
    }
    true
}

impl State {
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
        let Some(pull) = gh.data.list.iter().find(|p| p.id == id) else { return };
        let url = pull.unread(&gh.read).first().map_or(pull.url.as_str(), |a| a.url.as_str()).to_string();
        gh.read.mark(id, now());
        if !url.is_empty() && let Err(e) = tx.open_url(&url) { eprintln!("github: can't open {url}: {e:?}"); }
    }

    pub fn mark_pulls_read(&mut self) {
        let gh = &mut self.github;
        gh.read.mark_all(gh.data.list.iter().filter_map(|p| Some((p.id.as_str(), p.activity.last()?.at))));
    }
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
    match s.installed.as_mut().and_then(|w| w.check(clock)) {
        Some(installed::Change::Replaced) => { eprintln!("installed: a new version, restarting"); relaunch(); }
        Some(installed::Change::Removed) => { eprintln!("installed: the app was removed, quitting"); quit(); }
        None => {}
    }
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
    if let Some(peek) = s.peek.as_ref().filter(|_| !s.frozen) {
        if clock >= peek.until || s.island.is_open() {
            s.peek = None;
            changed = true;
        } else {
            tx.wake_at(peek.until);
        }
    }
    if !s.frozen {
        let t = now();
        changed |= take_pulls(s, clock);
        changed |= take_issues(s, clock);
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
fn scene_time(day: u32, h: u32, m: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 11, day).unwrap().and_hms_opt(h, m, 0).unwrap()
}
fn freeze(s: &mut State, at: NaiveDateTime) {
    s.frozen = true;
    s.geo = NotchGeo { width: 185.0, height: 32.0, real: true };
    s.data = oncall::mock("OP", at);
    s.snap = Snapshot::build(&s.data, at);
    s.github.show(pulls::mock(at), ReadState::memory(at - chrono::Duration::days(30)));
    s.linear.show(issues::mock(at), ReadState::memory(at - issues::look_back()));
}

fn spec() -> AppSpec<State> {
    AppSpec {
        scenes: &[
            ("collapsed", |s| freeze(s, scene_time(10, 14, 5))),
            ("open", |s| { freeze(s, scene_time(10, 14, 5)); s.island = Island::Open; }),
            ("incidents", |s| { freeze(s, scene_time(10, 14, 5)); s.island = Island::Open; s.fire = FireView::Incidents; }),
            ("offhours", |s| { freeze(s, scene_time(14, 11, 30)); s.island = Island::Open; }),
            ("pulls", |s| { freeze(s, scene_time(10, 14, 5)); s.island = Island::Open; s.section = Section::GitHub; }),
            ("linear", |s| { freeze(s, scene_time(10, 14, 5)); s.island = Island::Open; s.section = Section::Linear; }),
            ("peek", |s| {
                freeze(s, scene_time(10, 14, 5));
                let (pull, a) = (&s.github.data.list[0], &s.github.data.list[0].activity[1]);
                s.peek = Some(Peek { section: Section::GitHub, who: a.who.clone(), headline: format!("{} on #{}", a.headline(), pull.number),
                    detail: a.excerpt.clone(), until: 0.0 });
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
            println!("you are {}: {} pull requests, {} with something unread", found.login, found.list.len(), pulls::unread_count(&found.list, &read));
            for w in &found.warnings { println!("note: {w}"); }
            for (p, _) in pulls::shown(&found.list, &read) {
                let last = read.last_read(&p.id, p.read_on_github);
                println!("  {}#{} [{:?} · checks {:?} · review {:?}] {}", p.repo, p.number, p.status, p.checks, p.review, p.title);
                println!("    last read {}{}", last.format("%F %T"), p.read_on_github.map(|t| format!(" (on GitHub {})", t.format("%F %T"))).unwrap_or_default());
                for a in &p.activity {
                    println!("    {} {} {}{}", if a.at > last { "*" } else { " " }, a.at, a.headline(), if a.excerpt.is_empty() { String::new() } else { format!(": {}", a.excerpt) });
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--login") {
        std::process::exit(login::cli(args.get(i + 1).map(String::as_str)));
    }
    if args.iter().any(|a| a == "--version") { return println!("notch-assistant {}", env!("CARGO_PKG_VERSION")); }
    if args.iter().any(|a| a == "--probe-github") { return probe_github(); }
    if args.iter().any(|a| a == "--probe-linear") { return probe_linear(); }
    if args.iter().any(|a| a == "--probe" || a == "--probe-raw") {
        return probe(args.iter().any(|a| a == "--probe-raw"));
    }
    // Headless tooling renders mock scenes; only a real launch reads the token.
    let headless = args.iter().any(|a| a.starts_with("--snap") || a.starts_with("--script") || a.starts_with("--tree") || a.starts_with("--list"));
    let _ = MOCK_ONLY.set(headless || std::env::var_os("NOTCH_MOCK").is_some());
    let _ = HEADLESS.set(headless);
    if sandboxed() { store::use_memory(); }

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
            placement: Some(OverlayPlacement::Edge { edge: ScreenEdge::Top, margin: 0.0, align: Align::Center, inset: 0.0 }),
            // Above the menu bar; only the island itself (`.hit()`) takes the pointer.
            level: if demo::on() { OverlayLevel::AboveOverlays } else { OverlayLevel::AboveStatus },
            input: OverlayInput::Regions,
            prewarm: true,
            ..Default::default()
        }).setup(start_fetching), init());
        overlay.show();
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
