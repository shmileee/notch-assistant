//! GitHub client: your pull requests, what others did on them, the pull requests you're asked
//! to review, and when you last read each. Read-only.
//!
//! It looks at your notifications every minute and refreshes only what a changed one touches. A
//! look that finds nothing new is free: GitHub answers `304` and doesn't count it. What never
//! notifies (reactions, checks, merge state, a teammate answering a team's request) is caught by
//! cheap checks on a timer, and a one-point state query says when yours need refreshing.
//!
//! - `GET /notifications?all=true&participating=true`: the latest notification threads. A
//!   changed one says which pull request to refresh, and its `last_read_at` says when you last
//!   read that pull request on github.com. Reading one doesn't move it up the list, so a full
//!   refresh reads the threads of pull requests with activity directly
//!   (`GET /notifications/threads/{id}`), and the first one pages back to find them.
//! - `POST /graphql`: your open pull requests and those closed in the last three days, each with
//!   its last 20 comments, reviews, merges and closes and the reactions to them (`QUERY`). The
//!   state of your open ones: whether GitHub would merge each now, their checks, and how many
//!   reactions they and your latest comments have (`STATE`). The open pull requests waiting for
//!   a review from you or a team of yours (`REVIEWS`).
//!
//! Settings (`INTEGRATION`): a token (Keychain) and logins whose activity to ignore. Bots and
//! your own activity are always ignored.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Local, Utc};

use crate::time::Time;
use serde_json::{Value, json};

use crate::integrations::{Field, FieldKind, Integration};
use crate::feed::Fetched;
use crate::oncall::Origin;
use crate::pulls::{self, Activity, Checks, Kind, Merge, Pull, Pulls, Request, Review, Status, unread_count};
use crate::read::ReadState;

const API: &str = "https://api.github.com";

pub const INTEGRATION: Integration = Integration {
    id: "github",
    name: "GitHub",
    summary: "Your pull requests, what others did on them since you last looked, and the ones you're asked to review.",
    fields: &[
        Field { key: "token", label: "Token", kind: FieldKind::Secret, env: "NOTCH_GITHUB_TOKEN",
            help: "A classic token with the repo scope, authorized for your organization's SSO (gh auth token prints one). \
                   A fine-grained token can't read GitHub's notifications, so then only opening a pull request from the panel marks it read.",
            placeholder: || "Paste the token".into() },
        Field { key: "ignore", label: "Ignore activity from", kind: FieldKind::Text, env: "NOTCH_GITHUB_IGNORE",
            help: "Logins, comma-separated: machine users that comment like people. Bots and you are always ignored.",
            placeholder: || "None".into() },
    ],
    required: "token",
    test,
    legacy: &[],
};

/// How often to look at your notifications: GitHub's poll interval, unless it asks for longer.
pub const EVERY: Duration = Duration::from_secs(60);
/// How often each part refreshes when nothing calls for it sooner. With nothing happening that's
/// about 100 GraphQL points an hour (`STATE` 1, `REVIEWS` 3, `QUERY` 69) of the 5,000 that the
/// token shares with anything else using it (gh, say).
const STATE_EVERY: Duration = Duration::from_secs(5 * 60);
const REVIEWS_EVERY: Duration = Duration::from_secs(10 * 60);
/// Read times of threads that dropped off page 1, which reading on github.com doesn't move.
const THREADS_EVERY: Duration = Duration::from_secs(10 * 60);
/// Catches what the state query can't see: reactions to your inline review comments.
const OWN_EVERY: Duration = Duration::from_secs(60 * 60);
/// Yours and the requests to review this often instead while notifications can't be read.
const BLIND_EVERY: Duration = Duration::from_secs(3 * 60);
/// How far the first full refresh pages back for threads. GitHub gives at most 50 a page.
const PER_PAGE: usize = 50;
const FIRST_PAGES: usize = 10;
/// Threads read one by one in a full refresh, newest activity first. An unchanged one is free.
const THREAD_READS: usize = 10;
/// Pages of 100 requests to review. Bots' are dropped after the search too, so it can't stop at
/// 100.
const REVIEW_PAGES: usize = 3;
/// The least time between refreshes of a part that notifications call for, so a burst of them
/// costs one refresh. With it, and one page of requests to review, the panel spends at most
/// about 1,450 GraphQL points an hour.
const GAP: Duration = Duration::from_secs(3 * 60);
/// The first retry of a part that failed. Each one after waits twice as long, up to the part's
/// usual interval.
const RETRY: Duration = Duration::from_secs(60);

fn test() -> Result<String, String> {
    if crate::demo::on() {
        std::thread::sleep(Duration::from_millis(1200));
        return Ok("Connected as OP: 4 open pull requests and 3 to review, 4 with something unread".into());
    }
    let mut client = Client::from_settings().ok_or("No token saved")?;
    let now = Local::now();
    let pulls = client.fetch(now)?;
    let open = pulls.list.iter().filter(|p| matches!(p.status, Status::Open | Status::Draft)).count();
    let read = ReadState::load(pulls::READ_FILE, now);
    let unread = unread_count(&pulls.list, &read) + unread_count(&pulls.reviews, &read);
    let mut message = format!("Connected as {}: {open} open pull requests and {} to review, {unread} with something unread",
        pulls.login, pulls.reviews.len());
    for w in &pulls.trouble { message.push_str(". "); message.push_str(w); }
    Ok(message)
}

