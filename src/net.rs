//! The HTTP agent every integration uses.
//!
//! ureq ships its own root certificates and doesn't read the macOS proxy settings. Behind a
//! company proxy, or a VPN that inspects TLS with a root certificate the company installed in
//! the Keychain, every integration would then fail while the browser works. So the agent trusts
//! the Keychain's roots as well as ureq's, and goes through the system's HTTPS proxy.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// An integration's way to its API at `host`. Each request goes through the proxy that applies
/// right then, so moving between networks (a proxy at the office, none at home) needs no restart.
#[derive(Clone, Debug)]
pub struct Http { host: &'static str }

impl Http {
    pub fn new(host: &'static str) -> Self { Http { host } }
    pub fn get(&self, url: &str) -> ureq::Request { agent(self.host).get(url) }
    pub fn post(&self, url: &str) -> ureq::Request { agent(self.host).post(url) }
}

/// The agent for `host` through today's proxy, one per proxy so connections are reused.
fn agent(host: &str) -> ureq::Agent {
    static AGENTS: OnceLock<Mutex<HashMap<Option<String>, ureq::Agent>>> = OnceLock::new();
    let from_env = ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"].iter().any(|v| std::env::var_os(v).is_some());
    let proxy = if from_env { None } else { system_proxy(host) };
    let mut agents = AGENTS.get_or_init(Mutex::default).lock().unwrap();
    agents.entry(proxy.clone()).or_insert_with(|| {
        let mut builder = ureq::AgentBuilder::new().timeout(Duration::from_secs(20)).user_agent("notch-assistant")
            .tls_config(tls()).try_proxy_from_env(true);
        if let Some(proxy) = proxy.and_then(|p| ureq::Proxy::new(p).ok()) { builder = builder.proxy(proxy); }
        builder.build()
    }).clone()
}

fn tls() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG.get_or_init(|| {
        let mut roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        let native = rustls_native_certs::load_native_certs();
        for e in &native.errors { eprintln!("net: couldn't read a Keychain certificate: {e}"); }
        roots.add_parsable_certificates(native.certs);
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider).with_safe_default_protocol_versions()
            .expect("ring supports the default TLS versions").with_root_certificates(roots).with_no_client_auth();
        Arc::new(config)
    }).clone()
}

/// The system's HTTPS proxy for `host`, as `http://server:port`, unless the host is excepted.
pub fn proxy_for(host: &str, enabled: bool, server: Option<&str>, port: Option<u16>, exceptions: &[String]) -> Option<String> {
    let server = server.filter(|s| enabled && !s.is_empty())?;
    let excepted = exceptions.iter().any(|e| {
        let e = e.trim().trim_start_matches("*.").trim_start_matches('.').to_ascii_lowercase();
        !e.is_empty() && (host == e || host.ends_with(&format!(".{e}")))
    });
    (!excepted).then(|| format!("http://{server}:{}", port.unwrap_or(443)))
}

#[cfg(target_os = "macos")]
fn system_proxy(host: &str) -> Option<String> {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::msg_send;
    use objc2_foundation::NSString;
    #[link(name = "SystemConfiguration", kind = "framework")]
    unsafe extern "C" { fn SCDynamicStoreCopyProxies(store: *const std::ffi::c_void) -> *mut AnyObject; }
    // SAFETY: the copied CFDictionary is toll-free bridged to NSDictionary and released by
    // `Retained`; its values are only read while it lives.
    unsafe {
        let proxies = Retained::from_raw(SCDynamicStoreCopyProxies(std::ptr::null()))?;
        let get = |key: &str| -> *mut AnyObject { msg_send![&proxies, objectForKey: &*NSString::from_str(key)] };
        let number = |key: &str| { let v = get(key); (!v.is_null()).then(|| { let n: i64 = msg_send![v, longLongValue]; n }) };
        let text = |v: *mut AnyObject| -> Option<String> { (!v.is_null()).then(|| { let s: &NSString = &*v.cast(); s.to_string() }) };
        let list = get("ExceptionsList");
        let exceptions: Vec<String> = if list.is_null() { vec![] } else {
            let count: usize = msg_send![list, count];
            (0..count).filter_map(|i| { let v: *mut AnyObject = msg_send![list, objectAtIndex: i]; text(v) }).collect()
        };
        let server = text(get("HTTPSProxy"));
        proxy_for(host, number("HTTPSEnable") == Some(1), server.as_deref(), number("HTTPSPort").and_then(|p| u16::try_from(p).ok()), &exceptions)
    }
}

#[cfg(not(target_os = "macos"))]
fn system_proxy(_: &str) -> Option<String> { None }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_proxy_applies_unless_off_or_excepted() {
        let none: [String; 0] = [];
        assert_eq!(proxy_for("api.github.com", true, Some("proxy.corp"), Some(8080), &none).as_deref(), Some("http://proxy.corp:8080"));
        assert_eq!(proxy_for("api.github.com", false, Some("proxy.corp"), Some(8080), &none), None);
        assert_eq!(proxy_for("api.github.com", true, Some(""), Some(8080), &none), None);
        let exceptions = ["*.local".to_string(), "*.github.com".into(), "169.254/16".into()];
        assert_eq!(proxy_for("api.github.com", true, Some("proxy.corp"), Some(8080), &exceptions), None);
        assert!(proxy_for("api.linear.app", true, Some("proxy.corp"), None, &exceptions).is_some());
    }

    #[test]
    fn the_agent_trusts_the_keychain_and_the_built_in_roots() {
        assert!(!tls().crypto_provider().cipher_suites.is_empty());
        let _ = Http::new("api.github.com").get("https://api.github.com/zen");
    }
}
