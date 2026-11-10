//! The integrations the app knows, and the settings each one needs.
//!
//! An integration declares its fields; the Settings window renders them and `store` keeps them
//! (secrets in the Keychain, the rest in settings.json). Adding one means a new `Integration` in
//! `ALL` plus the module that uses its values. A different kind of auth gets a new `FieldKind`
//! with its own control in the Settings window; an OAuth sign-in, for instance, would be a
//! "Connect" button that stores the tokens it gets as secrets.

use crate::store;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    /// A token or password: Keychain only, never shown again once saved.
    Secret,
    /// Plain text in settings.json.
    Text,
}

pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
    pub help: &'static str,
    /// Shown when empty: the value used when nothing is set.
    pub placeholder: fn() -> String,
    /// An environment variable that overrides the stored value.
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

pub const ALL: &[&Integration] = &[&crate::firehydrant::INTEGRATION];

impl Integration {
    pub fn field(&self, key: &str) -> &Field {
        self.fields.iter().find(|f| f.key == key).unwrap_or_else(|| panic!("{}: no field {key}", self.id))
    }

    /// A field's value: its environment variable, else what's stored.
    pub fn value(&self, key: &str) -> Option<String> {
        let field = self.field(key);
        if let Ok(v) = std::env::var(field.env) {
            if !v.trim().is_empty() { return Some(v.trim().to_string()); }
        }
        match field.kind {
            FieldKind::Secret => store::secret(self.id, key).or_else(|| {
                self.legacy.iter().filter(|(f, _)| *f == key).find_map(|(_, service)| store::legacy_secret(service))
            }),
            FieldKind::Text => store::setting(self.id, key),
        }
    }

    /// A field's value, else its placeholder default.
    pub fn value_or_default(&self, key: &str) -> String {
        self.value(key).unwrap_or_else(|| (self.field(key).placeholder)())
    }

    pub fn configured(&self) -> bool { self.value(self.required).is_some() }

    /// Whether an environment variable is overriding this field.
    pub fn overridden(&self, key: &str) -> bool {
        std::env::var(self.field(key).env).is_ok_and(|v| !v.trim().is_empty())
    }
}