pub struct Client {
    token: String,
    ignore: HashSet<String>,
    agent: crate::net::Http,
    login: String,
    own: Vec<Pull>,
    reviews: Vec<Pull>,
    reviews_loaded: bool,
    /// Lists longer than the queries read: your open pull requests (by `QUERY` and by `STATE`),
    /// the recently closed ones, and the requests to review.
    own_cut: bool,
    state_cut: bool,
    reviews_cut: bool,
    /// From `STATE`, by pull request ID.
    states: HashMap<String, PullState>,
    /// Reaction counts from `STATE`, to see new reactions by.
    reactions: Option<Reactions>,
    warned: Warned,
    /// Notification threads by pull request (`key`), as found so far.
    threads: HashMap<String, Thread>,
    /// The pull requests on page 1 at the last look: their read times are current.
    on_page: HashSet<String>,
    /// Page 1's ETag: GitHub answers a look with it `304` while nothing changed.
    etag: Option<String>,
    /// No look before this: GitHub's poll interval.
    look_after: Option<SystemTime>,
    /// Why GitHub refused the last look, if it did.
    blind: Option<String>,
    /// Why the last look failed otherwise (a rate limit, the network), if it did.
    look_failed: Option<String>,
    /// The first read of threads paging back is done.
    paged: bool,
    /// When each part last refreshed.
    at: Ages,
    /// Parts that failed and are owed a refresh.
    owed: Plan,
    failed: Failures,
    /// Rate limited: no GraphQL or REST calls before these.
    graphql_after: Cell<Option<SystemTime>>,
    rest_after: Cell<Option<SystemTime>>,
    /// Since the last refresh: looks, GraphQL points, and REST calls that counted (not a `304`).
    looks: u32,
    points: Cell<u64>,
    calls: Cell<u32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Thread {
    id: String,
    /// When you last read it.
    read: Option<Time>,
    updated: Option<Time>,
    /// Why you get it: author, review_requested, comment, ...
    reason: String,
    /// For reading the thread alone: unchanged, that's free.
    etag: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct PullState { merge: String, approved: bool, checks: Checks }

/// Reaction counts by ID: of your open pull requests, and of your latest comments.
#[derive(Clone, Debug, Default, PartialEq)]
struct Reactions { open: HashMap<String, u64>, comments: HashMap<String, u64> }

#[derive(Default)]
struct Ages { own: Option<SystemTime>, state: Option<SystemTime>, reviews: Option<SystemTime>, threads: Option<SystemTime> }

/// What each part's last answer warned of (organisations hidden behind SSO).
#[derive(Default)]
struct Warned { own: Vec<String>, state: Vec<String>, reviews: Vec<String> }

/// What to refresh.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Plan { own: bool, state: bool, reviews: bool, threads: bool }

impl std::ops::BitOr for Plan {
    type Output = Plan;
    fn bitor(self, o: Plan) -> Plan {
        Plan { own: self.own || o.own, state: self.state || o.state, reviews: self.reviews || o.reviews, threads: self.threads || o.threads }
    }
}

#[derive(Default)]
struct Failures { state: Option<Failed>, reviews: Option<Failed> }

struct Failed { why: String, times: u32, retry: SystemTime }

impl Failed {
    fn again(last: Option<Failed>, why: String, every: Duration, now: SystemTime) -> Failed {
        let times = last.map_or(1, |f| f.times + 1);
        Failed { why, times, retry: now + RETRY.saturating_mul(2u32.saturating_pow(times - 1)).min(every) }
    }
}

fn ready(failed: &Option<Failed>, now: SystemTime) -> bool { failed.as_ref().is_none_or(|f| f.retry <= now) }

/// A call that failed. `refused` when GitHub said no for a reason other than a rate limit, which
/// trying again won't change.
struct Failure { message: String, refused: bool }

impl From<Failure> for String { fn from(f: Failure) -> String { f.message } }

impl Client {
    /// A client from the saved settings; `None` without a token.
    pub fn from_settings() -> Option<Self> {
        let token = INTEGRATION.value("token")?;
        let ignore = INTEGRATION.value("ignore").unwrap_or_default()
            .split(|c: char| c == ',' || c.is_whitespace()).filter(|s| !s.is_empty()).map(str::to_lowercase).collect();
        let agent = crate::net::Http::new("api.github.com");
        Some(Client::new(token, ignore, agent))
    }

    fn new(token: String, ignore: HashSet<String>, agent: crate::net::Http) -> Self {
        Client {
            token, ignore, agent, login: String::new(), own: vec![], reviews: vec![], reviews_loaded: false,
            own_cut: false, state_cut: false, reviews_cut: false, states: HashMap::new(), reactions: None,
            warned: Warned::default(), threads: HashMap::new(), on_page: HashSet::new(), etag: None,
            look_after: None, blind: None, look_failed: None, paged: false, at: Ages::default(), owed: Plan::default(),
            failed: Failures::default(), graphql_after: Cell::new(None), rest_after: Cell::new(None),
            looks: 0, points: Cell::new(0), calls: Cell::new(0),
        }
    }

    fn call(&self, req: ureq::Request, body: Option<Value>) -> Result<ureq::Response, Failure> {
        let graphql = body.is_some();
        let after = if graphql { &self.graphql_after } else { &self.rest_after };
        if let Some(t) = holding(after) { return Err(Failure { message: limited(t), refused: false }); }
        let req = req.set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", "application/vnd.github+json").set("X-GitHub-Api-Version", "2022-11-28");
        let result = match body { Some(b) => req.send_json(b), None => req.call() };
        match result {
            Ok(resp) => {
                if !graphql && resp.status() != 304 { self.calls.set(self.calls.get() + 1); }
                Ok(resp)
            }
            Err(ureq::Error::Status(code, resp)) => {
                let (failure, until) = refusal(code, resp);
                if let Some(t) = until { after.set(Some(t)); }
                Err(failure)
            }
            Err(e) => Err(Failure { message: format!("network: {e}"), refused: false }),
        }
    }

    fn rest(&self, path: &str) -> Result<Value, String> {
        self.call(self.agent.get(&format!("{API}{path}")), None)?.into_json().map_err(|e| format!("{path}: bad JSON ({e})"))
    }

    /// `path`'s JSON and ETag, or `None` when it hasn't changed since `etag`.
    fn rest_if(&self, path: &str, etag: Option<&str>) -> Result<Option<(Value, Option<String>)>, String> {
        let mut req = self.agent.get(&format!("{API}{path}"));
        if let Some(e) = etag { req = req.set("If-None-Match", e); }
        let resp = self.call(req, None)?;
        if resp.status() == 304 { return Ok(None); }
        let etag = resp.header("etag").map(str::to_string);
        Ok(Some((resp.into_json().map_err(|e| format!("{path}: bad JSON ({e})"))?, etag)))
    }

    /// A GraphQL query's data. Errors that come with data (an organization that hides behind
    /// SSO, say) are logged. GitHub reports a spent budget as an error with status 200.
    fn graphql(&self, query: &str, variables: Value, warnings: &mut Vec<String>) -> Result<Value, String> {
        let resp = self.call(self.agent.post(&format!("{API}/graphql")), Some(json!({ "query": query, "variables": variables })))?;
        let sso = "Some organizations hide their pull requests until you authorize the token for SSO".to_string();
        if resp.header("x-github-sso").is_some_and(|h| h.contains("partial-results")) && !warnings.contains(&sso) {
            warnings.push(sso);
        }
        let retry = retry_at(&resp);
        let mut body: Value = resp.into_json().map_err(|e| format!("graphql: bad JSON ({e})"))?;
        let errors: Vec<&Value> = body.get("errors").and_then(Value::as_array).into_iter().flatten().collect();
        let message = |e: &&Value| str_at(e, "message").unwrap_or_default().to_string();
        if errors.iter().any(|e| str_at(e, "type") == Some("RATE_LIMITED") || message(e).to_lowercase().contains("rate limit")) {
            self.graphql_after.set(Some(retry));
            return Err(limited(retry));
        }
        if body.get("data").is_none_or(Value::is_null) {
            return Err(format!("graphql: {}", errors.first().map(message).unwrap_or("no data".into())));
        }
        for e in &errors { eprintln!("github: {}", message(e)); }
        self.points.set(self.points.get() + body["data"]["rateLimit"]["cost"].as_u64().unwrap_or(0));
        Ok(body["data"].take())
    }

    /// Everything, whatever the notifications say.
    pub fn fetch(&mut self, now: Time) -> Result<Pulls, String> {
        self.at = Ages::default();
        self.failed = Failures::default();
        self.tick(now)
    }

    /// Looks at your notifications and refreshes what they say changed and what's due. Either way,
    /// what it returns is current as of `now`.
    pub fn tick(&mut self, now: Time) -> Result<Pulls, String> {
        if let Some(t) = holding(&self.graphql_after) { return Err(limited(t)); }
        let wall = SystemTime::now();
        let page = if self.blind.is_none() || self.at.own.is_none_or(|t| ago(t, wall) >= BLIND_EVERY) {
            let result = self.look();
            self.looked(result)
        } else { None };
        let changed = page.as_ref().map(|p| changes(&self.threads, p)).unwrap_or_default();
        let called = self.owed | triggers(&changed, &keys(&self.own), &keys(&self.reviews));
        let mut plan = self.plan(called, wall);
        if let Some(page) = page {
            self.on_page = page.iter().map(|(k, _)| k.clone()).collect();
            for (k, t) in page {
                let etag = self.threads.get(&k).and_then(|old| old.etag.clone());
                self.threads.insert(k, Thread { etag, ..t });
            }
        }
        if plan.state && self.refresh_state() {
            if soon(self.at.own, wall) { self.owed.own = true; } else { plan.own = true; }
        }
        if plan.own && let Err(e) = self.refresh_own(now) {
            self.owed.own = true;
            self.owed.reviews |= plan.reviews;
            return Err(e);
        }
        if plan.reviews { self.refresh_reviews(); }
        if plan.threads { self.read_threads(); }
        self.looks += 1;
        if plan != Plan::default() { self.log(&plan); }
        Ok(self.pulls(now))
    }

    /// What to refresh now: what's due, and what `called` for unless it refreshed moments ago or
    /// failed and waits to retry. What waits is owed.
    fn plan(&mut self, called: Plan, now: SystemTime) -> Plan {
        let mut plan = self.due(now);
        plan.own |= called.own && !soon(self.at.own, now);
        plan.reviews |= called.reviews && !soon(self.at.reviews, now);
        plan.state &= ready(&self.failed.state, now);
        plan.reviews &= ready(&self.failed.reviews, now);
        self.owed = Plan { own: called.own && !plan.own, reviews: called.reviews && !plan.reviews, ..Plan::default() };
        plan
    }

    /// The parts whose time has come.
    fn due(&self, now: SystemTime) -> Plan {
        let after = |at: Option<SystemTime>, every: Duration| at.is_none_or(|t| ago(t, now) >= every);
        let blind = self.unnotified();
        Plan {
            own: after(self.at.own, if blind { BLIND_EVERY } else { OWN_EVERY }),
            state: after(self.at.state, STATE_EVERY),
            reviews: after(self.at.reviews, if blind { BLIND_EVERY } else { REVIEWS_EVERY }),
            threads: !blind && after(self.at.threads, THREADS_EVERY),
        }
    }

    /// Notifications don't say what changed: GitHub refuses them, or they're failing for now. Then
    /// yours and the requests to review refresh every `BLIND_EVERY`.
    fn unnotified(&self) -> bool { self.blind.is_some() || self.look_failed.is_some() }

    /// Page 1 of your notifications as threads by `key`, or `None` if it hasn't changed since the
    /// last look.
    fn look(&mut self) -> Result<Option<Vec<(String, Thread)>>, Failure> {
        if self.look_after.is_some_and(|t| SystemTime::now() < t) { return Ok(None); }
        let mut req = self.agent.get(&format!("{API}/notifications?all=true&participating=true&per_page={PER_PAGE}"));
        if let Some(e) = &self.etag { req = req.set("If-None-Match", e); }
        let resp = self.call(req, None)?;
        self.take_look(resp).map_err(|message| Failure { message, refused: false })
    }

    /// Takes in a look's result. Only GitHub refusing the notifications makes it blind: a rate
    /// limit or the network says nothing about the token.
    fn looked(&mut self, result: Result<Option<Vec<(String, Thread)>>, Failure>) -> Option<Vec<(String, Thread)>> {
        match result {
            Ok(page) => { self.blind = None; self.look_failed = None; page }
            Err(f) if f.refused => {
                if self.blind.is_none() { eprintln!("github: notifications: {}", f.message); }
                self.blind = Some(f.message);
                self.look_failed = None;
                None
            }
            Err(f) => {
                if self.look_failed.is_none() { eprintln!("github: notifications: {}", f.message); }
                self.look_failed = Some(f.message);
                None
            }
        }
    }

    fn take_look(&mut self, resp: ureq::Response) -> Result<Option<Vec<(String, Thread)>>, String> {
        // Less a little, so a tick that comes a moment early doesn't skip a whole interval.
        let poll = resp.header("x-poll-interval").and_then(|s| s.parse().ok()).unwrap_or(0);
        self.look_after = Some(SystemTime::now() + Duration::from_secs(poll).saturating_sub(Duration::from_secs(5)));
        if resp.status() == 304 { return Ok(None); }
        let etag = resp.header("etag").map(str::to_string);
        let page: Value = resp.into_json().map_err(|e| format!("notifications: bad JSON ({e})"))?;
        let first = self.etag.is_none();
        self.etag = etag;
        let threads: Vec<_> = page.as_array().into_iter().flatten().filter_map(thread).collect();
        if first {
            // Nothing to compare the first page with: take it as what's seen, not as news.
            self.on_page = threads.iter().map(|(k, _)| k.clone()).collect();
            for (k, t) in threads { self.threads.insert(k, t); }
            return Ok(None);
        }
        Ok(Some(threads))
    }

    /// Your pull requests, with their activity.
    fn refresh_own(&mut self, now: Time) -> Result<(), String> {
        let mut warnings = vec![];
        let since = (now - chrono::Duration::days(3)).format("%Y-%m-%d");
        let variables = json!({
            "open": "is:pr is:open author:@me archived:false sort:updated-desc",
            "closed": format!("is:pr is:closed author:@me archived:false updated:>={since} sort:updated-desc"),
        });
        let data = self.graphql(QUERY, variables, &mut warnings)?;
        self.take_own(&data, warnings);
        Ok(())
    }

    fn take_own(&mut self, data: &Value, warnings: Vec<String>) {
        self.login = str_at(&data["viewer"], "login").unwrap_or_default().to_string();
        let who = Who { me: self.login.to_lowercase(), ignore: &self.ignore };
        let own = ["open", "closed"].iter()
            .flat_map(|k| data[*k]["nodes"].as_array().cloned().unwrap_or_default())
            .filter_map(|n| pull(&n, &who))
            .collect();
        self.own = own;
        self.own_cut = data["open"]["pageInfo"]["hasNextPage"] == json!(true);
        self.warned.own = warnings;
        self.at.own = Some(SystemTime::now());
    }

    /// Updates `states` and `reactions`; true when yours need refreshing (see `reacted`). On failure
    /// the last ones stand: a refresh is no reason to go offline.
    fn refresh_state(&mut self) -> bool {
        let q = "is:pr is:open author:@me archived:false sort:updated-desc";
        let mut warnings = vec![];
        match self.graphql(STATE, json!({ "q": q }), &mut warnings) {
            Ok(data) => { self.failed.state = None; self.warned.state = warnings; self.take_state(&data) }
            Err(e) => {
                eprintln!("github: state: {e}");
                self.failed.state = Some(Failed::again(self.failed.state.take(), e, STATE_EVERY, SystemTime::now()));
                false
            }
        }
    }

    fn take_state(&mut self, data: &Value) -> bool {
        self.at.state = Some(SystemTime::now());
        self.state_cut = data["search"]["pageInfo"]["hasNextPage"] == json!(true);
        let open: Vec<&Value> = data["search"]["nodes"].as_array().into_iter().flatten().collect();
        self.states = open.iter().filter_map(|n| Some((str_at(n, "id")?.to_string(), PullState {
            merge: str_at(n, "mergeStateStatus").unwrap_or_default().to_string(),
            approved: n["approvals"]["totalCount"].as_u64().unwrap_or(0) > 0,
            checks: checks(n),
        }))).collect();
        let counts = |nodes: &mut dyn Iterator<Item = &Value>| nodes
            .filter_map(|n| Some((str_at(n, "id")?.to_string(), n["reactions"]["totalCount"].as_u64().unwrap_or(0))))
            .collect();
        let now = Reactions {
            open: counts(&mut open.iter().copied()),
            comments: counts(&mut data["viewer"]["issueComments"]["nodes"].as_array().into_iter().flatten()),
        };
        let changed = self.reactions.as_ref().is_some_and(|before| reacted(before, &now));
        self.reactions = Some(now);
        changed
    }

    /// Updates the requests to review; on failure the last ones stand.
    fn refresh_reviews(&mut self) {
        let who = Who { me: self.login.to_lowercase(), ignore: &self.ignore };
        let mut warnings = vec![];
        match self.fetch_reviews(&who, &mut warnings) {
            Ok((found, cut)) => {
                self.reviews = found;
                self.reviews_cut = cut;
                self.reviews_loaded = true;
                self.failed.reviews = None;
                self.warned.reviews = warnings;
                self.at.reviews = Some(SystemTime::now());
            }
            Err(e) => {
                eprintln!("github: requests to review: {e}");
                self.owed.reviews = true;
                let every = if self.unnotified() { BLIND_EVERY } else { REVIEWS_EVERY };
                self.failed.reviews = Some(Failed::again(self.failed.reviews.take(), e, every, SystemTime::now()));
            }
        }
    }

    /// Pull requests waiting for your review or a team's, a page at a time. One updated between
    /// pages can come twice. Renovate and Dependabot, the bots that ask most, are left out of the
    /// search so it fits a page; other bots are dropped after it. True with them when there are
    /// more pages than it reads.
    fn fetch_reviews(&self, who: &Who, warnings: &mut Vec<String>) -> Result<(Vec<Pull>, bool), String> {
        let q = "is:pr is:open draft:false archived:false review-requested:@me -author:app/renovate -author:app/dependabot sort:updated-desc";
        let (mut reviews, mut seen, mut after) = (vec![], HashSet::new(), Value::Null);
        for _ in 0..REVIEW_PAGES {
            let data = self.graphql(REVIEWS, json!({ "q": q, "after": after }), warnings)?;
            let search = &data["search"];
            reviews.extend(search["nodes"].as_array().into_iter().flatten().filter_map(|n| review(n, who)).filter(|p| seen.insert(p.id.clone())));
            if search["pageInfo"]["hasNextPage"] != json!(true) { return Ok((reviews, false)); }
            after = search["pageInfo"]["endCursor"].clone();
        }
        Ok((reviews, true))
    }

    /// Brings read times up to date for pull requests with activity: the first time by paging
    /// back to find their threads, then by reading each thread that has dropped off page 1,
    /// unless you'd already read it past its newest activity.
    fn read_threads(&mut self) {
        self.at.threads = Some(SystemTime::now());
        let mut active: Vec<(String, Time, bool)> = self.own.iter().chain(&self.reviews)
            .filter_map(|p| Some((key(&p.repo, p.number), p.activity.last()?.at, p.counts()))).collect();
        active.sort_by_key(|(_, newest, _)| std::cmp::Reverse(*newest));
        if !self.paged {
            self.paged = true;
            for page in 2..=FIRST_PAGES {
                // A team's request often has no thread to find, so the paging back doesn't wait for one.
                if active.iter().filter(|a| a.2).all(|(k, ..)| self.threads.contains_key(k)) { break; }
                let threads = match self.rest(&format!("/notifications?all=true&participating=true&per_page={PER_PAGE}&page={page}")) {
                    Ok(v) => v.as_array().cloned().unwrap_or_default(),
                    Err(e) => { eprintln!("github: notifications page {page}: {e}"); self.paged = false; break; }
                };
                let full = threads.len() == PER_PAGE;
                for (k, t) in threads.iter().filter_map(thread) { self.threads.entry(k).or_insert(t); }
                if !full { break; }
            }
        }
        let mut reads = 0;
        for (k, newest, _) in &active {
            if reads == THREAD_READS { break; }
            let Some(t) = self.threads.get(k).filter(|t| !self.on_page.contains(k) && t.read.is_none_or(|r| r < *newest)).cloned() else { continue };
            reads += 1;
            match self.rest_if(&format!("/notifications/threads/{}", t.id), t.etag.as_deref()) {
                Ok(None) => {}
                Ok(Some((v, etag))) => if let Some((_, fresh)) = thread(&v) { self.threads.insert(k.clone(), Thread { etag, ..fresh }); },
                Err(e) => eprintln!("github: thread {}: {e}", t.id),
            }
        }
    }

    fn log(&mut self, plan: &Plan) {
        let parts: Vec<&str> = [(plan.own, "yours"), (plan.state, "state"), (plan.reviews, "to review"), (plan.threads, "read times")]
            .iter().filter(|(on, _)| *on).map(|(_, name)| *name).collect();
        eprintln!("github: refreshed {} after {} looks: {} GraphQL points, {} REST calls", parts.join(", "), self.looks, self.points.get(), self.calls.get());
        self.looks = 0;
        self.points.set(0);
        self.calls.set(0);
    }

    /// What the panel shows: the latest of each part, with the state and read times laid over.
    fn pulls(&self, now: Time) -> Pulls {
        let read = |p: &Pull| Pull { read_on_github: self.threads.get(&key(&p.repo, p.number)).and_then(|t| t.read), ..p.clone() };
        let own = self.own.iter().map(|p| {
            let mut p = read(p);
            if let Some(state) = self.states.get(&p.id) {
                p.checks = state.checks;
                // A repository that doesn't require reviews gives no decision.
                if p.review == Review::None && state.approved { p.review = Review::Approved; }
                p.merge = merge(&state.merge, p.status, p.review);
            }
            p
        }).collect();
        let mut trouble = vec![];
        if let Some(f) = &self.failed.reviews { trouble.push(format!("To review couldn't refresh: {}", f.why)); }
        if let Some(f) = &self.failed.state { trouble.push(format!("Checks and merge state couldn't refresh: {}", f.why)); }
        if let Some(e) = &self.look_failed { trouble.push(format!("Notifications couldn't refresh: {e}")); }
        // The recently closed list is a courtesy: one past its ten only loses an old merge.
        let mut limits = vec![];
        if self.own_cut || self.state_cut { limits.push("Showing the 100 most recently updated of your open pull requests".to_string()); }
        if self.reviews_cut { limits.push(format!("Showing the {} most recently updated requests to review", REVIEW_PAGES * 100)); }
        for w in self.warned.own.iter().chain(&self.warned.state).chain(&self.warned.reviews) {
            if !trouble.contains(w) { trouble.push(w.clone()); }
        }
        if let Some(e) = &self.blind {
            trouble.push(format!("GitHub's notifications aren't readable with this token ({e}), so only opening a pull request from the panel marks it read"));
        }
        Pulls {
            login: self.login.clone(), list: own, reviews: self.reviews.iter().map(read).collect(), reviews_loaded: self.reviews_loaded,
            origin: Origin::Live { at: now }, trouble, limits,
        }
    }
}

/// Pull requests whose notification changed since `seen`: with why you get it, and whether it's
/// new. A read on github.com changes only the read time, not this.
fn changes(seen: &HashMap<String, Thread>, page: &[(String, Thread)]) -> Vec<(String, String, bool)> {
    page.iter().filter_map(|(k, t)| {
        let before = seen.get(k);
        before.is_none_or(|s| s.updated < t.updated).then(|| (k.clone(), t.reason.clone(), before.is_none()))
    }).collect()
}

/// What changed notifications call for: yours if one of them changed (or a new one: you're its
/// author), the requests to review if a listed one changed or a new one came. A request you
/// already answered keeps its thread, so only a new thread is a new request; a request made
/// again on an old one waits for `REVIEWS_EVERY`.
fn triggers(changed: &[(String, String, bool)], own: &HashSet<String>, reviews: &HashSet<String>) -> Plan {
    Plan {
        own: changed.iter().any(|(k, reason, _)| own.contains(k) || reason == "author"),
        reviews: changed.iter().any(|(k, reason, new)| reviews.contains(k) || (*new && reason == "review_requested")),
        ..Plan::default()
    }
}

/// Whether yours need refreshing: a pull request of yours opened or closed, or a reaction came on
/// one, or on a comment of yours, seen both times. Your latest comments change as you comment,
/// which isn't news.
fn reacted(before: &Reactions, now: &Reactions) -> bool {
    let rose = |b: &HashMap<String, u64>, n: &HashMap<String, u64>| n.iter().any(|(id, count)| b.get(id).is_some_and(|was| count > was));
    before.open.keys().collect::<HashSet<_>>() != now.open.keys().collect::<HashSet<_>>()
        || rose(&before.open, &now.open) || rose(&before.comments, &now.comments)
}

fn keys(list: &[Pull]) -> HashSet<String> { list.iter().map(|p| key(&p.repo, p.number)).collect() }

fn soon(at: Option<SystemTime>, now: SystemTime) -> bool { at.is_some_and(|t| ago(t, now) < GAP) }

/// Wall-clock time since `t`, which keeps counting while the Mac sleeps, unlike `Instant`. A clock
/// set back makes it long ago, so a part refreshes rather than waits out the jump.
fn ago(t: SystemTime, now: SystemTime) -> Duration { now.duration_since(t).unwrap_or(Duration::MAX) }

/// Why GitHub refused a call, and until when if it was a rate limit. A secondary limit can come
/// as a bare 403 that says so only in its message.
fn refusal(code: u16, resp: ureq::Response) -> (Failure, Option<SystemTime>) {
    if code == 401 { return (Failure { message: "token rejected (401): check it in Settings".into(), refused: true }, None); }
    let retry = retry_at(&resp);
    let headers = resp.header("x-ratelimit-remaining") == Some("0") || resp.header("retry-after").is_some();
    let message = resp.into_json::<Value>().ok().and_then(|v| str_at(&v, "message").map(str::to_string));
    let says = message.as_deref().is_some_and(|m| ["rate limit", "abuse"].iter().any(|s| m.to_lowercase().contains(s)));
    if code == 429 || (code == 403 && (headers || says)) {
        return (Failure { message: limited(retry), refused: false }, Some(retry));
    }
    let message = format!("HTTP {code}{}", message.map(|m| format!(": {m}")).unwrap_or_default());
    (Failure { message, refused: (400..500).contains(&code) }, None)
}

/// When a rate-limited response says to try again: `retry-after`, else when the budget resets,
/// else in a minute.
fn retry_at(resp: &ureq::Response) -> SystemTime {
    SystemTime::now() + Duration::from_secs(wait(resp.header("retry-after"), resp.header("x-ratelimit-remaining"), resp.header("x-ratelimit-reset"), Utc::now().timestamp()))
}

/// When the rate limit in `after` ends, if it hasn't yet. Wall-clock time, because `Instant` stops
/// while the Mac sleeps, so after sleeping past the reset it would still wait out the rest.
fn holding(after: &Cell<Option<SystemTime>>) -> Option<SystemTime> { after.get().filter(|t| SystemTime::now() < *t) }

fn wait(retry_after: Option<&str>, remaining: Option<&str>, reset: Option<&str>, now: i64) -> u64 {
    if let Some(secs) = retry_after.and_then(|s| s.parse().ok()) { return secs; }
    match (remaining, reset.and_then(|s| s.parse::<i64>().ok())) {
        (Some("0"), Some(reset)) => (reset - now).max(0) as u64 + 1,
        _ => 60,
    }
}

fn limited(until: SystemTime) -> String {
    format!("rate limited until {}, as GitHub asks", DateTime::<Local>::from(until).format("%H:%M"))
}

/// A notification thread's pull request and what it says. A thread marked read without a read
/// time counts as read when it last changed.
fn thread(v: &Value) -> Option<(String, Thread)> {
    let subject = v.get("subject")?;
    if str_at(subject, "type")? != "PullRequest" { return None; }
    let (repo, number) = str_at(subject, "url")?.strip_prefix(&format!("{API}/repos/"))?.rsplit_once("/pulls/")?;
    let unread = v.get("unread").and_then(Value::as_bool).unwrap_or(true);
    let updated = time_at(v, "updated_at");
    let read = time_at(v, "last_read_at").or(if unread { None } else { updated });
    Some((key(repo, number.parse().ok()?), Thread {
        id: str_at(v, "id")?.to_string(), read, updated, reason: str_at(v, "reason").unwrap_or_default().to_string(), etag: None,
    }))
}

fn key(repo: &str, number: u64) -> String { format!("{}#{number}", repo.to_lowercase()) }

/// Whose activity counts: not yours, a bot's, or an ignored login's.
struct Who<'a> { me: String, ignore: &'a HashSet<String> }

impl Who<'_> {
    /// (display name, counts) for an author or actor.
    fn of(&self, actor: &Value) -> Option<(String, bool)> {
        let login = str_at(actor, "login")?;
        let lower = login.to_lowercase();
        let bot = str_at(actor, "__typename") == Some("Bot") || ["[bot]", "-bot", "_bot"].iter().any(|s| lower.ends_with(s));
        let counts = lower != self.me && !bot && !self.ignore.contains(&lower);
        let name = str_at(actor, "name").filter(|n| !n.trim().is_empty()).unwrap_or(login).to_string();
        Some((name, counts))
    }

