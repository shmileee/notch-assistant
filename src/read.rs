//! What you've read, for the integrations that notify (GitHub, Linear), and which unread
//! events are new since the last fetch.
//!
//! Each integration keeps a JSON file next to settings.json:
//! `{ "since": <first connected>, "read": { <id>: <time> } }`, times in RFC 3339. An id is
//! whatever the integration reads by (a pull request, an issue), and its time is when you
//! last read it: opened from the panel, or as the service reported. Something with no time
//! counts from `since`, so a first run doesn't light up everything old.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Local};
use crate::time::Time;
use serde_json::{Map, Value, json};

use crate::store;

#[derive(Clone, Debug, PartialEq)]
pub struct ReadState {
    file: &'static str,
    since: Time,
    read: HashMap<String, Time>,
    /// The mock's state lives in memory and never touches a file.
    saved: bool,
}

impl ReadState {
    /// What's saved in `<file>.json`; without it, counting starts at `since`.
    pub fn load(file: &'static str, since: Time) -> Self {
        let saved = store::state(file).unwrap_or(Value::Null);
        let since = saved.get("since").and_then(Value::as_str).and_then(parse).unwrap_or(since);
        let read = saved.get("read").and_then(Value::as_object).into_iter().flatten()
            .filter_map(|(id, t)| Some((id.clone(), parse(t.as_str()?)?))).collect();
        ReadState { file, since, read, saved: true }
    }

    /// For the mock: counting from `since`, in memory only.
    pub fn memory(since: Time) -> Self { ReadState { file: "", since, read: HashMap::new(), saved: false } }

    pub fn save(&self) {
        if !self.saved { return; }
        let read: Map<String, Value> = self.read.iter().map(|(id, t)| (id.clone(), Value::String(format(*t)))).collect();
        if let Err(e) = store::set_state(self.file, &json!({ "since": format(self.since), "read": read })) {
            eprintln!("{}: couldn't save what you've read: {e}", self.file);
        }
    }

    /// When you last read `id`: the later of the saved time and `elsewhere` (the service's own
    /// read time), else when counting started.
    pub fn last_read(&self, id: &str, elsewhere: Option<Time>) -> Time {
        self.read.get(id).copied().max(elsewhere).unwrap_or(self.since)
    }

    /// Keeps the read times a service reports, so something read there stays read once the
    /// service stops reporting it. Forgets ids no longer listed once their time is three months
    /// old: one fetch that misses something mustn't make it unread.
    pub fn sync<'a>(&mut self, listed: impl IntoIterator<Item = (&'a str, Option<Time>)>, now: Time) {
        let before = self.read.clone();
        let mut ids = HashSet::new();
        for (id, at) in listed {
            ids.insert(id.to_string());
            if let Some(at) = at { self.bump(id, at); }
        }
        self.read.retain(|id, t| ids.contains(id) || *t > now - Duration::days(90));
        if self.read != before { self.save(); }
    }

    pub fn mark(&mut self, id: &str, at: Time) {
        self.bump(id, at);
        self.save();
    }

    /// Marks each id read up to its time: the latest event shown, since something that
    /// happened after the last refresh hasn't been seen yet.
    pub fn mark_all<'a>(&mut self, shown: impl IntoIterator<Item = (&'a str, Time)>) {
        for (id, at) in shown { self.bump(id, at); }
        self.save();
    }

    fn bump(&mut self, id: &str, at: Time) {
        let t = self.read.entry(id.to_string()).or_insert(at);
        *t = (*t).max(at);
    }
}

pub fn parse(s: &str) -> Option<Time> {
    DateTime::parse_from_rfc3339(s).ok().map(|t| t.with_timezone(&Local))
}

fn format(t: Time) -> String { t.to_rfc3339() }

/// The unread events already announced, so a peek shows only what's new.
#[derive(Debug, Default)]
pub struct Announced(Option<HashSet<String>>);

impl Announced {
    /// Of the unread events, by key, those that weren't unread at the last update. The first
    /// update gives none: what's unread when the app connects is old news.
    pub fn update<T>(&mut self, unread: Vec<(String, T)>) -> Vec<T> {
        let before = self.0.replace(unread.iter().map(|(k, _)| k.clone()).collect());
        match before {
            Some(before) => unread.into_iter().filter(|(k, _)| !before.contains(k)).map(|(_, t)| t).collect(),
            None => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn at(h: u32, m: u32) -> Time { crate::time::at(2026, 11, 10, h, m) }

    #[test]
    fn the_latest_read_wins_and_since_is_the_fallback() {
        let mut read = ReadState::memory(at(8, 0));
        assert_eq!(read.last_read("p", None), at(8, 0));
        assert_eq!(read.last_read("p", Some(at(6, 0))), at(6, 0), "a service's read time counts from before `since` too");
        read.mark("p", at(11, 30));
        assert_eq!(read.last_read("p", Some(at(10, 0))), at(11, 30));
    }

    #[test]
    fn a_service_read_time_outlives_the_service_reporting_it() {
        let mut read = ReadState::memory(at(8, 0));
        read.sync([("p", Some(at(10, 0)))], at(12, 0));
        assert_eq!(read.last_read("p", None), at(10, 0));
        read.sync([], at(12, 0));
        assert_eq!(read.last_read("p", None), at(10, 0), "kept when one fetch misses it");
        read.sync([], at(12, 0) + Duration::days(91));
        assert_eq!(read.last_read("p", None), at(8, 0), "forgotten once unlisted for three months");
    }

    #[test]
    fn mark_all_stops_at_what_was_shown() {
        let mut read = ReadState::memory(at(8, 0));
        read.mark_all([("p", at(9, 0))]);
        assert_eq!(read.last_read("p", None), at(9, 0));
    }

    #[test]
    fn only_new_unread_events_are_announced() {
        let mut announced = Announced::default();
        let keys = |ks: &[&str]| ks.iter().map(|k| (k.to_string(), k.to_string())).collect::<Vec<_>>();
        assert!(announced.update(keys(&["a"])).is_empty(), "the first fetch announces nothing");
        assert_eq!(announced.update(keys(&["a", "b"])), ["b"]);
        assert!(announced.update(keys(&["a", "b"])).is_empty(), "announced once");
        assert!(announced.update(keys(&["b"])).is_empty(), "read: nothing to announce");
        assert_eq!(announced.update(keys(&["b", "c"])), ["c"]);
    }
}
