//! Calendar: your meetings today and tomorrow, from every calendar in the Calendar app
//! (`calendar.rs`) or the mock here, and what the panel picks from them at a given moment.
//!
//! EventKit hands over each occurrence of a repeating meeting as its own event, so nothing
//! here expands repeats. Declined and cancelled meetings never get this far.

use chrono::{Duration, NaiveDate, NaiveDateTime, NaiveTime};

use crate::oncall::Origin;

/// How long before a meeting the island peeks to say it's starting.
pub fn lead() -> Duration { Duration::minutes(5) }

/// How long before a meeting the collapsed island counts down to it.
pub fn countdown() -> Duration { Duration::minutes(15) }

/// Your answer to the invitation; a meeting nobody invited you to counts as `Yes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply { Yes, Maybe, NotYet }

/// The video call a meeting is on.
#[derive(Clone, Debug, PartialEq)]
pub struct Join {
    /// "Teams", "Zoom", …
    pub service: &'static str,
    /// Empty in the mock.
    pub url: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    /// EventKit's calendar item ID; empty in the mock.
    pub id: String,
    pub title: String,
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    pub all_day: bool,
    pub calendar: String,
    /// The calendar's colour, as 0xRRGGBB.
    pub color: u32,
    /// A room or an address; may be empty.
    pub location: String,
    pub join: Option<Join>,
    pub reply: Reply,
}

impl Event {
    /// The occurrences of a repeating meeting share an ID, so the start tells them apart.
    pub fn key(&self) -> String { format!("{}@{}", self.id, self.start.format("%Y%m%dT%H%M")) }

    pub fn covers(&self, t: NaiveDateTime) -> bool { self.start <= t && t < self.end }

    /// Whether any of it falls on `day`.
    pub fn on(&self, day: NaiveDate) -> bool {
        let from = day.and_time(NaiveTime::MIN);
        self.start < from + Duration::days(1) && self.end > from
    }

    /// "14:00–14:30".
    pub fn hours(&self) -> String { format!("{}–{}", self.start.format("%H:%M"), self.end.format("%H:%M")) }

    /// The room and the call it's on, else the calendar it's in.
    pub fn place(&self) -> String {
        let service = self.join.as_ref().map(|j| j.service);
        // A Teams invitation's location is "Microsoft Teams Meeting", which the service already says.
        let room = Some(self.location.trim()).filter(|l| !l.is_empty() && !l.contains("://")
            && !service.is_some_and(|s| l.to_lowercase().contains(&s.to_lowercase())));
        match (room, service) {
            (Some(room), Some(service)) => format!("{room} · {service}"),
            (Some(room), None) => room.to_string(),
            (None, Some(service)) => service.to_string(),
            (None, None) => self.calendar.clone(),
        }
    }
}

/// One fetch's worth of calendar.
#[derive(Clone, Debug, PartialEq)]
pub struct Agenda {
    /// How many calendars were read.
    pub calendars: usize,
    /// Today's and tomorrow's.
    pub events: Vec<Event>,
    pub origin: Origin,
}

impl Agenda {
    pub fn loading() -> Self { Agenda { calendars: 0, events: vec![], origin: Origin::Loading } }
}

/// The meeting the panel leads with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Focus<'a> {
    /// The one you're in; of several, the one that started last.
    Now(&'a Event),
    /// Else the next one today.
    Next(&'a Event),
}

impl<'a> Focus<'a> {
    pub fn event(self) -> &'a Event { match self { Focus::Now(e) | Focus::Next(e) => e } }
}

/// Today as the panel shows it at one moment.
pub struct Day<'a> {
    pub all_day: Vec<&'a Event>,
    pub focus: Option<Focus<'a>>,
    /// Today's other meetings that haven't ended, by start.
    pub later: Vec<&'a Event>,
    /// Tomorrow's meetings, by start.
    pub tomorrow: Vec<&'a Event>,
    /// All of today's meetings, ended ones too, by start.
    pub timed: Vec<&'a Event>,
}