    fn is_me(&self, actor: &Value) -> bool { str_at(actor, "login").is_some_and(|l| l.to_lowercase() == self.me) }

    fn asks(&self, reviewer: &Value) -> Option<Request> {
        match str_at(reviewer, "__typename")? {
            "User" if self.is_me(reviewer) => Some(Request::You),
            "Team" => Some(Request::Team(str_at(reviewer, "name")?.to_string())),
            _ => None,
        }
    }

    /// Others' reactions to something of yours.
    fn reactions(&self, node: &Value, on_comment: bool, excerpt: &str, url: &str, out: &mut Vec<Activity>) {
        for r in node["reactions"]["nodes"].as_array().into_iter().flatten() {
            let (Some((who, true)), Some(at)) = (self.of(&r["user"]), time_at(r, "createdAt")) else { continue };
            let emoji = emoji(str_at(r, "content").unwrap_or_default());
            out.push(Activity { who, at, kind: Kind::Reacted { emoji, on_comment }, excerpt: excerpt.into(), url: url.into() });
        }
    }
}

fn pull(n: &Value, who: &Who) -> Option<Pull> {
    let url = str_at(n, "url")?.to_string();
    let mut activity = vec![];
    who.reactions(n, false, "", &url, &mut activity);
    for item in n["timelineItems"]["nodes"].as_array().into_iter().flatten() {
        let item_url = str_at(item, "url").unwrap_or(&url);
        let text = excerpt(str_at(item, "bodyText").unwrap_or_default());
        let mut push = |actor: &Value, at: Option<Time>, kind: Kind, excerpt: String, url: &str| {
            if let (Some((name, true)), Some(at)) = (who.of(actor), at) {
                activity.push(Activity { who: name, at, kind, excerpt, url: url.into() });
            }
        };
        match str_at(item, "__typename").unwrap_or_default() {
            "IssueComment" if who.is_me(&item["author"]) => who.reactions(item, true, &text, item_url, &mut activity),
            "IssueComment" => push(&item["author"], time_at(item, "createdAt"), Kind::Commented, text, item_url),
            "PullRequestReview" if who.is_me(&item["author"]) => {
                who.reactions(item, true, &text, item_url, &mut activity);
                for c in item["comments"]["nodes"].as_array().into_iter().flatten() {
                    who.reactions(c, true, &excerpt(str_at(c, "bodyText").unwrap_or_default()), str_at(c, "url").unwrap_or(item_url), &mut activity);
                }
            }
            "PullRequestReview" => {
                let first = &item["comments"]["nodes"][0];
                let kind = match str_at(item, "state").unwrap_or_default() {
                    "APPROVED" => Kind::Approved,
                    "CHANGES_REQUESTED" => Kind::ChangesRequested,
                    // A reply to a line comment arrives as a review with no text of its own.
                    "COMMENTED" if text.is_empty() && !first.is_null() => Kind::Commented,
                    "COMMENTED" => Kind::Reviewed,
                    _ => continue,
                };
                let (text, item_url) = if text.is_empty() && !first.is_null() {
                    (excerpt(str_at(first, "bodyText").unwrap_or_default()), str_at(first, "url").unwrap_or(item_url))
                } else { (text, item_url) };
                push(&item["author"], time_at(item, "submittedAt"), kind, text, item_url);
            }
            "MergedEvent" => push(&item["actor"], time_at(item, "createdAt"), Kind::Merged, String::new(), &url),
            "ClosedEvent" => push(&item["actor"], time_at(item, "createdAt"), Kind::Closed, String::new(), &url),
            "ReopenedEvent" => push(&item["actor"], time_at(item, "createdAt"), Kind::Reopened, String::new(), &url),
            _ => {}
        }
    }
    activity.sort_by_key(|a| a.at);
    Some(Pull {
        id: str_at(n, "id")?.to_string(),
        repo: str_at(&n["repository"], "nameWithOwner")?.to_string(),
        number: n.get("number")?.as_u64()?,
        title: str_at(n, "title").unwrap_or_default().to_string(),
        author: String::new(),
        request: None,
        status: status(n),
        checks: checks(n),
        review: match str_at(n, "reviewDecision") {
            Some("APPROVED") => Review::Approved,
            Some("CHANGES_REQUESTED") => Review::ChangesRequested,
            Some("REVIEW_REQUIRED") => Review::Required,
            _ => Review::None,
        },
        merge: Merge::Waiting,
        updated: time_at(n, "updatedAt").unwrap_or_default(),
        url,
        activity,
        read_on_github: None,
    })
}

