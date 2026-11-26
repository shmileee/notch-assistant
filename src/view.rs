//! The island: a black shape that hugs the notch while collapsed and springs open into a
//! panel. Open, a sidebar picks an integration and its data shows beside it. The top strip
//! beside the notch says where that data comes from and holds the clock; the notch itself
//! hides whatever is drawn behind it, so content starts below it.

use chrono::{Duration, NaiveDateTime};
use rune::tok::TextStyle;
use rune::{Color, Cx, El, ScrollSpec, Spring, Transition, Tx, col, el, row, spacer, text};

use crate::island::{self, Event};
use crate::oncall::{Incident, Origin, Role, Shift, Snapshot, Week, initials, span, when};
use crate::issues::{self, Notice, Ticket};
use crate::pulls::{self, Activity, Checks, Merge, Pull, Request, Review, Status};
use crate::{FireView, Peek, PullView, Section, State};

/// The open panel. The sidebar takes `SIDEBAR` of its width; the rest is the section's.
pub const PANEL: (f32, f32) = (800.0, 330.0);
const SIDEBAR: f32 = 150.0;
/// The overlay canvas: the open panel plus room for its shadow. Only the island takes clicks.
pub const CANVAS: (f32, f32) = (PANEL.0 + 60.0, PANEL.1 + 40.0);
/// How far the collapsed island reaches past each side of the notch: further for each count of
/// unread things beside the status dot, further still while it peeks at something new.
const WING: f32 = 30.0;
const BADGE_WING: f32 = 24.0;
const PEEK_WING: f32 = 110.0;
/// The peek's line under the notch.
const PEEK_H: f32 = 46.0;

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
fn purple() -> Color { rgb(0xa371f7) }
fn indigo() -> Color { rgb(0x5e6ad2) }

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
    let peek = s.peek.as_ref().filter(|_| !open);
    let r = if open { 30.0 } else if peek.is_some() { 22.0 } else { s.geo.height * 0.42 };
    let body = if open { expanded(s) } else { collapsed(s, peek) };
    // Open, the panel is as tall as its section's content; the height springs with the width.
    let wing = if peek.is_some() { PEEK_WING } else { WING + BADGE_WING * alerts(s).len() as f32 };
    let height = s.geo.height + if peek.is_some() { PEEK_H } else { 0.0 };
    let shape = if open { col().w(PANEL.0).max_h(PANEL.1) } else { col().size(s.geo.width + 2.0 * wing, height) };
    let island = shape.key("island").hit().fill(BLACK)
        .corners([0.0, 0.0, r, r]).clip_corners([0.0, 0.0, r, r])
        .shadow(10.0, if open { 40.0 } else { 0.0 }, BLACK.alpha(if open { 0.45 } else { 0.0 }))
        .motion(SPRING)
        .on_hover_change(|s: &mut State, inside, tx| {
            let now = tx.clock();
            let event = if inside { Event::Enter { now } } else { Event::Leave { now } };
            if inside { s.follow_peek(); }
            for e in island::step(&mut s.seq, &mut s.island, event) {
                match e { island::Effect::WakeAt(due) => tx.wake_at(due) }
            }
        })
        .on_press(|s: &mut State| {
            s.follow_peek();
            let _ = island::step(&mut s.seq, &mut s.island, Event::Press);
        })
        .child(body);
    col().w_full().h_full().items_center().child(island)
}

fn unread_yours(s: &State) -> usize { pulls::unread_count(&s.github.data.list, &s.github.read) }

fn unread_reviews(s: &State) -> usize { pulls::unread_count(&s.github.data.reviews, &s.github.read) }

fn unread_pulls(s: &State) -> usize { unread_yours(s) + unread_reviews(s) }

fn unread_issues(s: &State) -> usize { issues::unread_count(&s.linear.data.list, &s.linear.read) }

/// The counts on the collapsed island, one per integration with something unread. None for a
/// mock, which would otherwise sit there for good on a Mac that never connects it; scenes and
/// demo mode show the mock's.
fn alerts(s: &State) -> Vec<(usize, Color)> {
    let real = |origin: &Origin| *origin != Origin::Mock || crate::sandboxed();
    let mut counts = vec![];
    if real(&s.github.data.origin) { counts.push((unread_pulls(s), purple())); }
    if real(&s.linear.data.origin) { counts.push((unread_issues(s), indigo())); }
    counts.retain(|(n, _)| *n > 0);
    counts
}

