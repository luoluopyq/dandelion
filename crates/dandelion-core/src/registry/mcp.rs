//! Official MCP registry -> `dandelion.mcp.servers`.
//!
//! Read-only and unauthenticated. The response envelope is
//! `{ servers: [{ server, _meta }], metadata: { nextCursor, count } }`; a
//! `server` carries `name` / `title` / `description` / `version` plus
//! `packages[]` and/or `remotes[]`.
//!
//! Secrets are never written into `settings.json`: a required or secret value
//! becomes an `McpEnvNeed` for the host to ask about, and the installed row
//! stays `enabled=false` until it is supplied. A server that is enabled but
//! missing its key would look connected while exposing almost no tools, which
//! is the exact failure `docs/g2-extensibility.md` §7.7 warns against.

use super::{encode_query, get_json};
use crate::config::{McpServerConfig, Settings};
use crate::mcp::BUILTIN_TOOL_NAMES;
use crate::{Error, Result};
use serde_json::Value;

/// How a registry entry can be installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum McpVariantKind {
    /// Remote Streamable HTTP endpoint (`remotes[]`).
    Remote,
    /// Local package launched as a stdio child (`packages[]`).
    Package,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpVariant {
    pub kind: McpVariantKind,
    /// `streamable-http` / `sse` for remote; the registry type for packages.
    pub label: String,
    /// Remote URL, or the package identifier.
    pub target: String,
    /// True when this shape can be turned into a server config as-is.
    pub supported: bool,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpRegistryEntry {
    pub name: String,
    pub title: String,
    pub description: String,
    pub version: String,
    pub repository_url: Option<String>,
    /// Suggested `dandelion.mcp.servers` id derived from `name`.
    pub suggested_id: String,
    pub is_latest: bool,
    pub variants: Vec<McpVariant>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpRegistryPage {
    pub entries: Vec<McpRegistryEntry>,
    pub next_cursor: Option<String>,
}

/// An environment variable the user still has to provide.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpEnvNeed {
    pub name: String,
    pub description: String,
    pub secret: bool,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpInstallPlan {
    pub server: McpServerConfig,
    /// Non-empty means the server was installed disabled.
    pub missing_env: Vec<McpEnvNeed>,
    pub notes: Vec<String>,
}

pub async fn search(
    query: &str,
    cursor: Option<&str>,
    settings: &Settings,
) -> Result<McpRegistryPage> {
    let body = fetch(query, cursor, settings).await?;
    Ok(parse_page(&body))
}

async fn fetch(query: &str, cursor: Option<&str>, settings: &Settings) -> Result<Value> {
    let base = settings.registry_mcp_url.trim();
    let sep = if base.contains('?') { '&' } else { '?' };
    let mut url = format!("{base}{sep}limit={}", settings.registry_max_results);
    let q = query.trim();
    if !q.is_empty() {
        url.push_str(&format!("&search={}", encode_query(q)));
    }
    if let Some(c) = cursor.map(str::trim).filter(|c| !c.is_empty()) {
        url.push_str(&format!("&cursor={}", encode_query(c)));
    }
    get_json(&url, settings).await
}

/// Resolve one entry by exact registry name, returning the **raw** registry
/// document so the mapping below sees the full `packages`/`remotes` detail.
/// Search is used rather than a by-name path so this keeps working across the
/// registry's `/v0` -> `/v0.1` endpoint shapes.
pub async fn resolve_raw(name: &str, version: Option<&str>, settings: &Settings) -> Result<Value> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::Invalid("mcp registry name must not be empty".into()));
    }
    let body = fetch(name, None, settings).await?;
    let empty = Vec::new();
    let items = body
        .get("servers")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);
    let mut matches: Vec<&Value> = items
        .iter()
        .filter(|item| parse_entry(item).is_some_and(|e| e.name.eq_ignore_ascii_case(name)))
        .collect();
    if matches.is_empty() {
        return Err(Error::Invalid(format!(
            "not found in the MCP registry: {name}"
        )));
    }
    if let Some(want) = version.map(str::trim).filter(|v| !v.is_empty()) {
        return matches
            .into_iter()
            .find(|item| parse_entry(item).is_some_and(|e| e.version == want))
            .cloned()
            .ok_or_else(|| Error::Invalid(format!("{name} has no version {want}")));
    }
    // Prefer the registry's own `isLatest`; fall back to the highest version.
    if let Some(item) = matches
        .iter()
        .find(|item| parse_entry(item).is_some_and(|e| e.is_latest))
    {
        return Ok((*item).clone());
    }
    matches.sort_by_key(|item| {
        parse_entry(item)
            .map(|e| version_key(&e.version))
            .unwrap_or_default()
    });
    Ok((*matches.last().expect("non-empty")).clone())
}