pub fn day(events: &[Event], now: NaiveDateTime) -> Day<'_> {
    let (today, tomorrow) = (now.date(), now.date() + Duration::days(1));
    fn by_start(mut list: Vec<&Event>) -> Vec<&Event> { list.sort_by_key(|e| (e.start, e.end)); list }
    let timed = by_start(events.iter().filter(|e| !e.all_day && e.on(today)).collect());
    let focus = timed.iter().filter(|e| e.covers(now)).max_by_key(|e| e.start).map(|e| Focus::Now(e))
        .or_else(|| timed.iter().find(|e| e.start > now).map(|e| Focus::Next(e)));
    let focused = |e: &Event| focus.is_some_and(|f| std::ptr::eq(f.event(), e));
    Day {
        all_day: events.iter().filter(|e| e.all_day && e.on(today)).collect(),
        focus,
        later: timed.iter().copied().filter(|e| e.end > now && !focused(e)).collect(),
        tomorrow: by_start(events.iter().filter(|e| !e.all_day && e.start.date() == tomorrow).collect()),
        timed,
    }
}

/// Meetings that start within `within` from now, soonest first.
pub fn starting(events: &[Event], now: NaiveDateTime, within: Duration) -> Vec<&Event> {
    let mut list: Vec<_> = events.iter().filter(|e| !e.all_day && e.start > now && e.start - now <= within).collect();
    list.sort_by_key(|e| e.start);
    list
}

/// "5m" to a meeting at 15:00 from 14:55:20: minutes rounded up, so it never says "0m" early.
pub fn until(d: Duration) -> String {
    let m = (d.num_seconds() + 59).div_euclid(60).max(0);
    if m < 60 { format!("{m}m") } else { crate::oncall::span(Duration::minutes(m)) }
}

/// How long a meeting lasts: "45m", "1h", "1h 30m".
pub fn length(d: Duration) -> String {
    let m = d.num_minutes().max(0);
    match (m / 60, m % 60) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

/// The call to join, from the first meeting link in `texts` (an event's link, location, notes).
pub fn join_link<'a>(texts: impl IntoIterator<Item = &'a str>) -> Option<Join> {
    texts.into_iter().flat_map(links).find_map(|url| service(&url).map(|service| Join { service, url }))
}

/// The `https://` links in `text`. Invitations wrap them in angle brackets or quotes, and a mail
/// filter may have wrapped them in an Outlook Safe Links redirect.
fn links(text: &str) -> Vec<String> {
    text.match_indices("https://").map(|(i, _)| {
        let rest = &text[i..];
        let end = rest.find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | ')' | ']')).unwrap_or(rest.len());
        let url = rest[..end].trim_end_matches(['.', ',', ';']);
        safe_link_target(url).unwrap_or_else(|| url.to_string())
    }).collect()
}

fn safe_link_target(url: &str) -> Option<String> {
    let (host, query) = url.strip_prefix("https://")?.split_once('?')?;
    if !host.split('/').next()?.ends_with("safelinks.protection.outlook.com") { return None; }
    let encoded = query.split('&').find_map(|pair| pair.strip_prefix("url="))?;
    Some(percent_decode(encoded))
}

fn percent_decode(s: &str) -> String {
    let (bytes, mut out, mut i) = (s.as_bytes(), Vec::with_capacity(s.len()), 0);
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => { out.push(b); i += 3; }
            (b, _) => { out.push(b); i += 1; }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The service a link joins a call on. Invitations also link to help pages and meeting
/// options on the same hosts, so each service's join paths are spelled out.
fn service(url: &str) -> Option<&'static str> {
    let rest = url.strip_prefix("https://")?;
    let split = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = rest[..split].split(':').next()?.to_ascii_lowercase();
    let path = rest[split..].to_ascii_lowercase();
    let on = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
    let under = |prefixes: &[&str]| prefixes.iter().any(|p| path.starts_with(p));
    let named = path.len() > 1;
    if on("teams.microsoft.com") && under(&["/l/meetup-join/", "/meet/"]) || on("teams.live.com") && under(&["/meet/"]) {
        Some("Teams")
    } else if on("zoom.us") && under(&["/j/", "/my/", "/w/", "/s/"]) {
        Some("Zoom")
    } else if on("meet.google.com") && path[1..].split(['/', '?']).next().is_some_and(|code| code.contains('-')) {
        Some("Google Meet")
    } else if on("webex.com") && (path.contains("/j.php") || under(&["/meet/", "/join/"])) {
        Some("Webex")
    } else if on("chime.aws") && named {
        Some("Chime")
    } else if on("whereby.com") && named {
        Some("Whereby")
    } else if on("meet.jit.si") && named {
        Some("Jitsi")
    } else {
        None
    }
}

