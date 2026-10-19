//! C8b light cross-session memory. Frozen once per session start.

use crate::config::Settings;
use crate::context_os::{
    Authority, MemoryDomain, MemoryEventDraft, MemoryEventKind, MemoryScope, Sensitivity, SourceRef,
};
use crate::{Error, Result};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const SOUL_FILE: &str = "DANDELION.soul.md";
pub const USER_FILE: &str = "DANDELION.user.md";
pub const MEMORY_FILE: &str = "DANDELION.memory.md";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryKind {
    Soul,
    User,
    Memory,
}

impl MemoryKind {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "soul" => Ok(Self::Soul),
            "user" => Ok(Self::User),
            "memory" => Ok(Self::Memory),
            other => Err(Error::Invalid(format!(
                "memory kind must be soul|user|memory (got {other})"
            ))),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct FrozenMemory {
    /// Replaces the G1 identity sentence when `Some`.
    pub identity: Option<String>,
    /// Injected after identity (user then notes). Empty = omit.
    pub segment: String,
}

pub fn soul_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SOUL_FILE)
}

pub fn user_path(data_dir: &Path) -> PathBuf {
    data_dir.join("memories").join(USER_FILE)
}

pub fn notes_path(data_dir: &Path) -> PathBuf {
    data_dir.join("memories").join(MEMORY_FILE)
}

pub fn path_for(data_dir: &Path, kind: MemoryKind) -> PathBuf {
    match kind {
        MemoryKind::Soul => soul_path(data_dir),
        MemoryKind::User => user_path(data_dir),
        MemoryKind::Memory => notes_path(data_dir),
    }
}

pub fn freeze(data_dir: &Path, settings: &Settings) -> FrozenMemory {
    if !settings.memory_enabled {
        return FrozenMemory::default();
    }
    let soul = read_capped(&soul_path(data_dir), settings.memory_soul_max_bytes, "soul");
    let user = read_capped(&user_path(data_dir), settings.memory_user_max_bytes, "user");
    let notes = read_capped(
        &notes_path(data_dir),
        settings.memory_memory_max_bytes,
        "memory",
    );
    let mut used = 0usize;
    let identity = match soul {
        Some(s) => {
            used += s.len();
            Some(s)
        }
        None => None,
    };
    let mut parts = Vec::new();
    if let Some(s) = user {
        if used + s.len() <= settings.memory_max_inject_bytes {
            used += s.len();
            parts.push(format!("用户画像：\n{s}"));
        } else {
            tracing::warn!("memory user skipped: would exceed maxInjectBytes");
        }
    }
    if let Some(s) = notes {
        if used + s.len() <= settings.memory_max_inject_bytes {
            parts.push(format!("跨会话笔记：\n{s}"));
        } else {
            tracing::warn!("memory notes skipped: would exceed maxInjectBytes");
        }
    }
    FrozenMemory {
        identity,
        segment: parts.join("\n\n"),
    }
}

fn read_capped(path: &Path, cap: usize, label: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() > cap {
        tracing::warn!(%label, cap, "memory file skipped: over byte cap");
        return None;
    }
    Some(trimmed.to_string())
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryFiles {
    pub soul: String,
    pub user: String,
    pub memory: String,
    pub soul_path: PathBuf,
    pub user_path: PathBuf,
    pub memory_path: PathBuf,
}

fn read_or_empty(path: &Path) -> String {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.to_string())
        .unwrap_or_default()
}

pub fn files(data_dir: &Path) -> MemoryFiles {
    MemoryFiles {
        soul: read_or_empty(&soul_path(data_dir)),
        user: read_or_empty(&user_path(data_dir)),
        memory: read_or_empty(&notes_path(data_dir)),
        soul_path: soul_path(data_dir),
        user_path: user_path(data_dir),
        memory_path: notes_path(data_dir),
    }
}

/// Converts the three C8b files into immutable P1 ledger drafts.  This leaves
/// the files in place: the existing prompt path keeps working until Context OS
/// injection is explicitly enabled and audited.
pub fn legacy_ledger_drafts(data_dir: &Path, settings: &Settings) -> Vec<MemoryEventDraft> {
    if !settings.memory_enabled {
        return vec![];
    }
    [
        (MemoryKind::Soul, "soul"),
        (MemoryKind::User, "user"),
        (MemoryKind::Memory, "memory"),
    ]
    .into_iter()
    .filter_map(|(kind, label)| {
        let content = read_capped(&path_for(data_dir, kind), legacy_cap(kind, settings), label)?;
        let mut digest = Sha256::new();
        digest.update(content.as_bytes());
        let version = format!("sha256:{:x}", digest.finalize());
        Some(MemoryEventDraft {
            domain: MemoryDomain::Internal,
            scope: MemoryScope::default(),
            kind: match kind {
                MemoryKind::Memory => MemoryEventKind::Fact,
                _ => MemoryEventKind::Preference,
            },
            payload: json!({"content": content, "legacyKind": label}),
            source_ref: SourceRef {
                kind: "legacy_memory_file".into(),
                id: label.into(),
                version: Some(version),
            },
            authority: Authority::UserConfirmed,
            valid_from: None,
            valid_to: None,
            confidence: 1.0,
            sensitivity: Sensitivity::Internal,
            revision_of: None,
        })
    })
    .collect()
}

