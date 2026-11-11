//! The island: a black shape that hugs the notch while collapsed and springs open into a
//! panel with tabs. The top strip beside the notch holds the tabs and the clock; the notch
//! itself hides whatever is drawn behind it, so content starts below it.

use chrono::Duration;
use rune::tok::TextStyle;
use rune::{Color, Cx, El, Spring, Transition, col, el, row, spacer, text};

use crate::island::{self, Event};
use crate::oncall::{Incident, Origin, Role, Shift, Snapshot, Week, span, when};
use crate::{State, Tab};

pub const PANEL: (f32, f32) = (660.0, 300.0);
/// The overlay canvas: the open panel plus room for its shadow. Only the island takes clicks.
pub const CANVAS: (f32, f32) = (PANEL.0 + 60.0, PANEL.1 + 40.0);
/// How far the collapsed island reaches past each side of the notch.
const WING: f32 = 30.0;

const BLACK: Color = Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };
const WHITE: Color = Color::WHITE;

fn rgb(hex: u32) -> Color {
    let c = |s: u32| ((hex >> s) & 0xff) as f32 / 255.0;
    Color { r: c(16), g: c(8), b: c(0), a: 1.0 }
}
fn dim() -> Color { WHITE.alpha(0.55) }
fn faint() -> Color { WHITE.alpha(0.08) }
fn green() -> Color { rgb(0x34d17a) }
fn amber() -> Color { rgb(0xf5b83d) }
fn blue() -> Color { rgb(0x3b7cf6) }
fn red() -> Color { rgb(0xff5a52) }

const SPRING: Spring = Spring::new(0.42, 0.22);

fn t(size: f32, weight: f32) -> TextStyle { TextStyle::new(size, weight) }
fn label(s: impl Into<String>, size: f32, weight: f32, color: Color) -> El { text(s.into()).font(t(size, weight)).color(color) }
fn caps(s: &str) -> El { text(s.to_uppercase()).font(t(10.0, 650.0).tracking(0.6)).color(dim()) }

fn role_color(role: Role) -> Color { match role { Role::Primary => green(), Role::Secondary => amber() } }

/// A round avatar with initials, ringed in its role's colour; the user's own gets a white ring.
fn avatar(initials: &str, d: f32, ring: Option<Color>) -> El {
    let mut a = el().size(d, d).radius(d / 2.0).fill(blue()).center()
        .child(label(initials, (d * 0.38).round(), 650.0, WHITE));
    if let Some(c) = ring { a = a.border(2.0, c); }
    a
}

pub fn view(s: &State, _: &mut Cx<State>) -> El {
    let open = s.island.is_open();
    let r = if open { 30.0 } else { s.geo.height * 0.42 };
    let body = if open { expanded(s) } else { collapsed(s) };
    // Open, the panel is as tall as its tab's content; the height springs with the width.
    let shape = if open { col().w(PANEL.0).max_h(PANEL.1) } else { col().size(s.geo.width + 2.0 * WING, s.geo.height) };
    let island = shape.key("island").hit().fill(BLACK)
        .corners([0.0, 0.0, r, r]).clip_corners([0.0, 0.0, r, r])
        .shadow(10.0, if open { 40.0 } else { 0.0 }, BLACK.alpha(if open { 0.45 } else { 0.0 }))
        .motion(SPRING)
        .on_hover_change(|s: &mut State, inside, tx| {
            let now = tx.clock();
            let event = if inside { Event::Enter { now } } else { Event::Leave { now } };
            for e in island::step(&mut s.seq, &mut s.island, event) {
                match e { island::Effect::WakeAt(due) => tx.wake_at(due) }
            }
        })
        .on_press(|s: &mut State| { let _ = island::step(&mut s.seq, &mut s.island, Event::Press); })
        .child(body);
    col().w_full().h_full().items_center().child(island)
}

/// Collapsed: a status dot on the left wing, the current primary on the right.
fn collapsed(s: &State) -> El {
    let snap = &s.snap;
    let dot = match (&snap.mine_now, &snap.primary) {
        (Some(mine), _) => role_color(mine.role),
        (None, Some(_)) => WHITE.alpha(0.35),
        (None, None) => WHITE.alpha(0.15),
    };
    let left = el().size(8.0, 8.0).radius(4.0).fill(dot);
    let right = match &snap.primary {
        Some(p) => avatar(&p.who, 18.0, None),
        None => el().size(18.0, 18.0),
    };
    row().key("collapsed").w_full().h_full().px(10.0).items_center().justify_between()
        .transition(Transition::opacity())
        .child(left).child(right)
}

