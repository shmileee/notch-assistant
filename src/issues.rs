//! Linear: your open issues and the issues and projects Linear notified you about, from Linear
//! (`linear.rs`) or the mock here, and which notifications you haven't read.
//!
//! A notification is unread while Linear has it unread (you haven't opened it there) and it
//! came after you last opened its issue from the panel. With neither, it counts from a week
//! before the app first connected: Linear keeps unread notifications for months, and only the
//! recent ones are news.

use chrono::{Duration, NaiveDateTime};

use crate::oncall::Origin;
use crate::read::ReadState;

/// Where Linear's read state is kept (`read.rs`).
pub const READ_FILE: &str = "linear-read";

/// How far back a first run counts unread notifications.
pub fn look_back() -> Duration { Duration::days(7) }

#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub id: String,
    pub at: NaiveDateTime,
    /// Read in Linear, or snoozed there.
    pub read: bool,
    /// Who did it; empty for Linear's own reminders (an SLA breach, say).
    pub who: String,
    /// "Ada Byron mentioned you", "SLA breached".
    pub headline: String,
    /// What they wrote; may be empty.
    pub excerpt: String,
    /// Where to open it; empty in the mock.
    pub url: String,
}

/// An issue or a project.
#[derive(Clone, Debug, PartialEq)]
pub struct Ticket {
    /// Linear's ID, which read state is kept under.
    pub id: String,
    /// "MAR-214"; empty for a project.
    pub key: String,
    pub title: String,
    /// Empty in the mock.
    pub url: String,
    /// "In Progress"; "Project" for a project.
    pub state: String,
    /// The state's colour in Linear, as 0xRRGGBB.
    pub color: u32,
    /// Open and assigned to you.
    pub assigned: bool,
    /// In a started state (In Progress, In Review).
    pub started: bool,
    pub updated: NaiveDateTime,
    /// Oldest first.
    pub notices: Vec<Notice>,
}

impl Ticket {
    /// The notifications you haven't read, oldest first.
    pub fn unread(&self, read: &ReadState) -> Vec<&Notice> {
        let last = read.last_read(&self.id, None);
        self.notices.iter().filter(|n| !n.read && n.at > last).collect()
    }

    /// "MAR-214 Rotate the credentials", or a project's name.
    pub fn name(&self) -> String {
        if self.key.is_empty() { self.title.clone() } else { format!("{} {}", self.key, self.title) }
    }
}

/// One fetch's worth of Linear.
#[derive(Clone, Debug, PartialEq)]
pub struct Issues {
    pub me: String,
    pub list: Vec<Ticket>,
    pub origin: Origin,
}

impl Issues {
    pub fn loading() -> Self { Issues { me: String::new(), list: vec![], origin: Origin::Loading } }
}

/// Unread notifications across tickets, keyed for `Announced`.
pub fn unread_events<'a>(list: &'a [Ticket], read: &ReadState) -> Vec<(String, (&'a Ticket, &'a Notice))> {
    list.iter().flat_map(|t| t.unread(read).into_iter().map(move |n| (n.id.clone(), (t, n)))).collect()
}

/// What the panel lists: tickets with something unread, newest first, then your open issues,
/// started ones first, by last update.
pub fn shown<'a>(list: &'a [Ticket], read: &ReadState) -> Vec<(&'a Ticket, Vec<&'a Notice>)> {
    let mut rows: Vec<_> = list.iter().map(|t| (t, t.unread(read))).filter(|(t, unread)| t.assigned || !unread.is_empty()).collect();
    let newest = |unread: &[&Notice]| unread.iter().map(|n| n.at).max();
    rows.sort_by_key(|(t, unread)| std::cmp::Reverse((!unread.is_empty(), newest(unread), t.started, t.updated)));
    rows
}

pub fn unread_count(list: &[Ticket], read: &ReadState) -> usize {
    list.iter().filter(|t| !t.unread(read).is_empty()).count()
}

/// Made-up issues for a made-up team: two with something unread, the same people as the
/// on-call mock.
pub fn mock(now: NaiveDateTime) -> Issues {
    let ago = |m: i64| now - Duration::minutes(m);
    let notice = |id: &str, m: i64, read: bool, who: &str, headline: &str, excerpt: &str| Notice {
        id: id.into(), at: ago(m), read, who: who.into(), headline: headline.into(), excerpt: excerpt.into(), url: String::new(),
    };
    let ticket = |id: &str, key: &str, title: &str, state: &str, color: u32, assigned: bool, started: bool, updated: i64, notices: Vec<Notice>| Ticket {
        id: id.into(), key: key.into(), title: title.into(), url: String::new(), state: state.into(), color, assigned, started,
        updated: ago(updated), notices,
    };
    let list = vec![
        ticket("mock-214", "MAR-214", "Rotate the ingest service's Kafka credentials", "In Progress", 0xf2c94c, true, true, 20, vec![
            notice("n1", 20, false, "JY", "JY mentioned you", "Can you pair on this after standup? The old secret expires on Friday."),
        ]),
        ticket("mock-176", "MAR-176", "Postmortem: the ingest outage on 3 Nov", "Done", 0x5e6ad2, false, false, 45, vec![
            notice("n2", 45, false, "ZQ", "ZQ moved it to Done", ""),
        ]),
        ticket("mock-198", "MAR-198", "Alert on consumer lag per partition", "In Review", 0x26b5ce, true, true, 130, vec![
            notice("n3", 130, true, "XV", "XV commented", "Looks good; let's ship it after the freeze."),
        ]),
        ticket("mock-231", "MAR-231", "Upgrade the CI runner images", "Todo", 0xe2e2e2, true, false, 2880, vec![]),
        ticket("mock-arm64", "", "Ingest on arm64", "Project", 0x95a2b3, false, false, 4000, vec![
            notice("n4", 4000, true, "XV", "XV added you to the project", ""),
        ]),
    ];
    Issues { me: "OP".into(), list, origin: Origin::Mock }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(h: u32, m: u32) -> NaiveDateTime { NaiveDate::from_ymd_opt(2026, 11, 10).unwrap().and_hms_opt(h, m, 0).unwrap() }

    #[test]
    fn unread_needs_linear_and_the_panel_to_agree() {
        let now = at(14, 5);
        let mut read = ReadState::memory(now - look_back());
        let list = mock(now).list;
        assert_eq!(list[0].unread(&read).len(), 1);
        assert!(list[2].unread(&read).is_empty(), "read in Linear");
        read.mark("mock-214", now);
        assert!(list[0].unread(&read).is_empty(), "opened from the panel");

        let old = Ticket { notices: vec![Notice { at: now - Duration::days(30), ..list[0].notices[0].clone() }], ..list[0].clone() };
        assert!(old.unread(&ReadState::memory(now - look_back())).is_empty(), "unread in Linear but from before the look-back");
    }

    #[test]
    fn the_mock_lists_unread_then_your_open_issues() {
        let now = at(14, 5);
        let issues = mock(now);
        let read = ReadState::memory(now - look_back());
        let rows: Vec<_> = shown(&issues.list, &read).iter().map(|(t, u)| (t.key.clone(), u.len())).collect();
        assert_eq!(rows, [("MAR-214".to_string(), 1), ("MAR-176".to_string(), 1), ("MAR-198".to_string(), 0), ("MAR-231".to_string(), 0)],
            "unread newest first, then open issues started first; the read project isn't listed");
        assert_eq!(unread_count(&issues.list, &read), 2);
        assert_eq!(unread_events(&issues.list, &read).len(), 2);
    }
}