fn version_key(raw: &str) -> Vec<u64> {
    raw.split(['.', '-', '+'])
        .map(|p| p.parse::<u64>().unwrap_or(0))
        .collect()
}

fn parse_page(body: &Value) -> McpRegistryPage {
    let entries = body
        .get("servers")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(parse_entry).collect())
        .unwrap_or_default();
    let next_cursor = body
        .get("metadata")
        .and_then(|m| m.get("nextCursor"))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());
    McpRegistryPage {
        entries,
        next_cursor,
    }
}

fn parse_entry(item: &Value) -> Option<McpRegistryEntry> {
    // Entries are `{ server, _meta }`, but tolerate a bare server object.
    let server = item.get("server").unwrap_or(item);
    let name = server.get("name")?.as_str()?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let str_at = |k: &str| {
        server
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let is_latest = item
        .get("_meta")
        .and_then(|m| m.get("io.modelcontextprotocol.registry/official"))
        .and_then(|o| o.get("isLatest"))
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    let mut variants = Vec::new();
    for r in server
        .get("remotes")
        .and_then(|v| v.as_array())
        .unwrap_or(&Vec::new())
    {
        let label = r
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let target = r
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let supported = !target.is_empty() && remote_transport(&label).is_some();
        variants.push(McpVariant {
            kind: McpVariantKind::Remote,
            label,
            target,
            supported,
        });
    }
    for p in server
        .get("packages")
        .and_then(|v| v.as_array())
        .unwrap_or(&Vec::new())
    {
        let label = p
            .get("registryType")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let target = p
            .get("identifier")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let hint = p.get("runtimeHint").and_then(|v| v.as_str()).unwrap_or("");
        variants.push(McpVariant {
            kind: McpVariantKind::Package,
            label: label.clone(),
            target: target.clone(),
            supported: !target.is_empty() && runtime_for(hint, &label).is_some(),
        });
    }
    Some(McpRegistryEntry {
        title: {
            let t = str_at("title");
            if t.is_empty() {
                name.clone()
            } else {
                t
            }
        },
        description: str_at("description"),
        version: str_at("version"),
        repository_url: server
            .get("repository")
            .and_then(|r| r.get("url"))
            .and_then(|u| u.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty()),
        suggested_id: derive_id(&name),
        is_latest,
        name,
        variants,
    })
}

fn remote_transport(kind: &str) -> Option<&'static str> {
    match kind.trim().to_ascii_lowercase().as_str() {
        "streamable-http" | "http" => Some("http"),
        "sse" => Some("sse"),
        _ => None,
    }
}

/// stdio launcher for a package, by `runtimeHint` then `registryType`.
/// Unknown types are refused rather than guessed at.
fn runtime_for(hint: &str, registry_type: &str) -> Option<(&'static str, Vec<String>)> {
    let hint = hint.trim().to_ascii_lowercase();
    let by_hint = match hint.as_str() {
        "npx" => Some(("npx", vec!["-y".to_string()])),
        "uvx" => Some(("uvx", Vec::new())),
        "dnx" => Some(("dnx", Vec::new())),
        "docker" => Some(("docker", vec!["run".into(), "-i".into(), "--rm".into()])),
        _ => None,
    };
    if by_hint.is_some() {
        return by_hint;
    }
    match registry_type.trim().to_ascii_lowercase().as_str() {
        "npm" => Some(("npx", vec!["-y".to_string()])),
        "pypi" => Some(("uvx", Vec::new())),
        "nuget" => Some(("dnx", Vec::new())),
        "oci" => Some(("docker", vec!["run".into(), "-i".into(), "--rm".into()])),
        _ => None,
    }
}

/// `io.github.owner/server` -> `io_github_owner_server`, matching the character
/// set MCP tool names already allow so `mcp__{id}__{tool}` stays clean.
pub fn derive_id(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "mcp_server".into()
    } else {
        trimmed
    }
}