const WORK: u32 = 0x1badf8;
const PERSONAL: u32 = 0x63da38;
const BIRTHDAYS: u32 = 0xcc73e1;

/// Made-up meetings for the made-up team of the other mocks, at fixed times of `now`'s day.
pub fn mock(now: NaiveDateTime) -> Agenda {
    let at = |day: i64, h: u32, m: u32| (now.date() + Duration::days(day)).and_hms_opt(h, m, 0).unwrap();
    let teams = || Some(Join { service: "Teams", url: String::new() });
    let meeting = |title: &str, (start, end): (NaiveDateTime, NaiveDateTime), calendar: &str, color, location: &str, join, reply| Event {
        id: String::new(), title: title.into(), start, end, all_day: false, calendar: calendar.into(), color, location: location.into(), join, reply,
    };
    let all_day = |title: &str, from: i64, days: i64, calendar: &str, color| Event {
        all_day: true, ..meeting(title, (at(from, 0, 0), at(from + days, 0, 0)), calendar, color, "", None, Reply::Yes)
    };
    let teams_room = "Microsoft Teams Meeting";
    let events = vec![
        all_day("Change freeze", -1, 5, "Work", WORK),
        all_day("ZQ's birthday", 0, 1, "Birthdays", BIRTHDAYS),
        meeting("Standup", (at(0, 9, 30), at(0, 9, 45)), "Work", WORK, teams_room, teams(), Reply::Yes),
        meeting("Ingest outage postmortem", (at(0, 13, 30), at(0, 14, 30)), "Work", WORK, teams_room, teams(), Reply::Yes),
        meeting("1:1 with ZQ", (at(0, 15, 0), at(0, 15, 30)), "Work", WORK, teams_room, teams(), Reply::Yes),
        meeting("Kafka upgrade planning", (at(0, 16, 0), at(0, 16, 45)), "Work", WORK, "Room Fjord", teams(), Reply::Maybe),
        meeting("Climbing", (at(0, 17, 30), at(0, 19, 0)), "Personal", PERSONAL, "Boulder hall", None, Reply::Yes),
        meeting("Standup", (at(1, 9, 30), at(1, 9, 45)), "Work", WORK, teams_room, teams(), Reply::Yes),
        meeting("Sprint review", (at(1, 11, 0), at(1, 12, 0)), "Work", WORK, teams_room, teams(), Reply::NotYet),
        meeting("Lunch with JY", (at(1, 12, 30), at(1, 13, 30)), "Personal", PERSONAL, "Noodle bar", None, Reply::Yes),
    ];
    Agenda { calendars: 3, events, origin: Origin::Mock }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime { NaiveDate::from_ymd_opt(2026, 11, d).unwrap().and_hms_opt(h, m, 0).unwrap() }
    fn titles(list: &[&Event]) -> Vec<String> { list.iter().map(|e| e.title.clone()).collect() }

    #[test]
    fn the_panel_leads_with_the_meeting_you_are_in_then_the_next() {
        let events = mock(at(10, 14, 5)).events;
        let d = day(&events, at(10, 14, 5));
        assert!(matches!(d.focus, Some(Focus::Now(e)) if e.title == "Ingest outage postmortem"));
        assert_eq!(titles(&d.later), ["1:1 with ZQ", "Kafka upgrade planning", "Climbing"]);
        assert_eq!(titles(&d.tomorrow), ["Standup", "Sprint review", "Lunch with JY"]);
        assert_eq!(titles(&d.all_day), ["Change freeze", "ZQ's birthday"]);
        assert_eq!(d.timed.len(), 5, "the timeline keeps this morning's standup");

        let d = day(&events, at(10, 14, 40));
        assert!(matches!(d.focus, Some(Focus::Next(e)) if e.title == "1:1 with ZQ"));
        assert_eq!(titles(&d.later), ["Kafka upgrade planning", "Climbing"]);

        let d = day(&events, at(10, 20, 0));
        assert!(d.focus.is_none() && d.later.is_empty());
    }

    #[test]
    fn of_overlapping_meetings_the_latest_started_leads_and_the_other_stays_listed() {
        let mut events = mock(at(10, 14, 5)).events;
        events.push(Event { title: "Focus time".into(), start: at(10, 13, 0), end: at(10, 17, 0), ..events[2].clone() });
        let d = day(&events, at(10, 14, 5));
        assert!(matches!(d.focus, Some(Focus::Now(e)) if e.title == "Ingest outage postmortem"));
        assert_eq!(titles(&d.later)[0], "Focus time");
    }

    #[test]
    fn a_meeting_across_midnight_is_on_both_days() {
        let e = Event { start: at(10, 23, 0), end: at(11, 1, 0), ..mock(at(10, 9, 0)).events[2].clone() };
        assert!(e.on(at(10, 0, 0).date()) && e.on(at(11, 0, 0).date()) && !e.on(at(12, 0, 0).date()));
    }

    #[test]
    fn starting_soon_and_the_countdown() {
        let events = mock(at(10, 14, 5)).events;
        assert!(starting(&events, at(10, 14, 50), lead()).is_empty());
        assert_eq!(titles(&starting(&events, at(10, 14, 50), countdown())), ["1:1 with ZQ"]);
        assert_eq!(titles(&starting(&events, at(10, 14, 56), lead())), ["1:1 with ZQ"]);
        assert!(starting(&events, at(10, 15, 0), lead()).is_empty(), "started is no longer starting");
        assert_eq!(until(Duration::seconds(4 * 60 + 40)), "5m");
        assert_eq!(until(Duration::minutes(15)), "15m");
        assert_eq!(until(Duration::minutes(130)), "2h 10m");
        assert_eq!((length(Duration::minutes(45)), length(Duration::minutes(60)), length(Duration::minutes(90))), ("45m".into(), "1h".into(), "1h 30m".into()));
    }

    #[test]
    fn join_links_are_found_and_help_pages_are_not() {
        let teams = "Microsoft Teams meeting\nJoin: <https://teams.microsoft.com/l/meetup-join/19%3ameeting_abc%40thread.v2/0?context=%7b%7d>\n\
            Meeting options: <https://teams.microsoft.com/meetingOptions/?organizerId=x>\nHelp: https://aka.ms/JoinTeamsMeeting";
        let found = join_link(["", "Microsoft Teams Meeting", teams]).unwrap();
        assert_eq!((found.service, found.url.as_str()), ("Teams", "https://teams.microsoft.com/l/meetup-join/19%3ameeting_abc%40thread.v2/0?context=%7b%7d"));
        assert_eq!(join_link(["Room 3, https://us02web.zoom.us/j/123456?pwd=abc."]).unwrap().url, "https://us02web.zoom.us/j/123456?pwd=abc");
        assert_eq!(join_link(["https://meet.google.com/abc-defg-hij"]).unwrap().service, "Google Meet");
        assert!(join_link(["https://support.google.com/meet", "https://zoom.us/support", "https://meet.google.com/"]).is_none());
        assert!(join_link(["no links here"]).is_none());

        let wrapped = "https://eur02.safelinks.protection.outlook.com/?url=https%3A%2F%2Fteams.microsoft.com%2Fl%2Fmeetup-join%2Fabc&data=x";
        assert_eq!(join_link([wrapped]).unwrap().url, "https://teams.microsoft.com/l/meetup-join/abc");
    }

    #[test]
    fn the_place_names_the_room_and_the_call() {
        let events = mock(at(10, 14, 5)).events;
        let place = |title: &str| events.iter().find(|e| e.title == title).unwrap().place();
        assert_eq!(place("1:1 with ZQ"), "Teams", "Teams' own location is dropped");
        assert_eq!(place("Kafka upgrade planning"), "Room Fjord · Teams");
        assert_eq!(place("Climbing"), "Boulder hall");
        assert_eq!(place("ZQ's birthday"), "Birthdays");
    }
}
