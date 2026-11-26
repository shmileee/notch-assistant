//! On-call data: shifts and incidents, from FireHydrant (`firehydrant.rs`) or the mock here,
//! and the `Snapshot` the view renders from them at a given moment.
//!
//! The mock is modelled on a real team's calendar in FireHydrant (November 2026): two weekly
//! rotations, primary and secondary, Monday 09:00 to Friday 17:00, where the secondary is
//! always last week's primary.

use chrono::{Datelike, Duration, NaiveDate, NaiveTime};
use crate::time::{self, Time};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role { Primary, Secondary }

impl Role {
    pub fn label(self) -> &'static str { match self { Role::Primary => "Primary", Role::Secondary => "Secondary" } }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Shift {
    pub role: Role,
    pub user_id: String,
    /// Initials, as FireHydrant's calendar shows them.
    pub who: String,
    pub start: Time,
    pub end: Time,
}

impl Shift {
    pub fn covers(&self, t: Time) -> bool { self.start <= t && t < self.end }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Incident {
    /// "SEV1", "SEV2", … as configured in FireHydrant; empty when unset.
    pub severity: String,
    pub title: String,
    /// The current milestone: "started", "acknowledged", "mitigated", …
    pub status: String,
    pub commander: Option<(String, String)>,
    pub started: Time,
}

/// Where the data came from, shown in the panel so mock data is never mistaken for live.
#[derive(Clone, Debug, PartialEq)]
pub enum Origin {
    Mock,
    Loading,
    Live { at: Time },
    /// A failed refresh. `stale` is when the data shown was last fetched, if ever.
    Error { message: String, stale: Option<Time> },
}

/// One fetch's worth of data.
#[derive(Clone, Debug, PartialEq)]
pub struct Data {
    pub team: String,
    /// The user's FireHydrant user ID (the mock uses initials).
    pub me: String,
    pub shifts: Vec<Shift>,
    pub incidents: Vec<Incident>,
    pub origin: Origin,
}

impl Data {
    pub fn loading() -> Self {
        Data { team: String::new(), me: String::new(), shifts: vec![], incidents: vec![], origin: Origin::Loading }
    }
}

pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    let first = |w: &str| w.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
    match words.as_slice() {
        [] => "?".into(),
        [one] => one.chars().take(2).collect::<String>().to_uppercase(),
        [a, .., b] => first(a) + &first(b),
    }
}

/// The rotation order seen in the calendar: ZQ → OP → XV → JY → ZQ …
const ORDER: [&str; 4] = ["ZQ", "OP", "XV", "JY"];

fn anchor() -> NaiveDate { NaiveDate::from_ymd_opt(2026, 11, 2).unwrap() } // ZQ's primary week

pub fn monday_of(d: NaiveDate) -> NaiveDate { d - Duration::days(d.weekday().num_days_from_monday() as i64) }

fn week_shifts(monday: NaiveDate) -> [Shift; 2] {
    let w = (monday - anchor()).num_days().div_euclid(7);
    let at = |d: NaiveDate, h| time::local(d.and_time(NaiveTime::from_hms_opt(h, 0, 0).unwrap()));
    let (start, end) = (at(monday, 9), at(monday + Duration::days(4), 17));
    let person = |i: i64| ORDER[i.rem_euclid(4) as usize].to_string();
    [
        Shift { role: Role::Primary, user_id: person(w), who: person(w), start, end },
        Shift { role: Role::Secondary, user_id: person(w - 1), who: person(w - 1), start, end },
    ]
}

/// Ten weeks of the mock rotation from `now`'s week; the user is `me` (initials).
pub fn mock(me: &str, now: Time) -> Data {
    let monday = monday_of(now.date_naive());
    let shifts = (0..10).flat_map(|i| week_shifts(monday + Duration::weeks(i))).collect();
    let person = |s: &str| Some((s.to_string(), s.to_string()));
    Data {
        team: "Team Marmalade".into(),
        me: me.into(),
        shifts,
        incidents: vec![
            Incident { severity: "SEV2".into(), title: "Elevated 5xx on api-gateway (eu-west-1)".into(), status: "Investigating".into(), commander: person("OP"), started: now - Duration::minutes(14) },
            Incident { severity: "SEV3".into(), title: "Kafka consumer lag on telemetry-ingest".into(), status: "Mitigated".into(), commander: person("ZQ"), started: now - Duration::minutes(96) },
        ],
        origin: Origin::Mock,
    }
}