fn expanded(s: &State) -> El {
    let body = match (&s.snap.origin, s.tab) {
        (Origin::Loading, _) => notice("Loading from FireHydrant…", None),
        (Origin::Error { message, stale: None }, _) => notice("Couldn't reach FireHydrant", Some(message)),
        (_, Tab::OnCall) => oncall(s),
        (_, Tab::Incidents) => incidents(s),
    };
    col().key("expanded").w_full().transition(Transition::blur_replace())
        .child(top_bar(s))
        .child(col().key(tab_key(s.tab)).w_full().px(18.0).pt(8.0).pb(18.0).child(body))
}

fn notice(title: &str, detail: Option<&str>) -> El {
    let mut c = card().w_full().child(label(title, 14.0, 600.0, WHITE));
    if let Some(d) = detail { c = c.child(label(d, 12.0, 500.0, dim()).wrap()); }
    c
}

/// Where the data comes from, so mock or stale data is never mistaken for live. `compact`
/// beside a real notch, where the strip is narrow.
fn origin_label(snap: &Snapshot, compact: bool) -> El {
    let (text, color) = match &snap.origin {
        Origin::Mock => ((if compact { "Mock · Connect" } else { "Mock data · Connect" }).to_string(), amber()),
        Origin::Loading => ("Loading…".to_string(), dim()),
        // It refreshes every 5 minutes, so the age only matters once a refresh was missed.
        Origin::Live { at } if snap.now - *at < Duration::minutes(10) => ("Live".to_string(), green()),
        Origin::Live { at } => (format!("Live · {} ago", span(snap.now - *at)), green()),
        Origin::Error { stale: Some(at), .. } => (format!("Offline · {} old", span(snap.now - *at)), red()),
        Origin::Error { stale: None, .. } => ("Offline".to_string(), red()),
    };
    let mut l = row().key("origin").shrink().gap(5.0).items_center().px(6.0).h(20.0).radius(10.0)
        .child(el().size(6.0, 6.0).radius(3.0).fill(color))
        .child(label(text, 11.0, 500.0, dim()).shrink().truncate());
    // Mock or failing: a click opens Settings to connect.
    if matches!(snap.origin, Origin::Mock | Origin::Error { .. }) {
        l = l.hit().hover(WHITE.alpha(0.1)).on_press(|_: &mut State| crate::open_settings());
    }
    l
}

fn tab_key(tab: Tab) -> &'static str { match tab { Tab::OnCall => "tab-oncall", Tab::Incidents => "tab-incidents" } }

/// Space kept clear on each side of a real notch.
const NOTCH_MARGIN: f32 = 10.0;

