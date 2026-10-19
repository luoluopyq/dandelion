//! C18: install extensions from outside the machine.
//!
//! Two read-only sources, both driven by `dandelion.registry.*`:
//! - `mcp`: the official MCP registry (`registry.modelcontextprotocol.io`),
//!   whose entries become `dandelion.mcp.servers` rows.
//! - `skill`: a `SKILL.md` URL or a git repository, landing through the same
//!   `import_into_data_dir` the local import already uses.
//!
//! Outbound HTTP lives here (core) on purpose: `dandelion-cli` must not depend
//! on `reqwest`, and the desktop has no `http:` capability. Hosts only call
//! `Runtime`.

pub mod mcp;
pub mod skill;

use crate::config::Settings;
use crate::{Error, Result, VERSION};
use std::time::Duration;

pub use mcp::{
    McpEnvNeed, McpInstallPlan, McpRegistryEntry, McpRegistryPage, McpVariant, McpVariantKind,
};
pub use skill::SkillInstallReport;

/// A registry or skill source is remote code-shaped content, so cleartext is
/// refused rather than warned about. The one exception is loopback: a
/// self-hosted mirror on `127.0.0.1` never crosses a network, so requiring TLS
/// there would only push people to disable the check outright.
pub fn require_secure_url(url: &str) -> Result<url_parts::Parts> {
    let parts = url_parts::split(url)
        .ok_or_else(|| Error::Invalid(format!("not a valid absolute URL: {url}")))?;
    if parts.scheme == "https" || (parts.scheme == "http" && is_loopback(&parts.host)) {
        return Ok(parts);
    }
    Err(Error::Invalid(format!(
        "must be an https:// URL (http:// is allowed only for loopback): {url}"
    )))
}

fn is_loopback(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    h == "localhost" || h == "::1" || h == "127.0.0.1" || h.starts_with("127.")
}

/// Deny-by-default host check for skill sources. An empty allowlist refuses
/// every remote install; it never means "allow anything".
fn require_allowed_host(url: &str, settings: &Settings) -> Result<()> {
    let parts = require_secure_url(url)?;
    let allowed = &settings.registry_skill_allowed_hosts;
    if allowed.is_empty() {
        return Err(Error::Invalid(
            "dandelion.registry.skillAllowedHosts is empty: remote skill install is disabled"
                .into(),
        ));
    }
    let host = parts.host.to_ascii_lowercase();
    let ok = allowed.iter().any(|a| {
        let a = a.trim().to_ascii_lowercase();
        !a.is_empty() && (host == a || host.ends_with(&format!(".{a}")))
    });
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "host not in dandelion.registry.skillAllowedHosts: {host}"
        )))
    }
}

fn client(settings: &Settings) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(
            settings.http_connect_timeout_ms.max(1),
        ))
        .timeout(Duration::from_millis(settings.registry_timeout_ms.max(1)))
        .user_agent(format!("Dandelion/{VERSION}"))
        .build()
        .map_err(|err| Error::Invalid(format!("registry http client: {err}")))
}

async fn get_text(url: &str, settings: &Settings) -> Result<String> {
    require_secure_url(url)?;
    let resp = client(settings)?
        .get(url)
        .send()
        .await
        .map_err(|err| Error::Invalid(format!("registry request failed: {err}")))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|err| Error::Invalid(format!("registry body failed: {err}")))?;
    if !status.is_success() {
        return Err(Error::Invalid(format!("registry HTTP {status}")));
    }
    Ok(body)
}

async fn get_json(url: &str, settings: &Settings) -> Result<serde_json::Value> {
    let body = get_text(url, settings).await?;
    serde_json::from_str(&body)
        .map_err(|err| Error::Invalid(format!("registry returned invalid JSON: {err}")))
}

/// Minimal absolute-URL split. The registry and skill sources only need scheme
/// and host, so this avoids pulling in a URL crate for two fields.
pub mod url_parts {
    pub struct Parts {
        pub scheme: String,
        pub host: String,
    }

    pub fn split(raw: &str) -> Option<Parts> {
        let raw = raw.trim();
        let (scheme, rest) = raw.split_once("://")?;
        if scheme.is_empty() || rest.is_empty() {
            return None;
        }
        let authority = rest
            .split(['/', '?', '#'])
            .next()
            .filter(|s| !s.is_empty())?;
        // Strip userinfo and port: `user:pw@host:443` -> `host`.
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let host = match host.strip_prefix('[') {
            Some(v6) => v6.split_once(']').map(|(h, _)| h).unwrap_or(v6),
            None => host.split(':').next().unwrap_or(host),
        };
        if host.is_empty() {
            return None;
        }
        Some(Parts {
            scheme: scheme.to_ascii_lowercase(),
            host: host.to_string(),
        })
    }
}

fn encode_query(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for b in raw.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_with(hosts: Vec<String>) -> Settings {
        let mut s = Settings::defaults();
        s.registry_skill_allowed_hosts = hosts;
        s
    }

    #[test]
    fn https_required_except_loopback() {
        assert!(require_secure_url("https://github.com/a/b").is_ok());
        assert!(require_secure_url("http://github.com/a/b").is_err());
        assert!(require_secure_url("file:///etc/passwd").is_err());
        assert!(require_secure_url("github.com/a/b").is_err());
        // A local mirror or test mock never leaves the machine.
        assert!(require_secure_url("http://127.0.0.1:8080/v0/servers").is_ok());
        assert!(require_secure_url("http://localhost:8080/v0/servers").is_ok());
    }

    #[test]
    fn empty_allowlist_denies() {
        let s = settings_with(Vec::new());
        assert!(require_allowed_host("https://github.com/a/b", &s).is_err());
    }

    #[test]
    fn allowlist_matches_host_and_subdomain() {
        let s = settings_with(vec!["github.com".into()]);
        assert!(require_allowed_host("https://github.com/a/b", &s).is_ok());
        assert!(require_allowed_host("https://raw.github.com/a/b", &s).is_ok());
        assert!(require_allowed_host("https://evil.com/a/b", &s).is_err());
        // A suffix that is not a dotted boundary must not pass.
        assert!(require_allowed_host("https://notgithub.com/a/b", &s).is_err());
    }

    #[test]
    fn host_parsed_without_userinfo_or_port() {
        let p = url_parts::split("https://user:pw@github.com:443/a").unwrap();
        assert_eq!(p.host, "github.com");
        let s = settings_with(vec!["github.com".into()]);
        assert!(require_allowed_host("https://user:pw@github.com:443/a", &s).is_ok());
        // userinfo must not be able to spoof an allowed host
        assert!(require_allowed_host("https://github.com@evil.com/a", &s).is_err());
    }

    #[test]
    fn query_encoded() {
        assert_eq!(encode_query("a b/c&d"), "a%20b%2Fc%26d");
        assert_eq!(encode_query("io.github.o/s"), "io.github.o%2Fs");
    }
}