/// One week of the strip: who covers it as primary and secondary, as (initials, user ID).
pub type Week = (NaiveDate, Option<(String, String)>, Option<(String, String)>);

/// Everything the panel shows about on-call, worked out once per refresh.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub team: String,
    pub me: String,
    pub now: Time,
    pub origin: Origin,
    pub primary: Option<Shift>,
    pub secondary: Option<Shift>,
    /// When the current primary's (else secondary's) rotation ends: the same person's shifts in
    /// a row, with nobody else's in between, count as one rotation.
    pub rotation_end: Option<Time>,
    /// The shift the user is on right now, if any.
    pub mine_now: Option<Shift>,
    /// When the user's current rotation ends, while on shift.
    pub mine_rotation_end: Option<Time>,
    /// The user's next shift: after the current rotation while on shift, else the next one.
    pub mine_next: Option<Shift>,
    /// The next handover when nobody is on (evenings, weekends).
    pub next_start: Option<Time>,
    /// This week and the next four, each naming whoever covers most of it.
    pub weeks: Vec<Week>,
    pub incidents: Vec<Incident>,
}

impl Snapshot {
    pub fn build(data: &Data, now: Time) -> Self {
        let shifts = &data.shifts;
        let me = &data.me;
        let current = |role| shifts.iter().filter(|s| s.role == role && s.covers(now)).min_by_key(|s| s.end).cloned();
        let this_monday = monday_of(now.date_naive());
        let weeks = (0..5).map(|i| {
            let monday = this_monday + Duration::weeks(i);
            let who = |role| most_coverage(shifts, role, time::midnight(monday), time::midnight(monday + Duration::weeks(1)));
            (monday, who(Role::Primary), who(Role::Secondary))
        }).collect();
        let (primary, secondary) = (current(Role::Primary), current(Role::Secondary));
        let mine_now = shifts.iter().filter(|s| &s.user_id == me && s.covers(now)).min_by_key(|s| s.role == Role::Secondary).cloned();
        let mine_rotation_end = mine_now.as_ref().map(|s| rotation_end(shifts, s));
        let after = mine_rotation_end.unwrap_or(now);
        Snapshot {
            team: data.team.clone(),
            me: me.clone(),
            now,
            origin: data.origin.clone(),
            rotation_end: primary.as_ref().or(secondary.as_ref()).map(|s| rotation_end(shifts, s)),
            primary,
            secondary,
            mine_next: shifts.iter().filter(|s| &s.user_id == me && s.start >= after && s.start > now).min_by_key(|s| s.start).cloned(),
            mine_now,
            mine_rotation_end,
            next_start: shifts.iter().filter(|s| s.start > now).map(|s| s.start).min(),
            weeks,
            incidents: data.incidents.clone(),
        }
    }
}

/// Who covers the most of `from..to` in `role`, as (initials, user ID); on a tie, whoever started first.
fn most_coverage(shifts: &[Shift], role: Role, from: Time, to: Time) -> Option<(String, String)> {
    let mut sorted: Vec<&Shift> = shifts.iter().filter(|s| s.role == role).collect();
    sorted.sort_by_key(|s| s.start);
    let mut people: Vec<(&Shift, Duration)> = Vec::new();
    for s in sorted {
        let covered = s.end.min(to) - s.start.max(from);
        if covered <= Duration::zero() { continue; }
        match people.iter_mut().find(|(p, _)| p.user_id == s.user_id) {
            Some((_, total)) => *total += covered,
            None => people.push((s, covered)),
        }
    }
    // `people` is in order of first start and min_by_key keeps the first of equals, so ties go to the earlier start.
    people.into_iter().min_by_key(|(_, total)| std::cmp::Reverse(*total)).map(|(s, _)| (s.who.clone(), s.user_id.clone()))
}

/// The end of the run of shifts `from` belongs to: the same person in the same role, each
/// starting within four days of the last one's end (so a long weekend doesn't split a rotation)
/// with nobody else's shift in that role starting in between.
fn rotation_end(shifts: &[Shift], from: &Shift) -> Time {
    let mut end = from.end;
    let mut run: Vec<&Shift> = shifts.iter().filter(|s| s.user_id == from.user_id && s.role == from.role && s.start >= from.start).collect();
    run.sort_by_key(|s| s.start);
    for s in run {
        let handed_over = shifts.iter().any(|o| o.role == from.role && o.user_id != from.user_id && end <= o.start && o.start < s.start);
        if s.start - end > Duration::days(4) || handed_over { break; }
        end = end.max(s.end);
    }
    end
}