/// The strip beside the notch: tabs on the left wing, the clock on the right.
///
/// A real notch hides whatever is drawn under it (screenshots still show it, the display
/// doesn't), so there the strip is two wings with the notch's width kept empty between them,
/// and the right wing shortens its text to fit. A virtual notch hides nothing.
fn top_bar(s: &State) -> El {
    let tab = |tab: Tab, name: &'static str, badge: Option<usize>| {
        let on = s.tab == tab;
        let mut chip = row().key(tab_key(tab)).hit().gap(6.0).items_center().px(10.0).h(24.0).radius(12.0)
            .fill(if on { WHITE.alpha(0.14) } else { WHITE.alpha(0.0) }).hover(WHITE.alpha(0.08))
            .on_press(move |s: &mut State| s.tab = tab)
            .child(label(name, 12.0, if on { 600.0 } else { 500.0 }, if on { WHITE } else { dim() }));
        if let Some(n) = badge.filter(|n| *n > 0) {
            chip = chip.child(el().h(16.0).min_w(16.0).px(5.0).radius(8.0).fill(red()).center().child(label(n.to_string(), 10.0, 700.0, WHITE)));
        }
        chip
    };
    let gear = el().key("settings").hit().size(22.0, 22.0).radius(11.0).center().hover(WHITE.alpha(0.12))
        .on_press(|_: &mut State| crate::open_settings())
        .child(label("⚙", 13.0, 500.0, WHITE.alpha(0.55)));
    let quit = el().key("quit").hit().size(22.0, 22.0).radius(11.0).center().hover(WHITE.alpha(0.12))
        .on_press(|_: &mut State| crate::quit())
        .child(label("×", 14.0, 500.0, WHITE.alpha(0.4)));
    let real = s.geo.real;
    let clock = label(s.snap.now.format(if real { "%H:%M" } else { "%a %-d %b  %H:%M" }).to_string(), 12.0, 600.0, WHITE);
    let tabs = row().gap(2.0).child(tab(Tab::OnCall, "On-call", None)).child(tab(Tab::Incidents, "Incidents", Some(s.snap.incidents.len())));
    let status = row().shrink().gap(10.0).items_center()
        .child(origin_label(&s.snap, real)).child(clock).child(row().gap(2.0).child(gear).child(quit));
    let (pad, bar) = (14.0, row().w_full().h(s.geo.height.max(30.0)).items_center());
    if !real {
        return bar.px(pad).child(tabs).child(spacer()).child(status);
    }
    // Wings and the gap add up to the panel's width; each wing clips what doesn't fit.
    let gap = s.geo.width + 2.0 * NOTCH_MARGIN;
    let wing = ((PANEL.0 - gap) / 2.0 - pad).max(0.0);
    bar.px(pad)
        .child(row().key("wing-left").w(wing).items_center().clip_rounded(0.0).child(tabs))
        .child(el().key("notch-gap").w(gap))
        .child(row().key("wing-right").w(wing).items_center().justify_end().clip_rounded(0.0).child(status))
}

fn card() -> El { col().radius(16.0).fill(WHITE.alpha(0.07)).padding(14.0).gap(10.0) }

fn oncall(s: &State) -> El {
    let snap = &s.snap;
    col().w_full().gap(12.0)
        .child(row().w_full().gap(12.0)
            .child(now_card(s).w(300.0))
            .child(you_card(s).grow(1.0)))
        .child(weeks(snap))
}

fn person(shift: &Shift, me: &str) -> El {
    let mine = shift.user_id == me;
    let name = if mine { "You".to_string() } else { shift.who.to_string() };
    row().gap(10.0).items_center()
        .child(avatar(&shift.who, 28.0, mine.then_some(WHITE)))
        .child(label(name, 13.0, 600.0, WHITE))
        .child(spacer())
        .child(row().px(8.0).h(20.0).radius(10.0).items_center().fill(role_color(shift.role).alpha(0.18))
            .child(label(shift.role.label(), 11.0, 600.0, role_color(shift.role))))
}

fn now_card(s: &State) -> El {
    let snap = &s.snap;
    let mut c = card().child(row().w_full().items_center().child(caps("On call now")).child(spacer())
        .child(label(&snap.team, 11.0, 500.0, dim())));
    match (&snap.primary, &snap.secondary) {
        (None, None) => {
            c = c.child(label("Off hours: nobody is on shift", 13.0, 500.0, dim()));
            if let Some(next) = snap.next_start {
                c = c.child(label(format!("Next handover {}", when(next)), 12.0, 500.0, dim()));
            }
        }
        (p, sec) => {
            for shift in [p, sec].into_iter().flatten() { c = c.child(person(shift, &snap.me)); }
            if let Some(end) = snap.rotation_end {
                c = c.child(label(format!("Rotation ends {}", when(end)), 11.0, 500.0, dim()));
            }
        }
    }
    c
}

fn you_card(s: &State) -> El {
    let snap = &s.snap;
    let mut c = card().child(caps("Your shift"));
    match &snap.mine_now {
        Some(shift) => {
            let total = (shift.end - shift.start).num_seconds().max(1) as f32;
            let done = ((snap.now - shift.start).num_seconds() as f32 / total).clamp(0.0, 1.0);
            let color = role_color(shift.role);
            c = c.child(row().gap(8.0).items_center()
                    .child(el().size(8.0, 8.0).radius(4.0).fill(color))
                    .child(label(format!("You're on call · {}", shift.role.label()), 15.0, 650.0, WHITE)))
                .child(label(match snap.mine_rotation_end.filter(|end| *end > shift.end) {
                    // Daily shifts: today's time left, and when the rotation as a whole ends.
                    Some(end) => format!("{} left today · rotation ends {}", span(shift.end - snap.now), when(end)),
                    None => format!("{} left · ends {}", span(shift.end - snap.now), when(shift.end)),
                }, 12.0, 500.0, dim()))
                .child(el().key("progress").w_full().h(6.0).radius(3.0).fill(faint())
                    .child(el().h_full().w((done * 260.0).max(6.0)).radius(3.0).fill(color)));
        }
        None => {
            c = c.child(row().gap(8.0).items_center()
                .child(el().size(8.0, 8.0).radius(4.0).fill(WHITE.alpha(0.3)))
                .child(label("You're off call", 15.0, 650.0, WHITE)));
        }
    }
    if let Some(next) = &snap.mine_next {
        c = c.child(row().gap(6.0).items_center()
            .child(label("Next", 12.0, 500.0, dim()))
            .child(label(format!("{} · {}", next.role.label(), when(next.start)), 12.0, 600.0, role_color(next.role)))
            .child(label(format!("in {}", span(next.start - snap.now)), 12.0, 500.0, dim())));
    }
    c
}

