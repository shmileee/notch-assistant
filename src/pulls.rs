//! Your pull requests and what other people did on them, the pull requests you're asked to
//! review, from GitHub (`github.rs`) or the mock here, and which of that you haven't read yet.
//!
//! Unread is what others did after you last read a pull request: the later of when you opened
//! it from the panel and when GitHub last saw you read it (the notification's `last_read_at`,
//! set when you open it on github.com). A pull request with neither counts from when the app
//! first connected, so a first run doesn't light up every old comment. GitHub sends no
//! notification for a reaction, so a reaction stays unread until you read the pull request
//! after it.

use chrono::Duration;

use crate::time::Time;

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
    ReviewRequested(Request),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request { You, Team(String) }

#[derive(Clone, Debug, PartialEq)]
pub struct Activity {
    /// Their name on GitHub, else their login.
    pub who: String,
    pub at: Time,
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
            Kind::ReviewRequested(Request::You) => "requested your review".to_string(),
            Kind::ReviewRequested(Request::Team(team)) => format!("requested review from {team}"),
        };
        format!("{} {what}", self.who)
    }

    /// Tells one event from another across refreshes, to notice the new ones.
    fn key(&self, pull: &Pull) -> String { format!("{}|{}|{}|{:?}", pull.id, self.at.timestamp(), self.who, self.kind) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status { Open, Draft, Merged, Closed }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checks { None, Pending, Passing, Failing }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Review { None, Required, Approved, ChangesRequested }

/// Whether GitHub would merge it now: `Ready` is approved with every requirement met.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Merge { Ready, Conflicts, Waiting }

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
    /// Who opened it, for one you're asked to review; empty for yours.
    pub author: String,
    /// Set for one you're asked to review.
    pub request: Option<Request>,
    pub status: Status,
    pub checks: Checks,
    pub review: Review,
    pub merge: Merge,
    pub updated: Time,
    /// What other people did, oldest first. Yours and bots' are left out. For one you're asked
    /// to review, just the request.
    pub activity: Vec<Activity>,
    /// When GitHub last saw you read it, if its notification was found.
    pub read_on_github: Option<Time>,
}

/// One fetch's worth of pull requests.
#[derive(Clone, Debug, PartialEq)]
pub struct Pulls {
    pub login: String,
    /// Your open pull requests, and the recently closed ones.
    pub list: Vec<Pull>,
    /// Others' pull requests waiting for a review from you or a team of yours.
    pub reviews: Vec<Pull>,
    /// Whether `reviews` has loaded at least once since connecting. Until then it's empty for
    /// want of an answer, so the requests that turn up later aren't new.
    pub reviews_loaded: bool,
    pub origin: Origin,
    /// What's wrong, one plain sentence each, for the panel to show: a part that couldn't
    /// refresh (its last good data stays), organisations hidden behind SSO, GitHub's read state
    /// unavailable.
    pub trouble: Vec<String>,
    /// Lists cut short at GitHub's page size: worth saying, but nothing is wrong.
    pub limits: Vec<String>,
}

impl Pulls {
    pub fn loading() -> Self {
        Pulls { login: String::new(), list: vec![], reviews: vec![], reviews_loaded: false, origin: Origin::Loading, trouble: vec![], limits: vec![] }
    }
}

impl Pull {
    /// What others did after you last read it, oldest first.
    pub fn unread(&self, read: &ReadState) -> Vec<&Activity> {
        let last = read.last_read(&self.id, self.read_on_github);
        self.activity.iter().filter(|a| a.at > last).collect()
    }

    /// Whether its unread count shows: anything on yours, and a request to review that asked
    /// you. One that asked only a team of yours is listed and announced but not counted.
    pub fn counts(&self) -> bool { !matches!(self.request, Some(Request::Team(_))) }
}

/// Unread events across pull requests, keyed for `Announced`.
pub fn unread_events<'a>(pulls: &'a [Pull], read: &ReadState) -> Vec<(String, (&'a Pull, &'a Activity))> {
    pulls.iter().flat_map(|p| p.unread(read).into_iter().map(move |a| (a.key(p), (p, a)))).collect()
}

/// What the panel lists: unread first (counted before not), newest event first, then the rest
/// with ready to merge and asked of you first, by last update. A closed pull request shows only
/// while something on it is unread.
pub fn shown<'a>(pulls: &'a [Pull], read: &ReadState) -> Vec<(&'a Pull, Vec<&'a Activity>)> {
    let mut rows: Vec<_> = pulls.iter().map(|p| (p, p.unread(read)))
        .filter(|(p, unread)| matches!(p.status, Status::Open | Status::Draft) || !unread.is_empty())
        .collect();
    let newest = |unread: &[&Activity]| unread.iter().map(|a| a.at).max();
    rows.sort_by_key(|(p, unread)| {
        let first = p.merge == Merge::Ready || p.request == Some(Request::You);
        std::cmp::Reverse((p.counts() && !unread.is_empty(), !unread.is_empty(), newest(unread), first, p.updated))
    });
    rows
}

