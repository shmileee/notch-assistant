//! Linear client: your open issues and your notifications. Read-only: one GraphQL query.
//!
//! - `viewer.assignedIssues`: your issues in a started or unstarted state.
//! - `notifications`: your latest 100, each with its issue or project. Only issue and project
//!   notifications are shown (a document's carries no title or link), and not a bot's (a status
//!   set by the GitHub integration, a sync error): reminders with no one behind them, like an
//!   SLA breach, still count.
//!
//! Settings (`INTEGRATION`): a personal API key (Keychain). It goes in the `Authorization`
//! header as it is: Linear refuses a key with a `Bearer` prefix.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{Local, NaiveDateTime};
use serde_json::{Value, json};

use crate::feed::Fetched;
use crate::integrations::{Field, FieldKind, Integration};
use crate::issues::{self, Issues, Notice, Ticket, unread_count};
use crate::oncall::Origin;
use crate::read::{ReadState, parse};

const API: &str = "https://api.linear.app/graphql";

pub const INTEGRATION: Integration = Integration {
    id: "linear",
    name: "Linear",
    summary: "Your open issues, and what Linear notified you about since you last looked.",
    fields: &[
        Field { key: "api_key", label: "API key", kind: FieldKind::Secret, env: "NOTCH_LINEAR_API_KEY",
            help: "Settings › Account › Security & access › Personal API keys in Linear. Read access is enough. Stored in your Keychain.",
            placeholder: || "Paste the key".into() },
    ],
    required: "api_key",
    test,
    legacy: &[],
};

pub const EVERY: Duration = Duration::from_secs(180);

fn test() -> Result<String, String> {
    if crate::demo::on() {
        std::thread::sleep(Duration::from_millis(1200));
        return Ok("Connected as OP: 3 open issues assigned to you, 2 with something unread".into());
    }
    let client = Client::from_settings().ok_or("No API key saved")?;
    let now = Local::now().naive_local();
    let issues = client.fetch(now)?;
    let open = issues.list.iter().filter(|t| t.assigned).count();
    let unread = unread_count(&issues.list, &ReadState::load(issues::READ_FILE, now - issues::look_back()));
    Ok(format!("Connected as {}: {open} open issues assigned to you, {unread} with something unread", issues.me))
}

pub struct Client {
    key: String,
    agent: ureq::Agent,
}

impl Client {
    /// A client from the saved settings; `None` without an API key.
    pub fn from_settings() -> Option<Self> {
        let key = INTEGRATION.value("api_key")?;
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(20)).user_agent("notch-assistant").build();
        Some(Client { key, agent })
    }

    /// The query's raw JSON, for `--probe-linear`'s troubleshooting.
    pub fn query(&self) -> Result<Value, String> {
        let req = self.agent.post(API).set("Authorization", &self.key);
        let body = match req.send_json(json!({ "query": QUERY })) {
            Ok(resp) => resp.into_json::<Value>().map_err(|e| format!("bad JSON ({e})"))?,
            // GraphQL errors (a field Linear renamed, rate limiting) come back as 400 with the reason.
            Err(ureq::Error::Status(code, resp)) => {
                let body = resp.into_json::<Value>().unwrap_or(Value::Null);
                return Err(match first_error(&body) {
                    Some(m) => format!("HTTP {code}: {m}"),
                    None if code == 401 => "API key rejected (401): check it in Settings".into(),
                    None => format!("HTTP {code}"),
                });
            }
            Err(e) => return Err(format!("network: {e}")),
        };
        match (body.get("data").filter(|d| !d.is_null()), first_error(&body)) {
            (None, m) => Err(m.unwrap_or_else(|| "no data".into())),
            (Some(_), Some(m)) => { eprintln!("linear: {m}"); Ok(body) }
            (Some(_), None) => Ok(body),
        }
    }

    pub fn fetch(&self, now: NaiveDateTime) -> Result<Issues, String> {
        Ok(parse_issues(&self.query()?["data"], now))
    }

    pub fn fetcher(self) -> impl FnMut() -> Result<Issues, String> + Send + 'static {
        move || self.fetch(Local::now().naive_local())
    }
}

impl Fetched for Issues { fn origin(&mut self) -> &mut Origin { &mut self.origin } }

fn first_error(body: &Value) -> Option<String> {
    body.get("errors")?.as_array()?.iter().find_map(|e| e.get("message")?.as_str().map(str::to_string))
}

