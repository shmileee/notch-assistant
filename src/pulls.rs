//! Your pull requests and what other people did on them, from GitHub (`github.rs`) or the mock
//! here, and which of that you haven't read yet.
//!
//! Unread is what others did after you last read a pull request: the later of when you opened
//! it from the panel and when GitHub last saw you read it (the notification's `last_read_at`,
//! set when you open it on github.com). A pull request with neither counts from when the app
//! first connected, so a first run doesn't light up every old comment. GitHub sends no
//! notification for a reaction, so a reaction stays unread until you read the pull request
//! after it.

use chrono::{Duration, NaiveDateTime};

use crate::oncall::Origin;
use crate::read::ReadState;

/// Where GitHub's read state is kept (`read.rs`).
pub const READ_FILE: &str = "github-read";

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Commented,
    Approved,
    ChangesRequested,
    Reviewed,
    /// A reaction, as its emoji, to your pull request (`on_comment` false) or a comment of yours.
    Reacted { emoji: &'static str, on_comment: bool },
    Merged,
    Closed,
    Reopened,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Activity {
    /// Their name on GitHub, else their login.
    pub who: String,
    pub at: NaiveDateTime,
    pub kind: Kind,
    /// What they wrote, or for a reaction what it was on; may be empty.
    pub excerpt: String,
    /// Where to open it; empty in the mock.
    pub url: String,
}

impl Activity {
    /// "XV approved", "ZQ reacted 👍 to your comment".
    pub fn headline(&self) -> String {
        let what = match &self.kind {
            Kind::Commented => "commented".to_string(),
            Kind::Approved => "approved".to_string(),
            Kind::ChangesRequested => "requested changes".to_string(),
            Kind::Reviewed => "reviewed".to_string(),
            Kind::Reacted { emoji, on_comment: true } => format!("reacted {emoji} to your comment"),
            Kind::Reacted { emoji, on_comment: false } => format!("reacted {emoji}"),
            Kind::Merged => "merged it".to_string(),
            Kind::Closed => "closed it".to_string(),
            Kind::Reopened => "reopened it".to_string(),
        };
        format!("{} {what}", self.who)
    }

    /// Tells one event from another across refreshes, to notice the new ones.
    fn key(&self, pull: &Pull) -> String { format!("{}|{}|{}|{:?}", pull.id, self.at, self.who, self.kind) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status { Open, Draft, Merged, Closed }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checks { None, Pending, Passing, Failing }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Review { None, Required, Approved, ChangesRequested }

#[derive(Clone, Debug, PartialEq)]
pub struct Pull {
    /// GitHub's node ID, which read state is kept under.
    pub id: String,
    /// "owner/name".
    pub repo: String,
    pub number: u64,
    pub title: String,
    /// Empty in the mock.
    pub url: String,
    pub status: Status,
    pub checks: Checks,
    pub review: Review,
    pub updated: NaiveDateTime,
    /// What other people did, oldest first. Yours and bots' are left out.
    pub activity: Vec<Activity>,
    /// When GitHub last saw you read it, if its notification was found.
    pub read_on_github: Option<NaiveDateTime>,
}

/// One fetch's worth of pull requests.
#[derive(Clone, Debug, PartialEq)]
pub struct Pulls {
    pub login: String,
    /// Your open pull requests, and the recently closed ones.
    pub list: Vec<Pull>,
    pub origin: Origin,
    /// Things that work but less well than they could (GitHub's read state unavailable,
    /// organisations hidden behind SSO), for the connection test and `--probe-github`.
    pub warnings: Vec<String>,
}

impl Pulls {
    pub fn loading() -> Self { Pulls { login: String::new(), list: vec![], origin: Origin::Loading, warnings: vec![] } }
}

impl Pull {
    /// What others did after you last read it, oldest first.
    pub fn unread(&self, read: &ReadState) -> Vec<&Activity> {
        let last = read.last_read(&self.id, self.read_on_github);
        self.activity.iter().filter(|a| a.at > last).collect()
    }
}

/// Unread events across pull requests, keyed for `Announced`.
pub fn unread_events<'a>(pulls: &'a [Pull], read: &ReadState) -> Vec<(String, (&'a Pull, &'a Activity))> {
    pulls.iter().flat_map(|p| p.unread(read).into_iter().map(move |a| (a.key(p), (p, a)))).collect()
}

/// What the panel lists: unread first, newest event first, then the other open ones by last
/// update. A closed pull request shows only while something on it is unread.
pub fn shown<'a>(pulls: &'a [Pull], read: &ReadState) -> Vec<(&'a Pull, Vec<&'a Activity>)> {
    let mut rows: Vec<_> = pulls.iter().map(|p| (p, p.unread(read)))
        .filter(|(p, unread)| matches!(p.status, Status::Open | Status::Draft) || !unread.is_empty())
        .collect();
    let newest = |unread: &[&Activity]| unread.iter().map(|a| a.at).max();
    rows.sort_by_key(|(p, unread)| std::cmp::Reverse((!unread.is_empty(), newest(unread), p.updated)));
    rows
}

pub fn unread_count(pulls: &[Pull], read: &ReadState) -> usize {
    pulls.iter().filter(|p| !p.unread(read).is_empty()).count()
}

/// Made-up pull requests: three with something unread, and the same people as the on-call mock.
pub fn mock(now: NaiveDateTime) -> Pulls {
    let ago = |m: i64| now - Duration::minutes(m);
    let did = |who: &str, m: i64, kind: Kind, excerpt: &str| Activity { who: who.into(), at: ago(m), kind, excerpt: excerpt.into(), url: String::new() };
    let pull = |id: &str, repo: &str, number: u64, title: &str, status, checks, review, activity: Vec<Activity>, read: i64| Pull {
        id: id.into(), repo: repo.into(), number, title: title.into(), url: String::new(), status, checks, review,
        updated: activity.iter().map(|a| a.at).max().unwrap_or(ago(600)), activity, read_on_github: Some(ago(read)),
    };
    let list = vec![
        pull("mock-482", "marmalade/telemetry-ingest", 482, "Raise the consumer lag alert to five minutes", Status::Open, Checks::Passing, Review::Approved, vec![
            did("XV", 40, Kind::Approved, ""),
            did("JY", 12, Kind::Commented, "Can we keep paging for lag over 15 minutes? The night shift relies on it."),
            did("ZQ", 3, Kind::Reacted { emoji: "👍", on_comment: true }, "Bumped it to 5m: the 60 s threshold paged us on every deploy."),
        ], 30),
        pull("mock-1291", "marmalade/api-gateway", 1291, "Retry idempotent requests on 503", Status::Open, Checks::Failing, Review::ChangesRequested, vec![
            did("ZQ", 130, Kind::ChangesRequested, "POST isn't idempotent here; gate the retry on the method."),
        ], 90),
        pull("mock-36", "marmalade/runbooks", 36, "Add the Kafka rebalance runbook", Status::Open, Checks::Passing, Review::Required, vec![
            did("XV", 25, Kind::Reacted { emoji: "🎉", on_comment: false }, ""),
        ], 300),
        pull("mock-479", "marmalade/telemetry-ingest", 479, "Pin the Kafka client to 3.7", Status::Merged, Checks::Passing, Review::Approved, vec![
            did("XV", 150, Kind::Approved, ""),
            did("XV", 48, Kind::Merged, ""),
        ], 120),
        pull("mock-77", "marmalade/platform-terraform", 77, "Move the ingest workers to arm64", Status::Draft, Checks::Pending, Review::Required, vec![], 900),
    ];
    Pulls { login: "OP".into(), list, origin: Origin::Mock, warnings: vec![] }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(h: u32, m: u32) -> NaiveDateTime { NaiveDate::from_ymd_opt(2026, 11, 10).unwrap().and_hms_opt(h, m, 0).unwrap() }

    fn pull(activity: &[(u32, u32)], read_on_github: Option<NaiveDateTime>) -> Pull {
        Pull {
            id: "p".into(), repo: "o/r".into(), number: 1, title: "T".into(), url: String::new(), status: Status::Open,
            checks: Checks::None, review: Review::None, updated: at(9, 0), read_on_github,
            activity: activity.iter().map(|&(h, m)| Activity { who: "XV".into(), at: at(h, m), kind: Kind::Commented, excerpt: String::new(), url: String::new() }).collect(),
        }
    }

    #[test]
    fn unread_is_what_happened_after_the_latest_read() {
        let mut read = ReadState::memory(at(8, 0));
        let p = pull(&[(7, 0), (9, 0), (11, 0)], None);
        assert_eq!(p.unread(&read).len(), 2, "never read anywhere: counts from when the app first connected");

        let p = pull(&[(7, 0), (9, 0), (11, 0)], Some(at(10, 0)));
        assert_eq!(p.unread(&read).len(), 1, "read on github.com");

        let p = pull(&[(7, 0), (9, 0), (11, 0)], Some(at(6, 0)));
        assert_eq!(p.unread(&read).len(), 3, "GitHub's read time counts from before the app connected too");

        read.mark("p", at(11, 30));
        assert!(p.unread(&read).is_empty(), "opened from the panel");
    }

    #[test]
    fn the_mock_lists_unread_first_and_hides_read_closed_ones() {
        let now = at(14, 5);
        let pulls = mock(now);
        let read = ReadState::memory(now - Duration::days(30));
        let rows = shown(&pulls.list, &read);
        assert_eq!(unread_events(&pulls.list, &read).len(), 4);
        let numbers: Vec<_> = rows.iter().map(|(p, u)| (p.number, u.len())).collect();
        assert_eq!(numbers, [(482, 2), (36, 1), (479, 1), (1291, 0), (77, 0)]);
        assert_eq!(unread_count(&pulls.list, &read), 3);

        let mut read = read;
        read.mark("mock-479", now);
        assert!(shown(&pulls.list, &read).iter().all(|(p, _)| p.number != 479), "merged and read: gone");
    }
}