pub fn unread_count(pulls: &[Pull], read: &ReadState) -> usize {
    pulls.iter().filter(|p| p.counts() && !p.unread(read).is_empty()).count()
}

/// Made-up pull requests, with the same people as the on-call mock: three of yours with
/// something unread, one of them ready to merge, and three to review, two of them new.
pub fn mock(now: Time) -> Pulls {
    let ago = |m: i64| now - Duration::minutes(m);
    let did = |who: &str, m: i64, kind: Kind, excerpt: &str| Activity { who: who.into(), at: ago(m), kind, excerpt: excerpt.into(), url: String::new() };
    let pull = |id: &str, repo: &str, number: u64, title: &str, status, checks, review, activity: Vec<Activity>, read: i64| Pull {
        id: id.into(), repo: repo.into(), number, title: title.into(), url: String::new(), author: String::new(), request: None,
        status, checks, review, merge: Merge::Waiting,
        updated: activity.iter().map(|a| a.at).max().unwrap_or(ago(600)), activity, read_on_github: Some(ago(read)),
    };
    let ready = |p: Pull| Pull { merge: Merge::Ready, ..p };
    let review = |id: &str, repo: &str, number: u64, title: &str, author: &str, request: Request, m: i64, checks, read: i64| Pull {
        author: author.into(), request: Some(request.clone()),
        ..pull(id, repo, number, title, Status::Open, checks, Review::Required, vec![did(author, m, Kind::ReviewRequested(request), "")], read)
    };
    let list = vec![
        ready(pull("mock-482", "marmalade/telemetry-ingest", 482, "Raise the consumer lag alert to five minutes", Status::Open, Checks::Passing, Review::Approved, vec![
            did("XV", 40, Kind::Approved, ""),
            did("JY", 12, Kind::Commented, "Can we keep paging for lag over 15 minutes? The night shift relies on it."),
            did("ZQ", 3, Kind::Reacted { emoji: "👍", on_comment: true }, "Bumped it to 5m: the 60 s threshold paged us on every deploy."),
        ], 30)),
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
    let team = || Request::Team("platform".into());
    let reviews = vec![
        review("mock-1302", "marmalade/api-gateway", 1302, "Add rate-limit headers to every response", "JY", Request::You, 8, Checks::Passing, 600),
        review("mock-490", "marmalade/telemetry-ingest", 490, "Batch writes to the cold store", "ZQ", team(), 50, Checks::Pending, 600),
        review("mock-41", "marmalade/runbooks", 41, "Document the failover drill", "XV", team(), 2900, Checks::None, 2000),
    ];
    Pulls { login: "OP".into(), list, reviews, reviews_loaded: true, origin: Origin::Mock, trouble: vec![], limits: vec![] }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn at(h: u32, m: u32) -> Time { crate::time::at(2026, 11, 10, h, m) }

    fn pull(activity: &[(u32, u32)], read_on_github: Option<Time>) -> Pull {
        Pull {
            id: "p".into(), repo: "o/r".into(), number: 1, title: "T".into(), url: String::new(), author: String::new(), request: None,
            status: Status::Open, checks: Checks::None, review: Review::None, merge: Merge::Waiting, updated: at(9, 0), read_on_github,
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
        read.mark("mock-482", now);
        assert_eq!(shown(&pulls.list, &read)[1].0.number, 482, "ready to merge goes before the other read ones");
    }

    #[test]
    fn only_requests_that_asked_you_count() {
        let now = at(14, 5);
        let pulls = mock(now);
        let read = ReadState::memory(now - Duration::days(30));
        let rows: Vec<_> = shown(&pulls.reviews, &read).iter().map(|(p, u)| (p.number, u.len())).collect();
        assert_eq!(rows, [(1302, 1), (490, 1), (41, 0)], "asked of you, then of your team, then read");
        assert_eq!(unread_count(&pulls.reviews, &read), 1, "the team's request is listed but not counted");
        assert_eq!(unread_events(&pulls.reviews, &read).len(), 2, "both are announced");
        assert_eq!(pulls.reviews[1].activity[0].headline(), "ZQ requested review from platform");
        assert!(pulls.reviews_loaded, "the mock's requests are as loaded as they get");
        assert!(!Pulls::loading().reviews_loaded);
    }
}
