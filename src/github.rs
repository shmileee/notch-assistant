//! GitHub client: your pull requests, what others did on them, and when you last read each.
//! Read-only.
//!
//! - `POST /graphql`: your open pull requests and those closed in the last three days, each
//!   with its last 20 comments, reviews, merges and closes and the reactions to them.
//! - `GET /notifications?all=true&participating=true`: the latest notification threads, whose
//!   `last_read_at` says when you last read a pull request on github.com. Reading one doesn't
//!   move it up that list, so a thread found once is then read directly
//!   (`GET /notifications/threads/{id}`), and the first fetch pages back to find the threads of
//!   pull requests with activity.
//!
//! Settings (`INTEGRATION`): a token (Keychain) and logins whose activity to ignore. Bots and
//! your own activity are always ignored.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Local, NaiveDateTime};
use serde_json::{Value, json};

use crate::integrations::{Field, FieldKind, Integration};
use crate::feed::Fetched;
use crate::oncall::Origin;
use crate::pulls::{self, Activity, Checks, Kind, Pull, Pulls, Review, Status, unread_count};
use crate::read::ReadState;

const API: &str = "https://api.github.com";

pub const INTEGRATION: Integration = Integration {
    id: "github",
    name: "GitHub",
    summary: "Your pull requests, and what others did on them since you last looked.",
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

/// How far the first fetch pages back for threads. GitHub gives at most 50 a page.
const PER_PAGE: usize = 50;
const FIRST_PAGES: usize = 10;

/// How often to refresh. A refresh costs about 40 GraphQL points, so this uses about 800 of the
/// token's 5,000 an hour, which it shares with anything else using the token (gh, say).
pub const EVERY: Duration = Duration::from_secs(180);

fn test() -> Result<String, String> {
    if crate::demo::on() {
        std::thread::sleep(Duration::from_millis(1200));
        return Ok("Connected as OP: 4 open pull requests, 3 with something unread".into());
    }
    let mut client = Client::from_settings().ok_or("No token saved")?;
    let now = Local::now().naive_local();
    let pulls = client.fetch(now)?;
    let open = pulls.list.iter().filter(|p| matches!(p.status, Status::Open | Status::Draft)).count();
    let unread = unread_count(&pulls.list, &ReadState::load(pulls::READ_FILE, now));
    let mut message = format!("Connected as {}: {open} open pull requests, {unread} with something unread", pulls.login);
    for w in &pulls.warnings { message.push_str(". "); message.push_str(w); }
    Ok(message)
}

pub struct Client {
    token: String,
    ignore: HashSet<String>,
    agent: ureq::Agent,
    /// Notification threads by pull request ("owner/repo#1", lower case), as found so far: the
    /// thread's ID and when you last read it.
    threads: HashMap<String, (String, Option<NaiveDateTime>)>,
    /// The first fetch's paging back is done.
    paged: bool,
}

impl Client {
    /// A client from the saved settings; `None` without a token.
    pub fn from_settings() -> Option<Self> {
        let token = INTEGRATION.value("token")?;
        let ignore = INTEGRATION.value("ignore").unwrap_or_default()
            .split(|c: char| c == ',' || c.is_whitespace()).filter(|s| !s.is_empty()).map(str::to_lowercase).collect();
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(20)).user_agent("notch-assistant").build();
        Some(Client { token, ignore, agent, threads: HashMap::new(), paged: false })
    }

    fn call(&self, req: ureq::Request, body: Option<Value>) -> Result<ureq::Response, String> {
        let req = req.set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", "application/vnd.github+json").set("X-GitHub-Api-Version", "2022-11-28");
        let result = match body { Some(b) => req.send_json(b), None => req.call() };
        match result {
            Ok(resp) => Ok(resp),
            Err(ureq::Error::Status(401, _)) => Err("token rejected (401): check it in Settings".into()),
            Err(ureq::Error::Status(code @ (403 | 429), resp)) if resp.header("x-ratelimit-remaining") == Some("0") || code == 429 =>
                Err("rate limited: GitHub allows 5,000 calls an hour per token".into()),
            Err(ureq::Error::Status(code, resp)) => {
                let message = resp.into_json::<Value>().ok().and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_string));
                Err(format!("HTTP {code}{}", message.map(|m| format!(": {m}")).unwrap_or_default()))
            }
            Err(e) => Err(format!("network: {e}")),
        }
    }

    fn rest(&self, path: &str) -> Result<Value, String> {
        self.call(self.agent.get(&format!("{API}{path}")), None)?.into_json().map_err(|e| format!("{path}: bad JSON ({e})"))
    }

    pub fn fetch(&mut self, now: NaiveDateTime) -> Result<Pulls, String> {
        let mut warnings = vec![];
        let since = (now - chrono::Duration::days(3)).format("%Y-%m-%d");
        let variables = json!({
            "open": "is:pr is:open author:@me archived:false sort:updated-desc",
            "closed": format!("is:pr is:closed author:@me archived:false updated:>={since} sort:updated-desc"),
        });
        let resp = self.call(self.agent.post(&format!("{API}/graphql")), Some(json!({ "query": QUERY, "variables": variables })))?;
        if resp.header("x-github-sso").is_some_and(|h| h.contains("partial-results")) {
            warnings.push("Some organizations hide their pull requests until you authorize the token for SSO".to_string());
        }
        let body: Value = resp.into_json().map_err(|e| format!("graphql: bad JSON ({e})"))?;
        let errors: Vec<&str> = body.get("errors").and_then(Value::as_array).into_iter().flatten()
            .filter_map(|e| e.get("message").and_then(Value::as_str)).collect();
        let Some(data) = body.get("data").filter(|d| !d.is_null()) else {
            return Err(format!("graphql: {}", errors.first().unwrap_or(&"no data")));
        };
        for e in &errors { eprintln!("github: {e}"); }

        let me = str_at(&data["viewer"], "login").unwrap_or_default().to_string();
        let who = Who { me: me.to_lowercase(), ignore: &self.ignore };
        let mut list: Vec<Pull> = ["open", "closed"].iter()
            .flat_map(|k| data[*k]["nodes"].as_array().cloned().unwrap_or_default())
            .filter_map(|n| pull(&n, &who))
            .collect();

        match self.read_times(&list) {
            Ok(times) => for p in &mut list { p.read_on_github = times.get(&key(&p.repo, p.number)).copied().flatten(); },
            Err(e) => {
                eprintln!("github: notifications: {e}");
                warnings.push(format!("GitHub's notifications aren't readable with this token ({e}), so only opening a pull request from the panel marks it read"));
            }
        }
        Ok(Pulls { login: me, list, origin: Origin::Live { at: now }, warnings })
    }

    /// When you last read each pull request on GitHub, by `key`: from the latest notifications,
    /// and for a pull request with activity whose thread has dropped off them, from the thread,
    /// unless you'd already read it past its newest activity.
    fn read_times(&mut self, pulls: &[Pull]) -> Result<HashMap<String, Option<NaiveDateTime>>, String> {
        let active: Vec<(String, NaiveDateTime)> = pulls.iter()
            .filter_map(|p| Some((key(&p.repo, p.number), p.activity.last()?.at))).collect();
        let pages = if self.paged { 1 } else { FIRST_PAGES };
        let mut times = HashMap::new();
        for page in 1..=pages {
            let threads = self.rest(&format!("/notifications?all=true&participating=true&per_page={PER_PAGE}&page={page}"))?;
            let threads = threads.as_array().cloned().unwrap_or_default();
            for (k, id, t) in threads.iter().filter_map(read_time) {
                self.threads.insert(k.clone(), (id, t));
                times.insert(k, t);
            }
            if threads.len() < PER_PAGE || active.iter().all(|(k, _)| self.threads.contains_key(k)) { break; }
        }
        self.paged = true;
        for (k, newest) in &active {
            if times.contains_key(k) { continue; }
            let Some((id, known)) = self.threads.get(k).cloned() else { continue };
            if known.is_some_and(|t| t >= *newest) {
                times.insert(k.clone(), known);
                continue;
            }
            match self.rest(&format!("/notifications/threads/{id}")) {
                Ok(thread) => if let Some((_, _, t)) = read_time(&thread) {
                    self.threads.insert(k.clone(), (id, t));
                    times.insert(k.clone(), t);
                },
                Err(e) => eprintln!("github: thread {id}: {e}"),
            }
        }
        Ok(times)
    }
}

