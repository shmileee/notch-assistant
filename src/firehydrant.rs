//! FireHydrant API client: the team's on-call shifts and open incidents. Read-only GETs.
//!
//! - `GET /v1/current_user`: who the token belongs to. An API key is a bot user, so "you" are
//!   found by name instead (`me_hint`), and the token's user is only the fallback.
//! - `GET /v1/teams/{team}/on_call_schedules?shift_time_window_start=…&shift_time_window_end=…`:
//!   the team's Signals schedules with the shifts overlapping that window. A schedule whose
//!   name contains "secondary" is the secondary rotation; the rest are primary.
//! - `GET /v1/incidents?teams={team}&page=…`: incidents, page by page up to `MAX_PAGES`, keeping
//!   the active ones.
//!
//! Settings (`INTEGRATION`): the API key (Keychain), the team ID and your name on the schedule.

use std::collections::HashSet;
use std::time::Duration;

use chrono::{DateTime, Local};
use crate::time::{self, Time};
use serde_json::Value;

use crate::integrations::{Field, FieldKind, Integration};
use crate::oncall::{Data, Incident, Origin, Role, Shift, initials, monday_of};

const API: &str = "https://api.firehydrant.io";

pub const INTEGRATION: Integration = Integration {
    id: "firehydrant",
    name: "FireHydrant",
    summary: "On-call shifts and open incidents for your team.",
    fields: &[
        Field { key: "api_key", label: "API key", kind: FieldKind::Secret, env: "FIREHYDRANT_API_TOKEN",
            help: "Settings › API Keys in FireHydrant (needs the Owner role to create). Stored in your Keychain.",
            placeholder: || "Paste the key".into() },
        Field { key: "team_id", label: "Team ID", kind: FieldKind::Text, env: "FIREHYDRANT_TEAM_ID",
            help: "From the team's URL: app.firehydrant.io/…/teams/<team ID>/overview.",
            placeholder: || "The team's ID, a UUID".into() },
        Field { key: "me", label: "Your name on the schedule", kind: FieldKind::Text, env: "FIREHYDRANT_ME",
            help: "An API key acts as a bot user, so your shifts are found by name (or FireHydrant user ID).",
            placeholder: || account_name().unwrap_or_default() },
    ],
    required: "api_key",
    test,
    legacy: &[("api_key", "firehydrant-api-token")],
};

fn test() -> Result<String, String> {
    if crate::demo::on() {
        std::thread::sleep(Duration::from_millis(1200));
        return Ok("Connected to Team Marmalade: 50 shifts in the next five weeks, 2 active incidents, you are OP".into());
    }
    let client = Client::from_settings().ok_or("No API key saved")?;
    let data = client.fetch(Local::now())?;
    let you = data.shifts.iter().find(|s| s.user_id == data.me).map(|s| s.who.clone());
    Ok(format!("Connected to {}: {} shifts in the next five weeks, {} active incidents{}",
        data.team, data.shifts.len(), data.incidents.len(),
        you.map_or_else(|| ". Your name wasn't found on any shift".to_string(), |w| format!(", you are {w}"))))
}

pub struct Client {
    token: String,
    pub team_id: String,
    agent: crate::net::Http,
}

impl Client {
    /// A client from the saved settings; `None` without an API key.
    pub fn from_settings() -> Option<Self> {
        let token = INTEGRATION.value("api_key")?;
        let team_id = INTEGRATION.value("team_id").unwrap_or_default();
        Some(Client { token, team_id, agent: crate::net::Http::new("api.firehydrant.io") })
    }

    fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<Value, String> {
        let mut req = self.agent.get(&format!("{API}{path}")).set("Authorization", &format!("Bearer {}", self.token));
        for (k, v) in query { req = req.query(k, v); }
        match req.call() {
            Ok(resp) => resp.into_json::<Value>().map_err(|e| format!("{path}: bad JSON ({e})")),
            Err(ureq::Error::Status(401, _)) => Err("token rejected (401): check it in Settings › API Keys".into()),
            Err(ureq::Error::Status(403, _)) => Err(format!("{path}: forbidden (403): the key lacks access")),
            Err(ureq::Error::Status(404, _)) => Err(format!("{path}: not found (404): check FIREHYDRANT_TEAM_ID")),
            Err(ureq::Error::Status(code, _)) => Err(format!("{path}: HTTP {code}")),
            Err(e) => Err(format!("network: {e}")),
        }
    }