/// A pull request waiting for your review or a team's, unless a bot opened it. Its activity is
/// the newest request for the review still pending, if someone who counts made it.
fn review(n: &Value, who: &Who) -> Option<Pull> {
    let (author, true) = who.of(&n["author"])? else { return None };
    let url = str_at(n, "url")?.to_string();
    let pending: Vec<Request> = n["reviewRequests"]["nodes"].as_array().into_iter().flatten()
        .filter_map(|r| who.asks(&r["requestedReviewer"])).collect();
    let asked = n["timelineItems"]["nodes"].as_array().into_iter().flatten().rev()
        .filter_map(|e| Some((e, who.asks(&e["requestedReviewer"]).filter(|r| pending.contains(r))?)));
    let (event, request) = if pending.contains(&Request::You) {
        (asked.filter(|(_, r)| *r == Request::You).map(|(e, _)| e).next(), Request::You)
    } else {
        match asked.into_iter().next() {
            Some((e, r)) => (Some(e), r),
            None => (None, pending.first()?.clone()),
        }
    };
    let activity = event.and_then(|e| {
        let (name, true) = who.of(&e["actor"])? else { return None };
        Some(Activity { who: name, at: time_at(e, "createdAt")?, kind: Kind::ReviewRequested(request.clone()), excerpt: String::new(), url: url.clone() })
    });
    Some(Pull {
        id: str_at(n, "id")?.to_string(),
        repo: str_at(&n["repository"], "nameWithOwner")?.to_string(),
        number: n.get("number")?.as_u64()?,
        title: str_at(n, "title").unwrap_or_default().to_string(),
        author,
        request: Some(request),
        status: status(n),
        checks: checks(n),
        review: Review::None,
        merge: Merge::Waiting,
        updated: time_at(n, "updatedAt").unwrap_or_default(),
        url,
        activity: activity.into_iter().collect(),
        read_on_github: None,
    })
}