/// "3d 2h", "5h 10m", "12m".
pub fn span(d: Duration) -> String {
    let m = d.num_minutes().max(0);
    let (days, hours, mins) = (m / 1440, (m / 60) % 24, m % 60);
    match (days, hours) {
        (0, 0) => format!("{mins}m"),
        (0, _) => format!("{hours}h {mins}m"),
        _ => format!("{days}d {hours}h"),
    }
}

/// "Fri 13 Nov, 17:00".
pub fn when(t: Time) -> String { t.format("%a %-d %b, %H:%M").to_string() }

#[cfg(test)]
mod tests {
    use super::*;

    fn at(d: u32, h: u32, m: u32) -> Time { time::at(2026, 11, d, h, m) }
    fn snap(t: Time) -> Snapshot { Snapshot::build(&mock("OP", t), t) }
    fn shift(who: &str, role: Role, start: Time, end: Time) -> Shift {
        Shift { role, user_id: who.into(), who: who.into(), start, end }
    }
    fn data(me: &str, shifts: Vec<Shift>) -> Data {
        Data { team: "T".into(), me: me.into(), shifts, incidents: vec![], origin: Origin::Mock }
    }
    fn strip(s: &Snapshot) -> Vec<(u32, String, String)> {
        s.weeks.iter().map(|(d, p, s)| (d.day(), p.clone().unwrap().0, s.clone().unwrap().0)).collect()
    }

    #[test]
    fn the_mock_matches_the_firehydrant_calendar() {
        let want = [(9, "OP", "ZQ"), (16, "XV", "OP"), (23, "JY", "XV"), (30, "ZQ", "JY"), (7, "OP", "ZQ")];
        assert_eq!(strip(&snap(at(10, 14, 5))), want.map(|(d, p, s)| (d, p.to_string(), s.to_string())));
    }

    #[test]
    fn the_strip_names_whoever_covers_most_of_each_week() {
        // Week-long 24/7 rotations handing over at `handover`'s weekday and time; the secondary is last week's primary.
        let weekly = |handover: Time| -> Vec<Shift> {
            (0..6).flat_map(|i: i64| {
                let start = handover + Duration::weeks(i - 1);
                let person = |j: i64| ORDER[j.rem_euclid(4) as usize];
                [shift(person(i), Role::Primary, start, start + Duration::weeks(1)),
                 shift(person(i - 1), Role::Secondary, start, start + Duration::weeks(1))]
            }).collect()
        };
        let want = [(9, "OP", "ZQ"), (16, "XV", "OP"), (23, "JY", "XV"), (30, "ZQ", "JY"), (7, "OP", "ZQ")]
            .map(|(d, p, s)| (d, p.to_string(), s.to_string()));
        for handover in [at(9, 14, 0), at(11, 9, 0)] {
            let s = Snapshot::build(&data("OP", weekly(handover)), at(10, 14, 5));
            assert_eq!(strip(&s), want, "handover {handover}");
        }

        let tie = Snapshot::build(&data("OP", weekly(at(12, 12, 0))), at(10, 14, 5));
        assert_eq!(strip(&tie)[0], (9, "ZQ".into(), "JY".into()), "84h each: the earlier start wins");
    }

    #[test]
    fn on_call_now_and_the_next_shift() {
        let s = snap(at(10, 14, 5));
        assert_eq!(s.primary.as_ref().unwrap().who, "OP");
        assert_eq!(s.mine_now.as_ref().unwrap().role, Role::Primary);
        assert_eq!((s.rotation_end, s.mine_rotation_end), (Some(at(13, 17, 0)), Some(at(13, 17, 0))));
        let next = s.mine_next.unwrap();
        assert_eq!((next.role, next.start), (Role::Secondary, at(16, 9, 0)));

        let weekend = snap(at(14, 12, 0));
        assert!(weekend.primary.is_none() && weekend.mine_now.is_none());
        assert_eq!(weekend.next_start, Some(at(16, 9, 0)));
    }