    /// Raw JSON of every call, for `--probe-raw`.
    pub fn raw(&self, now: Time) -> Result<Vec<(String, Value)>, String> {
        self.check_team()?;
        let (start, end) = window(now);
        Ok(vec![
            ("current_user".into(), self.get("/v1/current_user", &[])?),
            ("on_call_schedules".into(), self.get(&format!("/v1/teams/{}/on_call_schedules", self.team_id), &[("shift_time_window_start", &start), ("shift_time_window_end", &end), ("per_page", "50")])?),
            ("incidents".into(), self.get("/v1/incidents", &[("teams", &self.team_id), ("per_page", "50")])?),
        ])
    }

    fn check_team(&self) -> Result<(), String> {
        if self.team_id.is_empty() { Err("no team ID: add it in Settings (or set FIREHYDRANT_TEAM_ID)".into()) } else { Ok(()) }
    }

    pub fn fetch(&self, now: Time) -> Result<Data, String> {
        self.check_team()?;
        let token_user = self.get("/v1/current_user", &[])?;
        let token_user = str_at(&token_user, "id").unwrap_or_default().to_string();
        let hint = Some(INTEGRATION.value_or_default("me")).filter(|h| !h.is_empty());
        let mut me: Option<String> = None;

        let (start, end) = window(now);
        let schedules = self.get(&format!("/v1/teams/{}/on_call_schedules", self.team_id),
            &[("shift_time_window_start", &start), ("shift_time_window_end", &end), ("per_page", "50")])?;
        let schedules = list(&schedules);
        let team = schedules.iter().find_map(|s| s.get("team").and_then(|t| str_at(t, "name")))
            .unwrap_or("Team").to_string();
        let mut seen = HashSet::new();
        let mut shifts = Vec::new();
        for schedule in &schedules {
            let name = str_at(schedule, "name").unwrap_or_default().to_lowercase();
            let role = if name.contains("secondary") { Role::Secondary } else { Role::Primary };
            // Shifts sit on the schedule and on each rotation; take each once.
            let rotations = schedule.get("rotations").and_then(Value::as_array).into_iter().flatten();
            let all = schedule.get("shifts").and_then(Value::as_array).into_iter().flatten()
                .chain(rotations.flat_map(|r| r.get("shifts").and_then(Value::as_array).into_iter().flatten()));
            for shift in all {
                let id = str_at(shift, "id").unwrap_or_default().to_string();
                if !seen.insert(id) { continue; }
                let Some(user) = shift.get("user").filter(|u| !u.is_null()) else { continue }; // unclaimed
                let (Some(start), Some(end)) = (time_at(shift, "start_time"), time_at(shift, "end_time")) else { continue };
                let (id, name) = (str_at(user, "id").unwrap_or_default(), str_at(user, "name").unwrap_or("?"));
                if me.is_none() && hint.as_deref().is_some_and(|h| h == id || h.eq_ignore_ascii_case(name)) {
                    me = Some(id.to_string());
                }
                shifts.push(Shift { role, user_id: id.to_string(), who: initials(name), start, end });
            }
        }

        let incidents = pages(|page| self.get("/v1/incidents", &[("teams", &self.team_id), ("per_page", "50"), ("page", &page.to_string())]))?;
        let incidents = active_incidents(&incidents, now);

        let me = me.unwrap_or(token_user);
        Ok(Data { team, me, shifts, incidents, origin: Origin::Live { at: now } })
    }
}

/// This Mac account's full name: who "you" are on the schedule unless the settings say.
fn account_name() -> Option<String> {
    if crate::sandboxed() { return Some("Your full name".into()); }
    static NAME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        let out = std::process::Command::new("/usr/bin/id").arg("-F").output().ok()?;
        let name = String::from_utf8(out.stdout).ok()?.trim().to_string();
        (!name.is_empty()).then_some(name)
    }).clone()
}

/// From this week's Monday to five weeks on, as RFC 3339 with the local offset.
fn window(now: Time) -> (String, String) {
    let monday = monday_of(now.date_naive());
    (time::midnight(monday).to_rfc3339(), time::midnight(monday + chrono::Duration::weeks(5)).to_rfc3339())
}

/// A paginated response's `data`, or a bare array.
fn list(v: &Value) -> Vec<Value> {
    v.get("data").unwrap_or(v).as_array().cloned().unwrap_or_default()
}

const MAX_PAGES: u64 = 5;

