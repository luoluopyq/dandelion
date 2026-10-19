//! Seal `dandelion.apiKey` at the settings.json write boundary, plus the H3
//! [`CredentialStore`] for MCP secrets that must never enter settings.json,
//! project files, trace or logs.
//!
//! Memory always holds plaintext for Provider HTTP. On disk:
//! - Windows: `enc:dpapi:v1:` + DPAPI blob (per-user; copying the JSON does not copy the key)
//! - elsewhere: JSON value `enc:file`, plaintext in `{data_dir}/secrets/api-key` (0600 on Unix)
//!
//! H3: `FileCredentialStore` keeps MCP secrets in `{data_dir}/secrets/` under
//! `mcp__<server>__<name>` (DPAPI-encrypted payload on Windows, 0600 file
//! elsewhere). `McpServerConfig` values reference them as `secret:<name>`;
//! resolution happens at connect time only. OAuth/device-flow will plug in
//! behind the same [`CredentialStore`] trait once a cross-platform
//! implementation is validated (roadmap: `docs/harness-parity-rd-plan.md` §H3).

use crate::{Error, Result};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

const KEY_API_KEY: &str = "dandelion.apiKey";
const DPAPI_PREFIX: &str = "enc:dpapi:v1:";
const FILE_TOKEN: &str = "enc:file";
#[cfg(windows)]
const ENTROPY: &[u8] = b"dandelion.apiKey";

/// Reference prefix for MCP config values that point into the
/// [`CredentialStore`] instead of embedding the secret: `"secret:<name>"`.
pub const MCP_SECRET_REF: &str = "secret:";

/// Pluggable secret storage. `FileCredentialStore` is the shipped
/// implementation; a future validated OS-keychain / OAuth store can implement
/// the same trait without touching MCP connection code.
pub trait CredentialStore: Send + Sync {
    fn put(&self, name: &str, secret: &str) -> Result<()>;
    fn get(&self, name: &str) -> Result<Option<String>>;
    fn delete(&self, name: &str) -> Result<()>;
    fn list(&self) -> Vec<String>;
}

/// Secrets live under `{data_dir}/secrets/`, outside every project checkout
/// and outside settings.json. Names are sanitized to `[A-Za-z0-9._-]+`.
pub struct FileCredentialStore {
    data_dir: PathBuf,
}

impl FileCredentialStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
        }
    }

    fn path(&self, name: &str) -> Result<PathBuf> {
        Ok(self
            .data_dir
            .join("secrets")
            .join(sanitize_secret_name(name)?))
    }
}

impl CredentialStore for FileCredentialStore {
    fn put(&self, name: &str, secret: &str) -> Result<()> {
        let path = self.path(name)?;
        // Credentials read from stdin/env can carry stray whitespace that
        // silently breaks an auth header; store the trimmed value so `get`
        // returns exactly the secret on every platform (DPAPI included).
        let secret = secret.trim();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        #[cfg(windows)]
        {
            let blob = dpapi_crypt(secret.as_bytes(), true, mcp_entropy(name))?;
            let body = format!("{DPAPI_PREFIX}{}", crate::acp::encode_base64(&blob));
            std::fs::write(&path, body.as_bytes())?;
        }
        #[cfg(not(windows))]
        {
            std::fs::write(&path, secret.as_bytes())?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path)?.permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(&path, perms)?;
        }
        Ok(())
    }

    fn get(&self, name: &str) -> Result<Option<String>> {
        let path = self.path(name)?;
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(Error::Settings(format!(
                    "mcp secret {name} unreadable: {err}"
                )))
            }
        };
        if let Some(rest) = raw.strip_prefix(DPAPI_PREFIX) {
            #[cfg(windows)]
            {
                use crate::acp::decode_base64;
                let blob = decode_base64(rest.trim_end()).ok_or_else(|| {
                    Error::Settings(format!("mcp secret {name} is not valid base64"))
                })?;
                let bytes = dpapi_crypt(&blob, false, mcp_entropy(name))?;
                return String::from_utf8(bytes)
                    .map(Some)
                    .map_err(|_| Error::Settings(format!("mcp secret {name} is not UTF-8")));
            }
            #[cfg(not(windows))]
            {
                let _ = rest;
                return Err(Error::Settings(format!(
                    "mcp secret {name} is Windows DPAPI ciphertext; this host cannot decrypt it"
                )));
            }
        }
        Ok(Some(raw.trim_end_matches(['\r', '\n']).to_string()))
    }

    fn delete(&self, name: &str) -> Result<()> {
        let path = self.path(name)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(Error::Settings(format!("mcp secret {name}: {err}"))),
        }
    }

    fn list(&self) -> Vec<String> {
        let dir = self.data_dir.join("secrets");
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if entry
                    .file_name()
                    .to_str()
                    .is_some_and(|n| n != "api-key" && !n.starts_with('.'))
                {
                    out.push(name.to_string());
                }
            }
        }
        out.sort();
        out
    }
}

