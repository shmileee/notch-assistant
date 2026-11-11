//! FireHydrant API client: the team's on-call shifts and open incidents. Read-only GETs.
//!
//! - `GET /v1/current_user`: who the token belongs to. An API key is a bot user, so "you" are
//!   found by name instead (`me_hint`), and the token's user is only the fallback.
//! - `GET /v1/teams/{team}/on_call_schedules?shift_time_window_start=…&shift_time_window_end=…`:
//!   the team's Signals schedules with the shifts overlapping that window. A schedule whose
//!   name contains "secondary" is the secondary rotation; the rest are primary.
//! - `GET /v1/incidents?teams={team}`: incidents, keeping the active ones.
//!
//! Settings (`INTEGRATION`): the API key (Keychain), the team ID and your name on the schedule.

use std::collections::HashSet;
use std::time::Duration;

use chrono::{DateTime, Local, NaiveDateTime};
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
        return Ok("Connected to Team SRE: 50 shifts in the next five weeks, 2 active incidents, you are OP".into());
    }
    let client = Client::from_settings().ok_or("No API key saved")?;
    let data = client.fetch(Local::now().naive_local())?;
    let you = data.shifts.iter().find(|s| s.user_id == data.me).map(|s| s.who.clone());
    Ok(format!("Connected to {}: {} shifts in the next five weeks, {} active incidents{}",
        data.team, data.shifts.len(), data.incidents.len(),
        you.map_or_else(|| ". Your name wasn't found on any shift".to_string(), |w| format!(", you are {w}"))))
}

pub struct Client {
    token: String,
    pub team_id: String,
    agent: ureq::Agent,
}

impl Client {
    /// A client from the saved settings; `None` without an API key.
    pub fn from_settings() -> Option<Self> {
        let token = INTEGRATION.value("api_key")?;
        let team_id = INTEGRATION.value("team_id").unwrap_or_default();
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(20)).user_agent("notch-assistant").build();
        Some(Client { token, team_id, agent })
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
    pub fn raw(&self, now: NaiveDateTime) -> Result<Vec<(String, Value)>, String> {
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

    pub fn fetch(&self, now: NaiveDateTime) -> Result<Data, String> {
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

        let incidents = self.get("/v1/incidents", &[("teams", &self.team_id), ("per_page", "50")])?;
        let incidents = list(&incidents).iter()
            .filter(|i| i.get("active").and_then(Value::as_bool).unwrap_or(false))
            .map(|i| Incident {
                severity: str_at(i, "severity").unwrap_or_default().to_string(),
                title: str_at(i, "name").unwrap_or("Untitled incident").to_string(),
                status: capitalise(str_at(i, "current_milestone").unwrap_or_default()),
                commander: commander(i),
                started: time_at(i, "started_at").or_else(|| time_at(i, "created_at")).unwrap_or(now),
            })
            .collect();

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
fn window(now: NaiveDateTime) -> (String, String) {
    let monday = monday_of(now.date()).and_hms_opt(0, 0, 0).unwrap();
    let local = |t: NaiveDateTime| t.and_local_timezone(Local).earliest().map(|t| t.to_rfc3339()).unwrap_or_default();
    (local(monday), local(monday + chrono::Duration::weeks(5)))
}

/// A paginated response's `data`, or a bare array.
fn list(v: &Value) -> Vec<Value> {
    v.get("data").unwrap_or(v).as_array().cloned().unwrap_or_default()
}

fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> { v.get(key).and_then(Value::as_str) }

fn time_at(v: &Value, key: &str) -> Option<NaiveDateTime> {
    DateTime::parse_from_rfc3339(str_at(v, key)?).ok().map(|t| t.with_timezone(&Local).naive_local())
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

/// Refreshes every five minutes on its own thread, sending each result and waking the UI.
/// Dropping the returned sender stops it at once (that's how new settings take over); sending
/// on it refreshes now.
pub fn spawn(client: Client, tx: std::sync::mpsc::Sender<Result<Data, String>>, wake: rune::Waker) -> std::sync::mpsc::Sender<()> {
    use std::sync::mpsc::RecvTimeoutError;
    let (control, commands) = std::sync::mpsc::channel::<()>();
    std::thread::Builder::new().name("firehydrant".into()).spawn(move || loop {
        let result = client.fetch(Local::now().naive_local());
        if let Err(e) = &result { eprintln!("firehydrant: {e}"); }
        if tx.send(result).is_err() { return; }
        wake();
        match commands.recv_timeout(Duration::from_secs(300)) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }).expect("spawn firehydrant thread");
    control
}