fn badge(n: usize, color: Color) -> El {
    el().h(16.0).min_w(16.0).px(5.0).radius(8.0).fill(color).center().child(label(n.to_string(), 10.0, 700.0, WHITE))
}

/// Collapsed: a status dot on the left wing with the counts of unread things, the current
/// primary on the right. A peek adds a line under the notch.
fn collapsed(s: &State, peek: Option<&Peek>) -> El {
    let snap = &s.snap;
    let dot = match (&snap.mine_now, &snap.primary) {
        (Some(mine), _) => role_color(mine.role),
        (None, Some(_)) => WHITE.alpha(0.35),
        (None, None) => WHITE.alpha(0.15),
    };
    let mut left = row().gap(6.0).items_center().child(el().size(8.0, 8.0).radius(4.0).fill(dot));
    for (n, color) in alerts(s) { left = left.child(badge(n, color)); }
    let right = match &snap.primary {
        Some(p) => avatar(&p.who, 18.0, None),
        None => el().size(18.0, 18.0),
    };
    let top = row().w_full().h(s.geo.height).px(10.0).items_center().justify_between().child(left).child(right);
    let mut c = col().key("collapsed").w_full().h_full().transition(Transition::opacity()).child(top);
    if let Some(peek) = peek {
        c = c.child(row().key("peek").w_full().h(PEEK_H).px(16.0).pb(6.0).gap(10.0).items_center()
            .child(avatar(&initials(&peek.who), 26.0, None))
            .child(col().grow(1.0).min_w(0.0).gap(2.0)
                .child(label(&peek.headline, 12.5, 600.0, WHITE).truncate())
                .child(label(&peek.detail, 11.5, 500.0, dim()).truncate())));
    }
    c
}

fn expanded(s: &State) -> El {
    let (origin, service, _) = source(s, s.section);
    let body = match origin {
        Origin::Loading => notice(&format!("Loading from {service}…"), None),
        Origin::Error { message, stale: None } => notice(&format!("Couldn't reach {service}"), Some(message)),
        _ => match s.section {
            Section::FireHydrant => fire(s),
            Section::GitHub => pull_list(s),
            Section::Linear => ticket_list(s),
        },
    };
    col().key("expanded").w_full().transition(Transition::blur_replace())
        .child(top_bar(s))
        .child(row().w_full().pb(18.0)
            .child(sidebar(s))
            .child(col().key(body_key(s)).grow(1.0).min_w(0.0).px(18.0).pt(8.0).child(body)))
}

fn notice(title: &str, detail: Option<&str>) -> El {
    let mut c = card().w_full().child(label(title, 14.0, 600.0, WHITE));
    if let Some(d) = detail { c = c.child(label(d, 12.0, 500.0, dim()).wrap()); }
    c
}

/// A section's data source: its origin, the service's name and its integration's ID.
fn source(s: &State, section: Section) -> (&Origin, &'static str, &'static str) {
    match section {
        Section::FireHydrant => (&s.snap.origin, "FireHydrant", crate::firehydrant::INTEGRATION.id),
        Section::GitHub => (&s.github.data.origin, "GitHub", crate::github::INTEGRATION.id),
        Section::Linear => (&s.linear.data.origin, "Linear", crate::linear::INTEGRATION.id),
    }
}

fn origin_color(origin: &Origin) -> Color {
    match origin {
        Origin::Mock => amber(),
        Origin::Loading => dim(),
        Origin::Live { .. } => green(),
        Origin::Error { .. } => red(),
    }
}