/// Pick an id that does not collide with an existing server or a builtin tool
/// name. Builtins always win (`docs/g2-extensibility.md` §7.4).
fn unique_id(base: &str, existing: &[McpServerConfig]) -> Result<String> {
    if BUILTIN_TOOL_NAMES.contains(&base) {
        return Err(Error::Invalid(format!(
            "refusing id {base}: it is a builtin tool name"
        )));
    }
    if !existing.iter().any(|s| s.id == base) {
        return Ok(base.to_string());
    }
    for n in 2..100 {
        let candidate = format!("{base}-{n}");
        if !existing.iter().any(|s| s.id == candidate) {
            return Ok(candidate);
        }
    }
    Err(Error::Invalid(format!(
        "too many servers already named like {base}"
    )))
}

/// Turn a resolved entry into a server row. Pure so the mapping rules are unit
/// testable without a network or a Runtime.
pub fn plan_from_entry(
    raw: &Value,
    existing: &[McpServerConfig],
    settings: &Settings,
) -> Result<McpInstallPlan> {
    let entry =
        parse_entry(raw).ok_or_else(|| Error::Invalid("registry entry has no name".into()))?;
    let server_obj = raw.get("server").unwrap_or(raw);
    let id = unique_id(&entry.suggested_id, existing)?;
    let mut notes = Vec::new();
    let mut missing_env = Vec::new();

    // Remote first: nothing to install locally, so it is the cheaper shape.
    if let Some(remote) = server_obj
        .get("remotes")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter().find(|r| {
                r.get("url")
                    .and_then(|u| u.as_str())
                    .is_some_and(|u| !u.is_empty())
                    && remote_transport(r.get("type").and_then(|t| t.as_str()).unwrap_or(""))
                        .is_some()
            })
        })
    {
        let url = remote
            .get("url")
            .and_then(|u| u.as_str())
            .unwrap_or_default()
            .to_string();
        let transport = remote_transport(remote.get("type").and_then(|t| t.as_str()).unwrap_or(""))
            .unwrap_or("http");
        let mut headers = std::collections::HashMap::new();
        let mut bearer_token_env_var = None;
        for h in remote
            .get("headers")
            .and_then(|v| v.as_array())
            .unwrap_or(&Vec::new())
        {
            let Some(hname) = h.get("name").and_then(|v| v.as_str()).map(str::trim) else {
                continue;
            };
            if hname.is_empty() {
                continue;
            }
            let secret = h.get("isSecret").and_then(|v| v.as_bool()).unwrap_or(false);
            let required = h
                .get("isRequired")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let desc = h
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if secret || required {
                let env_name = env_name_for(&id, hname);
                if hname.eq_ignore_ascii_case("authorization") {
                    bearer_token_env_var = Some(env_name.clone());
                } else {
                    headers.insert(hname.to_string(), format!("${{env:{env_name}}}"));
                }
                missing_env.push(McpEnvNeed {
                    name: env_name,
                    description: if desc.is_empty() {
                        format!("value for header {hname}")
                    } else {
                        desc
                    },
                    secret,
                });
                continue;
            }
            if let Some(default) = h.get("default").and_then(|v| v.as_str()) {
                headers.insert(hname.to_string(), default.to_string());
            }
        }
        if let Some(vars) = remote.get("variables").and_then(|v| v.as_object()) {
            if !vars.is_empty() {
                notes.push(format!(
                    "url has template variables ({}); edit the url after install",
                    vars.keys().cloned().collect::<Vec<_>>().join(", ")
                ));
            }
        }
        let server = McpServerConfig {
            id,
            enabled: missing_env.is_empty(),
            url: Some(url),
            headers,
            bearer_token_env_var,
            transport: transport.into(),
            ..Default::default()
        };
        return finish(server, missing_env, notes, settings);
    }

    // Package fallback: launch it as a stdio child.
    let pkg = server_obj
        .get("packages")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter().find(|p| {
                let hint = p.get("runtimeHint").and_then(|v| v.as_str()).unwrap_or("");
                let ty = p.get("registryType").and_then(|v| v.as_str()).unwrap_or("");
                p.get("identifier")
                    .and_then(|i| i.as_str())
                    .is_some_and(|i| !i.is_empty())
                    && runtime_for(hint, ty).is_some()
            })
        })
        .ok_or_else(|| {
            Error::Invalid(format!(
                "{}: no supported remote or package form (only streamable-http/sse remotes and npm/pypi/nuget/oci packages are supported)",
                entry.name
            ))
        })?;

    let identifier = pkg
        .get("identifier")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let registry_type = pkg
        .get("registryType")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let hint = pkg
        .get("runtimeHint")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let (command, mut args) = runtime_for(hint, registry_type)
        .ok_or_else(|| Error::Invalid(format!("unsupported package type: {registry_type}")))?;
    let pkg_version = pkg.get("version").and_then(|v| v.as_str()).unwrap_or("");
    // npm pins the version in the spec itself; oci uses the image tag as given.
    let spec = if registry_type.eq_ignore_ascii_case("npm") && !pkg_version.is_empty() {
        format!("{identifier}@{pkg_version}")
    } else {
        identifier.to_string()
    };
    args.push(spec);
    for a in pkg
        .get("runtimeArguments")
        .and_then(|v| v.as_array())
        .unwrap_or(&Vec::new())
    {
        // Runtime arguments precede the package spec; keep the literal ones we
        // can place unambiguously and let the user edit the rest.
        if let Some(v) = literal_arg(a) {
            args.insert(args.len().saturating_sub(1), v);
        } else {
            notes.push("entry declares runtime arguments that need manual review".into());
        }
    }
    for a in pkg
        .get("packageArguments")
        .and_then(|v| v.as_array())
        .unwrap_or(&Vec::new())
    {
        match literal_arg(a) {
            Some(v) => args.push(v),
            None => notes.push("entry declares package arguments that need manual review".into()),
        }
    }

    let mut env = std::collections::HashMap::new();
    for e in pkg
        .get("environmentVariables")
        .and_then(|v| v.as_array())
        .unwrap_or(&Vec::new())
    {
        let Some(name) = e.get("name").and_then(|v| v.as_str()).map(str::trim) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let secret = e.get("isSecret").and_then(|v| v.as_bool()).unwrap_or(false);
        let required = e
            .get("isRequired")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if secret || required {
            missing_env.push(McpEnvNeed {
                name: name.to_string(),
                description: e
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                secret,
            });
            continue;
        }
        if let Some(default) = e.get("default").and_then(|v| v.as_str()) {
            env.insert(name.to_string(), default.to_string());
        }
    }

    let server = McpServerConfig {
        id,
        enabled: missing_env.is_empty(),
        command: command.into(),
        args,
        env,
        ..Default::default()
    };
    finish(server, missing_env, notes, settings)
}