fn parse_issues(data: &Value, now: NaiveDateTime) -> Issues {
    let me = str_at(&data["viewer"], "name").unwrap_or_default().to_string();
    let mut list: Vec<Ticket> = vec![];
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut add = |list: &mut Vec<Ticket>, t: Ticket| -> usize {
        *index.entry(t.id.clone()).or_insert_with(|| { list.push(t); list.len() - 1 })
    };
    for issue in data["viewer"]["assignedIssues"]["nodes"].as_array().into_iter().flatten() {
        if let Some(mut t) = ticket(issue, false) {
            t.assigned = true;
            add(&mut list, t);
        }
    }
    for n in data["notifications"]["nodes"].as_array().into_iter().flatten() {
        let (Some(t), Some(notice)) = (ticket(&n["issue"], false).or_else(|| ticket(&n["project"], true)), notice(n, now)) else { continue };
        let i = add(&mut list, t);
        list[i].notices.push(notice);
    }
    for t in &mut list { t.notices.sort_by_key(|n| n.at); }
    Issues { me, list, origin: Origin::Live { at: now } }
}

fn ticket(v: &Value, project: bool) -> Option<Ticket> {
    let id = str_at(v, "id")?.to_string();
    let state = &v["state"];
    Some(Ticket {
        id,
        key: str_at(v, "identifier").unwrap_or_default().to_string(),
        title: str_at(v, "title").or_else(|| str_at(v, "name")).unwrap_or_default().to_string(),
        url: str_at(v, "url").unwrap_or_default().to_string(),
        state: if project { "Project".into() } else { str_at(state, "name").unwrap_or_default().to_string() },
        color: str_at(state, "color").and_then(|c| u32::from_str_radix(c.trim_start_matches('#'), 16).ok()).unwrap_or(0x95a2b3),
        assigned: false,
        started: str_at(state, "type") == Some("started"),
        updated: str_at(v, "updatedAt").and_then(parse).unwrap_or_default(),
        notices: vec![],
    })
}

/// A notification as the panel shows it; `None` for a bot's.
fn notice(n: &Value, now: NaiveDateTime) -> Option<Notice> {
    let who = str_at(&n["actor"], "name").unwrap_or_default().to_string();
    if who.is_empty() && !n["botActor"].is_null() { return None; }
    let snoozed = str_at(n, "snoozedUntilAt").and_then(parse).is_some_and(|t| t > now);
    let comment = &n["comment"];
    let text = str_at(comment, "body").or_else(|| str_at(&n["projectUpdate"], "body")).unwrap_or_default();
    Some(Notice {
        id: str_at(n, "id")?.to_string(),
        at: str_at(n, "createdAt").and_then(parse)?,
        read: !n["readAt"].is_null() || snoozed,
        headline: headline(str_at(n, "type").unwrap_or_default(), &who, str_at(&n["issue"]["state"], "name").unwrap_or_default(),
            str_at(n, "reactionEmoji"), !n["parentComment"].is_null()),
        who,
        excerpt: excerpt(text),
        url: str_at(comment, "url").or_else(|| str_at(&n["projectUpdate"], "url")).unwrap_or_default().to_string(),
    })
}

/// "Ada Byron mentioned you", "SLA breached". Linear adds notification types over time, so an
/// unknown one reads as its name in words: `issueSomethingNew` is "something new".
fn headline(kind: &str, who: &str, state: &str, emoji: Option<&str>, reply: bool) -> String {
    let what = match kind {
        k if k.ends_with("Mention") => "mentioned you".to_string(),
        k if k.ends_with("NewComment") => (if reply { "replied" } else { "commented" }).to_string(),
        k if k.ends_with("Reaction") => format!("reacted {}", emoji.unwrap_or("")).trim_end().to_string(),
        "issueStatusChanged" if !state.is_empty() => format!("moved it to {state}"),
        "issueAssignedToYou" => "assigned it to you".into(),
        "issueUnassignedFromYou" => "unassigned you".into(),
        "issueUnblocked" => "unblocked it".into(),
        "issueBlocking" => "marked it blocked".into(),
        "issueSlaBreached" => "SLA breached".into(),
        "issueSlaHighRisk" => "SLA at high risk".into(),
        "projectAddedAsLead" => "made you the project lead".into(),
        "projectAddedAsMember" => "added you to the project".into(),
        "projectUpdateCreated" => "posted a project update".into(),
        k => words(k.trim_start_matches("issue").trim_start_matches("project")),
    };
    if who.is_empty() {
        let mut c = what.chars();
        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
    } else {
        format!("{who} {what}")
    }
}

/// "StatusChanged" as "status changed".
fn words(camel: &str) -> String {
    let mut out = String::new();
    for c in camel.chars() {
        if c.is_uppercase() && !out.is_empty() { out.push(' '); }
        out.extend(c.to_lowercase());
    }
    out
}

fn excerpt(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(160) {
        Some((i, _)) => format!("{}…", &flat[..i]),
        None => flat,
    }
}

fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> { v.get(key).and_then(Value::as_str) }

const QUERY: &str = r#"
query {
  viewer {
    name
    assignedIssues(first: 50, orderBy: updatedAt, filter: { state: { type: { in: ["started", "unstarted"] } } }) {
      nodes { ...issue }
    }
  }
  notifications(first: 100) {
    nodes {
      id type createdAt readAt snoozedUntilAt
      actor { name }
      botActor { name }
      ... on IssueNotification {
        reactionEmoji
        parentComment { id }
        comment { body url }
        issue { ...issue }
      }
      ... on ProjectNotification {
        reactionEmoji
        comment { body url }
        projectUpdate { body url }
        project { id name url updatedAt }
      }
    }
  }
}
fragment issue on Issue { id identifier title url updatedAt state { name type color } }
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn now() -> NaiveDateTime { NaiveDate::from_ymd_opt(2026, 11, 10).unwrap().and_hms_opt(14, 5, 0).unwrap() }

    #[test]
    fn headlines_read_naturally_and_unknown_types_still_make_sense() {
        assert_eq!(headline("issueCommentMention", "Ada Byron", "", None, false), "Ada Byron mentioned you");
        assert_eq!(headline("issueNewComment", "Ada Byron", "", None, true), "Ada Byron replied");
        assert_eq!(headline("issueEmojiReaction", "Ada Byron", "", Some("👍"), false), "Ada Byron reacted 👍");
        assert_eq!(headline("issueStatusChanged", "Ada Byron", "Done", None, false), "Ada Byron moved it to Done");
        assert_eq!(headline("issueSlaBreached", "", "", None, false), "SLA breached");
        assert_eq!(headline("issueSomethingNew", "Ada Byron", "", None, false), "Ada Byron something new");
        assert_eq!(headline("projectSomethingNew", "", "", None, false), "Something new");
    }

    #[test]
    fn notifications_group_by_issue_and_bots_and_documents_are_left_out() {
        let issue = |id: &str, key: &str| json!({"id": id, "identifier": key, "title": "T", "url": format!("https://linear.app/x/issue/{key}"),
            "updatedAt": "2026-11-10T09:00:00Z", "state": {"name": "In Progress", "type": "started", "color": "#f2c94c"}});
        let data = json!({
            "viewer": {"name": "O P", "assignedIssues": {"nodes": [issue("i1", "MAR-1")]}},
            "notifications": {"nodes": [
                {"id": "a", "type": "issueCommentMention", "createdAt": "2026-11-10T12:00:00Z", "readAt": null, "snoozedUntilAt": null,
                 "actor": {"name": "Ada Byron"}, "botActor": null, "comment": {"body": "can you\n look?", "url": "u-a"}, "issue": issue("i1", "MAR-1")},
                {"id": "b", "type": "issueStatusChanged", "createdAt": "2026-11-10T12:30:00Z", "readAt": null, "snoozedUntilAt": null,
                 "actor": null, "botActor": {"name": "GitHub"}, "issue": issue("i2", "MAR-2")},
                {"id": "c", "type": "issueSlaBreached", "createdAt": "2026-11-10T11:00:00Z", "readAt": null, "snoozedUntilAt": "2026-11-11T09:00:00Z",
                 "actor": null, "botActor": null, "issue": issue("i3", "MAR-3")},
                {"id": "d", "type": "documentMention", "createdAt": "2026-11-10T12:00:00Z", "readAt": null, "actor": {"name": "Ada Byron"}},
                {"id": "e", "type": "projectAddedAsMember", "createdAt": "2026-11-10T10:00:00Z", "readAt": "2026-11-10T10:05:00Z",
                 "actor": {"name": "Ada Byron"}, "botActor": null, "project": {"id": "p1", "name": "Arm64", "url": "u-p"}},
            ]},
        });
        let issues = parse_issues(&data, now());
        let got: Vec<_> = issues.list.iter().map(|t| (t.name(), t.state.as_str(), t.assigned, t.notices.iter().map(|n| (n.headline.as_str(), n.read)).collect::<Vec<_>>())).collect();
        assert_eq!(got, [
            ("MAR-1 T".to_string(), "In Progress", true, vec![("Ada Byron mentioned you", false)]),
            ("MAR-3 T".to_string(), "In Progress", false, vec![("SLA breached", true)]),
            ("Arm64".to_string(), "Project", false, vec![("Ada Byron added you to the project", true)]),
        ], "the bot's status change and the document mention are dropped; snoozed counts as read");
        let mention = &issues.list[0].notices[0];
        assert_eq!((mention.excerpt.as_str(), mention.url.as_str(), issues.list[0].color), ("can you look?", "u-a", 0xf2c94c));
    }
}
