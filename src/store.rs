//! Where integration settings live.
//!
//! - Secrets go in the login Keychain: service `notch-assistant`, account `<integration>.<field>`.
//!   They are read and written through `/usr/bin/security`, so the item's access list names that
//!   tool rather than this binary, and a rebuilt app doesn't prompt for Keychain access. Writes
//!   pass the secret on stdin (`security -i`), never on a command line other processes can see.
//! - Everything else goes in `~/Library/Application Support/notch-assistant/settings.json`, as
//!   `{ "<integration>": { "<field>": "value" } }`.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::{Map, Value};

const SERVICE: &str = "notch-assistant";
const SECURITY: &str = "/usr/bin/security";

fn account(integration: &str, field: &str) -> String { format!("{integration}.{field}") }

pub fn secret(integration: &str, field: &str) -> Option<String> {
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
pub fn legacy_secret(service: &str) -> Option<String> { keychain_read(service, "") }

pub fn set_secret(integration: &str, field: &str, value: &str) -> Result<(), String> {
    if value.chars().any(|c| c.is_control() || c == '"' || c == '\\') {
        return Err("the value has characters a key can't contain (quotes, backslashes or control characters)".into());
    }
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
    let _ = Command::new(SECURITY).args(["delete-generic-password", "-s", SERVICE, "-a", &account(integration, field)])
        .stdout(Stdio::null()).stderr(Stdio::null()).status();
}

/// Deletes an item stored before settings existed.
pub fn delete_legacy(service: &str) {
    let _ = Command::new(SECURITY).args(["delete-generic-password", "-s", service])
        .stdout(Stdio::null()).stderr(Stdio::null()).status();
}

fn settings_path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support/notch-assistant/settings.json"))
}

fn load() -> Map<String, Value> {
    settings_path().and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.as_object().cloned()).unwrap_or_default()
}

pub fn setting(integration: &str, field: &str) -> Option<String> {
    load().get(integration)?.get(field)?.as_str().map(str::to_string).filter(|s| !s.trim().is_empty())
}

/// Sets (or, with an empty value, clears) one setting.
pub fn set_setting(integration: &str, field: &str, value: &str) -> Result<(), String> {
    let path = settings_path().ok_or("no home folder")?;
    let mut all = load();
    let section = all.entry(integration).or_insert_with(|| Value::Object(Map::new()));
    let section = section.as_object_mut().ok_or("settings.json is malformed")?;
    if value.trim().is_empty() { section.remove(field); } else { section.insert(field.into(), Value::String(value.trim().into())); }
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(&path, serde_json::to_vec_pretty(&Value::Object(all)).unwrap()).map_err(|e| e.to_string())
}