/// Every item of a paginated list, following `pagination.next` for up to `MAX_PAGES` pages;
/// `get(n)` fetches page `n`, from 1. A response without `pagination` is the only page.
fn pages(mut get: impl FnMut(u64) -> Result<Value, String>) -> Result<Vec<Value>, String> {
    let (mut items, mut seen, mut page) = (Vec::new(), HashSet::new(), 1);
    loop {
        let v = get(page)?;
        // An item created while paging pushes the rest down a slot, so the next page repeats one.
        items.extend(list(&v).into_iter().filter(|i| str_at(i, "id").is_none_or(|id| seen.insert(id.to_string()))));
        match v.get("pagination").and_then(|p| p.get("next")).and_then(Value::as_u64) {
            Some(next) if next > page && page < MAX_PAGES => page = next,
            _ => return Ok(items),
        }
    }
}

fn active_incidents(items: &[Value], now: Time) -> Vec<Incident> {
    items.iter()
        .filter(|i| i.get("active").and_then(Value::as_bool).unwrap_or(false))
        .map(|i| Incident {
            severity: str_at(i, "severity").unwrap_or_default().to_string(),
            title: str_at(i, "name").unwrap_or("Untitled incident").to_string(),
            status: capitalise(str_at(i, "current_milestone").unwrap_or_default()),
            commander: commander(i),
            started: time_at(i, "started_at").or_else(|| time_at(i, "created_at")).unwrap_or(now),
        })
        .collect()
}

fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> { v.get(key).and_then(Value::as_str) }

fn time_at(v: &Value, key: &str) -> Option<Time> {
    DateTime::parse_from_rfc3339(str_at(v, key)?).ok().map(|t| t.with_timezone(&Local))
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default().replace('_', " ")
}

/// The active incident commander, as (initials, user ID).
fn commander(incident: &Value) -> Option<(String, String)> {
    incident.get("role_assignments")?.as_array()?.iter()
        .filter(|a| str_at(a, "status").is_none_or(|s| s == "active"))
        .find(|a| a.get("incident_role").and_then(|r| str_at(r, "name")).is_some_and(|n| n.to_lowercase().contains("commander")))
        .and_then(|a| a.get("user"))
        .map(|u| (initials(str_at(u, "name").unwrap_or("?")), str_at(u, "id").unwrap_or_default().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn page(n: u64, next: Option<u64>) -> Value {
        json!({ "data": [{ "id": format!("i{n}"), "active": n == 3 }], "pagination": { "page": n, "pages": 3, "next": next, "count": 3, "items": 1 } })
    }
    fn ids(items: &[Value]) -> Vec<&str> { items.iter().filter_map(|i| str_at(i, "id")).collect() }

    #[test]
    fn pages_follow_next_until_there_is_none() {
        let mut asked = vec![];
        let items = pages(|n| { asked.push(n); Ok(page(n, (n < 3).then_some(n + 1))) }).unwrap();
        assert_eq!(asked, [1, 2, 3]);
        assert_eq!(ids(&items), ["i1", "i2", "i3"]);
        let active = active_incidents(&items, Time::default());
        assert_eq!(active.len(), 1, "the active incident on the last page is kept");
    }

    #[test]
    fn a_response_without_pagination_is_the_only_page() {
        let mut asked = 0;
        let items = pages(|_| { asked += 1; Ok(json!({ "data": [{ "id": "a" }, { "id": "b" }] })) }).unwrap();
        assert_eq!((asked, ids(&items)), (1, vec!["a", "b"]));
    }

    #[test]
    fn pages_stop_at_the_cap_or_a_next_that_does_not_move_on() {
        let mut asked = vec![];
        let items = pages(|n| { asked.push(n); Ok(page(n, Some(n + 1))) }).unwrap();
        assert_eq!(asked, [1, 2, 3, 4, 5]);
        assert_eq!(items.len(), 5, "what was fetched before the cap is kept");

        let mut asked = vec![];
        pages(|n| { asked.push(n); Ok(page(n, Some(1))) }).unwrap();
        assert_eq!(asked, [1]);
    }

    #[test]
    fn a_repeated_item_is_kept_once_and_a_failed_page_fails_the_list() {
        let items = pages(|n| Ok(json!({ "data": [{ "id": "a" }, { "id": if n == 1 { "b" } else { "c" } }], "pagination": { "next": (n == 1).then_some(2) } }))).unwrap();
        assert_eq!(ids(&items), ["a", "b", "c"]);

        let failed = pages(|n| if n == 1 { Ok(page(1, Some(2))) } else { Err("HTTP 500".into()) });
        assert_eq!(failed, Err("HTTP 500".to_string()));
    }
}