/// Five weeks of the rotation, like FireHydrant's month view collapsed to one row.
fn weeks(snap: &Snapshot) -> El {
    let this_week = snap.weeks.first().map(|w| w.0);
    let mut strip = row().w_full().gap(8.0);
    for (monday, primary, secondary) in &snap.weeks as &[Week] {
        let current = Some(*monday) == this_week;
        let chip = |who: &Option<(String, String)>, role: Role| {
            let Some((initials, id)) = who else {
                return row().px(8.0).h(22.0).radius(11.0).items_center().fill(faint()).child(label("—", 11.0, 600.0, dim()));
            };
            let mine = *id == snap.me;
            row().gap(4.0).items_center().px(3.0).pr(7.0).h(22.0).radius(11.0)
                .fill(role_color(role).alpha(0.16))
                .child(avatar(initials, 16.0, mine.then_some(WHITE)))
                .child(label(if mine { "You" } else { initials.as_str() }, 11.0, 600.0, WHITE))
        };
        strip = strip.child(col().grow(1.0).gap(6.0).padding(8.0).radius(12.0)
            .fill(if current { WHITE.alpha(0.1) } else { WHITE.alpha(0.04) })
            .border(1.0, if current { WHITE.alpha(0.25) } else { WHITE.alpha(0.0) })
            .child(label(if current { "This week".to_string() } else { monday.format("%-d %b").to_string() }, 10.5, 600.0, if current { WHITE } else { dim() }))
            .child(chip(primary, Role::Primary))
            .child(chip(secondary, Role::Secondary)));
    }
    strip
}

fn incidents(s: &State) -> El {
    let snap = &s.snap;
    let mut list = col().w_full().gap(8.0);
    if snap.incidents.is_empty() {
        list = list.child(label("No open incidents", 13.0, 500.0, dim()));
    }
    for inc in &snap.incidents { list = list.child(incident(inc, snap.now - inc.started, &snap.me)); }
    col().w_full().gap(10.0).child(caps(&format!("Open incidents · {}", snap.team))).child(list)
}

fn incident(inc: &Incident, age: Duration, me: &str) -> El {
    let sev = if inc.severity.is_empty() { "—" } else { inc.severity.as_str() };
    let color = match sev.trim_start_matches(|c: char| !c.is_ascii_digit()).parse::<u32>() {
        Ok(0 | 1) => red(),
        Ok(2) => rgb(0xff8a3d),
        Ok(_) => amber(),
        Err(_) => dim(),
    };
    let commander = match &inc.commander {
        Some((_, id)) if id == me => "You".to_string(),
        Some((initials, _)) => initials.clone(),
        None => "unassigned".to_string(),
    };
    row().w_full().gap(12.0).items_center().padding(12.0).radius(14.0).fill(WHITE.alpha(0.07))
        .child(row().px(8.0).h(22.0).radius(6.0).items_center().fill(color.alpha(0.2)).child(label(sev, 11.0, 700.0, color)))
        .child(col().grow(1.0).min_w(0.0).gap(3.0)
            .child(label(&inc.title, 13.0, 600.0, WHITE).truncate())
            .child(label(format!("{} · {} ago · commander {}", inc.status, span(age), commander), 11.5, 500.0, dim())))
        .child(match &inc.commander {
            Some((initials, id)) => avatar(initials, 24.0, (id == me).then_some(WHITE)),
            None => el().size(24.0, 24.0),
        })
}