fn legacy_cap(kind: MemoryKind, settings: &Settings) -> usize {
    match kind {
        MemoryKind::Soul => settings.memory_soul_max_bytes,
        MemoryKind::User => settings.memory_user_max_bytes,
        MemoryKind::Memory => settings.memory_memory_max_bytes,
    }
}

pub fn show(data_dir: &Path) -> Result<String> {
    let mut out = String::new();
    for (label, path) in [
        ("soul", soul_path(data_dir)),
        ("user", user_path(data_dir)),
        ("memory", notes_path(data_dir)),
    ] {
        out.push_str(&format!("## {label} ({})\n", path.display()));
        match std::fs::read_to_string(&path) {
            Ok(t) if t.trim().is_empty() => out.push_str("(empty)\n\n"),
            Ok(t) => {
                out.push_str(t.trim());
                out.push_str("\n\n");
            }
            Err(_) => out.push_str("(missing)\n\n"),
        }
    }
    Ok(out)
}

pub fn set(data_dir: &Path, kind: MemoryKind, body: &str, settings: &Settings) -> Result<()> {
    let cap = match kind {
        MemoryKind::Soul => settings.memory_soul_max_bytes,
        MemoryKind::User => settings.memory_user_max_bytes,
        MemoryKind::Memory => settings.memory_memory_max_bytes,
    };
    if body.len() > cap {
        return Err(Error::Invalid(format!(
            "memory {} exceeds {} bytes",
            match kind {
                MemoryKind::Soul => "soul",
                MemoryKind::User => "user",
                MemoryKind::Memory => "memory",
            },
            cap
        )));
    }
    let path = path_for(data_dir, kind);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, body)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;

    #[test]
    fn freeze_soul_replaces_and_disable_skips() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(soul_path(dir.path()), "SOUL_UNIQUE_LINE\n").unwrap();
        let settings = Settings::defaults();
        let frozen = freeze(dir.path(), &settings);
        assert_eq!(frozen.identity.as_deref(), Some("SOUL_UNIQUE_LINE"));
        let mut off = settings.clone();
        off.memory_enabled = false;
        let frozen = freeze(dir.path(), &off);
        assert!(frozen.identity.is_none());
        assert!(frozen.segment.is_empty());
    }

    #[test]
    fn over_cap_skipped_empty_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("memories")).unwrap();
        std::fs::write(user_path(dir.path()), "").unwrap();
        std::fs::write(notes_path(dir.path()), "x".repeat(10_000)).unwrap();
        let frozen = freeze(dir.path(), &Settings::defaults());
        assert!(frozen.segment.is_empty());
    }

    #[test]
    fn memory_kind_parse_and_set_roundtrip() {
        assert!(matches!(
            MemoryKind::parse("soul").unwrap(),
            MemoryKind::Soul
        ));
        assert!(matches!(
            MemoryKind::parse("USER").unwrap(),
            MemoryKind::User
        ));
        assert!(MemoryKind::parse("wiki").is_err());
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings::defaults();
        set(
            dir.path(),
            MemoryKind::User,
            "Ada likes terse answers",
            &settings,
        )
        .unwrap();
        let frozen = freeze(dir.path(), &settings);
        assert!(frozen.segment.contains("Ada likes terse answers"));
        let shown = show(dir.path()).unwrap();
        assert!(shown.contains("Ada likes terse answers"));
        let err = set(
            dir.path(),
            MemoryKind::Soul,
            &"s".repeat(settings.memory_soul_max_bytes + 1),
            &settings,
        )
        .unwrap_err();
        assert!(err.to_string().contains("exceeds"), "{err}");
    }

    #[test]
    fn legacy_ledger_drafts_are_versioned_and_non_destructive() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings::defaults();
        set(
            dir.path(),
            MemoryKind::User,
            "Ada prefers concise answers",
            &settings,
        )
        .unwrap();
        let drafts = legacy_ledger_drafts(dir.path(), &settings);
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].source_ref.kind, "legacy_memory_file");
        assert!(drafts[0]
            .source_ref
            .version
            .as_deref()
            .unwrap()
            .starts_with("sha256:"));
        assert!(
            user_path(dir.path()).exists(),
            "migration must retain source files"
        );
    }
}