fn finish(
    server: McpServerConfig,
    missing_env: Vec<McpEnvNeed>,
    mut notes: Vec<String>,
    _settings: &Settings,
) -> Result<McpInstallPlan> {
    if !missing_env.is_empty() {
        notes.push(format!(
            "installed disabled: set {} then enable it",
            missing_env
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(McpInstallPlan {
        server,
        missing_env,
        notes,
    })
}

/// A fixed argument value with no placeholders. Anything templated is left to
/// the user rather than shipped half-substituted.
fn literal_arg(a: &Value) -> Option<String> {
    if a.get("variables").is_some() {
        return None;
    }
    let value = a
        .get("value")
        .and_then(|v| v.as_str())
        .or_else(|| a.get("default").and_then(|v| v.as_str()))
        .unwrap_or("");
    if value.contains('{') {
        return None;
    }
    let kind = a.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if kind == "named" {
        let name = a.get("name").and_then(|v| v.as_str()).unwrap_or("").trim();
        if name.is_empty() {
            return None;
        }
        return Some(if value.is_empty() {
            name.to_string()
        } else {
            format!("{name}={value}")
        });
    }
    if value.is_empty() {
        return None;
    }
    Some(value.to_string())
}

/// Env var name for a header-derived secret, e.g. `X-API-Key` on server
/// `acme_docs` -> `ACME_DOCS_X_API_KEY`.
fn env_name_for(id: &str, header: &str) -> String {
    let mut out = String::new();
    for ch in format!("{id}_{header}").chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_uppercase());
        } else {
            out.push('_');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn derive_id_sanitizes_registry_name() {
        assert_eq!(
            derive_id("io.github.owner/server"),
            "io_github_owner_server"
        );
        assert_eq!(
            derive_id("ai.smithery/Smithery-AI"),
            "ai_smithery_smithery-ai"
        );
        assert_eq!(derive_id("///"), "mcp_server");
    }

    #[test]
    fn page_parses_servers_and_cursor() {
        let body = json!({
            "servers": [{
                "server": {
                    "name": "ac.inference.sh/mcp",
                    "title": "inference.sh",
                    "description": "run apps",
                    "version": "1.0.1",
                    "remotes": [{"type": "streamable-http", "url": "https://api.inference.sh/mcp"}]
                },
                "_meta": {"io.modelcontextprotocol.registry/official": {"isLatest": true}}
            }],
            "metadata": {"nextCursor": "ac.inference.sh/mcp:1.0.1", "count": 1}
        });
        let page = parse_page(&body);
        assert_eq!(page.entries.len(), 1);
        let e = &page.entries[0];
        assert_eq!(e.name, "ac.inference.sh/mcp");
        assert_eq!(e.suggested_id, "ac_inference_sh_mcp");
        assert!(e.is_latest);
        assert_eq!(e.variants.len(), 1);
        assert!(e.variants[0].supported);
        assert_eq!(
            page.next_cursor.as_deref(),
            Some("ac.inference.sh/mcp:1.0.1")
        );
    }

    #[test]
    fn empty_body_is_empty_page() {
        let page = parse_page(&json!({}));
        assert!(page.entries.is_empty());
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn npm_package_becomes_npx() {
        let raw = json!({"server": {
            "name": "io.github.acme/files",
            "version": "2.0.0",
            "packages": [{
                "registryType": "npm",
                "identifier": "@acme/files-mcp",
                "version": "2.0.0",
                "transport": {"type": "stdio"}
            }]
        }});
        let plan = plan_from_entry(&raw, &[], &Settings::defaults()).unwrap();
        assert_eq!(plan.server.command, "npx");
        assert_eq!(plan.server.args, vec!["-y", "@acme/files-mcp@2.0.0"]);
        assert_eq!(plan.server.id, "io_github_acme_files");
        assert!(plan.server.enabled, "no secrets needed");
        assert!(plan.server.url.is_none());
    }

    #[test]
    fn pypi_and_oci_map_to_uvx_and_docker() {
        let pypi = json!({"server": {"name": "a/b", "packages": [
            {"registryType": "pypi", "identifier": "some-mcp"}
        ]}});
        assert_eq!(
            plan_from_entry(&pypi, &[], &Settings::defaults())
                .unwrap()
                .server
                .command,
            "uvx"
        );
        let oci = json!({"server": {"name": "a/c", "packages": [
            {"registryType": "oci", "identifier": "ghcr.io/x/y:1"}
        ]}});
        let plan = plan_from_entry(&oci, &[], &Settings::defaults()).unwrap();
        assert_eq!(plan.server.command, "docker");
        assert_eq!(plan.server.args, vec!["run", "-i", "--rm", "ghcr.io/x/y:1"]);
    }

    #[test]
    fn remote_preferred_over_package() {
        let raw = json!({"server": {
            "name": "a/b",
            "remotes": [{"type": "streamable-http", "url": "https://x/mcp"}],
            "packages": [{"registryType": "npm", "identifier": "p"}]
        }});
        let plan = plan_from_entry(&raw, &[], &Settings::defaults()).unwrap();
        assert_eq!(plan.server.url.as_deref(), Some("https://x/mcp"));
        assert_eq!(plan.server.transport, "http");
        assert!(plan.server.command.is_empty());
    }

    #[test]
    fn secret_env_lands_disabled_and_is_not_stored() {
        let raw = json!({"server": {
            "name": "a/b",
            "packages": [{
                "registryType": "npm",
                "identifier": "p",
                "environmentVariables": [
                    {"name": "ACME_TOKEN", "isSecret": true, "isRequired": true,
                     "description": "api token", "default": "leaked-should-not-persist"},
                    {"name": "ACME_REGION", "default": "us-east-1"}
                ]
            }]
        }});
        let plan = plan_from_entry(&raw, &[], &Settings::defaults()).unwrap();
        assert!(!plan.server.enabled, "missing secret must not be enabled");
        assert_eq!(plan.missing_env.len(), 1);
        assert_eq!(plan.missing_env[0].name, "ACME_TOKEN");
        assert!(plan.missing_env[0].secret);
        assert!(!plan.server.env.contains_key("ACME_TOKEN"));
        let json = serde_json::to_string(&plan.server).unwrap();
        assert!(
            !json.contains("leaked-should-not-persist"),
            "secret default must never reach settings.json: {json}"
        );
        // A non-secret default is still convenient to keep.
        assert_eq!(
            plan.server.env.get("ACME_REGION").map(String::as_str),
            Some("us-east-1")
        );
    }

    #[test]
    fn secret_header_becomes_env_placeholder() {
        let raw = json!({"server": {
            "name": "acme/docs",
            "remotes": [{
                "type": "streamable-http",
                "url": "https://x/mcp",
                "headers": [
                    {"name": "X-API-Key", "isSecret": true, "isRequired": true},
                    {"name": "X-Region", "default": "eu"}
                ]
            }]
        }});
        let plan = plan_from_entry(&raw, &[], &Settings::defaults()).unwrap();
        assert_eq!(
            plan.server.headers.get("X-API-Key").map(String::as_str),
            Some("${env:ACME_DOCS_X_API_KEY}")
        );
        assert_eq!(
            plan.server.headers.get("X-Region").map(String::as_str),
            Some("eu")
        );
        assert!(!plan.server.enabled);
        assert_eq!(plan.missing_env[0].name, "ACME_DOCS_X_API_KEY");
    }

    #[test]
    fn authorization_header_uses_bearer_env_var() {
        let raw = json!({"server": {
            "name": "acme/docs",
            "remotes": [{
                "type": "streamable-http",
                "url": "https://x/mcp",
                "headers": [{"name": "Authorization", "isSecret": true, "isRequired": true}]
            }]
        }});
        let plan = plan_from_entry(&raw, &[], &Settings::defaults()).unwrap();
        assert_eq!(
            plan.server.bearer_token_env_var.as_deref(),
            Some("ACME_DOCS_AUTHORIZATION")
        );
        assert!(!plan.server.headers.contains_key("Authorization"));
    }

    #[test]
    fn unsupported_distribution_is_refused() {
        let raw = json!({"server": {
            "name": "a/b",
            "packages": [{"registryType": "brew", "identifier": "x"}]
        }});
        let err = plan_from_entry(&raw, &[], &Settings::defaults()).unwrap_err();
        assert!(err.to_string().contains("no supported"), "{err}");

        let none = json!({"server": {"name": "a/b", "websiteUrl": "https://x"}});
        assert!(plan_from_entry(&none, &[], &Settings::defaults()).is_err());
    }

    #[test]
    fn builtin_name_is_refused() {
        let raw = json!({"server": {
            "name": "terminal",
            "packages": [{"registryType": "npm", "identifier": "p"}]
        }});
        let err = plan_from_entry(&raw, &[], &Settings::defaults()).unwrap_err();
        assert!(err.to_string().contains("builtin"), "{err}");
    }

    #[test]
    fn id_collision_gets_suffix() {
        let existing = vec![McpServerConfig {
            id: "a_b".into(),
            ..Default::default()
        }];
        let raw = json!({"server": {
            "name": "a/b",
            "packages": [{"registryType": "npm", "identifier": "p"}]
        }});
        let plan = plan_from_entry(&raw, &existing, &Settings::defaults()).unwrap();
        assert_eq!(plan.server.id, "a_b-2");
    }

    #[test]
    fn templated_url_is_flagged() {
        let raw = json!({"server": {
            "name": "a/b",
            "remotes": [{
                "type": "streamable-http",
                "url": "https://x/mcp/{tenant_id}",
                "variables": {"tenant_id": {"isRequired": true}}
            }]
        }});
        let plan = plan_from_entry(&raw, &[], &Settings::defaults()).unwrap();
        assert!(
            plan.notes.iter().any(|n| n.contains("tenant_id")),
            "{:?}",
            plan.notes
        );
    }

    #[test]
    fn resolve_picks_latest_by_version_without_flag() {
        assert!(version_key("1.0.10") > version_key("1.0.9"));
        assert!(version_key("2.0.0") > version_key("1.9.9"));
    }
}