/// From GitHub's `mergeStateStatus`. UNKNOWN, while GitHub works it out, isn't ready yet.
fn merge(state: &str, status: Status, review: Review) -> Merge {
    match state {
        "DIRTY" => Merge::Conflicts,
        "CLEAN" | "HAS_HOOKS" if status == Status::Open && review == Review::Approved => Merge::Ready,
        _ => Merge::Waiting,
    }
}

fn status(n: &Value) -> Status {
    match (str_at(n, "state"), n.get("isDraft").and_then(Value::as_bool).unwrap_or(false)) {
        (Some("MERGED"), _) => Status::Merged,
        (Some("CLOSED"), _) => Status::Closed,
        (_, true) => Status::Draft,
        _ => Status::Open,
    }
}

fn checks(n: &Value) -> Checks {
    match str_at(&n["commits"]["nodes"][0]["commit"]["statusCheckRollup"], "state") {
        Some("SUCCESS") => Checks::Passing,
        Some("FAILURE" | "ERROR") => Checks::Failing,
        Some("PENDING" | "EXPECTED") => Checks::Pending,
        _ => Checks::None,
    }
}

fn emoji(content: &str) -> &'static str {
    match content {
        "THUMBS_UP" => "👍", "THUMBS_DOWN" => "👎", "LAUGH" => "😄", "HOORAY" => "🎉",
        "CONFUSED" => "😕", "HEART" => "❤️", "ROCKET" => "🚀", "EYES" => "👀", _ => "·",
    }
}