/// Where the selected section's data comes from, so mock or stale data is never mistaken for
/// live.
fn origin_label(s: &State) -> El {
    let (origin, _, id) = source(s, s.section);
    let now = s.snap.now;
    let text = match origin {
        Origin::Mock => "Mock data · Connect".to_string(),
        Origin::Loading => "Loading…".to_string(),
        // Each refreshes every five minutes or sooner, so the age only matters once a refresh was missed.
        Origin::Live { at } if now - *at < Duration::minutes(10) => "Live".to_string(),
        Origin::Live { at } => format!("Live · {} ago", span(now - *at)),
        Origin::Error { stale: Some(at), .. } => format!("Offline · {} old", span(now - *at)),
        Origin::Error { stale: None, .. } => "Offline".to_string(),
    };
    let mut l = row().key("origin").shrink().gap(5.0).items_center().px(6.0).h(20.0).radius(10.0)
        .child(el().size(6.0, 6.0).radius(3.0).fill(origin_color(origin)))
        .child(label(text, 11.0, 500.0, dim()).shrink().truncate());
    // Mock or failing: a click opens Settings to connect.
    if matches!(origin, Origin::Mock | Origin::Error { .. }) {
        l = l.hit().hover(WHITE.alpha(0.1)).on_press(move |_: &mut State| crate::open_settings_for(id));
    }
    l
}

fn body_key(s: &State) -> &'static str {
    match (s.section, s.fire) {
        (Section::FireHydrant, FireView::OnCall) => "body-oncall",
        (Section::FireHydrant, FireView::Incidents) => "body-incidents",
        (Section::GitHub, _) => "body-github",
        (Section::Linear, _) => "body-linear",
    }
}

fn sidebar(s: &State) -> El {
    let item = |section: Section, key: &'static str, count: usize, color: Color| {
        let (origin, name, _) = source(s, section);
        let on = s.section == section;
        let mut r = row().key(key).hit().w_full().h(30.0).px(10.0).gap(8.0).items_center().radius(10.0)
            .fill(WHITE.alpha(if on { 0.12 } else { 0.0 })).hover(WHITE.alpha(0.07))
            .on_press(move |s: &mut State| s.section = section)
            .child(el().size(6.0, 6.0).radius(3.0).fill(origin_color(origin)))
            .child(label(name, 12.5, if on { 600.0 } else { 500.0 }, if on { WHITE } else { dim() }))
            .child(spacer());
        if count > 0 { r = r.child(badge(count, color)); }
        r
    };
    col().w(SIDEBAR).pl(12.0).pt(8.0).gap(2.0)
        .child(item(Section::FireHydrant, "nav-firehydrant", s.snap.incidents.len(), red()))
        .child(item(Section::GitHub, "nav-github", unread_pulls(s), purple()))
        .child(item(Section::Linear, "nav-linear", unread_issues(s), indigo()))
}

/// Space kept clear on each side of a real notch.
const NOTCH_MARGIN: f32 = 10.0;

/// The strip beside the notch: where the data comes from on the left wing, the clock on the
/// right.
///
/// A real notch hides whatever is drawn under it (screenshots still show it, the display
/// doesn't), so there the strip is two wings with the notch's width kept empty between them.
/// A virtual notch hides nothing.
fn top_bar(s: &State) -> El {
    let gear = el().key("settings").hit().size(22.0, 22.0).radius(11.0).center().hover(WHITE.alpha(0.12))
        .on_press(|_: &mut State| crate::open_settings())
        .child(label("⚙", 13.0, 500.0, WHITE.alpha(0.55)));
    let quit = el().key("quit").hit().size(22.0, 22.0).radius(11.0).center().hover(WHITE.alpha(0.12))
        .on_press(|_: &mut State| crate::quit())
        .child(label("×", 14.0, 500.0, WHITE.alpha(0.4)));
    let clock = label(s.snap.now.format("%a %-d %b  %H:%M").to_string(), 12.0, 600.0, WHITE);
    let right = row().gap(10.0).items_center().child(clock).child(row().gap(2.0).child(gear).child(quit));
    let (pad, bar) = (14.0, row().w_full().h(s.geo.height.max(30.0)).items_center());
    if !s.geo.real {
        return bar.px(pad).child(origin_label(s)).child(spacer()).child(right);
    }
    // Wings and the gap add up to the panel's width; each wing clips what doesn't fit.
    let gap = s.geo.width + 2.0 * NOTCH_MARGIN;
    let wing = ((PANEL.0 - gap) / 2.0 - pad).max(0.0);
    bar.px(pad)
        .child(row().key("wing-left").w(wing).items_center().clip_rounded(0.0).child(origin_label(s)))
        .child(el().key("notch-gap").w(gap))
        .child(row().key("wing-right").w(wing).items_center().justify_end().clip_rounded(0.0).child(right))
}