#[cfg(windows)]
fn mcp_entropy(name: &str) -> Vec<u8> {
    format!("dandelion.mcp.{name}").into_bytes()
}

/// Reject traversal and separators: secrets are addressed by flat names only.
pub fn sanitize_secret_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.len() > 128
        || !trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(Error::Settings(format!(
            "invalid secret name {name:?} (letters, digits, '.', '_', '-' only)"
        )));
    }
    Ok(trimmed.to_string())
}

/// Standard secret name for one MCP server variable: `mcp__<server>__<name>`.
pub fn mcp_secret_name(server_id: &str, var: &str) -> Result<String> {
    Ok(format!(
        "mcp__{}__{}",
        sanitize_secret_name(server_id)?,
        sanitize_secret_name(var)?
    ))
}

pub fn api_key_secret_path(data_dir: &Path) -> PathBuf {
    data_dir.join("secrets").join("api-key")
}

pub fn unseal_api_key(settings: &mut crate::config::Settings, data_dir: &Path) -> Result<()> {
    if let Some(raw) = settings.api_key.take() {
        if raw.is_empty() {
            settings.api_key = None;
        } else {
            settings.api_key = Some(unseal_value(&raw, data_dir)?);
        }
    }
    Ok(())
}

pub fn seal_api_key_in_map(
    map: &mut Map<String, Value>,
    encrypt: bool,
    data_dir: &Path,
) -> Result<()> {
    let raw = match map.get(KEY_API_KEY) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Null) | None => None,
        Some(Value::String(_)) => None,
        Some(_) => return Err(Error::Settings(format!("{KEY_API_KEY} must be a string"))),
    };
    match raw {
        None => {
            map.remove(KEY_API_KEY);
            clear_file_secret(data_dir);
            Ok(())
        }
        Some(s) => {
            let plain = unseal_value(&s, data_dir)?;
            if encrypt {
                let sealed = seal_value(&plain, data_dir)?;
                map.insert(KEY_API_KEY.into(), Value::String(sealed));
            } else {
                map.insert(KEY_API_KEY.into(), Value::String(plain));
                clear_file_secret(data_dir);
            }
            Ok(())
        }
    }
}

fn unseal_value(raw: &str, data_dir: &Path) -> Result<String> {
    if let Some(rest) = raw.strip_prefix(DPAPI_PREFIX) {
        #[cfg(windows)]
        {
            use crate::acp::decode_base64;
            let blob = decode_base64(rest).ok_or_else(|| {
                Error::Settings(format!("{KEY_API_KEY} DPAPI blob is not valid base64"))
            })?;
            let bytes = dpapi_unprotect(&blob)?;
            String::from_utf8(bytes)
                .map_err(|_| Error::Settings(format!("{KEY_API_KEY} DPAPI plaintext is not UTF-8")))
        }
        #[cfg(not(windows))]
        {
            let _ = rest;
            let _ = data_dir;
            Err(Error::Settings(format!(
                "{KEY_API_KEY} is Windows DPAPI ciphertext; this host cannot decrypt it"
            )))
        }
    } else if raw == FILE_TOKEN {
        let path = api_key_secret_path(data_dir);
        let text = std::fs::read_to_string(&path).map_err(|err| {
            Error::Settings(format!(
                "{KEY_API_KEY} is enc:file but {} is unreadable: {err}",
                path.display()
            ))
        })?;
        Ok(text.trim_end_matches(['\r', '\n']).to_string())
    } else {
        Ok(raw.to_string())
    }
}

fn seal_value(plain: &str, data_dir: &Path) -> Result<String> {
    #[cfg(windows)]
    {
        clear_file_secret(data_dir);
        let blob = dpapi_protect(plain.as_bytes())?;
        Ok(format!(
            "{DPAPI_PREFIX}{}",
            crate::acp::encode_base64(&blob)
        ))
    }
    #[cfg(not(windows))]
    {
        write_file_secret(data_dir, plain)?;
        Ok(FILE_TOKEN.to_string())
    }
}