fn excerpt(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(160) {
        Some((i, _)) => format!("{}…", &flat[..i]),
        None => flat,
    }
}

fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> { v.get(key).and_then(Value::as_str) }

fn time_at(v: &Value, key: &str) -> Option<Time> {
    DateTime::parse_from_rfc3339(str_at(v, key)?).ok().map(|t| t.with_timezone(&Local))
}

/// Your own and bots' timeline items take slots before they're dropped, hence 20. Each 5 more
/// cost about 16 points a run.
const QUERY: &str = r#"
query($open: String!, $closed: String!) {
  viewer { login }
  rateLimit { cost }
  open: search(query: $open, type: ISSUE, first: 100) { pageInfo { hasNextPage } nodes { ...pull } }
  closed: search(query: $closed, type: ISSUE, first: 10) { nodes { ...pull } }
}
fragment actor on Actor { login __typename ... on User { name } }
fragment reactions on Reactable {
  reactions(first: 5, orderBy: {field: CREATED_AT, direction: DESC}) { nodes { content createdAt user { login name } } }
}
fragment pull on PullRequest {
  id number title url isDraft state reviewDecision updatedAt
  repository { nameWithOwner }
  commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
  ...reactions
  timelineItems(last: 20, itemTypes: [ISSUE_COMMENT, PULL_REQUEST_REVIEW, MERGED_EVENT, CLOSED_EVENT, REOPENED_EVENT]) {
    nodes {
      __typename
      ... on IssueComment { url createdAt bodyText author { ...actor } ...reactions }
      ... on PullRequestReview {
        url state submittedAt bodyText author { ...actor } ...reactions
        comments(last: 1) { nodes { url bodyText ...reactions } }
      }
      ... on MergedEvent { createdAt actor { ...actor } }
      ... on ClosedEvent { createdAt actor { ...actor } }
      ... on ReopenedEvent { createdAt actor { ...actor } }
    }
  }
}
"#;

const STATE: &str = r#"
query($q: String!) {
  rateLimit { cost }
  viewer { issueComments(last: 20) { nodes { id reactions { totalCount } } } }
  search(query: $q, type: ISSUE, first: 100) {
    pageInfo { hasNextPage }
    nodes {
      ... on PullRequest {
        id mergeStateStatus reactions { totalCount }
        approvals: reviews(states: APPROVED) { totalCount }
        commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
      }
    }
  }
}
"#;

const REVIEWS: &str = r#"
query($q: String!, $after: String) {
  rateLimit { cost }
  search(query: $q, type: ISSUE, first: 100, after: $after) {
    pageInfo { hasNextPage endCursor }
    nodes {
      ... on PullRequest {
        id number title url isDraft state updatedAt
        repository { nameWithOwner }
        author { ...actor }
        commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
        reviewRequests(first: 10) { nodes { requestedReviewer { ...reviewer } } }
        timelineItems(last: 10, itemTypes: [REVIEW_REQUESTED_EVENT]) {
          nodes { ... on ReviewRequestedEvent { createdAt actor { ...actor } requestedReviewer { ...reviewer } } }
        }
      }
    }
  }
}
fragment actor on Actor { login __typename ... on User { name } }
fragment reviewer on RequestedReviewer { __typename ... on User { login } ... on Team { name } }
"#;

impl Client {
    pub fn fetcher(mut self) -> impl FnMut() -> Result<Pulls, String> + Send + 'static {
        let mut warned = false;
        move || {
            let result = self.tick(Local::now());
            if let (Ok(p), false) = (&result, warned) {
                for w in &p.trouble { eprintln!("github: {w}"); }
                warned = true;
            }
            result
        }
    }
}

