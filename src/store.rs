//! Where integration settings live.
//!
//! - Secrets go in the login Keychain: service `notch-assistant`, account `<integration>.<field>`.
//!   They are read and written through `/usr/bin/security`, so the item's access list names that
//!   tool rather than this binary, and a rebuilt app doesn't prompt for Keychain access. Writes
//!   pass the secret on stdin (`security -i`), never on a command line other processes can see.
//! - Everything else goes in `~/Library/Application Support/notch-assistant/settings.json`, as
//!   `{ "<integration>": { "<field>": "value" } }`.
//! - What the app keeps track of by itself (what you've read, say) goes in its own JSON file
//!   in the same folder (`state`, `set_state`).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Map, Value};

/// Demo mode keeps everything in memory, so a recording never touches real settings.
static MEMORY: std::sync::Mutex<Option<std::collections::HashMap<String, String>>> = std::sync::Mutex::new(None);

pub fn use_memory() { *MEMORY.lock().unwrap() = Some(Default::default()); }

fn memory<R>(f: impl FnOnce(&mut std::collections::HashMap<String, String>) -> R) -> Option<R> {
    MEMORY.lock().unwrap().as_mut().map(f)
}

const SERVICE: &str = "notch-assistant";
const SECURITY: &str = "/usr/bin/security";

fn account(integration: &str, field: &str) -> String { format!("{integration}.{field}") }

pub fn secret(integration: &str, field: &str) -> Option<String> {
    if let Some(v) = memory(|m| m.get(&account(integration, field)).cloned()) { return v; }
    keychain_read(SERVICE, &account(integration, field))
}

fn keychain_read(service: &str, account: &str) -> Option<String> {
    let mut cmd = Command::new(SECURITY);
    cmd.args(["find-generic-password", "-s", service]);
    if !account.is_empty() { cmd.args(["-a", account]); }
    let out = cmd.arg("-w").stderr(Stdio::null()).output().ok()?;
    let value = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !value.is_empty()).then_some(value)
}

/// Reads an item stored before settings existed (service only, any account).
pub fn legacy_secret(service: &str) -> Option<String> {
    if memory(|_| ()).is_some() { return None; }
    keychain_read(service, "")
}

pub fn set_secret(integration: &str, field: &str, value: &str) -> Result<(), String> {
    if value.chars().any(|c| c.is_control() || c == '"' || c == '\\') {
        return Err("the value has characters a key can't contain (quotes, backslashes or control characters)".into());
    }
    if memory(|m| m.insert(account(integration, field), value.to_string())).is_some() { return Ok(()); }
    let command = format!("add-generic-password -U -s {SERVICE} -a {} -l \"Notch assistant: {integration}\" -w \"{value}\"\n", account(integration, field));
    let mut child = Command::new(SECURITY).arg("-i").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped())
        .spawn().map_err(|e| format!("couldn't run security: {e}"))?;
    child.stdin.take().unwrap().write_all(command.as_bytes()).map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let err = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() || err.contains("error") {
        return Err(format!("Keychain refused it: {}", err.trim()));
    }
    Ok(())
}

pub fn delete_secret(integration: &str, field: &str) {
    if memory(|m| m.remove(&account(integration, field))).is_some() { return; }
    let _ = Command::new(SECURITY).args(["delete-generic-password", "-s", SERVICE, "-a", &account(integration, field)])
        .stdout(Stdio::null()).stderr(Stdio::null()).status();
}

/// Deletes an item stored before settings existed.
pub fn delete_legacy(service: &str) {
    if memory(|_| ()).is_some() { return; }
    let _ = Command::new(SECURITY).args(["delete-generic-password", "-s", service])
        .stdout(Stdio::null()).stderr(Stdio::null()).status();
}

fn path(file: &str) -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support/notch-assistant").join(file))
}

fn settings_path() -> Option<PathBuf> { path("settings.json") }

/// Reads a JSON file. One that doesn't parse is set aside as `<file>.corrupt`: read as empty, the
/// next save would replace it and lose what it held.
fn read_json(path: &Path) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(value) => Some(value),
        Err(e) => {
            let aside = path.with_extension("json.corrupt");
            eprintln!("store: {} doesn't parse ({e}); moved it to {}", path.display(), aside.display());
            let _ = std::fs::rename(path, aside);
            None
        }
    }
}