/// A notification thread's pull request, its ID, and when you last read it. A thread marked
/// read without a read time counts as read when it last changed.
fn read_time(thread: &Value) -> Option<(String, String, Option<NaiveDateTime>)> {
    let subject = thread.get("subject")?;
    if str_at(subject, "type")? != "PullRequest" { return None; }
    let (repo, number) = str_at(subject, "url")?.strip_prefix(&format!("{API}/repos/"))?.rsplit_once("/pulls/")?;
    let unread = thread.get("unread").and_then(Value::as_bool).unwrap_or(true);
    let t = time_at(thread, "last_read_at").or_else(|| if unread { None } else { time_at(thread, "updated_at") });
    Some((key(repo, number.parse().ok()?), str_at(thread, "id")?.to_string(), t))
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
        let mut push = |actor: &Value, at: Option<NaiveDateTime>, kind: Kind, excerpt: String, url: &str| {
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
    let draft = n.get("isDraft").and_then(Value::as_bool).unwrap_or(false);
    Some(Pull {
        id: str_at(n, "id")?.to_string(),
        repo: str_at(&n["repository"], "nameWithOwner")?.to_string(),
        number: n.get("number")?.as_u64()?,
        title: str_at(n, "title").unwrap_or_default().to_string(),
        status: match (str_at(n, "state"), draft) {
            (Some("MERGED"), _) => Status::Merged,
            (Some("CLOSED"), _) => Status::Closed,
            (_, true) => Status::Draft,
            _ => Status::Open,
        },
        checks: match str_at(&n["commits"]["nodes"][0]["commit"]["statusCheckRollup"], "state") {
            Some("SUCCESS") => Checks::Passing,
            Some("FAILURE" | "ERROR") => Checks::Failing,
            Some("PENDING" | "EXPECTED") => Checks::Pending,
            _ => Checks::None,
        },
        review: match str_at(n, "reviewDecision") {
            Some("APPROVED") => Review::Approved,
            Some("CHANGES_REQUESTED") => Review::ChangesRequested,
            Some("REVIEW_REQUIRED") => Review::Required,
            _ => Review::None,
        },
        updated: time_at(n, "updatedAt").unwrap_or_default(),
        url,
        activity,
        read_on_github: None,
    })
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

fn time_at(v: &Value, key: &str) -> Option<NaiveDateTime> {
    DateTime::parse_from_rfc3339(str_at(v, key)?).ok().map(|t| t.with_timezone(&Local).naive_local())
}

const QUERY: &str = r#"
query($open: String!, $closed: String!) {
  viewer { login }
  open: search(query: $open, type: ISSUE, first: 30) { nodes { ...pull } }
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
        comments(last: 3) { nodes { url bodyText ...reactions } }
      }
      ... on MergedEvent { createdAt actor { ...actor } }
      ... on ClosedEvent { createdAt actor { ...actor } }
      ... on ReopenedEvent { createdAt actor { ...actor } }
    }
  }
}
"#;

impl Client {
    pub fn fetcher(mut self) -> impl FnMut() -> Result<Pulls, String> + Send + 'static {
        let mut warned = false;
        move || {
            let result = self.fetch(Local::now().naive_local());
            if let (Ok(p), false) = (&result, warned) {
                for w in &p.warnings { eprintln!("github: {w}"); }
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
    fn a_thread_gives_its_pull_request_and_read_time() {
        let thread = |unread: bool, read: Value| json!({"id": "9", "unread": unread, "last_read_at": read, "updated_at": "2026-11-10T12:00:00Z",
            "subject": {"type": "PullRequest", "url": "https://api.github.com/repos/O/R/pulls/7"}});
        let (k, id, t) = read_time(&thread(true, json!("2026-11-10T10:00:00Z"))).unwrap();
        assert_eq!((k.as_str(), id.as_str()), ("o/r#7", "9"));
        assert_eq!(t, time_at(&json!({"t": "2026-11-10T10:00:00Z"}), "t"));
        assert_eq!(read_time(&thread(true, Value::Null)).unwrap().2, None, "never read");
        assert_eq!(read_time(&thread(false, Value::Null)).unwrap().2, time_at(&json!({"t": "2026-11-10T12:00:00Z"}), "t"));
    }
}