impl Fetched for Pulls { fn origin(&mut self) -> &mut Origin { &mut self.origin } }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn others_count_and_you_bots_and_ignored_logins_dont() {
        let ignore = HashSet::from(["scanner-account".to_string()]);
        let who = Who { me: "op".into(), ignore: &ignore };
        let counts = |v: Value| who.of(&v).map(|(_, c)| c);
        assert_eq!(counts(json!({"login": "xv", "__typename": "User", "name": "X V"})), Some(true));
        assert_eq!(counts(json!({"login": "OP", "__typename": "User"})), Some(false));
        assert_eq!(counts(json!({"login": "lint-checker", "__typename": "Bot"})), Some(false));
        assert_eq!(counts(json!({"login": "acme-security-bot", "__typename": "User"})), Some(false), "a machine user named like a bot");
        assert_eq!(counts(json!({"login": "Scanner-Account", "__typename": "User"})), Some(false));
        assert_eq!(who.of(&json!({"login": "xv", "name": "X V"})).unwrap().0, "X V");
    }

    #[test]
    fn activity_comes_from_comments_reviews_and_reactions_to_yours() {
        let ignore = HashSet::new();
        let who = Who { me: "op".into(), ignore: &ignore };
        let user = |l: &str| json!({"login": l, "__typename": "User"});
        let reaction = |l: &str, t: &str| json!({"content": "THUMBS_UP", "createdAt": t, "user": {"login": l}});
        let n = json!({
            "id": "PR_1", "number": 7, "title": "T", "url": "https://github.com/o/r/pull/7", "isDraft": false, "state": "OPEN",
            "reviewDecision": "APPROVED", "updatedAt": "2026-11-10T12:00:00Z", "repository": {"nameWithOwner": "o/r"},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "FAILURE"}}}]},
            "reactions": {"nodes": [reaction("xv", "2026-11-10T10:00:00Z")]},
            "timelineItems": {"nodes": [
                {"__typename": "IssueComment", "url": "u1", "createdAt": "2026-11-10T09:00:00Z", "bodyText": "mine", "author": user("op"),
                 "reactions": {"nodes": [reaction("jy", "2026-11-10T11:00:00Z"), reaction("op", "2026-11-10T11:05:00Z")]}},
                {"__typename": "IssueComment", "url": "u2", "createdAt": "2026-11-10T09:30:00Z", "bodyText": "scan  done", "author": {"login": "ci", "__typename": "Bot"}},
                {"__typename": "PullRequestReview", "url": "u3", "state": "COMMENTED", "submittedAt": "2026-11-10T09:45:00Z", "bodyText": "",
                 "author": user("zq"), "comments": {"nodes": [{"url": "u4", "bodyText": "nit:\n  rename"}]}},
                {"__typename": "PullRequestReview", "url": "u5", "state": "APPROVED", "submittedAt": "2026-11-10T09:50:00Z", "bodyText": "", "author": user("xv"), "comments": {"nodes": []}},
            ]},
        });
        let p = pull(&n, &who).unwrap();
        let got: Vec<_> = p.activity.iter().map(|a| (a.headline(), a.excerpt.as_str(), a.url.as_str())).collect();
        assert_eq!(got, [
            ("zq commented".to_string(), "nit: rename", "u4"),
            ("xv approved".to_string(), "", "u5"),
            ("xv reacted 👍".to_string(), "", "https://github.com/o/r/pull/7"),
            ("jy reacted 👍 to your comment".to_string(), "mine", "u1"),
        ]);
        assert_eq!((p.checks, p.review, p.status), (Checks::Failing, Review::Approved, Status::Open));
    }

    #[test]
    fn ready_to_merge_is_open_approved_and_clean() {
        assert_eq!(merge("CLEAN", Status::Open, Review::Approved), Merge::Ready);
        assert_eq!(merge("HAS_HOOKS", Status::Open, Review::Approved), Merge::Ready);
        assert_eq!(merge("CLEAN", Status::Open, Review::None), Merge::Waiting, "a repository that requires no review");
        assert_eq!(merge("CLEAN", Status::Draft, Review::Approved), Merge::Waiting);
        assert_eq!(merge("BLOCKED", Status::Open, Review::Approved), Merge::Waiting);
        assert_eq!(merge("UNKNOWN", Status::Open, Review::Approved), Merge::Waiting);
        assert_eq!(merge("DIRTY", Status::Open, Review::Approved), Merge::Conflicts);
    }

    #[test]
    fn a_review_request_is_for_you_before_your_team() {
        let ignore = HashSet::new();
        let who = Who { me: "op".into(), ignore: &ignore };
        let user = |l: &str| json!({"login": l, "__typename": "User"});
        let team = |n: &str| json!({"__typename": "Team", "name": n});
        let asked = |by: &str, t: &str, of: Value| json!({"createdAt": t, "actor": user(by), "requestedReviewer": of});
        let node = |pending: Vec<Value>, events: Vec<Value>, author: Value| json!({
            "id": "PR_2", "number": 9, "title": "T", "url": "https://github.com/o/r/pull/9", "isDraft": false, "state": "OPEN",
            "updatedAt": "2026-11-10T12:00:00Z", "repository": {"nameWithOwner": "o/r"}, "author": author,
            "reviewRequests": {"nodes": pending.into_iter().map(|r| json!({"requestedReviewer": r})).collect::<Vec<_>>()},
            "timelineItems": {"nodes": events},
        });
        let events = || vec![asked("xv", "2026-11-10T09:00:00Z", user("op")), asked("xv", "2026-11-10T10:00:00Z", team("platform"))];

        let p = review(&node(vec![user("op"), team("platform")], events(), user("xv")), &who).unwrap();
        assert_eq!(p.request, Some(Request::You));
        assert_eq!(p.activity.iter().map(Activity::headline).collect::<Vec<_>>(), ["xv requested your review"], "the request to you, though the team's is newer");

        let p = review(&node(vec![team("platform")], events(), user("xv")), &who).unwrap();
        assert_eq!((p.request, p.activity[0].headline()), (Some(Request::Team("platform".into())), "xv requested review from platform".to_string()));

        let p = review(&node(vec![team("platform")], vec![], user("xv")), &who).unwrap();
        assert!(p.activity.is_empty(), "requested too long ago to find: listed, never unread");
        assert!(review(&node(vec![team("platform")], events(), json!({"login": "deps", "__typename": "Bot"})), &who).is_none(), "a bot's");
    }

    #[test]
    fn a_thread_gives_its_pull_request_and_read_time() {
        let json = |unread: bool, read: Value| json!({"id": "9", "unread": unread, "last_read_at": read, "updated_at": "2026-11-10T12:00:00Z",
            "reason": "author", "subject": {"type": "PullRequest", "url": "https://api.github.com/repos/O/R/pulls/7"}});
        let (k, t) = thread(&json(true, json!("2026-11-10T10:00:00Z"))).unwrap();
        assert_eq!((k.as_str(), t.id.as_str(), t.reason.as_str()), ("o/r#7", "9", "author"));
        assert_eq!(t.read, time_at(&json!({"t": "2026-11-10T10:00:00Z"}), "t"));
        assert_eq!(thread(&json(true, Value::Null)).unwrap().1.read, None, "never read");
        assert_eq!(thread(&json(false, Value::Null)).unwrap().1.read, time_at(&json!({"t": "2026-11-10T12:00:00Z"}), "t"));
    }

    #[test]
    fn only_a_change_to_what_is_listed_refreshes_it() {
        let at = |h: u32| Some(crate::time::at(2026, 11, 10, h, 0));
        let t = |updated: u32, read: Option<u32>, reason: &str| Thread { updated: at(updated), read: read.and_then(at), reason: reason.into(), ..Default::default() };
        let seen = HashMap::from([
            ("o/r#1".to_string(), t(9, None, "author")),
            ("o/r#2".to_string(), t(9, None, "review_requested")),
            ("o/r#6".to_string(), t(9, None, "review_requested")),
        ]);
        let own = HashSet::from(["o/r#1".to_string()]);
        let reviews = HashSet::from(["o/r#2".to_string()]);
        let next = |page: Vec<(&str, Thread)>| {
            let page: Vec<_> = page.into_iter().map(|(k, t)| (k.to_string(), t)).collect();
            triggers(&changes(&seen, &page), &own, &reviews)
        };
        let none = Plan::default();
        assert_eq!(next(vec![("o/r#1", t(9, Some(10), "author"))]), none, "read on github.com: read times only");
        assert_eq!(next(vec![("o/r#1", t(11, None, "author"))]), Plan { own: true, ..none });
        assert_eq!(next(vec![("o/r#3", t(11, None, "author"))]), Plan { own: true, ..none }, "a new pull request of yours");
        assert_eq!(next(vec![("o/r#2", t(11, None, "review_requested"))]), Plan { reviews: true, ..none }, "a listed request");
        assert_eq!(next(vec![("o/r#4", t(11, None, "review_requested"))]), Plan { reviews: true, ..none }, "a new request");
        assert_eq!(next(vec![("o/r#6", t(11, None, "review_requested"))]), none, "a request answered before: no search");
        assert_eq!(next(vec![("o/r#5", t(11, None, "mention"))]), none, "someone else's pull request");
    }

    #[test]
    fn a_new_reaction_or_pull_request_refreshes_yours_and_your_comments_dont() {
        let counts = |pairs: &[(&str, u64)]| pairs.iter().map(|(id, n)| (id.to_string(), *n)).collect::<HashMap<_, _>>();
        let r = |open: &[(&str, u64)], comments: &[(&str, u64)]| Reactions { open: counts(open), comments: counts(comments) };
        let before = r(&[("p1", 0)], &[("c1", 0), ("c2", 1)]);
        assert!(!reacted(&before, &before));
        assert!(reacted(&before, &r(&[("p1", 1)], &[("c1", 0), ("c2", 1)])), "a reaction on a pull request");
        assert!(reacted(&before, &r(&[("p1", 0)], &[("c1", 1), ("c2", 1)])), "a reaction on a comment");
        assert!(reacted(&before, &r(&[("p1", 0), ("p2", 0)], &[("c1", 0), ("c2", 1)])), "a pull request opened");
        assert!(!reacted(&before, &r(&[("p1", 0)], &[("c2", 1), ("c3", 0)])), "you commented: the oldest dropped off");
        assert!(!reacted(&before, &r(&[("p1", 0)], &[("c1", 0), ("c2", 0)])), "a reaction taken back");
    }

    #[test]
    fn rate_limited_waits_as_github_says() {
        assert_eq!(wait(Some("30"), Some("0"), Some("1000"), 900), 30, "retry-after first");
        assert_eq!(wait(None, Some("0"), Some("1000"), 900), 101, "then the budget's reset");
        assert_eq!(wait(None, Some("12"), Some("1000"), 900), 60, "a secondary limit without either");
    }

    fn client() -> Client { Client::new("t".into(), HashSet::new(), crate::net::Http::new("api.github.com")) }

    fn response(head: &str, body: &str) -> ureq::Response { format!("{head}\r\n\r\n{body}").parse().unwrap() }

    fn noon() -> Time { crate::time::at(2026, 11, 10, 12, 0) }

    #[test]
    fn a_failed_part_retries_after_a_minute_then_twice_as_long_up_to_its_interval() {
        let now = SystemTime::now();
        let (mut last, mut minutes) = (None, vec![]);
        for _ in 0..5 {
            let f = Failed::again(last, "HTTP 502".into(), STATE_EVERY, now);
            minutes.push(f.retry.duration_since(now).unwrap().as_secs() / 60);
            last = Some(f);
        }
        assert_eq!(minutes, [1, 2, 4, 5, 5]);
    }

    #[test]
    fn a_part_waiting_to_retry_is_left_out_and_owed() {
        let mut c = client();
        let now = SystemTime::now();
        let failed = || Some(Failed { why: "HTTP 502".into(), times: 1, retry: now + RETRY });
        c.failed = Failures { state: failed(), reviews: failed() };
        let plan = c.plan(Plan { reviews: true, ..Plan::default() }, now);
        assert_eq!(plan, Plan { own: true, threads: true, ..Plan::default() }, "state and reviews are due but wait");
        assert!(c.owed.reviews, "called for, so owed");
        let plan = c.plan(c.owed, now + RETRY);
        assert!(plan.state && plan.reviews);
    }

    #[test]
    fn a_part_that_fails_keeps_its_last_data_and_says_why() {
        let mut c = client();
        assert!(!c.pulls(noon()).reviews_loaded);
        let who = Who { me: "op".into(), ignore: &HashSet::new() };
        c.reviews = vec![pull(&json!({"id": "PR_1", "number": 7, "url": "u", "repository": {"nameWithOwner": "o/r"}}), &who).unwrap()];
        c.reviews_loaded = true;
        c.graphql_after.set(Some(SystemTime::now() + Duration::from_secs(600)));
        c.refresh_reviews();
        assert!(!c.refresh_state());
        let p = c.pulls(noon());
        assert_eq!((p.reviews.len(), p.reviews_loaded), (1, true), "the last good list stays");
        assert!(p.trouble[0].starts_with("To review couldn't refresh: rate limited until "), "{:?}", p.trouble);
        assert!(p.trouble[1].starts_with("Checks and merge state couldn't refresh: rate limited until "), "{:?}", p.trouble);
        c.refresh_reviews();
        assert_eq!(c.failed.reviews.map(|f| f.times), Some(2));
    }

    #[test]
    fn a_list_cut_short_says_so_once() {
        let mut c = client();
        let page = |more: bool| json!({"pageInfo": {"hasNextPage": more}, "nodes": []});
        let own = |more: bool| json!({"viewer": {"login": "op"}, "open": page(more), "closed": page(false)});
        let cut = |c: &Client| c.pulls(noon()).limits.iter().filter(|t| t.starts_with("Showing the 100 most recently updated")).count();
        c.take_own(&own(true), vec![]);
        c.take_state(&json!({"search": page(true)}));
        assert_eq!(cut(&c), 1);
        c.take_own(&own(false), vec![]);
        assert_eq!(cut(&c), 1, "the state query still finds more");
        c.take_state(&json!({"search": page(false)}));
        assert_eq!(cut(&c), 0);

        c.take_own(&json!({"viewer": {"login": "op"}, "open": page(false), "closed": page(true)}), vec![]);
        c.reviews_cut = true;
        assert_eq!(c.pulls(noon()).limits, ["Showing the 300 most recently updated requests to review"]);
        assert!(c.pulls(noon()).trouble.is_empty(), "a limit isn't trouble");
    }

    #[test]
    fn a_part_is_due_once_its_wall_clock_interval_passed() {
        let mut c = client();
        let now = SystemTime::now();
        let ago = |secs: u64| Some(now - Duration::from_secs(secs));
        c.at = Ages { own: ago(0), state: ago(STATE_EVERY.as_secs() - 1), reviews: ago(8 * 3600), threads: Some(now + Duration::from_secs(3600)) };
        assert_eq!(c.due(now), Plan { reviews: true, threads: true, ..Plan::default() }, "after a night's sleep, and with the clock set back");
        assert!(c.due(now + Duration::from_secs(1)).state);
        c.at = Ages { own: ago(8 * 3600), state: ago(8 * 3600), reviews: ago(8 * 3600), threads: ago(8 * 3600) };
        assert_eq!(c.due(now), Plan { own: true, state: true, reviews: true, threads: true });
    }

    #[test]
    fn failing_notifications_refresh_yours_and_the_requests_to_review_sooner() {
        let mut c = client();
        let now = SystemTime::now();
        let then = Some(now - BLIND_EVERY);
        c.at = Ages { own: then, state: then, reviews: then, threads: then };
        assert_eq!(c.due(now), Plan::default());
        c.look_failed = Some("network: timed out".into());
        assert_eq!(c.due(now), Plan { own: true, reviews: true, ..Plan::default() });
    }

    #[test]
    fn paging_back_that_fails_is_tried_again() {
        let mut c = client();
        let who = Who { me: "op".into(), ignore: &HashSet::new() };
        let comment = json!({"__typename": "IssueComment", "url": "u", "createdAt": "2026-11-10T09:00:00Z", "bodyText": "hi", "author": {"login": "xv", "__typename": "User"}});
        c.own = vec![pull(&json!({"id": "PR_1", "number": 7, "url": "u", "repository": {"nameWithOwner": "o/r"}, "timelineItems": {"nodes": [comment]}}), &who).unwrap()];
        c.rest_after.set(Some(SystemTime::now() + Duration::from_secs(60)));
        c.read_threads();
        assert!(!c.paged);
    }

    #[test]
    fn sso_warnings_from_every_part_show_once() {
        let mut c = client();
        let sso = "Some organizations hide their pull requests until you authorize the token for SSO".to_string();
        c.warned = Warned { own: vec![], state: vec![sso.clone()], reviews: vec![sso.clone()] };
        assert_eq!(c.pulls(noon()).trouble, [sso]);
    }

    #[test]
    fn only_github_refusing_the_notifications_makes_it_blind() {
        let mut c = client();
        c.rest_after.set(Some(SystemTime::now() + Duration::from_secs(60)));
        let result = c.look();
        assert!(c.looked(result).is_none());
        assert_eq!(c.blind, None, "a rate limit says nothing about the token");
        assert!(c.pulls(noon()).trouble[0].starts_with("Notifications couldn't refresh: rate limited until "));
        c.looked(Err(Failure { message: "HTTP 403: Resource not accessible by personal access token".into(), refused: true }));
        let p = c.pulls(noon());
        assert!(p.trouble[0].starts_with("GitHub's notifications aren't readable with this token (HTTP 403"), "{:?}", p.trouble);
        assert_eq!(p.trouble.len(), 1);
        c.looked(Ok(None));
        assert!(c.pulls(noon()).trouble.is_empty());
    }

    #[test]
    fn the_etag_is_kept_only_once_its_page_is_read() {
        let mut c = client();
        let page = |body: &str| response("HTTP/1.1 200 OK\r\nETag: \"v2\"", body);
        assert!(c.take_look(page(r#"[{"id": "#)).is_err());
        assert_eq!(c.etag, None, "kept, the next look would get a 304 and miss what this page said");
        assert_eq!(c.take_look(page("[]")), Ok(None), "the first page is what's seen, not news");
        assert_eq!(c.etag.as_deref(), Some("\"v2\""));
    }

    #[test]
    fn a_bare_403_that_mentions_a_rate_limit_is_one() {
        let refused = |code: u16, message: &str| refusal(code, response(&format!("HTTP/1.1 {code} X"), &json!({"message": message}).to_string()));
        let in_a_minute = |until: Option<SystemTime>| until.and_then(|t| t.duration_since(SystemTime::now()).ok()).is_some_and(|d| d.as_secs().abs_diff(60) <= 2);
        for m in ["You have exceeded a secondary rate limit. Please wait a few minutes.", "You have triggered an abuse detection mechanism."] {
            let (f, until) = refused(403, m);
            assert!(in_a_minute(until) && !f.refused, "{m}");
        }
        let (f, until) = refused(403, "Resource not accessible by personal access token");
        assert_eq!((f.message.as_str(), f.refused, until), ("HTTP 403: Resource not accessible by personal access token", true, None));
        let (f, until) = refused(502, "Server Error");
        assert!(!f.refused && until.is_none(), "GitHub having a bad moment");
    }

    #[test]
    fn a_rate_limit_lasts_until_the_wall_clock_reset() {
        let reset = Utc::now().timestamp() + 100;
        let (_, until) = refusal(403, response(&format!("HTTP/1.1 403 X\r\nx-ratelimit-remaining: 0\r\nx-ratelimit-reset: {reset}"), "{}"));
        let at = until.unwrap().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() as i64;
        assert!((at - (reset + 1)).abs() <= 1, "{at} vs {reset}");
        assert_eq!(holding(&Cell::new(until)), until);
        assert_eq!(holding(&Cell::new(Some(SystemTime::now() - Duration::from_secs(1)))), None, "a reset that passed while the Mac slept");
    }
}
