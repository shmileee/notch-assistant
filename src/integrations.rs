//! The integrations the app knows, and the settings each one needs.
//!
//! An integration declares its fields; the Settings window renders them and `store` keeps them
//! (secrets in the Keychain, the rest in settings.json). Adding one means a new `Integration` in
//! `ALL` plus the module that uses its values. A different kind of auth gets a new `FieldKind`
//! with its own control in the Settings window: a macOS permission is a button that asks for
//! it, and an OAuth sign-in would be a "Connect" button that stores the tokens it gets as
//! secrets.

use crate::store;

#[derive(Clone, Copy)]
pub enum FieldKind {
    /// A token or password: Keychain only, never shown again once saved.
    Secret,
    /// Plain text in settings.json.
    Text,
    /// A macOS privacy permission the app asks for itself. The field has a value while it's
    /// granted.
    Permission(&'static Permission),
}

pub struct Permission {
    pub status: fn() -> Access,
    /// Shows the system prompt. `done` runs once it's answered, maybe on another thread.
    pub request: fn(done: Box<dyn FnOnce() + Send>),
    /// Opens the pane of System Settings where it's turned on and off.
    pub open_settings: fn(),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Granted,
    NotAsked,
    Denied,
    /// Blocked by whoever manages the Mac.
    Restricted,
    /// Running outside the app bundle, which macOS won't ask on behalf of.
    Unavailable,
}

pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
    pub help: &'static str,
    /// Shown when empty: the value used when nothing is set.
    pub placeholder: fn() -> String,
    /// An environment variable that overrides the stored value; empty for none.
    pub env: &'static str,
}

pub struct Integration {
    pub id: &'static str,
    pub name: &'static str,
    pub summary: &'static str,
    pub fields: &'static [Field],
    /// The secret without which the integration is off.
    pub required: &'static str,
    /// Checks the stored settings against the service; a one-line result either way.
    /// Runs on a worker thread.
    pub test: fn() -> Result<String, String>,
    /// Older places a secret was kept: (field, Keychain service).
    pub legacy: &'static [(&'static str, &'static str)],
}

pub const ALL: &[&Integration] = &[&crate::firehydrant::INTEGRATION, &crate::github::INTEGRATION, &crate::linear::INTEGRATION,
    &crate::calendar::INTEGRATION];

impl Integration {
    pub fn field(&self, key: &str) -> &Field {
        self.fields.iter().find(|f| f.key == key).unwrap_or_else(|| panic!("{}: no field {key}", self.id))
    }

    /// A field's value: its environment variable, else what's stored.
    pub fn value(&self, key: &str) -> Option<String> {
        let field = self.field(key);
        if self.overridden(key) { return std::env::var(field.env).ok().map(|v| v.trim().to_string()); }
        match field.kind {
            FieldKind::Secret => store::secret(self.id, key).or_else(|| {
                self.legacy.iter().filter(|(f, _)| *f == key).find_map(|(_, service)| store::legacy_secret(service))
            }),
            FieldKind::Text => store::setting(self.id, key),
            FieldKind::Permission(p) => ((p.status)() == Access::Granted).then(|| "granted".into()),
        }
    }

    /// A field's value, else its placeholder default.
    pub fn value_or_default(&self, key: &str) -> String {
        self.value(key).unwrap_or_else(|| (self.field(key).placeholder)())
    }

    pub fn configured(&self) -> bool { self.value(self.required).is_some() }

    /// Whether an environment variable is overriding this field.
    pub fn overridden(&self, key: &str) -> bool {
        let env = self.field(key).env;
        !env.is_empty() && std::env::var(env).is_ok_and(|v| !v.trim().is_empty())
    }
}