    #[test]
    fn daily_shifts_join_into_one_rotation() {
        let day = |d: u32, who: &str, role| Shift { role, user_id: who.into(), who: who.into(), start: at(d, 9, 0), end: at(d, 16, 0) };
        let mut shifts: Vec<Shift> = (9..=13).map(|d| day(d, "OP", Role::Primary)).collect();
        shifts.extend((16..=20).map(|d| day(d, "OP", Role::Secondary)));
        shifts.extend((16..=20).map(|d| day(d, "XV", Role::Primary)));
        let data = Data { team: "T".into(), me: "OP".into(), shifts, incidents: vec![], origin: Origin::Mock };

        let s = Snapshot::build(&data, at(10, 11, 0));
        assert_eq!(s.mine_rotation_end, Some(at(13, 16, 0)));
        assert_eq!(s.rotation_end, Some(at(13, 16, 0)));
        let next = s.mine_next.unwrap();
        assert_eq!((next.role, next.start), (Role::Secondary, at(16, 9, 0)), "next is the next rotation, not tomorrow");

        let evening = Snapshot::build(&data, at(10, 18, 0));
        assert_eq!(evening.mine_now, None);
        assert_eq!(evening.mine_next.unwrap().start, at(11, 9, 0), "off shift, next is tomorrow morning");

        let contiguous = vec![
            shift("OP", Role::Primary, at(9, 9, 0), at(10, 9, 0)),
            shift("OP", Role::Primary, at(10, 9, 0), at(11, 9, 0)),
            shift("XV", Role::Primary, at(11, 9, 0), at(12, 9, 0)),
        ];
        assert_eq!(Snapshot::build(&Data { shifts: contiguous, ..data }, at(9, 11, 0)).rotation_end, Some(at(11, 9, 0)));
    }

    #[test]
    fn a_daily_alternating_rotation_hands_over_every_day() {
        // 24h shifts from 09:00, OP and XV taking turns, so each starts the moment the other's ends.
        let shifts = (9..=13).map(|d| shift(if d % 2 == 1 { "OP" } else { "XV" }, Role::Primary, at(d, 9, 0), at(d + 1, 9, 0))).collect();
        let data = data("OP", shifts);

        let s = Snapshot::build(&data, at(9, 11, 0));
        assert_eq!((s.rotation_end, s.mine_rotation_end), (Some(at(10, 9, 0)), Some(at(10, 9, 0))));
        assert_eq!(s.mine_next.unwrap().start, at(11, 9, 0));

        let tuesday = Snapshot::build(&data, at(10, 11, 0));
        assert_eq!(tuesday.rotation_end, Some(at(11, 9, 0)));
        assert_eq!(tuesday.mine_next.unwrap().start, at(11, 9, 0));
    }

    #[test]
    fn a_long_weekend_does_not_split_a_rotation() {
        // Weekly from Wednesday 09:00, weekdays only, with Monday the 16th off: Fri 17:00 to Tue 09:00 is 3d 16h.
        let shifts = vec![
            shift("OP", Role::Primary, at(11, 9, 0), at(13, 17, 0)),
            shift("JY", Role::Secondary, at(16, 9, 0), at(16, 17, 0)),
            shift("OP", Role::Primary, at(17, 9, 0), at(18, 9, 0)),
            shift("XV", Role::Primary, at(18, 9, 0), at(20, 17, 0)),
            shift("XV", Role::Primary, at(23, 9, 0), at(25, 9, 0)),
            shift("OP", Role::Primary, at(25, 9, 0), at(27, 17, 0)),
        ];
        let s = Snapshot::build(&data("OP", shifts), at(12, 10, 0));
        assert_eq!((s.rotation_end, s.mine_rotation_end), (Some(at(18, 9, 0)), Some(at(18, 9, 0))));
        assert_eq!(s.mine_next.unwrap().start, at(25, 9, 0));

        let week_off = vec![shift("OP", Role::Primary, at(9, 9, 0), at(13, 17, 0)), shift("OP", Role::Primary, at(18, 9, 0), at(20, 17, 0))];
        assert_eq!(Snapshot::build(&data("OP", week_off), at(10, 10, 0)).rotation_end, Some(at(13, 17, 0)), "more than four days apart");
    }

    #[test]
    fn spans_and_initials_read_naturally() {
        assert_eq!(span(Duration::minutes(12)), "12m");
        assert_eq!(span(Duration::minutes(310)), "5h 10m");
        assert_eq!(span(Duration::minutes(3 * 1440 + 175)), "3d 2h");
        assert_eq!(initials("Ada Byron Lovelace"), "AL");
        assert_eq!(initials("ops"), "OP");
    }
}