/// Writes a file whole or not at all, so a crash mid-write can't leave half of it. Each write has
/// its own partial file: threads write too (the calendar's answer arrives on one).
fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::fs::create_dir_all(path.parent().ok_or("no folder")?).map_err(|e| e.to_string())?;
    let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let partial = path.with_extension(format!("json.{}-{n}.partial", std::process::id()));
    std::fs::write(&partial, serde_json::to_vec_pretty(value).unwrap()).map_err(|e| e.to_string())?;
    std::fs::rename(&partial, path).map_err(|e| e.to_string())
}

/// `<name>.json`, as the app last saved it.
pub fn state(name: &str) -> Option<Value> {
    if let Some(v) = memory(|m| m.get(&format!("state:{name}")).cloned()) { return v.and_then(|s| serde_json::from_str(&s).ok()); }
    read_json(&path(&format!("{name}.json"))?)
}

pub fn set_state(name: &str, value: &Value) -> Result<(), String> {
    if memory(|m| m.insert(format!("state:{name}"), value.to_string())).is_some() { return Ok(()); }
    write_json(&path(&format!("{name}.json")).ok_or("no home folder")?, value)
}

fn load() -> Map<String, Value> {
    settings_path().and_then(|p| read_json(&p)).and_then(|v| v.as_object().cloned()).unwrap_or_default()
}

/// Another copy holds the app's lock.
#[derive(Debug)]
pub struct Running;

/// Takes the app's lock and holds it while the returned file lives, so a second copy (a
/// `cargo run` beside the installed app) can't overwrite the first one's read marks. A restart
/// onto a new version overlaps the old process for a moment, so a held lock gets five seconds to
/// free up. `None` when the lock can't be taken at all (no home folder, a file system without
/// locks): better to run unguarded than not at all.
pub fn single_instance() -> Result<Option<std::fs::File>, Running> {
    let opened = path("app.lock").ok_or_else(|| "no home folder".to_string()).and_then(|path| {
        std::fs::create_dir_all(path.parent().ok_or("no folder")?).map_err(|e| e.to_string())?;
        std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path).map_err(|e| e.to_string())
    });
    let file = match opened {
        Ok(file) => file,
        Err(e) => { eprintln!("store: running without the single-copy lock: {e}"); return Ok(None); }
    };
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Some(file)),
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < until => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(std::fs::TryLockError::WouldBlock) => return Err(Running),
            Err(std::fs::TryLockError::Error(e)) => { eprintln!("store: running without the single-copy lock: {e}"); return Ok(None); }
        }
    }
}

pub fn setting(integration: &str, field: &str) -> Option<String> {
    if let Some(v) = memory(|m| m.get(&format!("setting:{integration}.{field}")).cloned()) { return v.filter(|s| !s.trim().is_empty()); }
    load().get(integration)?.get(field)?.as_str().map(str::to_string).filter(|s| !s.trim().is_empty())
}

/// Sets (or, with an empty value, clears) one setting. Writers take turns: each reads the file,
/// changes one value and writes it all back.
pub fn set_setting(integration: &str, field: &str, value: &str) -> Result<(), String> {
    static WRITING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    if memory(|m| m.insert(format!("setting:{integration}.{field}"), value.trim().to_string())).is_some() { return Ok(()); }
    let _turn = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let path = settings_path().ok_or("no home folder")?;
    let mut all = load();
    let section = all.entry(integration).or_insert_with(|| Value::Object(Map::new()));
    let section = section.as_object_mut().ok_or("settings.json is malformed")?;
    if value.trim().is_empty() { section.remove(field); } else { section.insert(field.into(), Value::String(value.trim().into())); }
    write_json(&path, &Value::Object(all))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("notch-store-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_write_replaces_the_file_whole_and_leaves_nothing_behind() {
        let dir = dir("write");
        let file = dir.join("read.json");
        write_json(&file, &serde_json::json!({ "a": 1 })).unwrap();
        write_json(&file, &serde_json::json!({ "a": 2 })).unwrap();
        assert_eq!(read_json(&file), Some(serde_json::json!({ "a": 2 })));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no partial file left");
    }

    #[test]
    fn a_file_that_doesnt_parse_is_kept_aside() {
        let dir = dir("corrupt");
        let file = dir.join("read.json");
        std::fs::write(&file, b"{ \"read\": { trunc").unwrap();
        assert_eq!(read_json(&file), None);
        assert!(!file.exists());
        assert_eq!(std::fs::read(dir.join("read.json.corrupt")).unwrap(), b"{ \"read\": { trunc");
    }
}
