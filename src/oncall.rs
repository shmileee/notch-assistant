//! On-call data: shifts and incidents, from FireHydrant (`firehydrant.rs`) or the mock here,
//! and the `Snapshot` the view renders from them at a given moment.
//!
//! The mock is modelled on a real team's calendar in FireHydrant (November 2026): two weekly
//! rotations, primary and secondary, Monday 09:00 to Friday 17:00, where the secondary is
//! always last week's primary.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime};

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
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
}

impl Shift {
    pub fn covers(&self, t: NaiveDateTime) -> bool { self.start <= t && t < self.end }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Incident {
    /// "SEV1", "SEV2", … as configured in FireHydrant; empty when unset.
    pub severity: String,
    pub title: String,
    /// The current milestone: "started", "acknowledged", "mitigated", …
    pub status: String,
    pub commander: Option<(String, String)>,
    pub started: NaiveDateTime,
}

/// Where the data came from, shown in the panel so mock data is never mistaken for live.
#[derive(Clone, Debug, PartialEq)]
pub enum Origin {
    Mock,
    Loading,
    Live { at: NaiveDateTime },
    /// A failed refresh. `stale` is when the data shown was last fetched, if ever.
    Error { message: String, stale: Option<NaiveDateTime> },
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

/// The rotation order seen in the calendar: DB → OP → RW → KB → DB …
const ORDER: [&str; 4] = ["DB", "OP", "RW", "KB"];

fn anchor() -> NaiveDate { NaiveDate::from_ymd_opt(2026, 11, 2).unwrap() } // DB's primary week

pub fn monday_of(d: NaiveDate) -> NaiveDate { d - Duration::days(d.weekday().num_days_from_monday() as i64) }

fn week_shifts(monday: NaiveDate) -> [Shift; 2] {
    let w = (monday - anchor()).num_days().div_euclid(7);
    let at = |d: NaiveDate, h| d.and_time(NaiveTime::from_hms_opt(h, 0, 0).unwrap());
    let (start, end) = (at(monday, 9), at(monday + Duration::days(4), 17));
    let person = |i: i64| ORDER[i.rem_euclid(4) as usize].to_string();
    [
        Shift { role: Role::Primary, user_id: person(w), who: person(w), start, end },
        Shift { role: Role::Secondary, user_id: person(w - 1), who: person(w - 1), start, end },
    ]
}

/// Ten weeks of the mock rotation from `now`'s week; the user is `me` (initials).
pub fn mock(me: &str, now: NaiveDateTime) -> Data {
    let monday = monday_of(now.date());
    let shifts = (0..10).flat_map(|i| week_shifts(monday + Duration::weeks(i))).collect();
    let person = |s: &str| Some((s.to_string(), s.to_string()));
    Data {
        team: "Team SRE".into(),
        me: me.into(),
        shifts,
        incidents: vec![
            Incident { severity: "SEV2".into(), title: "Elevated 5xx on api-gateway (eu-west-1)".into(), status: "Investigating".into(), commander: person("OP"), started: now - Duration::minutes(14) },
            Incident { severity: "SEV3".into(), title: "Kafka consumer lag on telemetry-ingest".into(), status: "Mitigated".into(), commander: person("DB"), started: now - Duration::minutes(96) },
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
    pub now: NaiveDateTime,
    pub origin: Origin,
    pub primary: Option<Shift>,
    pub secondary: Option<Shift>,
    /// When the current primary's (else secondary's) rotation ends: back-to-back daily shifts
    /// count as one rotation.
    pub rotation_end: Option<NaiveDateTime>,
    /// The shift the user is on right now, if any.
    pub mine_now: Option<Shift>,
    /// When the user's current rotation ends, while on shift.
    pub mine_rotation_end: Option<NaiveDateTime>,
    /// The user's next shift: after the current rotation while on shift, else the next one.
    pub mine_next: Option<Shift>,
    /// The next handover when nobody is on (evenings, weekends).
    pub next_start: Option<NaiveDateTime>,
    /// This week and the next four.
    pub weeks: Vec<Week>,
    pub incidents: Vec<Incident>,
}

impl Snapshot {
    pub fn build(data: &Data, now: NaiveDateTime) -> Self {
        let shifts = &data.shifts;
        let me = &data.me;
        let current = |role| shifts.iter().filter(|s| s.role == role && s.covers(now)).min_by_key(|s| s.end).cloned();
        let this_monday = monday_of(now.date());
        let weeks = (0..5).map(|i| {
            let monday = this_monday + Duration::weeks(i);
            let next = monday + Duration::weeks(1);
            // The shift covering the week's Monday midday, else the first that starts that week.
            let noon = monday.and_hms_opt(12, 0, 0).unwrap();
            let who = |role| shifts.iter().filter(|s| s.role == role)
                .find(|s| s.covers(noon))
                .or_else(|| shifts.iter().filter(|s| s.role == role && s.start.date() >= monday && s.start.date() < next).min_by_key(|s| s.start))
                .map(|s| (s.who.clone(), s.user_id.clone()));
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

/// The end of the run of shifts `from` belongs to: the same person in the same role, each
/// starting within three days of the last one's end (so a weekend doesn't split a rotation).
fn rotation_end(shifts: &[Shift], from: &Shift) -> NaiveDateTime {
    let mut end = from.end;
    let mut run: Vec<&Shift> = shifts.iter().filter(|s| s.user_id == from.user_id && s.role == from.role && s.start >= from.start).collect();
    run.sort_by_key(|s| s.start);
    for s in run {
        if s.start - end > Duration::days(3) { break; }
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
pub fn when(t: NaiveDateTime) -> String { t.format("%a %-d %b, %H:%M").to_string() }

#[cfg(test)]
mod tests {
    use super::*;

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, d).unwrap().and_hms_opt(h, m, 0).unwrap()
    }
    fn snap(t: NaiveDateTime) -> Snapshot { Snapshot::build(&mock("OP", t), t) }

    #[test]
    fn the_mock_matches_the_firehydrant_calendar() {
        let got: Vec<_> = snap(at(10, 14, 5)).weeks.iter()
            .map(|(d, p, s)| (d.day(), p.clone().unwrap().0, s.clone().unwrap().0)).collect();
        let want = [(9, "OP", "DB"), (16, "RW", "OP"), (23, "KB", "RW"), (30, "DB", "KB"), (7, "OP", "DB")];
        assert_eq!(got, want.map(|(d, p, s)| (d, p.to_string(), s.to_string())));
    }

    #[test]
    fn on_call_now_and_the_next_shift() {
        let s = snap(at(10, 14, 5));
        assert_eq!(s.primary.as_ref().unwrap().who, "OP");
        assert_eq!(s.mine_now.as_ref().unwrap().role, Role::Primary);
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
        shifts.extend((16..=20).map(|d| day(d, "RW", Role::Primary)));
        let data = Data { team: "T".into(), me: "OP".into(), shifts, incidents: vec![], origin: Origin::Mock };

        let s = Snapshot::build(&data, at(10, 11, 0));
        assert_eq!(s.mine_rotation_end, Some(at(13, 16, 0)));
        assert_eq!(s.rotation_end, Some(at(13, 16, 0)));
        let next = s.mine_next.unwrap();
        assert_eq!((next.role, next.start), (Role::Secondary, at(16, 9, 0)), "next is the next rotation, not tomorrow");

        let evening = Snapshot::build(&data, at(10, 18, 0));
        assert_eq!(evening.mine_now, None);
        assert_eq!(evening.mine_next.unwrap().start, at(11, 9, 0), "off shift, next is tomorrow morning");
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