#[cfg(not(windows))]
fn write_file_secret(data_dir: &Path, plain: &str) -> Result<()> {
    let path = api_key_secret_path(data_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, plain.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path)?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&path, perms)?;
    }
    Ok(())
}

fn clear_file_secret(data_dir: &Path) {
    let path = api_key_secret_path(data_dir);
    let _ = std::fs::remove_file(&path);
}

#[cfg(windows)]
fn dpapi_protect(plain: &[u8]) -> Result<Vec<u8>> {
    dpapi_crypt(plain, true, ENTROPY.to_vec())
}

#[cfg(windows)]
fn dpapi_unprotect(blob: &[u8]) -> Result<Vec<u8>> {
    dpapi_crypt(blob, false, ENTROPY.to_vec())
}

#[cfg(windows)]
fn dpapi_crypt(input: &[u8], protect: bool, entropy_in: Vec<u8>) -> Result<Vec<u8>> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let mut data = input.to_vec();
    let mut entropy = entropy_in;
    let blob_in = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_mut_ptr(),
    };
    let entropy_blob = CRYPT_INTEGER_BLOB {
        cbData: entropy.len() as u32,
        pbData: entropy.as_mut_ptr(),
    };
    let mut blob_out = CRYPT_INTEGER_BLOB::default();
    unsafe {
        let result = if protect {
            CryptProtectData(
                &blob_in,
                PCWSTR::null(),
                Some(&entropy_blob),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut blob_out,
            )
        } else {
            CryptUnprotectData(
                &blob_in,
                None,
                Some(&entropy_blob),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut blob_out,
            )
        };
        result.map_err(|err| {
            Error::Settings(format!(
                "{KEY_API_KEY} DPAPI {} failed: {err}",
                if protect { "protect" } else { "unprotect" }
            ))
        })?;
        if blob_out.pbData.is_null() {
            return Err(Error::Settings(format!(
                "{KEY_API_KEY} DPAPI returned an empty blob"
            )));
        }
        let slice = std::slice::from_raw_parts(blob_out.pbData, blob_out.cbData as usize);
        let out = slice.to_vec();
        let _ = LocalFree(HLOCAL(blob_out.pbData.cast()));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_store_roundtrip_and_list() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(dir.path());
        store
            .put(&mcp_secret_name("linear", "API_KEY").unwrap(), "sk-test-1")
            .unwrap();
        store
            .put(&mcp_secret_name("github", "TOKEN").unwrap(), " ghp_xxx ")
            .unwrap();
        assert_eq!(
            store
                .get(&mcp_secret_name("linear", "API_KEY").unwrap())
                .unwrap()
                .as_deref(),
            Some("sk-test-1")
        );
        assert_eq!(
            store
                .get(&mcp_secret_name("github", "TOKEN").unwrap())
                .unwrap()
                .as_deref(),
            Some("ghp_xxx")
        );
        let names = store.list();
        assert_eq!(names.len(), 2, "{names:?}");
        store
            .delete(&mcp_secret_name("linear", "API_KEY").unwrap())
            .unwrap();
        assert!(store
            .get(&mcp_secret_name("linear", "API_KEY").unwrap())
            .unwrap()
            .is_none());
        assert_eq!(store.list().len(), 1);
    }

    #[test]
    fn api_key_sealing_still_works_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = crate::config::Settings::defaults();
        settings.api_key = Some("plain-key".into());
        settings.encrypt_api_key = true;
        settings.save(dir.path()).unwrap();
        // The API key must not appear verbatim in the settings file.
        let raw = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
        assert!(!raw.contains("plain-key"), "plaintext key on disk");
        // Load path unseals back to plaintext memory.
        let loaded = crate::config::Settings::load(dir.path(), &Default::default()).unwrap();
        assert_eq!(loaded.api_key.as_deref(), Some("plain-key"));
    }

    #[test]
    fn secret_names_reject_traversal() {
        assert!(sanitize_secret_name("../evil").is_err());
        assert!(sanitize_secret_name("a/b").is_err());
        assert!(sanitize_secret_name("").is_err());
        assert!(mcp_secret_name("ok.server-1", "API_KEY").is_ok());
    }
}