fn tab(key: &'static str, name: &'static str, on: bool, count: usize, color: Color, pick: impl Fn(&mut State) + 'static) -> El {
    let mut tab = row().key(key).hit().gap(5.0).items_center().px(10.0).h(24.0).radius(12.0)
        .fill(WHITE.alpha(if on { 0.14 } else { 0.0 })).hover(WHITE.alpha(0.08))
        .on_press(pick)
        .child(label(name, 12.0, if on { 600.0 } else { 500.0 }, if on { WHITE } else { dim() }));
    if count > 0 { tab = tab.child(badge(count, color)); }
    tab
}

fn fire(s: &State) -> El {
    let switch = row().gap(2.0)
        .child(tab("view-oncall", "On-call", s.fire == FireView::OnCall, 0, red(), |s| s.fire = FireView::OnCall))
        .child(tab("view-incidents", "Incidents", s.fire == FireView::Incidents, s.snap.incidents.len(), red(), |s| s.fire = FireView::Incidents));
    col().w_full().gap(10.0).child(switch).child(match s.fire {
        FireView::OnCall => oncall(s),
        FireView::Incidents => incidents(s),
    })
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

/// Rows of an unread list, and the most it shows before it scrolls.
const ROW: f32 = 46.0;
const ROW_GAP: f32 = 6.0;
const ROWS: usize = 4;

fn unread_list(key: &'static str, title: El, any_unread: bool, mark_all: fn(&mut State), rows: Vec<El>, empty: &str) -> El {
    let mut header = row().w_full().h(24.0).items_center().child(title).child(spacer());
    if any_unread {
        header = header.child(row().key("mark-all-read").hit().px(9.0).h(22.0).radius(11.0).items_center().hover(WHITE.alpha(0.12))
            .on_press(mark_all)
            .child(label("Mark all read", 11.0, 600.0, dim())));
    }
    let count = rows.len();
    let mut list = col().w_full().gap(ROW_GAP);
    if rows.is_empty() { list = list.child(label(empty, 13.0, 500.0, dim())); }
    for r in rows { list = list.child(r); }
    let shown = count.min(ROWS) as f32;
    let height = (shown * ROW + (shown - 1.0).max(0.0) * ROW_GAP).max(20.0);
    let list = if count > ROWS {
        col().key(key).w_full().h(height).scroll(ScrollSpec { stick_bottom: false }).scroll_edge_fade(10.0).child(list)
    } else { list };
    col().w_full().gap(8.0).child(header).child(list)
}

/// `color` is the unread count's: the integration's when it counts, grey when it doesn't.
fn list_row(key: String, unread: usize, color: Color, title: &str, detail: String, right: El, open: impl Fn(&mut State, &mut Tx) + 'static) -> El {
    let new = unread > 0;
    let marker = if new { badge(unread, color) } else { el().size(16.0, 16.0) };
    row().key(key).hit().w_full().h(ROW).gap(10.0).items_center().px(12.0).radius(12.0)
        .fill(WHITE.alpha(if new { 0.1 } else { 0.05 })).hover(WHITE.alpha(0.15))
        .on_press_tx(open)
        .child(marker)
        .child(col().grow(1.0).min_w(0.0).gap(3.0)
            .child(label(title, 13.0, 600.0, if new { WHITE } else { WHITE.alpha(0.8) }).truncate())
            .child(label(detail, 11.5, 500.0, if new { WHITE.alpha(0.7) } else { dim() }).truncate()))
        .child(right)
}

fn chip(text: &str, color: Color) -> El {
    row().px(8.0).h(20.0).radius(10.0).items_center().fill(color.alpha(0.18)).child(label(text, 11.0, 600.0, color))
}

/// Your pull requests or those you're asked to review, unread first. A click opens one at its
/// oldest unread event and marks it read.
fn pull_list(s: &State) -> El {
    let gh = &s.github;
    let (list, key, empty) = match s.pulls {
        PullView::Yours => (&gh.data.list, "list-github", "No open pull requests"),
        PullView::Reviews => (&gh.data.reviews, "list-reviews", "Nothing to review"),
    };
    let shown = pulls::shown(list, &gh.read);
    let any_unread = shown.iter().any(|(_, unread)| !unread.is_empty());
    let rows = shown.into_iter().map(|(p, unread)| pull_row(p, &unread, s.snap.now)).collect();
    let switch = row().gap(2.0)
        .child(tab("view-yours", "Your pull requests", s.pulls == PullView::Yours, unread_yours(s), purple(), |s| s.pulls = PullView::Yours))
        .child(tab("view-reviews", "To review", s.pulls == PullView::Reviews, unread_reviews(s), purple(), |s| s.pulls = PullView::Reviews));
    unread_list(key, switch, any_unread, State::mark_pulls_read, rows, empty)
}

fn pull_row(p: &Pull, unread: &[&Activity], now: NaiveDateTime) -> El {
    let repo = p.repo.rsplit('/').next().unwrap_or(&p.repo);
    let place = format!("{repo} #{}", p.number);
    let detail = match (unread.last(), p.author.as_str()) {
        (Some(a), "") => {
            let what = format!("{} · {} ago", a.headline(), span(now - a.at));
            if a.excerpt.is_empty() { what } else { format!("{what}: {}", a.excerpt) }
        }
        (Some(a), _) => format!("{} · {place} · {} ago", a.headline(), span(now - a.at)),
        (None, "") => format!("{place} · updated {} ago", span(now - p.updated)),
        (None, author) => format!("{author} · {place} · updated {} ago", span(now - p.updated)),
    };
    let mut right = row().gap(8.0).items_center();
    match p.checks {
        Checks::Passing => right = right.child(label("✓", 12.0, 700.0, green())),
        Checks::Failing => right = right.child(label("✕", 12.0, 700.0, red())),
        Checks::Pending => right = right.child(el().size(7.0, 7.0).radius(3.5).fill(amber())),
        Checks::None => {}
    }
    let state = match (&p.request, p.status, p.merge, p.review) {
        (Some(Request::You), ..) => chip("You", purple()),
        (Some(Request::Team(team)), ..) => chip(team, dim()),
        (_, Status::Merged, ..) => chip("Merged", purple()),
        (_, Status::Closed, ..) => chip("Closed", red()),
        (_, Status::Draft, ..) => chip("Draft", dim()),
        // Solid, to stand out from the tinted "Approved" it follows.
        (_, _, Merge::Ready, _) => row().px(8.0).h(20.0).radius(10.0).items_center().fill(green()).child(label("Ready to merge", 11.0, 700.0, BLACK)),
        (_, _, Merge::Conflicts, _) => chip("Conflicts", rgb(0xff8a3d)),
        (.., Review::Approved) => chip("Approved", green()),
        (.., Review::ChangesRequested) => chip("Changes", rgb(0xff8a3d)),
        _ => chip("Open", dim()),
    };
    let color = if p.counts() { purple() } else { WHITE.alpha(0.25) };
    let id = p.id.clone();
    list_row(format!("pull-{}", p.id), unread.len(), color, &p.title, detail, right.child(state),
        move |s: &mut State, tx: &mut Tx| s.open_pull(&id, tx))
}

/// Linear: issues and projects with unread notifications, then your open issues. A click opens
/// one where its oldest unread notification points and marks it read.
fn ticket_list(s: &State) -> El {
    let ln = &s.linear;
    let rows = issues::shown(&ln.data.list, &ln.read).into_iter().map(|(t, unread)| ticket_row(t, &unread, s.snap.now)).collect();
    let unread = unread_issues(s);
    let title = caps(&if unread > 0 { format!("Linear · {unread} unread") } else { "Linear".to_string() });
    unread_list("list-linear", title, unread > 0, State::mark_issues_read, rows, "No open issues and nothing new")
}

fn ticket_row(t: &Ticket, unread: &[&Notice], now: NaiveDateTime) -> El {
    let detail = match unread.last() {
        Some(n) => {
            let what = format!("{} · {} ago", n.headline, span(now - n.at));
            if n.excerpt.is_empty() { what } else { format!("{what}: {}", n.excerpt) }
        }
        None if t.assigned => format!("Assigned to you · updated {} ago", span(now - t.updated)),
        None => format!("Updated {} ago", span(now - t.updated)),
    };
    let id = t.id.clone();
    list_row(format!("ticket-{}", t.id), unread.len(), indigo(), &t.name(), detail, chip(&t.state, rgb(t.color)),
        move |s: &mut State, tx: &mut Tx| s.open_ticket(&id, tx))
}
